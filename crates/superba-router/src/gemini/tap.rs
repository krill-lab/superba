//! Gemini statistics tap; native bytes are passed through unchanged.
use crate::stats::{
    body::EventTap,
    tap::{Outcome, TapSummary},
};
use crate::translate::{
    gemini::Decoder,
    ir::{Accumulator, Event, StopReason},
    stream::Sse,
};
use bytes::Bytes;
use std::time::{Duration, Instant};

pub(super) struct Tap {
    t0: Instant,
    parser: Sse,
    decoder: Decoder,
    state: Accumulator,
    summary: TapSummary,
    deltas: u64,
}
impl Tap {
    pub fn new(t0: Instant) -> Self {
        Self {
            t0,
            parser: Default::default(),
            decoder: Default::default(),
            state: Default::default(),
            summary: Default::default(),
            deltas: 0,
        }
    }
    fn record(&mut self, events: Vec<Event>, now: Instant) {
        self.summary
            .first_event
            .get_or_insert(now.duration_since(self.t0));
        for event in events {
            if matches!(event, Event::Delta { .. } | Event::BlockStart { .. }) {
                self.summary
                    .first_token
                    .get_or_insert(now.duration_since(self.t0));
                self.summary.last_delta = Some(now.duration_since(self.t0));
                self.deltas += 1;
            }
            let _ = self.state.push(&event);
        }
        let r = &self.state.response;
        self.summary.usage = super::super::translate::stats_usage(r.usage);
        self.summary.response_id = Some(r.id.clone()).filter(|s| !s.is_empty());
        self.summary.response_model = Some(r.model.clone()).filter(|s| !s.is_empty());
        self.summary.outcome = if let Some(e) = &r.error {
            Some(Outcome::Error {
                status: Some(e.status),
                code: Some(e.code.clone()),
            })
        } else {
            r.stop.as_ref().map(|s| match s {
                StopReason::MaxTokens | StopReason::Refusal => Outcome::Incomplete {
                    reason: Some(format!("{s:?}")),
                },
                _ => Outcome::Completed,
            })
        };
        if self.summary.outcome.is_some() {
            self.summary.end = Some(now.duration_since(self.t0));
        }
    }
}
impl EventTap for Tap {
    fn push_at(&mut self, chunk: Bytes, clock: &mut dyn FnMut() -> Instant, out: &mut Vec<Bytes>) {
        if let Ok(frames) = self.parser.push(&chunk) {
            for f in frames {
                if let Ok(v) = serde_json::from_slice(&f)
                    && let Ok(events) = self.decoder.decode(&v)
                {
                    self.record(events, clock());
                }
            }
        }
        out.push(chunk);
    }
    fn finish(&mut self, _out: &mut Vec<Bytes>) {
        let events = self.decoder.finish();
        self.record(events, Instant::now());
    }
    fn summary(&self) -> TapSummary {
        self.summary.clone()
    }
    fn holding(&self) -> bool {
        false
    }
    fn deltas(&self) -> u64 {
        self.deltas
    }
    fn first_token(&self) -> Option<Duration> {
        self.summary.first_token
    }
    fn may_inject(&self) -> bool {
        false
    }
    fn terminal(&self) -> &'static str {
        "finishReason"
    }
}
