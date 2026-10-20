//! The tap of an Anthropic Messages stream passed through to Claude Code:
//! timings, usage and the outcome for the request log, and the stats line
//! ([`super::line`]).
//!
//! Events pass one by one (an event split across chunks is kept back until it
//! completes). With the line, the `content_block_stop` of a text block is held
//! until the next event shows whether the answer ends there:
//! `message_delta` with `end_turn` → a `text_delta` with the line goes out
//! before the held stop; anything else releases what is held unchanged. An
//! answer ending with another block gets the line as a new text block.
//!
//! Usage follows the router's OpenAI semantics: input = `input_tokens` +
//! cache reads + cache writes.

use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use serde_json::{Value, json};

use super::events::{block_delta, block_start, block_stop};
use crate::stats::sse::{Boundary, MAX_EVENT_BYTES, parse_event};
use crate::stats::tap::{LineSetup, Outcome, TapSummary, render_line};
use crate::stats::usage::Usage;

/// Most bytes held for the line before giving up on it.
const MAX_HELD: usize = 1024 * 1024;

/// Anthropic usage numbers as they arrive (`message_start`, then
/// `message_delta` overrides what it carries).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnthropicUsage {
    /// `input_tokens` (without cache).
    pub input: Option<u64>,
    /// `cache_read_input_tokens`.
    pub cache_read: Option<u64>,
    /// `cache_creation_input_tokens`.
    pub cache_creation: Option<u64>,
    /// `output_tokens`.
    pub output: Option<u64>,
    /// `cache_creation.ephemeral_1h_input_tokens`.
    pub cache_creation_1h: Option<u64>,
}

impl AnthropicUsage {
    /// Merge a `usage` object (fields it carries win).
    pub fn merge(&mut self, usage: Option<&Value>) {
        let Some(u) = usage else { return };
        let n = |k: &str| u.get(k).and_then(Value::as_u64);
        self.input = n("input_tokens").or(self.input);
        self.cache_read = n("cache_read_input_tokens").or(self.cache_read);
        self.cache_creation = n("cache_creation_input_tokens").or(self.cache_creation);
        self.output = n("output_tokens").or(self.output);
        self.cache_creation_1h = u
            .pointer("/cache_creation/ephemeral_1h_input_tokens")
            .and_then(Value::as_u64)
            .or(self.cache_creation_1h);
    }

    /// In the router's semantics; `None` without any count.
    pub fn normalised(&self) -> Option<Usage> {
        if self.input.is_none() && self.output.is_none() {
            return None;
        }
        let read = self.cache_read.unwrap_or(0);
        let created = self.cache_creation.unwrap_or(0);
        Some(Usage {
            input: self.input.unwrap_or(0) + read + created,
            cached: self.cache_read,
            cache_write: self.cache_creation,
            cache_write_1h: self.cache_creation_1h,
            output: self.output.unwrap_or(0),
            reasoning: None,
        })
    }
}

/// The tap.
#[derive(Debug)]
pub struct AnthropicSse {
    t0: Instant,
    line: Option<LineSetup>,
    boundary: Boundary,
    partial: BytesMut,
    first_event: Option<Instant>,
    first_token: Option<Instant>,
    last_delta: Option<Instant>,
    end: Option<Instant>,
    deltas: u64,
    usage: AnthropicUsage,
    model: Option<String>,
    id: Option<String>,
    stop_reason: Option<String>,
    outcome: Option<Outcome>,
    /// Blocks started so far, and whether the last one is text.
    blocks: u64,
    last_is_text: bool,
    held: Vec<Bytes>,
    held_bytes: usize,
    /// Index of the text block whose stop is held.
    held_index: Option<u64>,
    injected: bool,
}

