//! Shared server state: the current configuration generation (members, lines,
//! health, stickiness, HTTP clients), totals and the event ring.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use http::HeaderValue;
use http_body_util::combinators::UnsyncBoxBody;
use superba_core::{Error, Result, Secret};

use crate::config::{LineConfig, MemberConfig, RouterConfig, RouterSecrets};
use crate::health::{BreakerPolicy, LineHealth};
use crate::limits::{Budget, Limits, Place};
use crate::paths::RouterPaths;
use crate::select::Topology;
use crate::stats::hub::StatsHub;
use crate::status::{RouteEvent, RouteEventKind};
use crate::sticky::StickyTable;

/// Boxed error of response bodies.
pub(crate) type BoxError = Box<dyn std::error::Error + Send + Sync>;
/// Response body type of every handler.
pub(crate) type RespBody = UnsyncBoxBody<Bytes, BoxError>;

/// Number of routing events kept for the status page.
const EVENT_RING: usize = 64;

/// Lock a mutex, recovering from poisoning (state stays usable after a panic in
/// another task; every critical section leaves it consistent).
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Unix milliseconds now.
pub(crate) fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Per-line counters.
#[derive(Debug, Default)]
pub(crate) struct LineCounters {
    pub requests: AtomicU64,
    pub failovers_away: AtomicU64,
    pub broken_streams: AtomicU64,
    pub websockets: AtomicU64,
    pub active_websockets: AtomicU64,
}

/// One line at run time.
#[derive(Debug)]
pub(crate) struct LineRt {
    pub cfg: LineConfig,
    pub health: Mutex<LineHealth>,
    pub counters: LineCounters,
    /// A health probe is in flight.
    pub probing: AtomicBool,
}

/// One member at run time.
pub(crate) struct MemberRt {
    pub cfg: MemberConfig,
    /// `Authorization: Bearer <member key>` (marked sensitive).
    pub auth: HeaderValue,
    pub lines: Vec<LineRt>,
}

impl std::fmt::Debug for MemberRt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemberRt")
            .field("id", &self.cfg.id)
            .field("lines", &self.lines.len())
            .finish_non_exhaustive()
    }
}

/// One configuration generation. In-flight requests keep the generation they
/// started with; a reload swaps in a new one.
pub(crate) struct Generation {
    pub number: u64,
    pub config: RouterConfig,
    pub router_token: Secret,
    pub members: Vec<MemberRt>,
    /// Upstream client for HTTP requests (HTTP/1.1 or h2).
    pub http: reqwest::Client,
    /// Upstream client for WebSocket handshakes (HTTP/1.1 only).
    pub ws: reqwest::Client,
    /// Client for unauthenticated health probes (with a total timeout).
    pub probe: reqwest::Client,
    pub sticky: Mutex<StickyTable>,
}

impl Generation {
    /// Build a generation; validates the configuration and the secrets.
    /// `budget` sizes the upstream connection pool.
    pub fn build(
        number: u64,
        config: RouterConfig,
        secrets: &RouterSecrets,
        budget: &Budget,
    ) -> Result<Self> {
        config.validate()?;
        secrets.validate_for(&config)?;
        let t = &config.tuning;
        let policy = BreakerPolicy {
            threshold: t.breaker_threshold,
            base: Duration::from_millis(t.breaker_base_ms),
            max: Duration::from_millis(t.breaker_max_ms),
        };
        let mut members = Vec::with_capacity(config.members.len());
        for m in &config.members {
            let key = secrets.key(&m.id).ok_or_else(|| Error::Secret {
                message: format!("no API key for router member {:?}", m.id),
            })?;
            let mut auth =
                HeaderValue::from_str(&format!("Bearer {}", key.expose())).map_err(|_| {
                    Error::Secret {
                        message: format!(
                            "the API key of router member {:?} contains invalid characters",
                            m.id
                        ),
                    }
                })?;
            auth.set_sensitive(true);
            members.push(MemberRt {
                cfg: m.clone(),
                auth,
                lines: m
                    .lines
                    .iter()
                    .map(|l| LineRt {
                        cfg: l.clone(),
                        health: Mutex::new(LineHealth::new(policy)),
                        counters: LineCounters::default(),
                        probing: AtomicBool::new(false),
                    })
                    .collect(),
            });
        }
        let http = build_client(&config, ClientKind::Http, budget)?;
        let ws = build_client(&config, ClientKind::WebSocket, budget)?;
        let probe = build_client(&config, ClientKind::Probe, budget)?;
        Ok(Self {
            number,
            sticky: Mutex::new(StickyTable::new(
                Duration::from_secs(t.sticky_ttl_secs),
                t.sticky_capacity,
            )),
            router_token: secrets.router_token.clone(),
            config,
            members,
            http,
            ws,
            probe,
        })
    }

