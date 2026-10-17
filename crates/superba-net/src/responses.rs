//! Minimal authenticated Responses request (user-triggered health check, plan 4
//! #31; compatible-site validation, plan S10). Costs a few tokens.
//!
//! The request is what the Codex backend accepts on every path (`stream: true`,
//! `store: false`, `instructions`, one user message) and nothing else, so it
//! works on the Krill paths and on plain OpenAI-compatible sites alike.

use serde_json::{Value, json};

use crate::classify::{error_message, short};
use crate::http::Answer;

/// Instructions of the probe request.
const PROBE_INSTRUCTIONS: &str = "Reply with exactly one word: pong.";
/// User message of the probe request.
const PROBE_INPUT: &str = "ping";

/// Body of `POST <openai_base_url>/responses`.
pub(crate) fn responses_body(model: &str) -> Vec<u8> {
    json!({
        "model": model,
        "instructions": PROBE_INSTRUCTIONS,
        "input": [message("user", "input_text", PROBE_INPUT)],
        "stream": true,
        "store": false,
    })
    .to_string()
    .into_bytes()
}

/// Body of `POST <openai_base_url>/responses/compact` (Codex remote compaction:
/// model, instructions and the items to compact; answers `{"output": [...]}`).
pub(crate) fn compact_body(model: &str) -> Vec<u8> {
    json!({
        "model": model,
        "instructions": PROBE_INSTRUCTIONS,
        "input": [
            message("user", "input_text", PROBE_INPUT),
            message("assistant", "output_text", "pong"),
        ],
    })
    .to_string()
    .into_bytes()
}

fn message(role: &str, kind: &str, text: &str) -> Value {
    json!({"type": "message", "role": role, "content": [{"type": kind, "text": text}]})
}

/// Data payloads of the events of an SSE stream, following the WHATWG
/// event-stream rules: lines end with CRLF, LF or CR; lines starting with `:` are
/// comments; the `data` lines of one event are joined with `\n`; a blank line
/// dispatches the event. With `complete_only`, an event that has not been
/// dispatched yet (stream still open) is left out.
pub(crate) fn sse_events(text: &str, complete_only: bool) -> Vec<String> {
    fn take_line(rest: &str) -> Option<(&str, &str)> {
        let i = rest.find(['\r', '\n'])?;
        let skip = if rest[i..].starts_with("\r\n") { 2 } else { 1 };
        Some((&rest[..i], &rest[i + skip..]))
    }
    let mut events = Vec::new();
    let mut data: Vec<&str> = Vec::new();
    let mut has_data = false;
    let mut rest = text;
    loop {
        let (line, terminated) = match take_line(rest) {
            Some((line, next)) => {
                rest = next;
                (line, true)
            }
            None => (std::mem::take(&mut rest), false),
        };
        if !terminated && (complete_only || line.is_empty()) {
            break;
        }
        if line.is_empty() {
            if has_data {
                events.push(data.join("\n"));
            }
            data.clear();
            has_data = false;
        } else if !line.starts_with(':') {
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            if field == "data" {
                data.push(value.strip_prefix(' ').unwrap_or(value));
                has_data = true;
            }
        }
        if !terminated {
            break;
        }
    }
    if !complete_only && has_data {
        events.push(data.join("\n"));
    }
    events
}

/// Outcome of one event payload: `Some(Ok)` for a finished response,
/// `Some(Err)` for a failure, `None` for anything else.
fn terminal(payload: &str) -> Option<Result<(), String>> {
    let payload = payload.trim();
    if payload.is_empty() || payload == "[DONE]" {
        return None;
    }
    let Ok(ev) = serde_json::from_str::<Value>(payload) else {
        // Lenient fallback for servers that omit the blank line between events:
        // try each data line on its own.
        if payload.contains('\n') {
            return payload.lines().find_map(terminal);
        }
        return None;
    };
    match ev.get("type").and_then(Value::as_str) {
        Some("response.completed" | "response.incomplete") => Some(Ok(())),
        Some("response.failed") => Some(Err(short(
            ev.pointer("/response/error/message")
                .and_then(Value::as_str)
                .unwrap_or("response.failed"),
        ))),
        Some("error") => Some(Err(short(
            ev.get("message")
                .or_else(|| ev.pointer("/error/message"))
                .and_then(Value::as_str)
                .unwrap_or("error event"),
        ))),
        _ => None,
    }
}

/// Bytes searched for a terminal event marker before parsing the stream.
const TERMINAL_SCAN_WINDOW: usize = 256 * 1024;

/// Early-stop predicate for [`crate::http::Request::until`]: true once a
/// terminal event has been fully received (servers may keep the stream open).
pub(crate) fn stream_finished(body: &[u8]) -> bool {
    let tail = &body[body.len().saturating_sub(TERMINAL_SCAN_WINDOW)..];
    let has_marker = [
        &b"response.completed"[..],
        b"response.incomplete",
        b"response.failed",
        b"\"error\"",
    ]
    .iter()
    .any(|m| tail.windows(m.len()).any(|w| w == *m));
    if !has_marker {
        return false;
    }
    // A trailing CR may be the first half of a CRLF still in flight.
    let body = body.strip_suffix(b"\r").unwrap_or(body);
    sse_events(&String::from_utf8_lossy(body), true)
        .iter()
        .any(|e| terminal(e).is_some())
}

