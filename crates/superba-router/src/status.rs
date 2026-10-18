//! Status and metrics served by the admin API (`GET /_superba/v1/status`) and
//! read by the GUI and `spb`. Never contains keys, tokens or bodies.

use serde::{Deserialize, Serialize};
use superba_core::RouterStatsStatus;

use crate::config::MemberKind;
use crate::health::LineState;

/// Service name reported by the health endpoint.
pub const SERVICE_NAME: &str = "superba-router";

/// Unauthenticated liveness answer (`GET /_superba/v1/health`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthReply {
    /// Always `superba-router`.
    pub service: String,
    /// Router version.
    pub version: String,
    /// Random id of this process; matches `runtime.json`.
    pub instance_id: String,
}

/// Full router status.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RouterStatus {
    /// Always `superba-router`.
    pub service: String,
    /// Router version.
    pub version: String,
    /// Random id of this process.
    pub instance_id: String,
    /// Process id.
    pub pid: u32,
    /// Listening address (`127.0.0.1:<port>`).
    pub listen: String,
    /// Codex `openai_base_url` served by this router.
    pub base_url: String,
    /// Start time, Unix milliseconds.
    pub started_at_ms: u64,
    /// Uptime in milliseconds.
    pub uptime_ms: u64,
    /// Configuration generation (increments on each reload).
    pub config_generation: u64,
    /// Members in priority order.
    pub members: Vec<MemberStatus>,
    /// Totals since start.
    pub totals: Totals,
    /// Conversations currently pinned.
    pub sticky_sessions: usize,
    /// Most recent routing events, newest last.
    pub recent: Vec<RouteEvent>,
    /// Request recording (`None` without a `stats` configuration).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<RouterStatsStatus>,
    /// This computer's side: the descriptor budget and how often it ran short
    /// (default from a router that does not report it).
    #[serde(default)]
    pub local: LocalStatus,
    /// WSL listeners currently bound (`<adapter address>:<port>`, user bug 45).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wsl_listen: Vec<String>,
}

/// The router's local budget ([`crate::limits`]) and the local shortages it
/// met. These are never counted against a line.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalStatus {
    /// Soft open-file limit found at start (`None`: unlimited or not
    /// applicable, as on Windows).
    pub open_files_limit: Option<u64>,
    /// Soft open-file limit now.
    pub open_files_limit_now: Option<u64>,
    /// Proxied requests and WebSocket sessions allowed at once.
    pub max_concurrent: u64,
    /// Requests and WebSocket sessions admitted now.
    pub in_flight: u64,
    /// Inbound connections allowed at once.
    pub max_connections: u64,
    /// Inbound connections open now.
    pub connections: u64,
    /// Requests answered `local_router_busy` because every admission was in use.
    pub busy: u64,
    /// Unix milliseconds of the last such answer.
    pub last_busy_ms: Option<u64>,
    /// "Too many open files" errors met locally (accept, upstream connect,
    /// probe, request log).
    pub file_limit_errors: u64,
    /// The last of them.
    pub last_file_limit: Option<LocalProblem>,
}

impl LocalStatus {
    /// The view the switch service, the GUI and `spb` show.
    pub fn view(&self) -> superba_core::RouterLocalView {
        superba_core::RouterLocalView {
            open_files_limit: self.open_files_limit_now.or(self.open_files_limit),
            max_concurrent: self.max_concurrent,
            in_flight: self.in_flight,
            busy: self.busy,
            file_limit_errors: self.file_limit_errors,
            last_problem_ms: self.last_problem_ms(),
            last_file_limit: self
                .last_file_limit
                .as_ref()
                .map(|p| format!("{}: {}", p.place, p.detail)),
        }
    }

    /// Unix milliseconds of the last local shortage (busy answer or file
    /// limit), if any.
    pub fn last_problem_ms(&self) -> Option<u64> {
        self.last_busy_ms
            .into_iter()
            .chain(self.last_file_limit.as_ref().map(|p| p.at_ms))
            .max()
    }
}

