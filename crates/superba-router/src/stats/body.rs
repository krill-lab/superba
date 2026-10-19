//! The response body wrapper of a tracked HTTP request: feeds the SSE tap (or
//! collects a small JSON answer), keeps the in-flight numbers current and
//! finishes the request's row when the body ends, breaks or is dropped.

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use http_body::{Body, Frame, SizeHint};
use serde::Deserialize;
use superba_core::{InflightPhase, RequestStatus};

use super::hub::{End, Tracker};
use super::tap::{SseTap, TapSummary};
use super::usage::{Usage, UsageWire};

/// Largest JSON answer inspected for usage / error codes.
const JSON_CAP: usize = 4 * 1024 * 1024;

/// An event stream tap of another protocol than Responses (Anthropic
/// Messages on the Claude Code ingress, Chat Completions on the OpenAI
/// ingress): it passes events on, may hold some back for the stats line, and
/// measures the answer.
pub(crate) trait EventTap: Send {
    /// Process one chunk; `out` receives the bytes to send now.
    fn push_at(&mut self, chunk: Bytes, clock: &mut dyn FnMut() -> Instant, out: &mut Vec<Bytes>);
    /// The stream ended: send what is held or incomplete.
    fn finish(&mut self, out: &mut Vec<Bytes>);
    /// The measurements so far.
    fn summary(&self) -> TapSummary;
    /// Events are being held.
    fn holding(&self) -> bool;
    /// Output deltas so far.
    fn deltas(&self) -> u64;
    /// `t0` → first output, once seen.
    fn first_token(&self) -> Option<Duration>;
    /// The tap may change the stream (it has a line setup).
    fn may_inject(&self) -> bool;
    /// The event a complete stream ends with (`message_stop`, `[DONE]`).
    fn terminal(&self) -> &'static str;
}

/// What the wrapper does with the bytes.
pub(crate) enum Mode {
    /// An SSE stream through the tap.
    Sse(Box<SseTap>),
    /// Another protocol's event stream through its tap.
    Events(Box<dyn EventTap>),
    /// A JSON answer, copied (capped) for its usage or error code.
    Json {
        /// The copy.
        buf: BytesMut,
        /// More than [`JSON_CAP`] arrived.
        over: bool,
        /// HTTP status.
        status: u16,
    },
    /// Anything else: timing only.
    Plain {
        /// HTTP status.
        status: u16,
    },
}

impl Mode {
    /// A JSON answer with `status`.
    pub fn json(status: u16) -> Mode {
        Mode::Json {
            buf: BytesMut::new(),
            over: false,
            status,
        }
    }
}

/// The wrapper.
pub(crate) struct TapBody<B: Body> {
    inner: B,
    mode: Mode,
    tracker: Option<Tracker>,
    queue: VecDeque<Bytes>,
    ended: bool,
    rewrites: bool,
    /// `Content-Length` of the upstream body, when known.
    expected: Option<u64>,
    /// Upstream bytes seen.
    passed: u64,
}

impl<B: Body> TapBody<B> {
    /// Wrap `inner`.
    pub fn new(inner: B, mode: Mode, tracker: Tracker) -> Self {
        let rewrites = match &mode {
            Mode::Sse(t) => t.tap().may_inject(),
            Mode::Events(t) => t.may_inject(),
            _ => false,
        };
        let expected = inner.size_hint().exact();
        Self {
            inner,
            mode,
            tracker: Some(tracker),
            queue: VecDeque::new(),
            ended: false,
            rewrites,
            expected,
            passed: 0,
        }
    }
}

impl<B: Body> TapBody<B> {
    fn on_data(&mut self, data: Bytes) {
        self.passed = self.passed.saturating_add(data.len() as u64);
        match &mut self.mode {
            Mode::Sse(tap) => {
                let mut out = Vec::new();
                // Each event is timed when it is parsed, not when the read began.
                tap.push_at(data, &mut Instant::now, &mut out);
                self.queue.extend(out);
                if let Some(t) = &self.tracker {
                    let phase = if tap.tap().holding() {
                        InflightPhase::Holding
                    } else {
                        InflightPhase::Streaming
                    };
                    let tap = tap.tap();
                    t.inflight().update(phase, tap.deltas(), tap.first_token());
                }
            }
            Mode::Events(tap) => {
                let mut out = Vec::new();
                tap.push_at(data, &mut Instant::now, &mut out);
                self.queue.extend(out);
                if let Some(t) = &self.tracker {
                    let phase = if tap.holding() {
                        InflightPhase::Holding
                    } else {
                        InflightPhase::Streaming
                    };
                    t.inflight().update(phase, tap.deltas(), tap.first_token());
                }
            }
            Mode::Json { buf, over, .. } => {
                if !*over {
                    if buf.len() + data.len() > JSON_CAP {
                        *over = true;
                        buf.clear();
                    } else {
                        buf.extend_from_slice(&data);
                    }
                }
                self.queue.push_back(data);
            }
            Mode::Plain { .. } => self.queue.push_back(data),
        }
    }