impl AnthropicSse {
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
            usage: AnthropicUsage::default(),
            model: None,
            id: None,
            stop_reason: None,
            outcome: None,
            blocks: 0,
            last_is_text: false,
            held: Vec::new(),
            held_bytes: 0,
            held_index: None,
            injected: false,
        }
    }

    /// The tap may change the stream (it has a line setup).
    pub fn may_inject(&self) -> bool {
        self.line.is_some()
    }

    /// Events are being held.
    pub fn holding(&self) -> bool {
        self.held_index.is_some()
    }

    /// Output deltas so far.
    pub fn deltas(&self) -> u64 {
        self.deltas
    }

    /// `t0` → first output, once seen.
    pub fn first_token(&self) -> Option<Duration> {
        self.first_token
            .map(|t| t.saturating_duration_since(self.t0))
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
            out.push(self.partial.split().freeze());
        }
    }

    /// The stream ended: send what is held or incomplete.
    pub fn finish(&mut self, out: &mut Vec<Bytes>) {
        if !self.partial.is_empty() {
            let raw = self.partial.split().freeze();
            self.on_event(raw, Instant::now(), out);
        }
        self.release(out);
    }

    fn release(&mut self, out: &mut Vec<Bytes>) {
        out.append(&mut self.held);
        self.held_bytes = 0;
        self.held_index = None;
    }

    fn suffix(&self) -> Option<String> {
        let line = self.line.as_ref()?;
        Some(render_line(
            line,
            &self.summary(),
            self.usage.normalised(),
            None,
        ))
    }

    fn on_event(&mut self, raw: Bytes, now: Instant, out: &mut Vec<Bytes>) {
        let ev = parse_event(&raw);
        let Ok(v) = serde_json::from_slice::<Value>(&ev.data) else {
            // Comments, keep-alives.
            if self.holding() {
                self.held.push(raw);
            } else {
                out.push(raw);
            }
            return;
        };
        let kind = ev
            .name
            .filter(|n| !n.is_empty())
            .or_else(|| v.get("type").and_then(Value::as_str))
            .unwrap_or("")
            .to_owned();
        if kind != "ping" {
            self.first_event.get_or_insert(now);
        }
        match kind.as_str() {
            "message_start" => {
                let m = v.get("message");
                self.usage.merge(m.and_then(|m| m.get("usage")));
                self.model = m
                    .and_then(|m| m.get("model"))
                    .and_then(Value::as_str)
                    .map(|s| s.chars().take(64).collect());
                self.id = m
                    .and_then(|m| m.get("id"))
                    .and_then(Value::as_str)
                    .map(crate::stats::tap::clip_id);
            }
            "content_block_start" => {
                self.blocks += 1;
                self.last_is_text = v
                    .get("content_block")
                    .and_then(|b| b.get("type"))
                    .and_then(Value::as_str)
                    == Some("text");
            }
            "content_block_delta" => {
                let t = v
                    .get("delta")
                    .and_then(|d| d.get("type"))
                    .and_then(Value::as_str);
                if t != Some("signature_delta") {
                    self.deltas += 1;
                    self.first_token.get_or_insert(now);
                    self.last_delta = Some(now);
                }
            }
            "message_delta" => {
                // The answer's end for the line's speed (`message_stop`
                // follows at once).
                self.end = Some(now);
                self.usage.merge(v.get("usage"));
                if let Some(r) = v
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(Value::as_str)
                {
                    self.stop_reason = Some(r.to_owned());
                }
            }
            "message_stop" => {
                self.end = Some(now);
                self.outcome
                    .get_or_insert(match self.stop_reason.as_deref() {
                        Some(r @ ("max_tokens" | "refusal" | "model_context_window_exceeded")) => {
                            Outcome::Incomplete {
                                reason: Some(r.to_owned()),
                            }
                        }
                        _ => Outcome::Completed,
                    });
            }
            "error" => {
                self.end = Some(now);
                let code = v
                    .get("error")
                    .and_then(|e| e.get("type"))
                    .and_then(Value::as_str)
                    .map(|s| s.chars().take(64).collect());
                self.outcome = Some(Outcome::Error { status: None, code });
            }
            _ => {}
        }
        self.route(&kind, raw, out);
    }

    /// Pass, hold or inject around this event.
    fn route(&mut self, kind: &str, raw: Bytes, out: &mut Vec<Bytes>) {
        if self.line.is_none() {
            out.push(raw);
            return;
        }
        if let Some(index) = self.held_index {
            match kind {
                "message_delta" if self.stop_reason.as_deref() == Some("end_turn") => {
                    if let Some(suffix) = self.suffix() {
                        out.push(block_delta(
                            index,
                            json!({"type": "text_delta", "text": suffix}),
                        ));
                        self.injected = true;
                    }
                    self.release(out);
                    out.push(raw);
                }
                "content_block_start" | "message_delta" | "message_stop" | "error" => {
                    self.release(out);
                    out.push(raw);
                }
                _ => {
                    self.held_bytes += raw.len();
                    self.held.push(raw);
                    if self.held_bytes > MAX_HELD {
                        self.line = None;
                        self.release(out);
                    }
                }
            }
            return;
        }
        match kind {
            "content_block_stop" if self.last_is_text => {
                let index = serde_json::from_slice::<Value>(&parse_event(&raw).data)
                    .ok()
                    .and_then(|v| v.get("index").and_then(Value::as_u64))
                    .unwrap_or(self.blocks.saturating_sub(1));
                self.held_index = Some(index);
                self.held_bytes = raw.len();
                self.held.push(raw);
            }
            "message_delta"
                if self.stop_reason.as_deref() == Some("end_turn") && !self.injected =>
            {
                // The answer ends without a text block: the line gets its own.
                if let Some(suffix) = self.suffix() {
                    let index = self.blocks;
                    self.blocks += 1;
                    out.push(block_start(index, json!({"type": "text", "text": ""})));
                    out.push(block_delta(
                        index,
                        json!({"type": "text_delta", "text": suffix}),
                    ));
                    out.push(block_stop(index));
                    self.injected = true;
                }
                out.push(raw);
            }
            _ => out.push(raw),
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
            usage: self.usage.normalised(),
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

impl crate::stats::body::EventTap for AnthropicSse {
    fn push_at(&mut self, chunk: Bytes, clock: &mut dyn FnMut() -> Instant, out: &mut Vec<Bytes>) {
        AnthropicSse::push_at(self, chunk, clock, out);
    }
    fn finish(&mut self, out: &mut Vec<Bytes>) {
        AnthropicSse::finish(self, out);
    }
    fn summary(&self) -> TapSummary {
        AnthropicSse::summary(self)
    }
    fn holding(&self) -> bool {
        AnthropicSse::holding(self)
    }
    fn deltas(&self) -> u64 {
        AnthropicSse::deltas(self)
    }
    fn first_token(&self) -> Option<Duration> {
        AnthropicSse::first_token(self)
    }
    fn may_inject(&self) -> bool {
        AnthropicSse::may_inject(self)
    }
    fn terminal(&self) -> &'static str {
        "message_stop"
    }
}