/// Judge a 2xx Responses answer: an SSE stream must reach `response.completed`
/// (or `response.incomplete`, which still proves the path works); a JSON answer
/// must carry a completed status or an `output` array.
pub(crate) fn judge_responses(a: &Answer) -> Result<(), String> {
    let text = String::from_utf8_lossy(&a.body);
    let looks_sse = a
        .content_type
        .as_deref()
        .is_some_and(|c| c.contains("event-stream"))
        || text.trim_start().starts_with("event:")
        || text.trim_start().starts_with("data:");
    if looks_sse {
        if let Some(outcome) = sse_events(&text, false).iter().find_map(|e| terminal(e)) {
            return outcome;
        }
        return Err(if a.truncated {
            "stream too long, no response.completed seen".to_owned()
        } else {
            "stream ended without response.completed".to_owned()
        });
    }
    let v: Value = serde_json::from_slice(&a.body)
        .map_err(|_| "answer is neither an event stream nor JSON".to_owned())?;
    if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
        return Err(error_message(err.to_string().as_bytes())
            .unwrap_or_else(|| "error in response".to_owned()));
    }
    match v.get("status").and_then(Value::as_str) {
        Some("completed" | "incomplete") => Ok(()),
        Some(other) => Err(format!("response status {other}")),
        None if v.get("output").is_some_and(Value::is_array) => Ok(()),
        None => Err("JSON answer without status or output".to_owned()),
    }
}

/// A compact answer is a JSON object with an `output` array.
pub(crate) fn judge_compact(a: &Answer) -> bool {
    a.is_success()
        && serde_json::from_slice::<Value>(&a.body)
            .ok()
            .is_some_and(|v| v.get("output").is_some_and(Value::is_array))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn answer(ct: &str, body: &str) -> Answer {
        Answer {
            status: 200,
            edge_header: None,
            content_type: Some(ct.into()),
            cors: false,
            location: None,
            body: body.as_bytes().to_vec(),
            truncated: false,
            elapsed: Duration::from_millis(5),
            retry_after: None,
            etag: None,
        }
    }

    #[test]
    fn request_bodies_have_the_codex_shape() {
        let v: Value = serde_json::from_slice(&responses_body("gpt-x")).unwrap();
        assert_eq!(v["model"], "gpt-x");
        assert_eq!(v["stream"], true);
        assert_eq!(v["store"], false);
        assert_eq!(v["input"][0]["content"][0]["type"], "input_text");
        assert!(v.get("max_output_tokens").is_none());
        let v: Value = serde_json::from_slice(&compact_body("gpt-x")).unwrap();
        assert_eq!(v["input"].as_array().unwrap().len(), 2);
        assert!(v.get("stream").is_none());
    }

    #[test]
    fn sse_streams_must_complete() {
        let ok = "event: response.created\ndata: {\"type\":\"response.created\"}\n\nevent: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{}}\n\n";
        assert_eq!(judge_responses(&answer("text/event-stream", ok)), Ok(()));
        let failed = "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"model not found\"}}}\n";
        assert_eq!(
            judge_responses(&answer("text/event-stream", failed)),
            Err("model not found".into())
        );
        let err = "data: {\"type\":\"error\",\"message\":\"overloaded\"}\n";
        assert_eq!(
            judge_responses(&answer("text/event-stream", err)),
            Err("overloaded".into())
        );
        let cut = "data: {\"type\":\"response.created\"}\n";
        assert!(judge_responses(&answer("text/event-stream", cut)).is_err());
        // SSE recognised without a content type, [DONE] tolerated.
        let bare = "data: [DONE]\ndata: {\"type\":\"response.incomplete\"}\n";
        assert_eq!(judge_responses(&answer("", bare)), Ok(()));
    }

    #[test]
    fn sse_parsing_follows_the_event_stream_rules() {
        // Multi-line data, CRLF and CR line endings, comments, field without space.
        let text = ": comment\r\ndata: {\"type\":\r\ndata:\"response.completed\"}\r\n\r\nevent: x\rdata: two\r\r";
        assert_eq!(
            sse_events(text, true),
            vec![
                "{\"type\":\n\"response.completed\"}".to_owned(),
                "two".to_owned()
            ]
        );
        assert_eq!(judge_responses(&answer("text/event-stream", text)), Ok(()));
        // An event without its blank line counts only at end of stream.
        let open = "data: {\"type\":\"response.completed\"}\n";
        assert!(sse_events(open, true).is_empty());
        assert_eq!(sse_events(open, false).len(), 1);
        assert!(!stream_finished(open.as_bytes()));
        assert!(stream_finished(
            b"data: {\"type\":\"response.completed\"}\n\n"
        ));
        assert!(stream_finished(
            b"data: {\"type\":\"error\",\"message\":\"x\"}\r\n\r\n"
        ));
        assert!(!stream_finished(
            b"data: {\"type\":\"response.created\"}\n\n"
        ));
        assert!(!stream_finished(
            b"data: {\"type\":\"response.completed\"}\r\n\r"
        ));
    }

    #[test]
    fn json_answers_are_judged_by_status_or_output() {
        let j = |b: &str| judge_responses(&answer("application/json", b));
        assert_eq!(j(r#"{"status":"completed","output":[]}"#), Ok(()));
        assert_eq!(j(r#"{"output":[]}"#), Ok(()));
        assert!(j(r#"{"status":"failed"}"#).is_err());
        assert_eq!(j(r#"{"error":{"message":"nope"}}"#), Err("nope".into()));
        assert!(j(r#"{"id":"x"}"#).is_err());
        assert!(j("<html>").is_err());
        assert!(judge_compact(&answer(
            "application/json",
            r#"{"output":[]}"#
        )));
        assert!(!judge_compact(&answer("application/json", r#"{"x":1}"#)));
    }
}
