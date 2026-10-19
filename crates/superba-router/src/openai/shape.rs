//! Request shaping on the OpenAI ingress (harness research), kept to what the
//! tools are known to need:
//!
//! - **Privacy**: top-level `dsh_*` fields (dsh's session-log upload
//!   `dsh_session_log`, up to 8 MiB of the whole conversation, and
//!   `dsh_plugin_packages`) never leave the router, on any ingress.
//! - **`store: false`** is added to a Responses request that does not say
//!   (ZCode omits it) and does not continue a stored response
//!   (`previous_response_id`), toward every member. Responses defaults to
//!   `store: true`, which keeps the conversation at upstreams that store
//!   (OpenAI, Azure, many gateways); every router member serves Codex, which
//!   always sends `store: false`, so every member accepts it.
//! - **Fields a member refuses** ([`super::config::OpenAiMember::drop_fields`],
//!   and on Chat Completions the extra reasoning fields ZCode sends at once
//!   — `thinking`, `enable_thinking`, `reasoning`, `reasoning_effort` — left
//!   out once a member answered 400 / 422 naming one ([`rejected_field`])).
//!
//! Responses bodies are changed at the top level only: every other member
//! keeps its exact bytes.

use std::borrow::Cow;

use serde_json::Value;
use serde_json::value::RawValue;

use crate::stats::strip::split_object;

/// The extra reasoning fields some tools send on Chat Completions (ZCode
/// sends all of them), left out for a member that refused one.
pub const REASONING_EXTRAS: &[&str] = &[
    "thinking",
    "enable_thinking",
    "reasoning",
    "reasoning_effort",
];

/// A top-level field that never leaves the router (dsh's uploads).
pub fn is_private_field(key: &str) -> bool {
    key.starts_with("dsh_")
}

/// What [`shape_top`] changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Shaped {
    /// The new body.
    pub body: Vec<u8>,
    /// Top-level keys removed.
    pub removed: Vec<String>,
    /// `"store":false` was added.
    pub store_added: bool,
}

/// The top level of `body` without the members `drop` names and, with
/// `add_store` and no `store` member, with `"store":false` appended; `None`
/// when nothing changes (or `body` is not a JSON object).
pub fn shape_top(body: &[u8], drop: &dyn Fn(&str) -> bool, add_store: bool) -> Option<Shaped> {
    let top = split_object(body)?;
    let removed: Vec<String> = top
        .iter()
        .filter(|(k, _)| drop(k))
        .map(|(k, _)| k.to_string())
        .collect();
    let add_store = add_store && !top.iter().any(|(k, _)| k == "store");
    if removed.is_empty() && !add_store {
        return None;
    }
    let kept: Vec<(&Cow<'_, str>, &RawValue)> = top
        .iter()
        .filter(|(k, _)| !drop(k))
        .map(|(k, v)| (k, *v))
        .collect();
    let mut out = Vec::with_capacity(body.len() + 16);
    out.push(b'{');
    for (i, (key, value)) in kept.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        out.extend_from_slice(serde_json::to_string(key.as_ref()).ok()?.as_bytes());
        out.push(b':');
        out.extend_from_slice(value.get().as_bytes());
    }
    if add_store {
        if !kept.is_empty() {
            out.push(b',');
        }
        out.extend_from_slice(br#""store":false"#);
    }
    out.push(b'}');
    Some(Shaped {
        body: out,
        removed,
        store_added: add_store,
    })
}

/// Remove the private top-level fields of a parsed body; the keys removed.
pub fn drop_private(body: &mut Value) -> Vec<String> {
    let Some(obj) = body.as_object_mut() else {
        return Vec::new();
    };
    let keys: Vec<String> = obj
        .keys()
        .filter(|k| is_private_field(k))
        .cloned()
        .collect();
    for k in &keys {
        obj.remove(k);
    }
    keys
}

/// Whether `word` occurs in `text` as a whole identifier (not inside
/// `reasoning_effort` when looking for `reasoning`).
fn has_word(text: &str, word: &str) -> bool {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        !before.is_some_and(ident) && !after.is_some_and(ident)
    })
}

/// Words of an answer refusing a request field.
const REFUSAL_WORDS: &[&str] = &[
    "unrecognized",
    "unrecognised",
    "unknown",
    "unsupported",
    "not supported",
    "not permitted",
    "not allowed",
    "extra",
    "unexpected",
    "invalid",
];

/// The field of `candidates` a 400 / 422 answer `body` refuses (its
/// `error.param`, or the field named in its message next to a refusal word:
/// "Unrecognized request argument supplied: thinking", "Extra inputs are not
/// permitted", ...).
pub fn rejected_field(body: &[u8], candidates: &[&str]) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let err = v.get("error").unwrap_or(&v);
    if let Some(param) = err.get("param").and_then(Value::as_str)
        && let Some(c) = candidates.iter().find(|c| **c == param)
    {
        return Some((*c).to_owned());
    }
    let message = [err.get("message"), v.get("message"), v.get("detail")]
        .into_iter()
        .flatten()
        .map(|m| match m {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    if !REFUSAL_WORDS.iter().any(|w| message.contains(w)) {
        return None;
    }
    candidates
        .iter()
        .find(|c| has_word(&message, c))
        .map(|c| (*c).to_owned())
}
