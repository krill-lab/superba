//! The router's local budget: how many proxied requests and WebSocket sessions
//! run at once, how many inbound connections are open, and what happens when
//! this process runs out of file descriptors (user bug 35).
//!
//! Every proxied request holds two sockets (Codex's connection and the upstream
//! one) and a WebSocket session holds two for its whole life; launchd starts
//! agents with a soft limit of 256 open files. Unbounded, about 110 concurrent
//! requests exhausted it, and the `EMFILE` of the next upstream connect was
//! counted as the line's failure: breakers opened and every line looked down.
//!
//! - **Admissions** ([`Budget::max_concurrent`]): a semaphore bounds proxied
//!   HTTP requests (until their response body ends) and WebSocket sessions
//!   (handshake to close). A request that gets no admission within
//!   [`Budget::busy_wait_ms`] is answered 503 `local_router_busy` with
//!   `Connection: close` and `Retry-After`; it never reaches a member and is
//!   never a failover. The admin API is not counted, so status keeps working.
//! - **Inbound connections** ([`Budget::max_connections`]): at the cap the
//!   connection just accepted waits for a place (further ones wait in the
//!   listen backlog) and the open connections are asked to stop keep-alive:
//!   idle ones close at once, busy ones after their response, nothing in
//!   flight is cut. An upgraded WebSocket keeps its connection's place.
//! - **Idle keep-alive**: an idle connection from Codex is closed after
//!   `Tuning::inbound_idle_timeout_secs` (150 s), longer than HTTP clients keep
//!   idle connections (reqwest / hyper: 90 s), so Codex never sends a request
//!   on a connection the router has just closed. Only while the inbound budget
//!   is nearly full ([`Budget::pressure_connections`], 90 %) is a connection
//!   idle for `Tuning::pressure_idle_timeout_ms` (5 s) closed, one at a time
//!   until the pressure is gone; a connection with a response in flight is
//!   never closed for being idle.
//! - **Upstream pool**: idle connections per upstream host are capped
//!   ([`Budget::pool_max_idle_per_host`]; none for WebSocket handshakes) and
//!   closed after `Tuning::pool_idle_timeout_secs`.
//! - **Local file limits**: `EMFILE` / `ENFILE` (Windows: too many open files or
//!   sockets) met while accepting, connecting upstream (DNS included), probing
//!   or opening the request log is this computer's problem, never a line's: no
//!   breaker, no failover, no "line down". The request gets 503
//!   `local_router_busy` naming the limit, a warning is logged at most once a
//!   minute, and [`crate::status::LocalStatus`] counts it.
//!
//! With the soft open-file limit `L` found at start the defaults keep the
//! descriptors in use below `L`: `L/4` admissions (4 to 512; 512 without a
//! limit) use at most `L/4` upstream sockets, at most `L/2` inbound connections
//! are open, idle upstream connections are capped per host, and the rest is left
//! for the request log, the listener and the runtime.

use std::fmt;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use superba_core::fdlimit::{self, FileLimitHit};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::config::Tuning;
use crate::state::{lock, unix_ms};
use crate::status::{LocalProblem, LocalStatus};

/// Most admissions ever derived from the open-file limit (and the default
/// where there is no limit, as on Windows).
pub const MAX_CONCURRENT_CAP: usize = 512;
/// Fewest admissions derived from a (very low) open-file limit.
pub const MIN_CONCURRENT: usize = 4;
/// Most idle upstream connections kept per host by default.
pub const POOL_IDLE_CAP: usize = 16;
/// At most one warning per kind of local problem in this period.
pub const WARN_EVERY: Duration = Duration::from_secs(60);
/// `Retry-After` of a `local_router_busy` answer, in seconds.
pub const RETRY_AFTER_SECS: u64 = 1;

/// Error type and code of the router's 503 for local shortages.
pub const LOCAL_BUSY_CODE: &str = "local_router_busy";

