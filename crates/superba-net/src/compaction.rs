//! Codex remote compaction check ([`superba_core::LineProber::probe_compaction`],
//! GitHub issue 3).
//!
//! Codex 0.155+ never calls `…/responses/compact`: it compacts by sending the
//! conversation with a `compaction_trigger` input item to the ordinary
//! `…/responses` endpoint and expects exactly one `compaction` output item
//! (`encrypted_content`) back. The check sends that request for a
//! two-message conversation (a few tokens) and reads the answer:
//!
//! | Answer | Result |
//! |---|---|
//! | a stream (or JSON) with exactly one `compaction` item | native |
//! | a completed answer without one (the trigger was ignored) | unsupported |
//! | 400 / 404 / 422 / 501 whose error names the item (`compaction_trigger`, an invalid input type, …) | unsupported |
//! | `response.failed` / `error` event naming the item | unsupported |
//! | 401 / 403 / 429 / 5xx, a refused model, an edge interception, no answer, anything else | unknown |
//!
//! A 404 / 405 that does not name the item means the address has no Responses
//! endpoint at all, which says nothing about compaction (unknown).

use serde_json::{Value, json};
use superba_core::{
    CompactionCheck, CompactionSupport, Error, ResolvedEndpoints, Result, Secret,
    codex::{COMPACTION_ITEM, COMPACTION_TRIGGER_ITEM},
};

use crate::HttpLineProber;
use crate::classify::{edge_detail, edge_intercepted_stream, error_message, short};
use crate::http::{
    ACCEPT_SSE, Answer, Request, Transport, TransportKind, display_url, ensure_key_target, redact,
};
use crate::keys::join_base;
use crate::responses::{sse_events, stream_finished};

/// Body bytes kept for the answer (a compaction blob is a few KiB).
const BODY_LIMIT: usize = 4 * 1024 * 1024;
/// Instructions of the check request.
const INSTRUCTIONS: &str = "Reply with exactly one word: pong.";

/// Item types that hold a compacted history (`compaction_summary` is an alias
/// Codex accepts).
pub(crate) fn is_compaction_item(kind: &str) -> bool {
    kind == COMPACTION_ITEM || kind == "compaction_summary"
}

/// Body of the check: `POST <openai_base_url>/responses`, streamed, not
/// stored, a user and an assistant message and the trigger.
pub(crate) fn compaction_body(model: &str) -> Vec<u8> {
    json!({
        "model": model,
        "instructions": INSTRUCTIONS,
        "input": [
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "ping"}]},
            {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "pong"}]},
            {"type": COMPACTION_TRIGGER_ITEM},
        ],
        "stream": true,
        "store": false,
    })
    .to_string()
    .into_bytes()
}

/// An error text that blames the compaction request itself (the trigger item,
/// an unknown input type) rather than the key, the model or the server.
pub(crate) fn names_compaction(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    if m.contains("compaction") || m.contains("compact") {
        return true;
    }
    let about_input = m.contains("input") || m.contains("item");
    let refusal = [
        "invalid value",
        "unknown",
        "unsupported",
        "not supported",
        "invalid type",
        "invalid_union",
        "unrecognized",
        "unexpected",
        "does not match",
        "is not one of",
    ]
    .iter()
    .any(|w| m.contains(w));
    about_input && refusal
}

/// An error text about the model (not found, not allowed): the check could
/// not run, which says nothing about compaction.
fn names_model(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("model")
}

/// Compaction items in a list of output items (`output` of a JSON answer or
/// of `response.completed`).
fn count_items(output: Option<&Value>) -> usize {
    output
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|i| {
                    i.get("type")
                        .and_then(Value::as_str)
                        .is_some_and(is_compaction_item)
                })
                .count()
        })
        .unwrap_or(0)
}

/// What one event of the stream says.
enum Event {
    /// An output item finished; `true` when it is a compaction item.
    Item(bool),
    /// The response completed; the compaction items in its `output`.
    Completed(usize),
    /// The response failed or an error event came, with its message.
    Failed(String),
    /// Anything else.
    Other,
}

fn event(payload: &str) -> Event {
    let Ok(v) = serde_json::from_str::<Value>(payload.trim()) else {
        return Event::Other;
    };
    match v.get("type").and_then(Value::as_str) {
        Some("response.output_item.done") => Event::Item(
            v.pointer("/item/type")
                .and_then(Value::as_str)
                .is_some_and(is_compaction_item),
        ),
        Some("response.completed" | "response.incomplete") => {
            Event::Completed(count_items(v.pointer("/response/output")))
        }
        Some("response.failed") => Event::Failed(short(
            v.pointer("/response/error/message")
                .and_then(Value::as_str)
                .unwrap_or("response.failed"),
        )),
        Some("error") => Event::Failed(short(
            v.get("message")
                .or_else(|| v.pointer("/error/message"))
                .and_then(Value::as_str)
                .unwrap_or("error event"),
        )),
        _ => Event::Other,
    }
}

/// The verdict on the number of compaction items of a finished answer.
fn by_count(status: u16, n: usize) -> CompactionCheck {
    match n {
        1 => CompactionCheck::new(
            CompactionSupport::Native,
            Some(status),
            "answered with one compaction item",
        ),
        0 => CompactionCheck::new(
            CompactionSupport::Unsupported,
            Some(status),
            "answered without a compaction item (the compaction request was ignored)",
        ),
        n => CompactionCheck::new(
            CompactionSupport::Unsupported,
            Some(status),
            format!("answered with {n} compaction items (Codex expects exactly one)"),
        ),
    }
}

