//! Synthetic Responses streams modelled on the shapes captured from the real
//! app (spike router-stats §1.1), and helpers to check what Codex receives.
#![allow(dead_code)]

use serde_json::{Value, json};
use superba_core::codex::STATS_LINE_MARKER;

/// One event as `(type, payload)`.
pub type Ev = (String, Value);

fn ev(t: &str, mut v: Value) -> Ev {
    v["type"] = json!(t);
    (t.to_owned(), v)
}

/// `event: <type>\ndata: <json>\n\n` per event (`crlf`: CRLF line ends).
pub fn sse(events: &[Ev], crlf: bool) -> String {
    let nl = if crlf { "\r\n" } else { "\n" };
    events
        .iter()
        .map(|(t, v)| format!("event: {t}{nl}data: {v}{nl}{nl}"))
        .collect()
}

/// The usage object of the fixtures.
pub fn usage(input: u64, cached: u64, output: u64) -> Value {
    json!({
        "input_tokens": input,
        "input_tokens_details": {"cached_tokens": cached, "cache_write_tokens": 0},
        "output_tokens": output,
        "output_tokens_details": {"reasoning_tokens": 0},
        "total_tokens": input + output,
    })
}

/// Item kinds of a fixture response.
#[derive(Clone, Debug)]
pub enum Item {
    /// An assistant message with `phase` (None = no phase) and text deltas.
    Message {
        id: &'static str,
        phase: Option<&'static str>,
        deltas: Vec<&'static str>,
    },
    /// An encrypted reasoning item that arrives whole.
    Reasoning { id: &'static str },
    /// A function call with argument deltas.
    Tool { id: &'static str },
}

/// How the response ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    Completed,
    Failed,
    Incomplete,
    /// The stream stops without a terminal event.
    Cut,
}

/// A whole response stream.
pub fn response(items: &[Item], ending: Ending, u: Value) -> Vec<Ev> {
    let mut out = vec![
        ev(
            "response.created",
            json!({"sequence_number": 0, "response": {"id": "resp_1", "model": "gpt-x", "status": "in_progress", "usage": null, "output": []}}),
        ),
        ev(
            "response.in_progress",
            json!({"sequence_number": 1, "response": {"id": "resp_1", "status": "in_progress"}}),
        ),
    ];
    let mut done_items = Vec::new();
    for (i, item) in items.iter().enumerate() {
        match item {
            Item::Message { id, phase, deltas } => {
                let mut added = json!({"id": id, "type": "message", "role": "assistant", "status": "in_progress", "content": []});
                if let Some(p) = phase {
                    added["phase"] = json!(p);
                }
                out.push(ev(
                    "response.output_item.added",
                    json!({"output_index": i, "item": added}),
                ));
                out.push(ev(
                    "response.content_part.added",
                    json!({"item_id": id, "output_index": i, "content_index": 0, "part": {"type": "output_text", "text": "", "annotations": []}}),
                ));
                for d in deltas {
                    out.push(ev(
                        "response.output_text.delta",
                        json!({"item_id": id, "output_index": i, "content_index": 0, "delta": d, "logprobs": [], "obfuscation": "xYz"}),
                    ));
                }
                let text: String = deltas.concat();
                out.push(ev(
                    "response.metadata",
                    json!({"response_id": "resp_1", "metadata": {"m": 1}}),
                ));
                out.push(ev(
                    "response.output_text.done",
                    json!({"item_id": id, "output_index": i, "content_index": 0, "text": text, "logprobs": []}),
                ));
                out.push(ev(
                    "response.content_part.done",
                    json!({"item_id": id, "output_index": i, "content_index": 0, "part": {"type": "output_text", "text": text, "annotations": []}}),
                ));
                let mut done = json!({"id": id, "type": "message", "role": "assistant", "status": "completed", "content": [{"type": "output_text", "text": text, "annotations": []}]});
                if let Some(p) = phase {
                    done["phase"] = json!(p);
                }
                out.push(ev(
                    "response.output_item.done",
                    json!({"output_index": i, "item": done.clone()}),
                ));
                done_items.push(done);
            }
            Item::Reasoning { id } => {
                let item = json!({"id": id, "type": "reasoning", "summary": [], "encrypted_content": "gAAAA"});
                out.push(ev(
                    "response.output_item.added",
                    json!({"output_index": i, "item": item.clone()}),
                ));
                out.push(ev(
                    "response.output_item.done",
                    json!({"output_index": i, "item": item.clone()}),
                ));
                done_items.push(item);
            }
            Item::Tool { id } => {
                let added = json!({"id": id, "type": "function_call", "name": "shell", "call_id": "call_1", "arguments": ""});
                out.push(ev(
                    "response.output_item.added",
                    json!({"output_index": i, "item": added}),
                ));
                for d in ["{\"cmd\":", "\"ls\"}"] {
                    out.push(ev(
                        "response.function_call_arguments.delta",
                        json!({"item_id": id, "output_index": i, "delta": d}),
                    ));
                }
                let done = json!({"id": id, "type": "function_call", "name": "shell", "call_id": "call_1", "arguments": "{\"cmd\":\"ls\"}"});
                out.push(ev(
                    "response.output_item.done",
                    json!({"output_index": i, "item": done.clone()}),
                ));
                done_items.push(done);
            }
        }
    }
    match ending {
        Ending::Completed => out.push(ev(
            "response.completed",
            json!({"response": {"id": "resp_1", "model": "gpt-x-2026", "status": "completed", "service_tier": "default", "output": done_items, "usage": u}}),
        )),
        Ending::Failed => out.push(ev(
            "response.failed",
            json!({"response": {"id": "resp_1", "status": "failed", "error": {"code": "server_error", "message": "boom"}, "usage": null}}),
        )),
        Ending::Incomplete => out.push(ev(
            "response.incomplete",
            json!({"response": {"id": "resp_1", "status": "incomplete", "incomplete_details": {"reason": "max_output_tokens"}, "output": done_items, "usage": u}}),
        )),
        Ending::Cut => {}
    }
    out
}

/// The plain final answer fixture: one `final_answer` message.
pub fn text_answer() -> Vec<Ev> {
    response(
        &[Item::Message {
            id: "msg_1",
            phase: Some("final_answer"),
            deltas: vec!["Hello", " wor", "ld."],
        }],
        Ending::Completed,
        usage(4002, 3328, 51),
    )
}

/// Parse an SSE byte stream into JSON events (`data:` payloads).
pub fn parse_sse(bytes: &[u8]) -> Vec<Value> {
    let text = String::from_utf8(bytes.to_vec()).expect("utf-8 stream");
    let text = text.replace("\r\n", "\n");
    text.split("\n\n")
        .filter(|b| !b.trim().is_empty())
        .map(|block| {
            let data: Vec<&str> = block
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(|d| d.strip_prefix(' ').unwrap_or(d))
                .collect();
            serde_json::from_str(&data.join("\n")).expect("event JSON")
        })
        .collect()
}

/// The text of `item_id` as Codex would assemble it, from every place it
/// appears. Panics when they disagree.
pub fn consistent_text(events: &[Value], item_id: &str) -> String {
    let mut deltas = String::new();
    let mut done = None;
    let mut part = None;
    let mut item = None;
    let mut completed = None;
    for e in events {
        match e["type"].as_str().unwrap_or_default() {
            "response.output_text.delta" if e["item_id"] == item_id => {
                deltas.push_str(e["delta"].as_str().unwrap());
            }
            "response.output_text.done" if e["item_id"] == item_id => {
                done = Some(e["text"].as_str().unwrap().to_owned());
            }
            "response.content_part.done" if e["item_id"] == item_id => {
                part = Some(e["part"]["text"].as_str().unwrap().to_owned());
            }
            "response.output_item.done" if e["item"]["id"] == item_id => {
                item = Some(e["item"]["content"][0]["text"].as_str().unwrap().to_owned());
            }
            "response.completed" => {
                for o in e["response"]["output"].as_array().unwrap() {
                    if o["id"] == item_id {
                        completed = Some(o["content"][0]["text"].as_str().unwrap().to_owned());
                    }
                }
            }
            _ => {}
        }
    }
    assert_eq!(Some(&deltas), done.as_ref(), "deltas vs output_text.done");
    assert_eq!(done, part, "output_text.done vs content_part.done");
    assert_eq!(part, item, "content_part.done vs output_item.done");
    if let Some(c) = completed {
        assert_eq!(item.as_ref(), Some(&c), "output_item.done vs completed");
    }
    deltas
}

/// The stats line at the end of `text`, if any.
pub fn line_of(text: &str) -> Option<&str> {
    let pos = text.rfind("\n\n")?;
    let tail = &text[pos + 2..];
    tail.contains(STATS_LINE_MARKER).then_some(tail)
}

/// A Responses request body with history: a user message, an assistant
/// message (with `assistant_text`), and a new user message.
pub fn request_body(model: &str, assistant_text: &str, extra: Value) -> Value {
    let mut body = json!({
        "model": model,
        "instructions": "be brief",
        "input": [
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hi ⚡\u{2060} user text stays"}]},
            {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": assistant_text}]},
            {"type": "function_call_output", "call_id": "c1", "output": "tool ⚡\u{2060} output stays"},
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "next"}]}
        ],
        "stream": true,
        "prompt_cache_key": "019a7c00-0000-7000-8000-0000000000e1",
    });
    if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            b.insert(k.clone(), v.clone());
        }
    }
    body
}

/// `x-codex-turn-metadata` for a user turn.
pub fn turn_metadata(turn_id: &str, kind: &str, source: &str) -> String {
    json!({
        "session_id": "019a7c00-0000-7000-8000-0000000000e1",
        "thread_id": "019a7c00-0000-7000-8000-0000000000e1",
        "turn_id": turn_id,
        "request_kind": kind,
        "thread_source": source,
    })
    .to_string()
}