/// The numbers in effect for this process (fixed at start; a reload changes
/// only the upstream pool).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    /// Soft open-file limit found at start (`None`: unlimited or not
    /// applicable, as on Windows).
    pub open_files_limit: Option<u64>,
    /// Proxied requests and WebSocket sessions at once.
    pub max_concurrent: usize,
    /// Inbound connections open at once.
    pub max_connections: usize,
    /// Open inbound connections from which idle ones are closed early (90 %
    /// of [`Budget::max_connections`]).
    pub pressure_connections: usize,
    /// Idle upstream connections kept per host (HTTP client).
    pub pool_max_idle_per_host: usize,
    /// How long a request waits for an admission.
    pub busy_wait_ms: u64,
}

impl Budget {
    /// The budget for a soft open-file limit (`None`: unlimited) and the
    /// configured overrides.
    pub fn new(open_files_limit: Option<u64>, tuning: &Tuning) -> Self {
        let max_concurrent =
            tuning
                .max_concurrent
                .filter(|n| *n > 0)
                .unwrap_or_else(|| match open_files_limit {
                    Some(limit) => usize::try_from(limit / 4)
                        .unwrap_or(usize::MAX)
                        .clamp(MIN_CONCURRENT, MAX_CONCURRENT_CAP),
                    None => MAX_CONCURRENT_CAP,
                });
        let max_connections = tuning
            .max_connections
            .filter(|n| *n > 0)
            .unwrap_or_else(|| {
                let four_each = max_concurrent.saturating_mul(4);
                let half_limit = open_files_limit
                    .map_or(four_each, |l| usize::try_from(l / 2).unwrap_or(usize::MAX));
                // Room for every admitted request plus a few waiting ones.
                half_limit
                    .min(four_each)
                    .max(max_concurrent.saturating_add(MIN_CONCURRENT))
            });
        let pool_max_idle_per_host = tuning
            .pool_max_idle_per_host
            .unwrap_or_else(|| (max_concurrent / 16).clamp(2, POOL_IDLE_CAP));
        let max_connections = max_connections.max(1);
        // 90 % of the cap, at least one place below it.
        let below_cap = (max_connections / 10).max(1).min(max_connections - 1);
        Self {
            open_files_limit,
            max_concurrent,
            max_connections,
            pressure_connections: max_connections - below_cap,
            pool_max_idle_per_host,
            busy_wait_ms: tuning.busy_wait_ms,
        }
    }

    /// The budget for this process's current soft open-file limit.
    pub fn for_this_process(tuning: &Tuning) -> Self {
        Self::new(fdlimit::open_files_limit(), tuning)
    }
}

/// The open-file limit `err` ran into, searching its whole source chain
/// (reqwest → hyper-util → the `io::Error` of `socket()`, `connect()` or the
/// DNS lookup) and the custom errors wrapped in an `io::Error`.
pub fn file_limit_in(err: &(dyn std::error::Error + 'static)) -> Option<FileLimitHit> {
    let mut current = Some(err);
    let mut depth = 0;
    while let Some(e) = current {
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            if let Some(hit) = fdlimit::file_limit_hit(io) {
                return Some(hit);
            }
            // `io::Error::source` skips the wrapped error itself.
            if let Some(inner) = io.get_ref()
                && depth < 16
                && let Some(hit) = file_limit_in(inner)
            {
                return Some(hit);
            }
        }
        if let Some(superba_core::Error::Io { kind, .. }) = e.downcast_ref::<superba_core::Error>()
            && kind == fdlimit::IO_KIND_TOO_MANY_OPEN_FILES
        {
            return Some(FileLimitHit::Process);
        }
        depth += 1;
        if depth > 16 {
            break;
        }
        current = e.source();
    }
    None
}

/// True for a Superba error that is "too many open files".
pub fn is_file_limit_error(err: &superba_core::Error) -> bool {
    matches!(err, superba_core::Error::Io { kind, .. } if kind == fdlimit::IO_KIND_TOO_MANY_OPEN_FILES)
}

/// Where a local file limit was met.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    /// Accepting a connection from Codex.
    Accept,
    /// Connecting to a member line (`member/line`).
    Upstream(String),
    /// Probing a member line (`member/line`).
    Probe(String),
    /// Opening or writing the request log.
    RequestLog,
    /// Reading the configuration.
    Config,
}

