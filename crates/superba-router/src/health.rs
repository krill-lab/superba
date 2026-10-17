//! Per-line health: a passive circuit breaker with exponential backoff.
//!
//! - A connect / TLS failure, a reset before headers or a stream broken by the
//!   upstream opens the breaker at once ("hard" failure).
//! - 502/503/504 answers and edge interceptions ("soft" failures) open it after
//!   `threshold` consecutive ones.
//! - An open breaker stays open for its backoff (5 s doubling up to 60 s by
//!   default). Afterwards the line is half-open: the next request or health probe
//!   decides; a failure re-opens it with the doubled backoff, a success closes it
//!   and resets the backoff.
//!
//! Pure state over [`Instant`]s passed in, so tests drive it deterministically.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Breaker parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BreakerPolicy {
    /// Consecutive soft failures that open the breaker.
    pub threshold: u32,
    /// First open period.
    pub base: Duration,
    /// Longest open period.
    pub max: Duration,
}

impl Default for BreakerPolicy {
    fn default() -> Self {
        Self {
            threshold: 2,
            base: Duration::from_secs(5),
            max: Duration::from_secs(60),
        }
    }
}

/// Externally visible breaker state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineState {
    /// Serving normally.
    Healthy,
    /// Recently failed; skipped until its backoff ends.
    Open,
    /// Backoff over; the next request or probe decides.
    HalfOpen,
}

/// Health of one line.
#[derive(Clone, Debug)]
pub struct LineHealth {
    policy: BreakerPolicy,
    consecutive_failures: u32,
    open_until: Option<Instant>,
    next_backoff: Duration,
    last_error: Option<String>,
    last_ok: Option<Instant>,
    ttfb_ms: Option<f64>,
}

impl LineHealth {
    /// A healthy line.
    pub fn new(policy: BreakerPolicy) -> Self {
        Self {
            policy,
            consecutive_failures: 0,
            open_until: None,
            next_backoff: policy.base,
            last_error: None,
            last_ok: None,
            ttfb_ms: None,
        }
    }

    /// State at `now`.
    pub fn state(&self, now: Instant) -> LineState {
        match self.open_until {
            None => LineState::Healthy,
            Some(until) if now < until => LineState::Open,
            Some(_) => LineState::HalfOpen,
        }
    }

    /// True unless the breaker is open at `now`.
    pub fn is_available(&self, now: Instant) -> bool {
        self.state(now) != LineState::Open
    }

    /// When an open breaker turns half-open.
    pub fn open_until(&self) -> Option<Instant> {
        self.open_until
    }

    /// Consecutive failures since the last success.
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// Last failure description (never contains a key or body).
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Time of the last success.
    pub fn last_ok(&self) -> Option<Instant> {
        self.last_ok
    }

    /// Smoothed time to response headers, in milliseconds.
    pub fn ttfb_ms(&self) -> Option<u64> {
        self.ttfb_ms.map(|v| v.round() as u64)
    }

    /// Response headers arrived and were passed on (any status that is not a
    /// failover reason). Closes the breaker.
    pub fn on_success(&mut self, now: Instant, ttfb: Option<Duration>) {
        self.consecutive_failures = 0;
        self.open_until = None;
        self.next_backoff = self.policy.base;
        self.last_ok = Some(now);
        if let Some(t) = ttfb {
            let ms = t.as_secs_f64() * 1000.0;
            self.ttfb_ms = Some(match self.ttfb_ms {
                None => ms,
                Some(prev) => prev * 0.7 + ms * 0.3,
            });
        }
    }

    /// Connect/TLS failure, reset before headers or a stream broken upstream:
    /// open at once. Returns true if the breaker went from closed to open.
    pub fn on_hard_failure(&mut self, now: Instant, reason: impl Into<String>) -> bool {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.last_error = Some(reason.into());
        self.open(now)
    }

    /// 502/503/504 or an edge interception: open after `threshold` in a row (at
    /// once when half-open). Returns true if the breaker went from closed to open.
    pub fn on_soft_failure(&mut self, now: Instant, reason: impl Into<String>) -> bool {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.last_error = Some(reason.into());
        if self.open_until.is_some() || self.consecutive_failures >= self.policy.threshold {
            self.open(now)
        } else {
            false
        }
    }

    /// Result of an unauthenticated health probe.
    pub fn on_probe(&mut self, now: Instant, alive: bool, detail: Option<String>) -> bool {
        if alive {
            let was_open = self.open_until.is_some();
            self.consecutive_failures = 0;
            self.open_until = None;
            self.next_backoff = self.policy.base;
            was_open
        } else {
            self.on_hard_failure(now, detail.unwrap_or_else(|| "health probe failed".into()))
        }
    }

    fn open(&mut self, now: Instant) -> bool {
        let was_closed = self.open_until.is_none();
        self.open_until = Some(now + self.next_backoff);
        self.next_backoff = (self.next_backoff * 2).min(self.policy.max);
        was_closed
    }
}