/// A failure message: unsupported when it names the compaction request.
fn by_message(status: Option<u16>, message: String) -> CompactionCheck {
    if names_compaction(&message) {
        CompactionCheck::new(CompactionSupport::Unsupported, status, message)
    } else if names_model(&message) {
        CompactionCheck::unknown(status, format!("the check's model was refused: {message}"))
    } else {
        CompactionCheck::unknown(status, message)
    }
}

/// Judge a 2xx answer (SSE stream or JSON).
fn judge_success(a: &Answer) -> CompactionCheck {
    let text = String::from_utf8_lossy(&a.body);
    let looks_sse = a
        .content_type
        .as_deref()
        .is_some_and(|c| c.contains("event-stream"))
        || text.trim_start().starts_with("event:")
        || text.trim_start().starts_with("data:");
    if looks_sse {
        let mut items = 0usize;
        for payload in sse_events(&text, false) {
            match event(&payload) {
                Event::Item(true) => items += 1,
                Event::Item(false) | Event::Other => {}
                // Items that only appear in the final output still count.
                Event::Completed(n) => return by_count(a.status, items.max(n)),
                Event::Failed(m) => return by_message(Some(a.status), m),
            }
        }
        return CompactionCheck::unknown(
            Some(a.status),
            if a.truncated {
                "the answer was too long to read to its end"
            } else {
                "the stream ended without response.completed"
            },
        );
    }
    let Ok(v) = serde_json::from_slice::<Value>(&a.body) else {
        return CompactionCheck::unknown(
            Some(a.status),
            "the answer is neither an event stream nor JSON",
        );
    };
    if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
        let m = error_message(err.to_string().as_bytes()).unwrap_or_else(|| "error".to_owned());
        return by_message(Some(a.status), m);
    }
    if v.get("output").is_some_and(Value::is_array) {
        return by_count(a.status, count_items(v.get("output")));
    }
    CompactionCheck::unknown(Some(a.status), "JSON answer without output")
}

/// Judge one answer to the check request.
pub(crate) fn judge_compaction(a: &Answer) -> CompactionCheck {
    if edge_intercepted_stream(a) {
        return CompactionCheck::unknown(Some(a.status), edge_detail(a));
    }
    if a.is_success() {
        return judge_success(a);
    }
    let message = error_message(&a.body);
    match a.status {
        401 | 403 => CompactionCheck::unknown(Some(a.status), "the key was rejected"),
        429 => CompactionCheck::unknown(Some(a.status), "rate limited; try again later"),
        400 | 404 | 405 | 409 | 415 | 422 | 501 => match message {
            Some(m) if names_compaction(&m) => {
                CompactionCheck::new(CompactionSupport::Unsupported, Some(a.status), m)
            }
            Some(m) if a.status == 404 || a.status == 405 => CompactionCheck::unknown(
                Some(a.status),
                format!("no Responses endpoint at this address: {m}"),
            ),
            None if a.status == 404 || a.status == 405 => {
                CompactionCheck::unknown(Some(a.status), "no Responses endpoint at this address")
            }
            Some(m) => by_message(Some(a.status), m),
            None => CompactionCheck::unknown(Some(a.status), format!("HTTP {}", a.status)),
        },
        s => CompactionCheck::unknown(
            Some(s),
            match message {
                Some(m) => format!("HTTP {s}: {m}"),
                None => format!("HTTP {s}"),
            },
        ),
    }
}

/// An inconclusive check after a transport failure.
fn transport_check(t: &Transport) -> CompactionCheck {
    CompactionCheck::unknown(
        None,
        match t.kind {
            TransportKind::Timeout => "no complete answer before the timeout".to_owned(),
            _ => short(&t.message),
        },
    )
}

impl HttpLineProber {
    /// See [`superba_core::LineProber::probe_compaction`].
    pub(crate) async fn probe_compaction_impl(
        &self,
        endpoints: &ResolvedEndpoints,
        key: &Secret,
        model: &str,
    ) -> Result<CompactionCheck> {
        let base = &endpoints.openai_base_url;
        Self::ensure_key(key)?;
        ensure_key_target(base, key)?;
        if base.origin() != endpoints.origin.url().origin() {
            return Err(Error::invalid(format!(
                "openai_base_url {} is not on the endpoints' origin {}",
                display_url(base),
                endpoints.origin
            )));
        }
        let model = model.trim();
        if model.is_empty() {
            return Err(Error::invalid(
                "a model is required for the compaction check",
            ));
        }
        let url = join_base(base, "responses");
        let req = Request::get(url, self.config.responses_timeout, BODY_LIMIT)
            .with_key(key)
            .with_json(compaction_body(model), ACCEPT_SSE)
            .until(stream_finished);
        let mut check = match self.http.send(req).await {
            Ok(a) => judge_compaction(&a),
            Err(t) => transport_check(&t),
        };
        if let Some(d) = &check.detail {
            check.detail = Some(redact(d, Some(key)));
        }
        Ok(check)
    }
}
