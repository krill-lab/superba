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
//! - A failed health probe of a closed line only makes it suspect: the line keeps
//!   serving and is probed again after [`RECHECK`]; a second failure in a row
//!   opens it. One blip of this computer's network must not take a good line
//!   out of service.
//! - When every line is open at once ([`LineHealth::all_down`]), the cause is
//!   most likely this computer's network rather than every line together: the
//!   router probes all of them every few seconds and keeps their backoff at its
//!   first step (see `probe::health_loop`).
//!
//! Pure state over [`Instant`]s passed in, so tests drive it deterministically.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Delay before a suspect line (one failed probe) is probed again.
pub const RECHECK: Duration = Duration::from_millis(1500);

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
    /// A probe failed while the line was closed: probe again at this time.
    recheck_at: Option<Instant>,
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
            recheck_at: None,
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
        self.recheck_at = None;
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

    /// Result of an unauthenticated health probe. A failure opens an open or
    /// half-open line again, but a closed line only after a second failed probe
    /// in a row ([`RECHECK`] apart). Returns true when the breaker opened or
    /// closed.
    pub fn on_probe(&mut self, now: Instant, alive: bool, detail: Option<String>) -> bool {
        if alive {
            let was_open = self.open_until.is_some();
            self.consecutive_failures = 0;
            self.open_until = None;
            self.recheck_at = None;
            self.next_backoff = self.policy.base;
            was_open
        } else {
            let detail = detail.unwrap_or_else(|| "health probe failed".into());
            if self.open_until.is_none() && self.recheck_at.is_none() {
                self.last_error = Some(detail);
                self.recheck_at = Some(now + RECHECK);
                return false;
            }
            self.recheck_at = None;
            self.on_hard_failure(now, detail)
        }
    }

    /// A suspect line is due for its second probe at `now`.
    pub fn recheck_due(&self, now: Instant) -> bool {
        self.open_until.is_none() && self.recheck_at.is_some_and(|at| at <= now)
    }

    /// Every line is open: keep the next open period at its first step, so the
    /// lines come back as soon as this computer's network does.
    pub fn all_down(&mut self) {
        self.next_backoff = self.policy.base;
    }

    fn open(&mut self, now: Instant) -> bool {
        let was_closed = self.open_until.is_none();
        self.open_until = Some(now + self.next_backoff);
        self.next_backoff = (self.next_backoff * 2).min(self.policy.max);
        was_closed
    }
}
