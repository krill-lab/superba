//! The response tap: timings, usage and the hold / inject state machine of one
//! Responses response, shared by HTTP SSE ([`SseTap`]) and WebSocket frames.
//!
//! Events pass through untouched until the **candidate**: the
//! `response.output_text.done` of the last output item when that item is an
//! assistant message with phase `final_answer` (or none). From there the tap
//! holds events until
//!
//! - `response.output_item.added` / `response.content_part.added` (something
//!   follows): release the held events unchanged and keep streaming;
//! - `response.completed`: render the stats line and inject it
//!   ([`Decision::Inject`]; see [`super::inject`]);
//! - `response.failed` / `response.incomplete` / `error`, the end of the
//!   stream, or more than [`MAX_HELD_BYTES`] held: release unchanged.
//!
//! The tap reads only small parts of a few events (borrowed, unknown fields
//! skipped); deltas cost a timestamp and a counter.

use std::borrow::Cow;
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use serde::Deserialize;
use superba_core::codex::ORIGINATOR_EXEC;
use superba_core::{Multiplier, PriceEntry, RequestKind, StatsLineStyle, Usd};

use super::meta::RequestMeta;
use super::sse::{self, Boundary, EventKind, MAX_EVENT_BYTES, sniff_type};
use super::usage::{Usage, UsageWire};
use super::{StatsLineConfig, inject, price};

/// Most bytes held back while waiting for `response.completed`.
pub const MAX_HELD_BYTES: usize = 4 * 1024 * 1024;

/// Whole milliseconds of `d`: the unit every timing mark is stored, shown and
/// computed with (the stats line, the request log and the GUI agree to the ms).
pub fn whole_ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Whether the stats line may be added to this request's answer (spike RS 3.3
/// rules 2-4; the transport rules are checked by the caller).
pub fn line_allowed(line: &StatsLineConfig, meta: &RequestMeta) -> bool {
    let originator_ok = meta.originator.as_deref().is_some_and(|o| {
        o != ORIGINATOR_EXEC && line.originators.iter().any(|allowed| allowed == o)
    });
    originator_ok
        && meta.request_kind == Some(RequestKind::Turn)
        && meta.thread_source.as_deref() == Some("user")
        && !meta.structured_output
        && !meta.prewarm
}

/// Totals of the earlier requests of the same turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TurnSoFar {
    /// Input tokens (incl. cached).
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Cached tokens.
    pub cached: u64,
    /// Requests.
    pub requests: u32,
    /// Known cost.
    pub cost: Option<Usd>,
}

/// What the line needs besides this response's own numbers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineSetup {
    /// Template (placeholders unfilled).
    pub template: String,
    /// Quote style.
    pub style: StatsLineStyle,
    /// Earlier requests of the turn.
    pub turn: TurnSoFar,
    /// Prices of this request's model, when known.
    pub price: Option<PriceEntry>,
    /// Member ratio.
    pub ratio: Multiplier,
}

/// What the holder does with the event just fed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Forward it.
    Pass,
    /// Keep it (start or continue holding).
    Hold,
    /// Forward the held events unchanged, then this one.
    Release,
    /// Inject `suffix` (`"\n\n> ⚡ …"`) into the held events and this
    /// `response.completed`, after a synthetic delta.
    Inject(String),
}

/// How a response ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// `response.completed`.
    Completed,
    /// `response.failed`.
    Failed {
        /// `error.code`.
        code: Option<String>,
    },
    /// `response.incomplete`.
    Incomplete {
        /// `incomplete_details.reason`.
        reason: Option<String>,
    },
    /// An `error` event.
    Error {
        /// Its `status`, if any.
        status: Option<u16>,
        /// Its `error.code` (else `error.type`).
        code: Option<String>,
    },
}

/// The candidate message part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// `item_id`.
    pub item_id: String,
    /// `output_index`.
    pub output_index: u64,
    /// `content_index`.
    pub content_index: u64,
}