impl fmt::Display for Place {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Place::Accept => f.write_str("accepting a connection"),
            Place::Upstream(route) => write!(f, "connecting to {route}"),
            Place::Probe(route) => write!(f, "probing {route}"),
            Place::RequestLog => f.write_str("opening the request log"),
            Place::Config => f.write_str("reading the configuration"),
        }
    }
}

/// One admitted request or WebSocket session; its place is free again when
/// it is dropped.
#[derive(Debug)]
pub(crate) struct Admission {
    _permit: OwnedSemaphorePermit,
}

/// The place of one inbound connection, shared by the connection task and an
/// upgraded WebSocket that outlives it.
#[derive(Clone, Debug)]
pub(crate) struct ConnSlot {
    _permit: Arc<OwnedSemaphorePermit>,
}

/// A connection closing for the pressure ([`Limits::claim_pressure_close`]).
#[derive(Debug)]
pub(crate) struct PressureClose {
    closing: Arc<AtomicU64>,
}

impl Drop for PressureClose {
    fn drop(&mut self) {
        self.closing.fetch_sub(1, Ordering::AcqRel);
    }
}

/// No admission within the wait.
#[derive(Debug)]
pub(crate) struct Busy;

/// A warning that is due at most once per [`WARN_EVERY`].
#[derive(Debug, Default)]
struct Throttle {
    last: Option<Instant>,
    suppressed: u64,
}

impl Throttle {
    /// `Some(n)` when a warning is due now (`n` occurrences were not logged
    /// since the previous one).
    fn due(&mut self, now: Instant) -> Option<u64> {
        match self.last {
            Some(t) if now.saturating_duration_since(t) < WARN_EVERY => {
                self.suppressed += 1;
                None
            }
            _ => {
                self.last = Some(now);
                Some(std::mem::take(&mut self.suppressed))
            }
        }
    }
}

/// Run-time admissions, connection places and local problem counters.
#[derive(Debug)]
pub(crate) struct Limits {
    pub budget: Budget,
    admissions: Arc<Semaphore>,
    connections: Arc<Semaphore>,
    /// Bumped when an accepted connection waits for a place: open connections
    /// stop keep-alive (idle ones close now, busy ones after their response).
    shed: tokio::sync::watch::Sender<u64>,
    /// Bumped when a connection opens with the budget nearly full: idle
    /// connections switch to the short idle timeout while the pressure lasts.
    pressure: tokio::sync::watch::Sender<u64>,
    /// Connections closing for the pressure (not yet gone).
    pressure_closing: Arc<AtomicU64>,
    busy: AtomicU64,
    file_limit: AtomicU64,
    last_busy_ms: AtomicU64,
    last_problem: Mutex<Option<LocalProblem>>,
    warn_busy: Mutex<Throttle>,
    warn_files: Mutex<Throttle>,
}

impl Limits {
    pub fn new(budget: Budget) -> Self {
        Self {
            budget,
            admissions: Arc::new(Semaphore::new(budget.max_concurrent.max(1))),
            connections: Arc::new(Semaphore::new(budget.max_connections.max(1))),
            shed: tokio::sync::watch::channel(0).0,
            pressure: tokio::sync::watch::channel(0).0,
            pressure_closing: Arc::new(AtomicU64::new(0)),
            busy: AtomicU64::new(0),
            file_limit: AtomicU64::new(0),
            last_busy_ms: AtomicU64::new(0),
            last_problem: Mutex::new(None),
            warn_busy: Mutex::new(Throttle::default()),
            warn_files: Mutex::new(Throttle::default()),
        }
    }

    /// An admission, waiting at most `busy_wait`.
    pub async fn admit(&self, busy_wait: Duration) -> Result<Admission, Busy> {
        let sem = self.admissions.clone();
        if let Ok(permit) = sem.clone().try_acquire_owned() {
            return Ok(Admission { _permit: permit });
        }
        match tokio::time::timeout(busy_wait, sem.acquire_owned()).await {
            Ok(Ok(permit)) => Ok(Admission { _permit: permit }),
            _ => Err(Busy),
        }
    }