    /// What compaction routing needs to know about the members.
    pub fn member_facts(&self) -> Vec<crate::compaction::MemberFacts<'_>> {
        self.members
            .iter()
            .map(|m| crate::compaction::MemberFacts {
                id: &m.cfg.id,
                name: &m.cfg.name,
                compaction: m.cfg.compaction,
            })
            .collect()
    }

    /// `member/line` label for logs and the route header.
    pub fn label(&self, member: usize, line: usize) -> String {
        let m = &self.members[member];
        format!("{}/{}", m.cfg.id, m.lines[line].cfg.id)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClientKind {
    Http,
    WebSocket,
    Probe,
}

fn build_client(
    config: &RouterConfig,
    kind: ClientKind,
    budget: &Budget,
) -> Result<reqwest::Client> {
    let t = &config.tuning;
    // Idle upstream connections hold descriptors too: capped per host, closed
    // after a while (reqwest keeps any number for 90 s by default).
    let idle_per_host = budget.pool_max_idle_per_host;
    let mut b = reqwest::Client::builder()
        .connect_timeout(Duration::from_millis(t.connect_timeout_ms))
        // A redirect is passed back to Codex, never followed with a member's key.
        .redirect(reqwest::redirect::Policy::none())
        // Bodies pass through byte for byte: never decode a response, even if
        // feature unification enables reqwest's decompression elsewhere.
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .tcp_keepalive(Duration::from_secs(30))
        .tcp_nodelay(true)
        .pool_idle_timeout(Duration::from_secs(t.pool_idle_timeout_secs));
    if !config.use_system_proxy {
        b = b.no_proxy();
    }
    if let Some(path) = &config.extra_ca_file {
        let pem = std::fs::read(path).map_err(|e| Error::io(path, &e))?;
        let certs = reqwest::Certificate::from_pem_bundle(&pem)
            .map_err(|e| Error::parse("router extra CA file", e))?;
        b = b.tls_certs_merge(certs);
    }
    match kind {
        ClientKind::Http => b = b.pool_max_idle_per_host(idle_per_host),
        // Upgrades need HTTP/1.1; ALPN must not pick h2. An upgraded connection
        // leaves the pool, and one that was refused is not worth keeping.
        ClientKind::WebSocket => b = b.http1_only().pool_max_idle_per_host(0),
        ClientKind::Probe => {
            b = b
                .timeout(Duration::from_millis(t.probe_timeout_ms))
                .pool_max_idle_per_host(0);
        }
    }
    b.build()
        .map_err(|e| Error::other(format!("router HTTP client: {}", e.without_url())))
}

/// Topology view used by candidate selection.
pub(crate) struct View<'a> {
    pub generation: &'a Generation,
    pub websocket_only: bool,
    /// Members a compaction-aware request may go to ([`crate::compaction`]).
    pub route: &'a crate::compaction::Route,
}

impl Topology for View<'_> {
    fn members(&self) -> usize {
        self.generation.members.len()
    }
    fn lines(&self, member: usize) -> usize {
        self.generation.members[member].lines.len()
    }
    fn eligible(&self, member: usize) -> bool {
        (!self.websocket_only || self.generation.members[member].cfg.websocket)
            && self.route.allows(member)
    }
    fn available(&self, member: usize, line: usize, now: Instant) -> bool {
        lock(&self.generation.members[member].lines[line].health).is_available(now)
    }
    fn reopens_at(&self, member: usize, line: usize) -> Option<Instant> {
        lock(&self.generation.members[member].lines[line].health).open_until()
    }
}

/// Totals since start.
#[derive(Debug, Default)]
pub(crate) struct Totals {
    pub requests: AtomicU64,
    pub active_requests: AtomicU64,
    pub websockets: AtomicU64,
    pub active_websockets: AtomicU64,
    pub failovers: AtomicU64,
    pub unavailable: AtomicU64,
    pub rejected: AtomicU64,
    pub ws_refused: AtomicU64,
}

/// State shared by every connection.
pub(crate) struct Shared {
    pub generation: RwLock<Arc<Generation>>,
    pub paths: Option<RouterPaths>,
    pub instance_id: String,
    pub admin_token: Secret,
    pub started: Instant,
    pub started_at_ms: u64,
    pub listen: SocketAddr,
    pub totals: Totals,
    pub events: Mutex<VecDeque<RouteEvent>>,
    pub shutdown: tokio::sync::watch::Sender<bool>,
    /// Request log, live rows, requests in flight, turn totals.
    pub stats: Arc<StatsHub>,
    /// zstd and WebSocket refusals met at run time (kept across reloads).
    pub transports: crate::transport::TransportMemory,
    /// Admissions, connection places and local shortages.
    pub limits: Limits,
    /// When to try opening the request log again (it failed for want of
    /// descriptors).
    pub stats_retry: Mutex<Option<Instant>>,
    /// WSL listeners currently bound (user bug 45).
    pub wsl_bound: Mutex<Vec<SocketAddr>>,
}