/// Everything the tap measured.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TapSummary {
    /// `t0` → first Responses event (one with a type; SSE comments and
    /// keep-alives do not count).
    pub first_event: Option<Duration>,
    /// `t0` → first output.
    pub first_token: Option<Duration>,
    /// `t0` → last output delta.
    pub last_delta: Option<Duration>,
    /// `t0` → terminal event.
    pub end: Option<Duration>,
    /// Usage of the terminal event.
    pub usage: Option<Usage>,
    /// `response.model`.
    pub response_model: Option<String>,
    /// The upstream's id of the answer (`resp_…`; Anthropic `msg_…`).
    pub response_id: Option<String>,
    /// `response.service_tier`.
    pub response_tier: Option<String>,
    /// How it ended (`None`: no terminal event seen).
    pub outcome: Option<Outcome>,
    /// Output deltas seen.
    pub deltas: u64,
    /// The stats line was injected.
    pub injected: bool,
    /// An event was too large to inspect.
    pub uninspected: bool,
    /// Fingerprints of the `compaction` items of the answer.
    pub compactions: Vec<crate::compaction::Fingerprint>,
}

impl TapSummary {
    /// Output tokens per second × 1000 from the first event to the terminal
    /// event, in whole ms ([`superba_core::tps_milli`]: `None` for windows
    /// under 200 ms, including zero, or under 10 output tokens). The stats line
    /// and the request row both use this value.
    pub fn tps_milli(&self) -> Option<u64> {
        let usage = self.usage?;
        superba_core::tps_milli(
            usage.output,
            whole_ms(self.first_event?),
            whole_ms(self.end?),
        )
    }

    /// First output in whole ms (the request row's 首字 and the stats line's
    /// `{ttft}`).
    pub fn first_token_ms(&self) -> Option<u64> {
        self.first_token.map(whole_ms)
    }
}

#[derive(Clone, Debug)]
struct Item {
    kind: Option<String>,
    role: Option<String>,
    phase: Option<String>,
    id: Option<String>,
}

