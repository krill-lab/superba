//! View model of the local router page: whether Codex is pointed at the router,
//! the per-user service, the configured members in priority order and the live
//! status from the router's admin API. Pure data and helpers (unit tested).

use superba_core::ConfigState;

/// Kind of a member. The official ChatGPT login is never a member.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MemberKind {
    /// A Krill relay; its lines are interchangeable entry points.
    Krill,
    /// A custom OpenAI-compatible site.
    Compatible,
}

/// Breaker state of a line, as the router reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineHealth {
    /// Taking traffic.
    Up,
    /// Breaker open: skipped until it half-opens.
    Down,
    /// Half-open: the next request or probe decides.
    Probing,
    /// No live data (router not running).
    Unknown,
}

/// Live metrics of one line.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LineMetrics {
    /// Smoothed time to response headers.
    pub ttfb_ms: Option<u64>,
    /// Requests answered through this line.
    pub requests: u64,
    /// Attempts that failed over away from it.
    pub failovers_away: u64,
    /// Streams broken after the response started.
    pub broken_streams: u64,
    /// WebSockets open now.
    pub active_websockets: u64,
    /// Milliseconds until an open breaker half-opens.
    pub reopens_in_ms: Option<u64>,
    /// Last failure (redacted by the router).
    pub last_error: Option<String>,
}

/// One line of a member.
#[derive(Clone, Debug, PartialEq)]
pub struct LineView {
    /// Line id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Upstream base URL (no credentials).
    pub base_url: String,
    /// Health.
    pub health: LineHealth,
    /// Metrics, when the router is running.
    pub metrics: Option<LineMetrics>,
}

/// One member in priority order.
#[derive(Clone, Debug, PartialEq)]
pub struct MemberView {
    /// Member id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Kind.
    pub kind: MemberKind,
    /// Lines in priority order.
    pub lines: Vec<LineView>,
    /// Serves Responses over WebSocket.
    pub websocket: bool,
    /// Number of model mapping entries.
    pub model_mappings: usize,
    /// A key is stored for the member.
    pub has_key: bool,
}

impl MemberView {
    /// Lines currently taking traffic.
    pub fn healthy_lines(&self) -> usize {
        self.lines
            .iter()
            .filter(|l| matches!(l.health, LineHealth::Up | LineHealth::Probing))
            .count()
    }
}

/// A site that could join the router.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    /// Site id.
    pub site_id: String,
    /// Profile whose key the member would use.
    pub profile_id: String,
    /// Display name.
    pub name: String,
    /// Kind.
    pub kind: MemberKind,
}

/// The per-user service that keeps the router running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceView {
    /// Backend label (`systemd --user`, `LaunchAgent`, ...).
    pub backend: String,
    /// The service definition exists.
    pub installed: bool,
    /// Running, when the service manager can tell.
    pub running: Option<bool>,
    /// Process id, when known.
    pub pid: Option<u32>,
    /// Short diagnostic.
    pub detail: Option<String>,
}

/// Totals since the router started.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    /// Proxied requests.
    pub requests: u64,
    /// In flight now.
    pub active_requests: u64,
    /// Failovers.
    pub failovers: u64,
    /// Requests no member could serve.
    pub unavailable: u64,
    /// WebSockets open now.
    pub active_websockets: u64,
    /// Conversations pinned to a member.
    pub sticky_sessions: usize,
}

/// A recent routing event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventView {
    /// Unix milliseconds.
    pub at_ms: u64,
    /// i18n key of the kind (`router.event.failover`, ...).
    pub kind_key: &'static str,
    /// Severity of the kind.
    pub severe: bool,
    /// Member name or id.
    pub member: Option<String>,
    /// Line name or id.
    pub line: Option<String>,
    /// Short detail.
    pub detail: Option<String>,
}

