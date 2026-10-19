//! The tap of a Chat Completions stream: timings, usage, ids and the outcome
//! for the request log, and the stats line ([`super::line`]).
//!
//! Events pass one by one (an event split across chunks is kept back until
//! it completes). With the line, the chunk that ends the answer with
//! `finish_reason: "stop"` is held until the next event, because the usage
//! (`stream_options.include_usage`) comes in a chunk of its own after it:
//!
//! - a usage chunk (no choices), `[DONE]` or the end of the stream: the line
//!   goes into the final `delta.content` — a chunk of its own before the held
//!   one, or appended to the held chunk's own content — then the held events;
//! - anything else (another choice, more deltas, an error): the held events
//!   go out unchanged and the answer gets no line.
//!
//! No line when the answer calls tools (`tool_calls` / `function_call`
//! deltas), has several choices or ends otherwise (`length`, ...).
//!
//! Usage follows the router's semantics ([`crate::stats::usage`]): input
//! includes cached tokens.

use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use serde_json::{Value, json};

use crate::stats::sse::{Boundary, MAX_EVENT_BYTES, parse_event};
use crate::stats::tap::{LineSetup, Outcome, TapSummary, clip_id, render_line};
use crate::stats::usage::Usage;

/// Most bytes held for the line before giving up on it.
const MAX_HELD: usize = 1024 * 1024;

/// The usage of a Chat Completions `usage` object.
pub fn chat_usage(usage: &Value) -> Option<Usage> {
    if !usage.is_object() {
        return None;
    }
    serde_json::from_value(usage.clone())
        .ok()
        .and_then(Usage::from_wire)
}

/// What a chunk tells about its choices.
#[derive(Default)]
struct ChunkFacts {
    /// Output arrived (content, reasoning or a tool call).
    output: bool,
    tool_call: bool,
    /// A choice other than the first.
    other_choice: bool,
    /// The first choice's `finish_reason`.
    finish: Option<String>,
    /// The chunk has no choices (the usage chunk).
    no_choices: bool,
}

fn non_empty(v: Option<&Value>) -> bool {
    match v {
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Null) | None => false,
        Some(Value::Array(a)) => !a.is_empty(),
        Some(_) => true,
    }
}

fn facts(chunk: &Value) -> ChunkFacts {
    let mut f = ChunkFacts::default();
    let choices = chunk.get("choices").and_then(Value::as_array);
    f.no_choices = choices.is_none_or(|c| c.is_empty());
    for c in choices.into_iter().flatten() {
        let index = c.get("index").and_then(Value::as_u64).unwrap_or(0);
        if index != 0 {
            f.other_choice = true;
            continue;
        }
        if let Some(d) = c.get("delta") {
            let call = non_empty(d.get("tool_calls")) || non_empty(d.get("function_call"));
            f.tool_call |= call;
            f.output |= call
                || non_empty(d.get("content"))
                || non_empty(d.get("reasoning_content"))
                || non_empty(d.get("reasoning"))
                || non_empty(d.get("refusal"));
        }
        if let Some(r) = c.get("finish_reason").and_then(Value::as_str) {
            f.finish = Some(r.to_owned());
        }
    }
    f
}

/// The tap.
#[derive(Debug)]
pub struct ChatSse {
    t0: Instant,
    line: Option<LineSetup>,
    boundary: Boundary,
    partial: BytesMut,
    first_event: Option<Instant>,
    first_token: Option<Instant>,
    last_delta: Option<Instant>,
    end: Option<Instant>,
    deltas: u64,
    usage: Option<Usage>,
    model: Option<String>,
    id: Option<String>,
    outcome: Option<Outcome>,
    tool_calls: bool,
    several: bool,
    held: Vec<Bytes>,
    held_bytes: usize,
    /// The held chunk that ends the answer.
    held_chunk: Option<Value>,
    injected: bool,
}