/// The per-response state machine.
#[derive(Debug)]
pub struct ResponseTap {
    t0: Instant,
    line: Option<LineSetup>,
    first_event: Option<Instant>,
    first_token: Option<Instant>,
    last_delta: Option<Instant>,
    end: Option<Instant>,
    deltas: u64,
    items: Vec<Item>,
    usage: Option<Usage>,
    response_model: Option<String>,
    response_id: Option<String>,
    response_tier: Option<String>,
    outcome: Option<Outcome>,
    candidate: Option<Candidate>,
    holding: bool,
    held_bytes: usize,
    injected: bool,
    uninspected: bool,
    /// Fingerprints of the `compaction` output items ([`crate::compaction`]).
    compactions: Vec<crate::compaction::Fingerprint>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ItemPeek<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    role: Option<Cow<'a, str>>,
    #[serde(borrow)]
    phase: Option<Cow<'a, str>>,
    #[serde(borrow)]
    id: Option<Cow<'a, str>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ItemEventPeek<'a> {
    #[serde(borrow)]
    item: Option<ItemPeek<'a>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct TextDonePeek<'a> {
    #[serde(borrow)]
    item_id: Option<Cow<'a, str>>,
    output_index: Option<u64>,
    content_index: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ErrorPeek {
    code: Option<serde_json::Value>,
    #[serde(rename = "type")]
    kind: Option<serde_json::Value>,
}

impl ErrorPeek {
    fn code(&self) -> Option<String> {
        let s = |v: &serde_json::Value| match v {
            serde_json::Value::String(s) if !s.is_empty() => Some(clip(s)),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        };
        self.code
            .as_ref()
            .and_then(s)
            .or_else(|| self.kind.as_ref().and_then(s))
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct IncompletePeek {
    reason: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ResponsePeek<'a> {
    usage: Option<UsageWire>,
    #[serde(borrow)]
    id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    model: Option<Cow<'a, str>>,
    #[serde(borrow)]
    service_tier: Option<Cow<'a, str>>,
    error: Option<ErrorPeek>,
    incomplete_details: Option<IncompletePeek>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct TerminalPeek<'a> {
    #[serde(borrow)]
    response: Option<ResponsePeek<'a>>,
    status: Option<u16>,
    error: Option<ErrorPeek>,
}

/// Short codes only (no message text).
fn clip(s: &str) -> String {
    s.chars().take(64).collect()
}

/// Ids are kept whole up to 128 characters.
pub(crate) fn clip_id(s: &str) -> String {
    s.chars().take(128).collect()
}

impl ResponseTap {
    /// A tap for a request received at `t0`; `line` is `Some` only when the
    /// request may get the stats line.
    pub fn new(t0: Instant, line: Option<LineSetup>) -> Self {
        Self {
            t0,
            line,
            first_event: None,
            first_token: None,
            last_delta: None,
            end: None,
            deltas: 0,
            items: Vec::new(),
            usage: None,
            response_model: None,
            response_id: None,
            response_tier: None,
            outcome: None,
            candidate: None,
            holding: false,
            held_bytes: 0,
            injected: false,
            uninspected: false,
            compactions: Vec::new(),
        }
    }

    /// The tap may inject (it has a line setup).
    pub fn may_inject(&self) -> bool {
        self.line.is_some()
    }

    /// Events are being held.
    pub fn holding(&self) -> bool {
        self.holding
    }

    /// The held candidate, while holding or after an injection.
    pub fn candidate(&self) -> Option<&Candidate> {
        self.candidate.as_ref()
    }

    /// Output deltas so far.
    pub fn deltas(&self) -> u64 {
        self.deltas
    }

    /// `t0` → first output, once seen (the in-flight view shows the value the
    /// row will get).
    pub fn first_token(&self) -> Option<Duration> {
        self.first_token
            .map(|t| t.saturating_duration_since(self.t0))
    }

    /// Record that an event too large to inspect passed.
    pub fn note_uninspected(&mut self, now: Instant) {
        self.first_event.get_or_insert(now);
        self.uninspected = true;
    }

    /// Count `bytes` more held bytes; past [`MAX_HELD_BYTES`] the hold is
    /// abandoned (no line for this response) and `false` is returned: the
    /// holder must release what it holds.
    pub fn note_held(&mut self, bytes: usize) -> bool {
        self.held_bytes = self.held_bytes.saturating_add(bytes);
        if self.held_bytes > MAX_HELD_BYTES {
            self.abandon();
            return false;
        }
        true
    }

    /// Stop holding and never inject for this response.
    pub fn abandon(&mut self) {
        self.holding = false;
        self.held_bytes = 0;
        self.line = None;
    }

    /// The injection could not be applied (unexpected event JSON).
    pub fn injection_failed(&mut self) {
        self.injected = false;
        self.abandon();
    }

    /// Feed one complete event (its type and JSON payload) processed at `now`.
    ///
    /// The first event is the first one with a type: SSE comments, keep-alives
    /// and `[DONE]` that a relay may send together with the headers do not
    /// start the TPS window.
    pub fn on_event(&mut self, kind: EventKind, json: &[u8], now: Instant) -> Decision {
        if self.first_event.is_none() && (kind != EventKind::Other || sniff_type(json).is_some()) {
            self.first_event = Some(now);
        }
        match kind {
            EventKind::TextDelta | EventKind::OutputDelta => {
                self.deltas += 1;
                self.first_token.get_or_insert(now);
                self.last_delta = Some(now);
            }
            EventKind::ItemDone => {
                // Items that arrive whole (encrypted reasoning, tool calls).
                self.first_token.get_or_insert(now);
                if let Some(fp) = crate::compaction::output_item(json) {
                    self.compactions.push(fp);
                }
            }
            EventKind::ItemAdded => {
                let peek: ItemEventPeek<'_> = serde_json::from_slice(json).unwrap_or_default();
                let item = peek.item.unwrap_or_default();
                self.items.push(Item {
                    kind: item.kind.map(Cow::into_owned),
                    role: item.role.map(Cow::into_owned),
                    phase: item.phase.map(Cow::into_owned),
                    id: item.id.map(Cow::into_owned),
                });
            }
            EventKind::Created if self.response_id.is_none() => {
                let peek: TerminalPeek<'_> = serde_json::from_slice(json).unwrap_or_default();
                self.response_id = peek.response.and_then(|r| r.id).as_deref().map(clip_id);
            }
            _ => {}
        }
        if kind.is_terminal() {
            self.end = Some(now);
            self.read_terminal(kind, json);
        }
        if self.holding {
            return match kind {
                EventKind::ItemAdded | EventKind::PartAdded => self.release(),
                EventKind::Completed => match self.line_suffix() {
                    Some(suffix) => {
                        self.holding = false;
                        self.injected = true;
                        Decision::Inject(suffix)
                    }
                    None => self.release(),
                },
                k if k.is_terminal() => self.release(),
                _ => Decision::Hold,
            };
        }
        if kind == EventKind::TextDone
            && self.line.is_some()
            && self.outcome.is_none()
            && let Some(c) = self.candidate_for(json)
        {
            self.candidate = Some(c);
            self.holding = true;
            self.held_bytes = 0;
            return Decision::Hold;
        }
        Decision::Pass
    }

    fn release(&mut self) -> Decision {
        self.holding = false;
        self.held_bytes = 0;
        self.candidate = None;
        Decision::Release
    }

    fn candidate_for(&self, json: &[u8]) -> Option<Candidate> {
        let last = self.items.last()?;
        if last.kind.as_deref() != Some("message")
            || last.role.as_deref().is_some_and(|r| r != "assistant")
            || last.phase.as_deref().is_some_and(|p| p != "final_answer")
        {
            return None;
        }
        let peek: TextDonePeek<'_> = serde_json::from_slice(json).ok()?;
        let item_id = peek.item_id?.into_owned();
        if last.id.as_deref().is_some_and(|id| id != item_id) {
            return None;
        }
        Some(Candidate {
            item_id,
            output_index: peek.output_index.unwrap_or((self.items.len() - 1) as u64),
            content_index: peek.content_index.unwrap_or(0),
        })
    }

    fn read_terminal(&mut self, kind: EventKind, json: &[u8]) {
        if kind == EventKind::Completed && self.compactions.is_empty() {
            self.compactions = crate::compaction::completed_items(json);
        }
        let peek: TerminalPeek<'_> = serde_json::from_slice(json).unwrap_or_default();
        if let Some(r) = &peek.response {
            self.response_model = r.model.as_deref().map(clip);
            self.response_tier = r.service_tier.as_deref().map(clip);
            if let Some(id) = r.id.as_deref() {
                self.response_id = Some(clip_id(id));
            }
        }
        let response = peek.response;
        self.outcome = Some(match kind {
            EventKind::Completed => Outcome::Completed,
            // Krill's busy answer names the event `response.failed` but carries
            // the error at the top level, as an `error` event does (user bug 58).
            EventKind::Failed => Outcome::Failed {
                code: response
                    .as_ref()
                    .and_then(|r| r.error.as_ref())
                    .and_then(ErrorPeek::code)
                    .or_else(|| peek.error.as_ref().and_then(ErrorPeek::code)),
            },
            EventKind::Incomplete => Outcome::Incomplete {
                reason: response
                    .as_ref()
                    .and_then(|r| r.incomplete_details.as_ref())
                    .and_then(|d| d.reason.as_deref().map(clip)),
            },
            _ => Outcome::Error {
                status: peek.status,
                code: peek.error.as_ref().and_then(ErrorPeek::code),
            },
        });
        if let Some(wire) = response.and_then(|r| r.usage) {
            self.usage = Usage::from_wire(wire);
        }
    }

    /// The suffix to append (`"\n\n> ⚡ …"`), when the line applies.
    fn line_suffix(&self) -> Option<String> {
        let line = self.line.as_ref()?;
        self.candidate.as_ref()?;
        Some(render_line(
            line,
            &self.summary(),
            self.usage,
            self.response_tier.as_deref(),
        ))
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
            response_model: self.response_model.clone(),
            response_id: self.response_id.clone(),
            response_tier: self.response_tier.clone(),
            outcome: self.outcome.clone(),
            deltas: self.deltas,
            injected: self.injected,
            uninspected: self.uninspected,
            compactions: self.compactions.clone(),
        }
    }
}

/// `12345` → `12,345`.
pub fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The tap over an SSE byte stream.
///
/// Chunks are forwarded as they arrive (the same `Bytes`, no copy) while the
/// tap only times events and reads the terminal event. When the response may
/// get the stats line, the bytes of an event that is still incomplete at the
/// end of a chunk are kept back until the event completes (a candidate must be
/// held from its first byte); complete events still pass zero copy.
#[derive(Debug)]
pub struct SseTap {
    tap: ResponseTap,
    boundary: Boundary,
    /// Bytes of the current incomplete event: kept back, or a copy for
    /// inspection (see `carry`).
    partial: BytesMut,
    carry: Carry,
    held: Vec<(EventKind, Bytes)>,
}

/// State of the current incomplete event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Carry {
    /// No incomplete event.
    None,
    /// Its bytes are in `partial` and were not forwarded yet.
    KeptBack,
    /// Its bytes were forwarded; `partial` holds a copy when `inspect`.
    Forwarded { inspect: bool },
}

impl SseTap {
    /// A tap over `tap`.
    pub fn new(tap: ResponseTap) -> Self {
        Self {
            tap,
            boundary: Boundary::default(),
            partial: BytesMut::new(),
            carry: Carry::None,
            held: Vec::new(),
        }
    }

    /// The event-level tap.
    pub fn tap(&self) -> &ResponseTap {
        &self.tap
    }

    /// Process one upstream chunk with every event timed at `now` (tests);
    /// `out` receives the chunks to forward now.
    pub fn push(&mut self, chunk: Bytes, now: Instant, out: &mut Vec<Bytes>) {
        self.push_at(chunk, &mut || now, out);
    }

    /// Process one upstream chunk; each complete event is timed by `clock` when
    /// it is parsed (not when the read began), so events that share a read with
    /// the headers or with each other keep their own order and times. `out`
    /// receives the chunks to forward now.
    pub fn push_at(
        &mut self,
        chunk: Bytes,
        clock: &mut dyn FnMut() -> Instant,
        out: &mut Vec<Bytes>,
    ) {
        // `chunk[seg..]` is neither forwarded nor held yet.
        let mut seg = 0usize;
        let mut pos = 0usize;
        while let Some(end) = self.boundary.find(&chunk, pos) {
            let carry = std::mem::replace(&mut self.carry, Carry::None);
            let ev_start = pos;
            pos = end;
            let (raw, kept_back) = match carry {
                Carry::None => (Some(chunk.slice(ev_start..end)), false),
                Carry::KeptBack => {
                    self.partial.extend_from_slice(&chunk[ev_start..end]);
                    (Some(self.partial.split().freeze()), true)
                }
                Carry::Forwarded { inspect: true }
                    if self.partial.len() + (end - ev_start) <= MAX_EVENT_BYTES =>
                {
                    self.partial.extend_from_slice(&chunk[ev_start..end]);
                    (Some(self.partial.split().freeze()), false)
                }
                Carry::Forwarded { .. } => {
                    self.partial.clear();
                    (None, false)
                }
            };
            let Some(raw) = raw else {
                // Too large to inspect; its bytes pass as they came.
                self.tap.note_uninspected(clock());
                continue;
            };
            let ev = sse::parse_event(&raw);
            let kind = sse::event_kind(&ev);
            let mut decision = self.tap.on_event(kind, &ev.data, clock());
            if matches!(carry, Carry::Forwarded { .. }) && decision != Decision::Pass {
                // Its beginning is already out: it can only pass.
                self.tap.abandon();
                decision = Decision::Pass;
            }
            match decision {
                Decision::Pass => {
                    if kept_back {
                        out.push(raw);
                        seg = end;
                    }
                }
                Decision::Hold => {
                    if ev_start > seg {
                        out.push(chunk.slice(seg..ev_start));
                    }
                    seg = end;
                    let len = raw.len();
                    self.held.push((kind, raw));
                    if !self.tap.note_held(len) {
                        self.release_held(out);
                    }
                }
                Decision::Release => {
                    if ev_start > seg {
                        out.push(chunk.slice(seg..ev_start));
                    }
                    self.release_held(out);
                    if kept_back {
                        out.push(raw);
                        seg = end;
                    } else {
                        seg = ev_start;
                    }
                }
                Decision::Inject(suffix) => {
                    if ev_start > seg {
                        out.push(chunk.slice(seg..ev_start));
                    }
                    let held = std::mem::take(&mut self.held);
                    let rewritten = self
                        .tap
                        .candidate()
                        .cloned()
                        .and_then(|c| inject::rewrite_sse(&held, &raw, &c, &suffix));
                    match rewritten {
                        Some(r) => out.extend(r),
                        None => {
                            self.tap.injection_failed();
                            out.extend(held.into_iter().map(|(_, raw)| raw));
                            out.push(raw);
                        }
                    }
                    seg = end;
                }
            }
        }
        self.push_tail(&chunk, seg, pos, out);
    }

    /// Handle `chunk[pos..]`, the beginning (or continuation) of an incomplete
    /// event; `chunk[seg..pos]` is still to be forwarded.
    fn push_tail(&mut self, chunk: &Bytes, seg: usize, pos: usize, out: &mut Vec<Bytes>) {
        let tail = &chunk[pos..];
        let keep_back = matches!(self.carry, Carry::None | Carry::KeptBack)
            && (self.tap.holding() || self.tap.may_inject());
        if keep_back {
            if pos > seg {
                out.push(chunk.slice(seg..pos));
            }
            if tail.is_empty() {
                return;
            }
            let fits = self.partial.len() + tail.len() <= MAX_EVENT_BYTES
                && (!self.tap.holding() || self.tap.note_held(tail.len()));
            if fits {
                self.partial.extend_from_slice(tail);
                self.carry = Carry::KeptBack;
            } else {
                // Too much to keep back: everything goes out, the event passes
                // uninspected.
                self.release_held(out);
                if !self.partial.is_empty() {
                    out.push(self.partial.split().freeze());
                }
                out.push(chunk.slice(pos..));
                self.partial.clear();
                self.carry = Carry::Forwarded { inspect: false };
                if self.tap.holding() {
                    self.tap.abandon();
                }
            }
            return;
        }
        // Observer: forward everything not yet out.
        if seg == 0 {
            out.push(chunk.clone());
        } else if seg < chunk.len() {
            out.push(chunk.slice(seg..));
        }
        if tail.is_empty() {
            return;
        }
        self.carry = match self.carry {
            Carry::Forwarded { inspect: false } => Carry::Forwarded { inspect: false },
            _ if self.partial.len() + tail.len() > MAX_EVENT_BYTES => {
                self.partial.clear();
                Carry::Forwarded { inspect: false }
            }
            _ => {
                self.partial.extend_from_slice(tail);
                Carry::Forwarded { inspect: true }
            }
        };
    }

    fn release_held(&mut self, out: &mut Vec<Bytes>) {
        out.extend(self.held.drain(..).map(|(_, raw)| raw));
    }

    /// End of the upstream stream: everything still kept back goes out
    /// unchanged.
    pub fn finish(&mut self, out: &mut Vec<Bytes>) {
        if self.tap.holding() {
            self.tap.abandon();
        }
        self.release_held(out);
        if self.carry == Carry::KeptBack && !self.partial.is_empty() {
            out.push(self.partial.split().freeze());
        }
        self.partial.clear();
        self.carry = Carry::None;
    }

    /// Into the event-level tap (for the summary).
    pub fn into_tap(self) -> ResponseTap {
        self.tap
    }
}

/// The stats line suffix (`"\n\n> ⚡ …"`) of an answer measured as `summary`
/// with `usage` (tier `tier`), added to the turn so far in `line`. Shared by
/// the Responses tap and the Claude Code ingress.
pub(crate) fn render_line(
    line: &LineSetup,
    summary: &TapSummary,
    usage: Option<Usage>,
    tier: Option<&str>,
) -> String {
    let own = usage.unwrap_or_default();
    let tps = summary.tps_milli().map(|m| group_digits((m + 500) / 1000));
    let ttft = summary
        .first_token_ms()
        .map(|ms| format!("{:.2}s", ms as f64 / 1000.0));
    let own_cost = line
        .price
        .as_ref()
        .filter(|_| usage.is_some())
        .and_then(|p| price::cost(p, &own, tier, line.ratio));
    let cost = match (line.turn.cost, own_cost) {
        (None, None) => None,
        (a, b) => Some(Usd::from_micros(
            a.map_or(0, Usd::micros) + b.map_or(0, Usd::micros),
        )),
    };
    let values = inject::LineValues {
        tps,
        ttft,
        input: line.turn.input.saturating_add(own.input),
        output: line.turn.output.saturating_add(own.output),
        cached: line.turn.cached.saturating_add(own.cached.unwrap_or(0)),
        cost,
        requests: line.turn.requests.saturating_add(1),
    };
    inject::render_suffix(&line.template, line.style, &values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use superba_core::codex::STATS_LINE_MARKER;

    #[test]
    fn digits_are_grouped() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1000), "1,000");
        assert_eq!(group_digits(12_345_678), "12,345,678");
    }

    #[test]
    fn the_marker_is_the_bolt_and_word_joiner() {
        assert_eq!(STATS_LINE_MARKER, "⚡\u{2060}");
    }
}
