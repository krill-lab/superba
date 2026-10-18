//! Requests to an Anthropic-native upstream (Krill's `/v1/messages`,
//! DeepSeek / Kimi / GLM Anthropic endpoints): the client's request with the
//! member's key and per-member adjustments.
//!
//! Headers: everything Claude Code sent except hop-by-hop headers, `Host`,
//! `Content-Length`, its credential (`Authorization` / `x-api-key` hold the
//! router token, never forwarded), `Accept-Encoding` (the answer is read for
//! the request log) and the router's internal headers; `anthropic-beta`
//! filtered by the member's [`BetaPolicy`](super::config::BetaPolicy);
//! `anthropic-version` added when missing.
//!
//! Body (re-serialised only when something changes): the upstream model name,
//! the member's `drop_fields` removed, and `role: "system"` messages folded
//! into user text for members that do not take them.

use bytes::Bytes;
use http::header::{ACCEPT_ENCODING, AUTHORIZATION, CONTENT_LENGTH, HOST};
use http::{HeaderMap, HeaderValue};
use serde_json::{Value, json};

use super::catalog::ANTHROPIC_VERSION;
use super::rt::MemberClaude;
use crate::forward::{CODEX_HOST_HEADER, HARNESS_HEADER, HOP_BY_HOP, connection_tokens};

/// The request headers for member `c`.
pub(crate) fn headers(src: &HeaderMap, c: &MemberClaude) -> HeaderMap {
    let listed = connection_tokens(src);
    let mut out = HeaderMap::with_capacity(src.len() + 2);
    let mut betas: Vec<String> = Vec::new();
    for (name, value) in src {
        let n = name.as_str();
        if HOP_BY_HOP.contains(&n) || listed.iter().any(|t| t == n) {
            continue;
        }
        if name == HOST
            || name == CONTENT_LENGTH
            || name == AUTHORIZATION
            || name == ACCEPT_ENCODING
            || n == super::API_KEY_HEADER
            || n == CODEX_HOST_HEADER
            || n == HARNESS_HEADER
        {
            continue;
        }
        if n == "anthropic-beta" {
            if let Ok(v) = value.to_str() {
                betas.push(v.to_owned());
            }
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    if let Some(v) = c.cfg.betas.filter(&betas.join(","))
        && let Ok(v) = HeaderValue::from_str(&v)
    {
        out.insert("anthropic-beta", v);
    }
    if !out.contains_key("anthropic-version") {
        out.insert(
            "anthropic-version",
            HeaderValue::from_static(ANTHROPIC_VERSION),
        );
    }
    out.insert(c.auth.0.clone(), c.auth.1.clone());
    out
}

/// The body for member `c` with `model` upstream: `(body, notes)`; `None`
/// keeps the client's bytes.
pub(crate) fn body(req: &Value, c: &MemberClaude, model: &str) -> (Option<Bytes>, Vec<String>) {
    let mut v = req.clone();
    let mut notes = Vec::new();
    let mut changed = false;
    let Some(obj) = v.as_object_mut() else {
        return (None, notes);
    };
    if obj.get("model").and_then(Value::as_str) != Some(model) {
        obj.insert("model".into(), model.into());
        changed = true;
    }
    for f in &c.cfg.drop_fields {
        if obj.remove(f).is_some() {
            changed = true;
            notes.push(format!("field {f} left out for this provider"));
        }
    }
    if !c.cfg.system_messages {
        let folded = fold_system_messages(&mut v);
        if folded > 0 {
            changed = true;
            notes.push(format!("{folded} system message(s) sent as user text"));
        }
    }
    if !changed {
        return (None, notes);
    }
    (serde_json::to_vec(&v).ok().map(Bytes::from), notes)
}

/// The text of a message's content (a string, or its text blocks).
fn text_of(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Fold `role: "system"` messages into user text (the previous user message,
/// else the next one, else a user message of their own).
fn fold_system_messages(v: &mut Value) -> u32 {
    let Some(messages) = v.get_mut("messages").and_then(Value::as_array_mut) else {
        return 0;
    };
    let mut folded = 0;
    let mut i = 0;
    while i < messages.len() {
        if messages[i].get("role").and_then(Value::as_str) != Some("system") {
            i += 1;
            continue;
        }
        let text = text_of(messages[i].get("content"));
        let block = json!({"type": "text", "text": format!("<system-reminder>\n{text}\n</system-reminder>")});
        let is_user = |m: &Value| m.get("role").and_then(Value::as_str) == Some("user");
        let target = if i > 0 && is_user(&messages[i - 1]) {
            Some(i - 1)
        } else if i + 1 < messages.len() && is_user(&messages[i + 1]) {
            Some(i + 1)
        } else {
            None
        };
        folded += 1;
        match target {
            Some(t) => {
                append_block(&mut messages[t], block);
                messages.remove(i);
            }
            None => {
                messages[i] = json!({"role": "user", "content": [block]});
                i += 1;
            }
        }
    }
    folded
}

fn append_block(message: &mut Value, block: Value) {
    let content = message
        .get_mut("content")
        .map(Value::take)
        .unwrap_or(Value::Null);
    let mut blocks = match content {
        Value::String(s) => vec![json!({"type": "text", "text": s})],
        Value::Array(a) => a,
        _ => Vec::new(),
    };
    blocks.push(block);
    if let Some(obj) = message.as_object_mut() {
        obj.insert("content".into(), Value::Array(blocks));
    }
}