impl ChatSse {
    /// A tap for a request received at `t0`; `line` when the answer may get
    /// the stats line.
    pub fn new(t0: Instant, line: Option<LineSetup>) -> Self {
        Self {
            t0,
            line,
            boundary: Boundary::default(),
            partial: BytesMut::new(),
            first_event: None,
            first_token: None,
            last_delta: None,
            end: None,
            deltas: 0,
            usage: None,
            model: None,
            id: None,
            outcome: None,
            tool_calls: false,
            several: false,
            held: Vec::new(),
            held_bytes: 0,
            held_chunk: None,
            injected: false,
        }
    }

    /// Process one chunk; `out` receives the bytes to send now.
    pub fn push_at(
        &mut self,
        chunk: Bytes,
        clock: &mut dyn FnMut() -> Instant,
        out: &mut Vec<Bytes>,
    ) {
        let mut pos = 0;
        while let Some(end) = self.boundary.find(&chunk, pos) {
            self.partial.extend_from_slice(&chunk[pos..end]);
            pos = end;
            let raw = self.partial.split().freeze();
            self.on_event(raw, clock(), out);
        }
        self.partial.extend_from_slice(&chunk[pos..]);
        if self.partial.len() > MAX_EVENT_BYTES {
            // Not an event stream worth reading: pass as it came.
            self.release(out);
            self.line = None;
            out.push(self.partial.split().freeze());
        }
    }

    /// The stream ended: the line when the answer ended with `stop`, then
    /// what is held or incomplete.
    pub fn finish(&mut self, out: &mut Vec<Bytes>) {
        if !self.partial.is_empty() {
            let raw = self.partial.split().freeze();
            self.on_event(raw, Instant::now(), out);
        }
        if self.held_chunk.is_some() {
            self.inject(out);
        }
        self.release(out);
    }

    fn release(&mut self, out: &mut Vec<Bytes>) {
        out.append(&mut self.held);
        self.held_bytes = 0;
        self.held_chunk = None;
    }

    fn hold(&mut self, raw: Bytes, out: &mut Vec<Bytes>) {
        self.held_bytes += raw.len();
        self.held.push(raw);
        if self.held_bytes > MAX_HELD {
            self.line = None;
            self.release(out);
        }
    }

    /// The held answer gets the line, then everything held goes out.
    fn inject(&mut self, out: &mut Vec<Bytes>) {
        let (Some(line), Some(mut chunk)) = (self.line.as_ref(), self.held_chunk.take()) else {
            self.release(out);
            return;
        };
        let suffix = render_line(line, &self.summary(), self.usage, None);
        let own = chunk
            .pointer_mut("/choices/0/delta/content")
            .and_then(|c| match c {
                Value::String(s) if !s.is_empty() => Some(s),
                _ => None,
            });
        match own {
            Some(text) => {
                // The ending chunk has text of its own: the line follows it.
                text.push_str(&suffix);
                if let Some(first) = self.held.first_mut() {
                    *first = Bytes::from(format!("data: {chunk}\n\n"));
                }
            }
            None => {
                let mut extra = json!({
                    "id": chunk.get("id").cloned().unwrap_or(Value::Null),
                    "object": chunk.get("object").cloned().unwrap_or_else(|| json!("chat.completion.chunk")),
                    "created": chunk.get("created").cloned().unwrap_or(json!(0)),
                    "model": chunk.get("model").cloned().unwrap_or(Value::Null),
                    "choices": [{"index": 0, "delta": {"content": suffix}, "finish_reason": null}],
                });
                if let (Some(fp), Some(obj)) =
                    (chunk.get("system_fingerprint"), extra.as_object_mut())
                {
                    obj.insert("system_fingerprint".into(), fp.clone());
                }
                out.push(Bytes::from(format!("data: {extra}\n\n")));
            }
        }
        self.injected = true;
        self.release(out);
    }