    /// The place of a connection just accepted. At the cap, open connections
    /// are asked to stop keep-alive (so idle ones close instead of holding
    /// their descriptor) and this waits until one closes.
    pub async fn connection(&self) -> ConnSlot {
        let sem = self.connections.clone();
        let permit = match sem.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                self.shed.send_modify(|n| *n = n.wrapping_add(1));
                tracing::debug!(
                    max_connections = self.budget.max_connections,
                    "connection cap reached; closing idle keep-alive connections"
                );
                sem.acquire_owned()
                    .await
                    .expect("the connection semaphore is never closed")
            }
        };
        let slot = ConnSlot {
            _permit: Arc::new(permit),
        };
        if self.under_pressure() {
            self.pressure.send_modify(|n| *n = n.wrapping_add(1));
        }
        slot
    }

    /// The inbound budget is nearly full ([`Budget::pressure_connections`]).
    pub fn under_pressure(&self) -> bool {
        self.open_connections() >= self.budget.pressure_connections
    }

    /// Signals that the inbound budget became nearly full (see
    /// [`Limits::under_pressure`]).
    pub fn pressure_signal(&self) -> tokio::sync::watch::Receiver<u64> {
        self.pressure.subscribe()
    }

    /// Permission to close one idle connection for the pressure: given only
    /// while the connections not already closing for it still reach
    /// [`Budget::pressure_connections`], so idle connections close one at a
    /// time until the pressure is gone. Hold it until the connection is gone.
    pub fn claim_pressure_close(&self) -> Option<PressureClose> {
        let closing = self.pressure_closing.fetch_add(1, Ordering::AcqRel);
        let open = u64::try_from(self.open_connections()).unwrap_or(u64::MAX);
        let threshold = u64::try_from(self.budget.pressure_connections).unwrap_or(u64::MAX);
        if open.saturating_sub(closing) >= threshold {
            Some(PressureClose {
                closing: self.pressure_closing.clone(),
            })
        } else {
            self.pressure_closing.fetch_sub(1, Ordering::AcqRel);
            None
        }
    }

    /// Signals for a connection to stop keep-alive (see [`Limits::connection`]).
    pub fn shed_signal(&self) -> tokio::sync::watch::Receiver<u64> {
        self.shed.subscribe()
    }

    /// Admissions in use.
    pub fn in_flight(&self) -> usize {
        self.budget
            .max_concurrent
            .max(1)
            .saturating_sub(self.admissions.available_permits())
    }

    /// Inbound connections open.
    pub fn open_connections(&self) -> usize {
        self.budget
            .max_connections
            .max(1)
            .saturating_sub(self.connections.available_permits())
    }

    /// Count a `local_router_busy` answer for want of an admission. `Some(n)`
    /// when it should be logged and shown as an event now (`n` earlier ones
    /// were not).
    pub fn note_busy(&self) -> Option<u64> {
        self.busy.fetch_add(1, Ordering::Relaxed);
        self.last_busy_ms.store(unix_ms(), Ordering::Relaxed);
        lock(&self.warn_busy).due(Instant::now())
    }

    /// Count a local file-limit error. `Some(n)` when it should be logged and
    /// shown as an event now (`n` earlier ones were not).
    pub fn note_file_limit(&self, place: &Place, detail: &str) -> Option<u64> {
        self.file_limit.fetch_add(1, Ordering::Relaxed);
        *lock(&self.last_problem) = Some(LocalProblem {
            at_ms: unix_ms(),
            place: place.to_string(),
            detail: detail.chars().take(200).collect(),
        });
        lock(&self.warn_files).due(Instant::now())
    }

    /// Snapshot for the admin status.
    pub fn status(&self) -> LocalStatus {
        let u = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
        let last_busy = self.last_busy_ms.load(Ordering::Relaxed);
        LocalStatus {
            open_files_limit: self.budget.open_files_limit,
            open_files_limit_now: fdlimit::open_files_limit(),
            max_concurrent: u(self.budget.max_concurrent),
            in_flight: u(self.in_flight()),
            max_connections: u(self.budget.max_connections),
            connections: u(self.open_connections()),
            busy: self.busy.load(Ordering::Relaxed),
            last_busy_ms: (last_busy != 0).then_some(last_busy),
            file_limit_errors: self.file_limit.load(Ordering::Relaxed),
            last_file_limit: lock(&self.last_problem).clone(),
        }
    }
}