/// Live data from the admin API.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Live {
    /// Router version.
    pub version: String,
    /// Listening address (`127.0.0.1:<port>`).
    pub listen: String,
    /// Uptime.
    pub uptime_ms: u64,
    /// Totals.
    pub totals: Totals,
    /// Newest last.
    pub events: Vec<EventView>,
    /// Local capacity and shortages (user bug 35).
    pub local: superba_core::RouterLocalView,
}

/// How long the router page warns about a local shortage after the last one.
pub const LOCAL_SHORTAGE_WINDOW_MS: u64 = 15 * 60 * 1000;

/// The router ran short locally within [`LOCAL_SHORTAGE_WINDOW_MS`] before
/// `now_ms`.
pub fn recent_shortage(local: &superba_core::RouterLocalView, now_ms: u64) -> bool {
    local.ran_short()
        && local
            .last_problem_ms
            .is_some_and(|at| now_ms.saturating_sub(at) <= LOCAL_SHORTAGE_WINDOW_MS)
}

/// Where Codex sends its requests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Directly to a site (router off).
    Direct,
    /// To the local router on this port.
    Router {
        /// Loopback port.
        port: u16,
    },
    /// Unknown (no configuration read yet).
    Unknown,
}

impl Mode {
    /// Detect router mode from the observed configuration (see
    /// [`Mode::from_base_url`]).
    pub fn from_config(config: Option<&ConfigState>) -> Mode {
        match config {
            None => Mode::Unknown,
            Some(c) => Mode::from_base_url(c.openai_base_url.as_deref()),
        }
    }

    /// `openai_base_url` on `http://127.0.0.1:<port>` (or `localhost`, `[::1]`)
    /// means Codex talks to the local router; anything else is direct.
    pub fn from_base_url(base: Option<&str>) -> Mode {
        let Some(base) = base else {
            return Mode::Direct;
        };
        match url::Url::parse(base) {
            Ok(url)
                if url.scheme() == "http"
                    && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")) =>
            {
                Mode::Router {
                    port: url.port().unwrap_or(80),
                }
            }
            _ => Mode::Direct,
        }
    }

    /// True when Codex goes through the router.
    pub fn is_router(&self) -> bool {
        matches!(self, Mode::Router { .. })
    }
}

/// Everything the page shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    /// Members in priority order.
    pub members: Vec<MemberView>,
    /// Sites that could be added.
    pub candidates: Vec<Candidate>,
    /// Names of sites that cannot join yet because no key is stored for them.
    pub keyless: Vec<String>,
    /// The service, when the platform has one.
    pub service: Option<ServiceView>,
    /// Live status; `Err` text when the router does not answer.
    pub live: Result<Live, String>,
    /// Configured port, when a configuration exists.
    pub port: Option<u16>,
    /// Request recording counters of the running router.
    pub recording: Option<superba_core::RouterStatsStatus>,
    /// Every provider as a possible member with its eligibility, from the same
    /// profile set as `members` (the gate of 开启; user bug 39).
    pub eligibility: Vec<superba_switch::router_doctor::RouterCandidate>,
    /// The router program, when it lives on a network share (a Windows UNC
    /// path or mapped network drive): the service may not start at login.
    pub network_exe: Option<String>,
    /// Claude Code on the router (its providers), when it is set to use it.
    pub claude: Option<superba_core::RouterClaudeView>,
}

/// Move the member at `from` one step up (`up = true`) or down; returns the new
/// order, or `None` when it cannot move.
pub fn reorder<T: Clone>(items: &[T], from: usize, up: bool) -> Option<Vec<T>> {
    let to = if up {
        from.checked_sub(1)?
    } else {
        from.checked_add(1).filter(|&t| t < items.len())?
    };
    if from >= items.len() {
        return None;
    }
    let mut v = items.to_vec();
    v.swap(from, to);
    Some(v)
}

/// `1h 05m`, `3m 20s`, `12s` from milliseconds.
pub fn duration_text(ms: u64) -> String {
    let s = ms / 1000;
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {sec:02}s")
    } else {
        format!("{sec}s")
    }
}

/// Group thousands with commas: `12,345`.
pub fn count_text(n: u64) -> String {
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