    fn on_event(&mut self, raw: Bytes, now: Instant, out: &mut Vec<Bytes>) {
        let ev = parse_event(&raw);
        let data = ev.data.trim_ascii();
        if data == b"[DONE]" {
            self.end.get_or_insert(now);
            self.outcome.get_or_insert(Outcome::Completed);
            if self.held_chunk.is_some() {
                self.inject(out);
            }
            out.push(raw);
            return;
        }
        let Ok(v) = serde_json::from_slice::<Value>(data) else {
            // Comments, keep-alives.
            if self.held_chunk.is_some() {
                self.hold(raw, out);
            } else {
                out.push(raw);
            }
            return;
        };
        self.first_event.get_or_insert(now);
        if let Some(e) = v.get("error").filter(|e| !e.is_null()) {
            self.end = Some(now);
            let code = [e.get("code"), e.get("type")]
                .into_iter()
                .flatten()
                .find_map(|c| match c {
                    Value::String(s) if !s.is_empty() => Some(s.chars().take(64).collect()),
                    Value::Number(n) => Some(n.to_string()),
                    _ => None,
                });
            self.outcome = Some(Outcome::Error { status: None, code });
            self.line = None;
            self.release(out);
            out.push(raw);
            return;
        }
        if let Some(u) = v.get("usage").and_then(chat_usage) {
            self.usage = Some(u);
        }
        if self.model.is_none() {
            self.model = v
                .get("model")
                .and_then(Value::as_str)
                .filter(|m| !m.is_empty())
                .map(|m| m.chars().take(64).collect());
        }
        if self.id.is_none() {
            self.id = v
                .get("id")
                .and_then(Value::as_str)
                .filter(|i| !i.is_empty())
                .map(clip_id);
        }
        let f = facts(&v);
        if f.output {
            self.deltas += 1;
            self.first_token.get_or_insert(now);
            self.last_delta = Some(now);
        }
        self.tool_calls |= f.tool_call;
        self.several |= f.other_choice;
        if let Some(reason) = &f.finish {
            self.end = Some(now);
            self.outcome = Some(match reason.as_str() {
                "length" | "content_filter" => Outcome::Incomplete {
                    reason: Some(reason.clone()),
                },
                _ => Outcome::Completed,
            });
        }
        if self.line.is_none() {
            out.push(raw);
            return;
        }
        if self.held_chunk.is_some() {
            if f.no_choices {
                // The usage chunk: the line has its numbers now.
                self.inject(out);
                out.push(raw);
            } else {
                // Something follows the end: no line.
                self.line = None;
                self.release(out);
                out.push(raw);
            }
            return;
        }
        let ends = f.finish.as_deref() == Some("stop")
            && !self.tool_calls
            && !self.several
            && !self.injected;
        if ends {
            self.held_chunk = Some(v);
            self.hold(raw, out);
        } else {
            out.push(raw);
        }
    }

    /// The measurements so far.
    pub fn summary(&self) -> TapSummary {
        let since = |t: Option<Instant>| t.map(|t| t.saturating_duration_since(self.t0));
        TapSummary {
            first_event: since(self.first_event),
            first_token: since(self.first_token),
            last_delta: since(self.last_delta),
            end: since(self.end),
            usage: self.usage,
            response_model: self.model.clone(),
            response_id: self.id.clone(),
            response_tier: None,
            outcome: self.outcome.clone(),
            deltas: self.deltas,
            injected: self.injected,
            uninspected: false,
            compactions: Vec::new(),
        }
    }
}

impl crate::stats::body::EventTap for ChatSse {
    fn push_at(&mut self, chunk: Bytes, clock: &mut dyn FnMut() -> Instant, out: &mut Vec<Bytes>) {
        ChatSse::push_at(self, chunk, clock, out);
    }
    fn finish(&mut self, out: &mut Vec<Bytes>) {
        ChatSse::finish(self, out);
    }
    fn summary(&self) -> TapSummary {
        ChatSse::summary(self)
    }
    fn holding(&self) -> bool {
        self.held_chunk.is_some()
    }
    fn deltas(&self) -> u64 {
        self.deltas
    }
    fn first_token(&self) -> Option<Duration> {
        self.first_token
            .map(|t| t.saturating_duration_since(self.t0))
    }
    fn may_inject(&self) -> bool {
        self.line.is_some()
    }
    fn terminal(&self) -> &'static str {
        "[DONE]"
    }
}