impl Shared {
    /// The current generation.
    pub fn current(&self) -> Arc<Generation> {
        self.generation
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Replace the generation (and apply its statistics settings).
    pub fn install(&self, generation: Generation) {
        self.apply_stats(generation.config.stats.as_ref());
        *self.generation.write().unwrap_or_else(|p| p.into_inner()) = Arc::new(generation);
    }

    /// Start (or adjust) the request log. A start that failed because this
    /// process is out of descriptors is a local shortage and is tried again a
    /// minute later ([`Shared::retry_stats`]).
    pub fn apply_stats(&self, cfg: Option<&crate::stats::StatsConfig>) {
        match self.stats.apply(cfg) {
            Ok(()) => *lock(&self.stats_retry) = None,
            Err(e) if crate::limits::is_file_limit_error(&e) => {
                self.local_file_limit(Place::RequestLog, &e.detail());
                *lock(&self.stats_retry) = Some(Instant::now() + crate::limits::WARN_EVERY);
            }
            Err(e) => {
                *lock(&self.stats_retry) = None;
                tracing::warn!(error = %e, "request log unavailable; not recording");
            }
        }
    }

    /// Open the request log again if it failed for want of descriptors and its
    /// retry is due.
    pub fn retry_stats(&self) {
        let due = lock(&self.stats_retry).is_some_and(|at| Instant::now() >= at);
        if due {
            self.apply_stats(self.current().config.stats.as_ref());
        }
    }

    /// Record a routing event (and log it).
    pub fn event(
        &self,
        kind: RouteEventKind,
        member: Option<&str>,
        line: Option<&str>,
        detail: Option<String>,
    ) {
        let ev = RouteEvent {
            at_ms: unix_ms(),
            kind,
            member: member.map(str::to_owned),
            line: line.map(str::to_owned),
            detail,
        };
        tracing::info!(
            kind = ?ev.kind,
            member = ev.member.as_deref().unwrap_or("-"),
            line = ev.line.as_deref().unwrap_or("-"),
            detail = ev.detail.as_deref().unwrap_or(""),
            "route event"
        );
        self.push_event(ev);
    }

    /// A local "too many open files" at `place`: counted, logged as a warning
    /// and shown as an event at most once a minute, never a line's failure.
    /// Returns the text for the 503 answer.
    pub fn local_file_limit(&self, place: Place, error: &dyn std::fmt::Display) -> String {
        let detail = error.to_string();
        let limit = self.limits.budget.open_files_limit;
        let text = match limit {
            Some(n) => format!(
                "too many open files on this computer (limit {n}) while {place}; no line was marked down"
            ),
            None => format!(
                "too many open files on this computer while {place}; no line was marked down"
            ),
        };
        if let Some(suppressed) = self.limits.note_file_limit(&place, &detail) {
            tracing::warn!(
                %place,
                error = %detail,
                open_files_limit = ?limit,
                in_flight = self.limits.in_flight(),
                connections = self.limits.open_connections(),
                not_logged_since = suppressed,
                "local open-file limit reached; answering 503 local_router_busy (not a line failure)"
            );
            self.push_event(RouteEvent {
                at_ms: unix_ms(),
                kind: RouteEventKind::LocalLimit,
                member: None,
                line: None,
                detail: Some(text.clone()),
            });
        }
        text
    }

    /// A request that got no admission in time. Returns the text for the 503
    /// answer.
    pub fn local_busy(&self) -> String {
        let max = self.limits.budget.max_concurrent;
        let text = format!(
            "local router busy: {max} requests and WebSocket sessions are already in progress; try again shortly"
        );
        if let Some(suppressed) = self.limits.note_busy() {
            tracing::warn!(
                max_concurrent = max,
                open_files_limit = ?self.limits.budget.open_files_limit,
                not_logged_since = suppressed,
                "every admission in use; answering 503 local_router_busy"
            );
            self.push_event(RouteEvent {
                at_ms: unix_ms(),
                kind: RouteEventKind::LocalLimit,
                member: None,
                line: None,
                detail: Some(text.clone()),
            });
        }
        text
    }

    fn push_event(&self, ev: RouteEvent) {
        let mut ring = lock(&self.events);
        if ring.len() >= EVENT_RING {
            ring.pop_front();
        }
        ring.push_back(ev);
    }

    /// Ask the server to stop.
    pub fn request_shutdown(&self) {
        let _ = self.shutdown.send(true);
    }
}

/// Decrements a gauge when dropped.
pub(crate) struct GaugeGuard {
    shared: Arc<Shared>,
    pick: fn(&Totals) -> &AtomicU64,
}

impl GaugeGuard {
    /// Increment the gauge now; decrement on drop.
    pub fn new(shared: Arc<Shared>, pick: fn(&Totals) -> &AtomicU64) -> Self {
        pick(&shared.totals).fetch_add(1, Ordering::Relaxed);
        Self { shared, pick }
    }
}

impl Drop for GaugeGuard {
    fn drop(&mut self) {
        (self.pick)(&self.shared.totals).fetch_sub(1, Ordering::Relaxed);
    }
}