/// A local "too many open files" error.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalProblem {
    /// Unix milliseconds.
    pub at_ms: u64,
    /// What the router was doing (`connecting to krill/cdn`, ...).
    pub place: String,
    /// The OS error (no secret, URL or body).
    pub detail: String,
}

/// One member.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemberStatus {
    /// Member id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Kind.
    pub kind: MemberKind,
    /// Serves WebSocket.
    pub websocket: bool,
    /// Number of model mapping entries.
    pub model_mappings: usize,
    /// Lines in priority order.
    pub lines: Vec<LineStatus>,
}

/// One line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LineStatus {
    /// Line id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Upstream base URL (no credentials).
    pub base_url: String,
    /// Breaker state.
    pub state: LineState,
    /// Consecutive failures since the last success.
    pub consecutive_failures: u32,
    /// Milliseconds until an open breaker turns half-open.
    pub reopens_in_ms: Option<u64>,
    /// Last failure (redacted; no key, body or query string).
    pub last_error: Option<String>,
    /// Milliseconds since the last success.
    pub last_ok_ms_ago: Option<u64>,
    /// Smoothed time to response headers.
    pub ttfb_ms: Option<u64>,
    /// Requests answered through this line.
    pub requests: u64,
    /// Attempts that failed over away from this line.
    pub failovers_away: u64,
    /// Streams broken by the upstream after the response started.
    pub broken_streams: u64,
    /// WebSocket connections opened through this line.
    pub websockets: u64,
    /// WebSocket connections currently open through this line.
    pub active_websockets: u64,
    /// Responses request bodies are zstd-compressed for this line now (its
    /// probe found zstd support and it has not refused a zstd body since).
    #[serde(default)]
    pub compress_requests: bool,
    /// The line is offered for WebSocket handshakes now (no 426 from its probe
    /// or a recent handshake).
    #[serde(default)]
    pub offers_websocket: bool,
}

/// Totals since start.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Totals {
    /// Proxied HTTP requests (WebSocket handshakes excluded).
    pub requests: u64,
    /// Requests currently in flight (until their response body ends).
    pub active_requests: u64,
    /// WebSocket connections opened.
    pub websockets: u64,
    /// WebSocket connections currently open.
    pub active_websockets: u64,
    /// Attempts that failed over to another line or member.
    pub failovers: u64,
    /// Requests answered `router_unavailable` (every candidate failed).
    pub unavailable: u64,
    /// Requests refused for a missing or wrong router token.
    pub rejected: u64,
    /// WebSocket handshakes answered 426 by the router itself.
    pub ws_refused: u64,
}

/// Kind of a routing event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteEventKind {
    /// An attempt failed before any byte reached Codex; the next candidate was tried.
    Failover,
    /// A line's breaker opened.
    LineDown,
    /// A line's breaker closed again.
    LineUp,
    /// Every candidate failed; Codex got `router_unavailable`.
    Unavailable,
    /// The upstream broke a stream after the response started.
    StreamBroken,
    /// A conversation moved to another member.
    MemberChanged,
    /// The configuration was reloaded.
    Reloaded,
    /// This computer ran short (too many open files, or every admission in
    /// use): requests got 503 `local_router_busy`; no line was blamed. Shown at
    /// most once a minute.
    LocalLimit,
    /// A compacted conversation (or a compaction request) was kept off
    /// members that cannot read it, or refused because no member can
    /// ([`crate::compaction`]).
    Compaction,
    /// A member could not read content another provider encrypted in a
    /// conversation; the router left it out and sent the request again
    /// ([`crate::cipher`]).
    Ciphertext,
}

/// One routing event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteEvent {
    /// Unix milliseconds.
    pub at_ms: u64,
    /// Kind.
    pub kind: RouteEventKind,
    /// Member id, when the event concerns one.
    pub member: Option<String>,
    /// Line id, when the event concerns one.
    pub line: Option<String>,
    /// Short detail (status code, error kind). Never secrets or bodies.
    pub detail: Option<String>,
}