    /// The upstream body ended normally.
    fn on_end(&mut self) {
        let end = match &mut self.mode {
            Mode::Sse(tap) => {
                let mut out = Vec::new();
                tap.finish(&mut out);
                self.queue.extend(out);
                let summary = tap.tap().summary();
                let status = summary.outcome.is_none().then_some(RequestStatus::Broken);
                End {
                    error: status
                        .as_ref()
                        .map(|_| "the stream ended before response.completed".to_owned()),
                    summary: Some(summary),
                    status,
                    json_usage: None,
                    json_id: None,
                }
            }
            Mode::Events(tap) => {
                let mut out = Vec::new();
                tap.finish(&mut out);
                self.queue.extend(out);
                let summary = tap.summary();
                let status = summary.outcome.is_none().then_some(RequestStatus::Broken);
                let terminal = tap.terminal();
                End {
                    error: status
                        .as_ref()
                        .map(|_| format!("the stream ended before {terminal}")),
                    summary: Some(summary),
                    status,
                    json_usage: None,
                    json_id: None,
                }
            }
            Mode::Json { buf, over, status } => {
                let (usage, code, id) = if *over {
                    (None, None, None)
                } else {
                    json_answer(buf)
                };
                let status = *status;
                End {
                    status: (!(200..300).contains(&status))
                        .then_some(RequestStatus::Http { status, code }),
                    json_usage: usage,
                    json_id: id,
                    ..End::default()
                }
            }
            Mode::Plain { status } => {
                let status = *status;
                End {
                    status: (!(200..300).contains(&status))
                        .then_some(RequestStatus::Http { status, code: None }),
                    ..End::default()
                }
            }
        };
        if let Some(t) = self.tracker.take() {
            t.finish(end);
        }
    }

    fn on_error(&mut self, detail: String) {
        let summary = match &self.mode {
            Mode::Sse(tap) => Some(tap.tap().summary()),
            Mode::Events(tap) => Some(tap.summary()),
            _ => None,
        };
        if let Some(t) = self.tracker.take() {
            t.finish(End {
                summary,
                status: Some(RequestStatus::Broken),
                error: Some(detail),
                json_usage: None,
                json_id: None,
            });
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct JsonAnswer {
    id: Option<String>,
    usage: Option<UsageWire>,
    response: Option<JsonResponse>,
    error: Option<JsonError>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct JsonResponse {
    id: Option<String>,
    usage: Option<UsageWire>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct JsonError {
    code: Option<serde_json::Value>,
    #[serde(rename = "type")]
    kind: Option<serde_json::Value>,
}

/// Usage, error code and the answer's id of a JSON answer.
fn json_answer(buf: &[u8]) -> (Option<Usage>, Option<String>, Option<String>) {
    let Ok(a) = serde_json::from_slice::<JsonAnswer>(buf) else {
        return (None, None, None);
    };
    let (rid, rusage) = a.response.map(|r| (r.id, r.usage)).unwrap_or_default();
    let id =
        a.id.or(rid)
            .filter(|i| !i.is_empty())
            .map(|i| super::tap::clip_id(&i));
    let usage = a.usage.or(rusage).and_then(Usage::from_wire);
    let code = a.error.and_then(|e| {
        [e.code, e.kind]
            .into_iter()
            .flatten()
            .find_map(|v| match v {
                serde_json::Value::String(s) if !s.is_empty() => Some(s.chars().take(64).collect()),
                serde_json::Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
    });
    (usage, code, id)
}

impl<B> Body for TapBody<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::fmt::Display,
{
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, B::Error>>> {
        let this = self.get_mut();
        loop {
            if let Some(b) = this.queue.pop_front() {
                return Poll::Ready(Some(Ok(Frame::data(b))));
            }
            if this.ended {
                return Poll::Ready(None);
            }
            match ready!(Pin::new(&mut this.inner).poll_frame(cx)) {
                Some(Ok(frame)) => match frame.into_data() {
                    Ok(data) => this.on_data(data),
                    Err(frame) => return Poll::Ready(Some(Ok(frame))),
                },
                Some(Err(e)) => {
                    this.ended = true;
                    this.on_error(format!("stream broken: {e}").chars().take(200).collect());
                    return Poll::Ready(Some(Err(e)));
                }
                None => {
                    this.ended = true;
                    this.on_end();
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        if !self.queue.is_empty() {
            return false;
        }
        // The SSE tap may still release held bytes at the end; other answers
        // pass unchanged, so the inner body knows.
        self.ended
            || (!matches!(self.mode, Mode::Sse(_) | Mode::Events(_)) && self.inner.is_end_stream())
    }

    fn size_hint(&self) -> SizeHint {
        if self.rewrites {
            SizeHint::default()
        } else {
            self.inner.size_hint()
        }
    }
}

impl<B: Body> Drop for TapBody<B> {
    fn drop(&mut self) {
        if self.tracker.is_none() {
            return;
        }
        // hyper stops polling a body once its `Content-Length` is written.
        let complete = self.queue.is_empty()
            && (self.inner.is_end_stream() || self.expected.is_some_and(|e| self.passed >= e));
        if complete {
            self.on_end();
            return;
        }
        // Dropped before the end: Codex went away (Stop, turn aborted).
        if let Some(t) = self.tracker.take() {
            let summary = match &self.mode {
                Mode::Sse(tap) => Some(tap.tap().summary()),
                Mode::Events(tap) => Some(tap.summary()),
                _ => None,
            };
            t.finish(End {
                summary,
                status: Some(RequestStatus::Cancelled),
                ..End::default()
            });
        }
    }
}
