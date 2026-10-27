//! Router configuration (`router/config.json`) and secrets (`router/secrets.json`).
//!
//! The configuration holds no secret: members (Krill lines and custom compatible
//! sites, in priority order), their lines as full base URLs, per-member model
//! mapping and tuning. The secrets file (0600) holds the router token Codex
//! presents and each member's own API key. Keys live there, not in the OS keyring,
//! because the router is a background service that must start at login without a
//! keyring prompt; the protection is the same as Codex's own `auth.json`, which in
//! router mode holds only the local router token.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use superba_core::{CompactionSupport, Error, Result, Secret, SiteKind};
use url::Url;
use zeroize::Zeroize;

use crate::paths::RouterPaths;
use crate::stats::StatsConfig;
use crate::token;

/// Current format of both files.
pub const CONFIG_FORMAT: u32 = 1;

/// Path prefix Codex is pointed at: `openai_base_url =
/// "http://127.0.0.1:<port>/backend-api/codex"`. Requests below it are routed with
/// the remainder appended to each line's base URL.
pub const ROUTER_BASE_PATH: &str = "/backend-api/codex";

/// Alternative prefix for OpenAI-style clients (`http://127.0.0.1:<port>/v1`),
/// served like [`ROUTER_BASE_PATH`] (Codex's path). The other coding tools
/// use the OpenAI ingress ([`crate::openai::PREFIX`]).
pub const ROUTER_V1_PATH: &str = "/v1";

/// Hosts of the official ChatGPT login. The official login is never a router
/// member (decision 6): it stays a direct switch target.
const CHATGPT_HOSTS: &[&str] = &["chatgpt.com", "chat.openai.com", "auth.openai.com"];

/// The whole router configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouterConfig {
    /// Merge member catalogs and route models to their observed endpoints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<crate::merged_catalog::CatalogConfig>,
    /// File format ([`CONFIG_FORMAT`]).
    pub format: u32,
    /// Loopback port (always bound on `127.0.0.1`). Persisted because Codex's
    /// `openai_base_url` names it. Handed to [`crate::manager::RouterManager`],
    /// `0` keeps the stored port (a first configuration gets the stable port of
    /// its data directory, [`crate::port`]); bound directly
    /// ([`crate::RouterServer::bind`]), `0` picks an ephemeral port (tests only).
    pub listen_port: u16,
    /// Members in priority order (first = preferred).
    pub members: Vec<MemberConfig>,
    /// Extra providers for explicitly applied tools; they never join Codex failover.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_only_members: Vec<String>,
    /// Timeouts, stickiness and breaker parameters.
    #[serde(default)]
    pub tuning: Tuning,
    /// Use the system / environment proxy settings for upstream requests.
    #[serde(default = "default_true")]
    pub use_system_proxy: bool,
    /// Extra PEM root certificate(s) to trust for upstream TLS (e2e forwarders).
    #[serde(default)]
    pub extra_ca_file: Option<PathBuf>,
    /// Request log, stats line and prices. `None` (tests): requests and
    /// WebSockets pass byte for byte and nothing is recorded. Superba always
    /// writes it (see
    /// [`RouterManager::write_config`](crate::manager::RouterManager::write_config)).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<StatsConfig>,
    /// Extra IPv4 addresses to listen on for WSL 2 distributions behind NAT
    /// (the Windows host's address on the WSL adapter, user bug 45), on the same
    /// port. Only source tokens of WSL hosts (`wsl:<distro>`) are accepted
    /// there, and the admin API answers nothing but `health`. Bound while the
    /// address exists (WSL creates the adapter when its VM starts).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wsl_listen: Vec<std::net::Ipv4Addr>,
    /// The Claude Code ingress (`/claude/v1/*`): how members serve Anthropic
    /// Messages and which models go where. `None`: the ingress answers 404
    /// for every model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<crate::claude::ClaudeConfig>,
    /// The OpenAI ingress (`/openai/v1/*`, the other coding tools): which
    /// members serve Responses and Chat Completions where. `None`: every
    /// member serves its defaults ([`crate::openai::config`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai: Option<crate::openai::OpenAiConfig>,
    /// Native Gemini v1beta endpoints and model discovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gemini: Option<crate::translate::routing::GeminiConfig>,
    /// Directional protocol conversion switches (all enabled by default).
    #[serde(default)]
    pub translation: crate::translate::routing::TranslationConfig,
    /// Explicit verified L2 routes for callers without a merged catalog.
    /// Ignored for selection when `catalog` is configured.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub synthetic_models: Vec<crate::compaction_proxy::SyntheticModel>,
    /// Mixed login: Codex keeps the user's official ChatGPT login and presents
    /// its access token here ([`crate::official`]). `None`: only router tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub official_client: Option<crate::official::OfficialClientConfig>,
    /// OAuth token endpoint for stored official logins (tests point it at a
    /// fake); `None` = [`crate::official::OFFICIAL_TOKEN_URL`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub official_token_url: Option<Url>,
}

/// An address the router may listen on for WSL: a private (RFC 1918) or
/// shared (RFC 6598, `100.64/10`) IPv4 unicast address. WSL's NAT network uses
/// such a range; never loopback, link-local, public or broadcast addresses.
pub fn wsl_listen_allowed(ip: std::net::Ipv4Addr) -> bool {
    let o = ip.octets();
    let shared = o[0] == 100 && (64..128).contains(&o[1]);
    (ip.is_private() || shared) && !ip.is_broadcast() && o[3] != 0 && o[3] != 255
}

fn default_true() -> bool {
    true
}

/// Kind of a member.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberKind {
    /// A Krill relay (its lines are interchangeable entry points).
    Krill,
    /// A custom OpenAI-compatible Responses API site.
    Compatible,
    /// The user's own official ChatGPT login (`chatgpt.com/backend-api/codex`,
    /// [`crate::official`]): Codex only, never shared with other tools.
    #[serde(rename = "official_chatgpt")]
    Official,
}

impl MemberKind {
    /// Member kind for a Superba site kind; refuses the official ChatGPT login.
    pub fn for_site(kind: SiteKind) -> Result<Self> {
        match kind {
            SiteKind::Krill => Ok(MemberKind::Krill),
            SiteKind::Compatible => Ok(MemberKind::Compatible),
            SiteKind::Official => Err(Error::invalid(
                "the official ChatGPT login is never routed through the local router",
            )),
            _ => Err(Error::invalid("this site kind cannot be a router member")),
        }
    }
}

/// One provider: a Krill relay or a custom compatible site, with its own key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberConfig {
    /// Stable id (`[a-z0-9._-]`), also the key of its secret.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Kind.
    pub kind: MemberKind,
    /// Lines in priority order; interchangeable at any request boundary.
    pub lines: Vec<LineConfig>,
    /// The member serves Responses over WebSocket. When false, WebSocket
    /// handshakes routed to it are answered 426 locally (Codex then uses HTTP).
    /// Superba sets it from the site kind and the lines' probe results
    /// ([`MemberConfig::apply_websocket_hints`]).
    #[serde(default)]
    pub websocket: bool,
    /// Model names rewritten in request bodies for this member (`codex name` →
    /// `member name`).
    #[serde(default)]
    pub model_map: BTreeMap<String, String>,
    /// Codex remote compaction of this member (Superba's check, the user's
    /// choice, or the built-in fact for Krill): compaction requests and
    /// compacted conversations never go to a member marked `unsupported`, and
    /// a conversation compacted by one member stays with it
    /// ([`crate::compaction`]).
    #[serde(default, skip_serializing_if = "unknown_compaction")]
    pub compaction: CompactionSupport,
}

fn unknown_compaction(c: &CompactionSupport) -> bool {
    *c == CompactionSupport::Unknown
}

/// One entry point of a member.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineConfig {
    /// Stable id, unique within the member.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Full base URL the request remainder is appended to, e.g.
    /// `https://api.kbsfdxc.net/backend-api/codex` or `https://api.example.com/v1`.
    pub base_url: Url,
    /// Unauthenticated health probe; default `<base_url>/models` (any answer other
    /// than 502-504 or an edge interception counts as alive, 401 included).
    #[serde(default)]
    pub probe_url: Option<Url>,
    /// The line accepts zstd request bodies (Superba's transport probe):
    /// `Some(true)` = Responses request bodies Codex sent uncompressed are
    /// compressed for it; `None` (not checked) and `Some(false)` = sent as they
    /// are. See [`crate::transport`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zstd: Option<bool>,
    /// The line serves the Responses WebSocket for this member's key (probe):
    /// `Some(false)` = never offered for a WebSocket (the handshake got 426);
    /// `None` = not checked (offered when the member serves WebSocket).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub websocket: Option<bool>,
}

impl MemberConfig {
    /// Derive [`MemberConfig::websocket`] from the lines' probe results: a
    /// Krill member serves WebSocket unless every line refused the handshake
    /// (WebSocket is a per-account gate, the same on every line); a compatible
    /// site only when some line upgraded one.
    pub fn apply_websocket_hints(&mut self) {
        self.websocket = match self.kind {
            MemberKind::Krill => self.lines.iter().any(|l| l.websocket != Some(false)),
            MemberKind::Compatible => self.lines.iter().any(|l| l.websocket == Some(true)),
            MemberKind::Official => true,
        };
    }
}

impl LineConfig {
    /// The probe URL in effect.
    pub fn effective_probe_url(&self) -> Url {
        self.probe_url
            .clone()
            .unwrap_or_else(|| join_path(&self.base_url, "/models", None))
    }
}

/// Statuses that fail over by default: the gateway errors.
pub const DEFAULT_FAILOVER_STATUSES: &[u16] = &[502, 503, 504];

/// Statuses the failover setting may name (the others always pass through).
pub const CONFIGURABLE_FAILOVER_STATUSES: &[u16] = &[401, 403, 429, 500, 502, 503, 504];

impl Tuning {
    /// The failover and recovery settings in effect.
    pub fn failover(&self) -> superba_core::RouterFailover {
        superba_core::RouterFailover {
            enabled: self.failover_enabled,
            fixed_route: self.fixed_route.clone(),
            cooldown_secs: u32::try_from(self.breaker_base_ms / 1000).unwrap_or(u32::MAX),
            max_cooldown_secs: u32::try_from(self.breaker_max_ms / 1000).unwrap_or(u32::MAX),
            failover_statuses: self.failover_statuses.clone(),
            sticky_idle_minutes: u32::try_from(self.sticky_ttl_secs / 60).unwrap_or(u32::MAX),
            return_to_preferred: self.return_to_preferred,
        }
    }

    /// Apply failover and recovery settings (validated first).
    pub fn set_failover(&mut self, f: &superba_core::RouterFailover) -> Result<()> {
        f.validate()?;
        self.failover_enabled = f.enabled;
        self.fixed_route = f.fixed_route.clone();
        self.breaker_base_ms = u64::from(f.cooldown_secs) * 1000;
        self.breaker_max_ms = u64::from(f.max_cooldown_secs) * 1000;
        let mut statuses = f.failover_statuses.clone();
        statuses.sort_unstable();
        statuses.dedup();
        self.failover_statuses = statuses;
        self.sticky_ttl_secs = u64::from(f.sticky_idle_minutes) * 60;
        self.return_to_preferred = f.return_to_preferred;
        Ok(())
    }
}

/// Timeouts, stickiness and breaker parameters. Defaults follow the router spike.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tuning {
    /// Use automatic selection, retries and background health probes.
    /// Real requests record passive health even when this is off.
    pub failover_enabled: bool,
    /// Stable exit used when failover is disabled.
    pub fixed_route: Option<superba_core::RouterFixedRoute>,
    /// TCP + TLS connect timeout per attempt. There is deliberately no
    /// time-to-first-byte timeout: Codex has none and relays may queue.
    pub connect_timeout_ms: u64,
    /// Idle time after which a conversation's pin is forgotten.
    pub sticky_ttl_secs: u64,
    /// Maximum number of pinned conversations (least recently used are dropped).
    pub sticky_capacity: usize,
    /// Largest request body accepted (bodies are buffered for failover).
    pub max_body_bytes: usize,
    /// Consecutive 502/503/504 answers (or edge interceptions) that open a line's
    /// breaker. Connect failures and broken streams open it at once.
    pub breaker_threshold: u32,
    /// First open period of a breaker; doubles on each re-open.
    pub breaker_base_ms: u64,
    /// Longest open period.
    pub breaker_max_ms: u64,
    /// HTTP statuses that fail over to the next line (and count as a soft
    /// failure of the line). Every other status is passed to the client. An
    /// edge interception always fails over.
    pub failover_statuses: Vec<u16>,
    /// A conversation pinned to a fallback member returns to an earlier
    /// member once one of its lines is available again (off: it stays on the
    /// fallback until it has been idle for [`Tuning::sticky_ttl_secs`]).
    pub return_to_preferred: bool,
    /// Timeout of one health probe.
    pub probe_timeout_ms: u64,
    /// Probe every line at start and open lines when their backoff ends. When
    /// off, breakers recover only through real requests.
    pub active_probes: bool,
    /// Proxied requests and WebSocket sessions served at once (each holds two
    /// sockets). `None`: a quarter of the soft open-file limit found at start,
    /// 4 to 512 (512 without a limit). Read at start. See [`crate::limits`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<usize>,
    /// Inbound connections open at once (more wait in the listen backlog).
    /// `None`: half the soft open-file limit, at most four per admission. Read
    /// at start.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_connections: Option<usize>,
    /// How long a request waits for an admission before the router answers 503
    /// `local_router_busy`.
    pub busy_wait_ms: u64,
    /// Idle upstream connections kept per host. `None`: one per 16 admissions,
    /// 2 to 16.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_max_idle_per_host: Option<usize>,
    /// Idle upstream connections are closed after this long.
    pub pool_idle_timeout_secs: u64,
    /// An idle keep-alive connection from Codex is closed after this long (also
    /// the time a client gets to send a request's headers). Longer than HTTP
    /// clients keep idle connections (reqwest / hyper: 90 s), so Codex never
    /// reuses a connection the router has just closed.
    pub inbound_idle_timeout_secs: u64,
    /// While the inbound connection budget is nearly full, a connection idle
    /// this long is closed (see [`crate::limits`]).
    pub pressure_idle_timeout_ms: u64,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            failover_enabled: true,
            fixed_route: None,
            connect_timeout_ms: 4_000,
            sticky_ttl_secs: 2 * 60 * 60,
            sticky_capacity: 4_096,
            max_body_bytes: 256 * 1024 * 1024,
            breaker_threshold: 2,
            breaker_base_ms: 5_000,
            breaker_max_ms: 60_000,
            failover_statuses: DEFAULT_FAILOVER_STATUSES.to_vec(),
            return_to_preferred: false,
            probe_timeout_ms: 5_000,
            active_probes: true,
            max_concurrent: None,
            max_connections: None,
            busy_wait_ms: 2_000,
            pool_max_idle_per_host: None,
            pool_idle_timeout_secs: 90,
            inbound_idle_timeout_secs: 150,
            pressure_idle_timeout_ms: 5_000,
        }
    }
}

impl RouterConfig {
    /// A configuration with defaults for `members` on `listen_port`.
    pub fn new(listen_port: u16, members: Vec<MemberConfig>) -> Self {
        Self {
            catalog: None,
            format: CONFIG_FORMAT,
            listen_port,
            members,
            tuning: Tuning::default(),
            use_system_proxy: true,
            extra_ca_file: None,
            stats: None,
            wsl_listen: Vec::new(),
            claude: None,
            tool_only_members: Vec::new(),
            openai: None,
            gemini: None,
            translation: Default::default(),
            synthetic_models: Vec::new(),
            official_client: None,
            official_token_url: None,
        }
    }

    /// The member serves Codex requests (not reserved for Claude Code).
    pub fn serves_codex(&self, id: &str) -> bool {
        !self.tool_only_members.iter().any(|m| m == id)
            && !self.claude.as_ref().is_some_and(|c| c.claude_only(id))
    }

    /// The OAuth token endpoint of stored official logins.
    pub fn official_token_url(&self) -> Url {
        self.official_token_url.clone().unwrap_or_else(|| {
            Url::parse(crate::official::OFFICIAL_TOKEN_URL).expect("valid constant URL")
        })
    }

    /// Look up a member.
    pub fn member(&self, id: &str) -> Option<&MemberConfig> {
        self.members.iter().find(|m| m.id == id)
    }

    /// Fixed exit, falling back to the first configured Codex line before one
    /// has been explicitly selected. This never reads runtime health.
    pub fn fixed_exit(&self) -> Option<(usize, usize)> {
        if let Some(route) = &self.tuning.fixed_route {
            let member = self.members.iter().position(|m| m.id == route.member_id)?;
            let line = self.members[member]
                .lines
                .iter()
                .position(|l| l.id == route.line_id)?;
            return Some((member, line));
        }
        self.members
            .iter()
            .enumerate()
            .find(|(_, m)| self.serves_codex(&m.id) && !m.lines.is_empty())
            .or_else(|| {
                self.members
                    .iter()
                    .enumerate()
                    .find(|(_, m)| !m.lines.is_empty())
            })
            .map(|(member, _)| (member, 0))
    }

    /// Retain the exit by stable ids when members are edited or reordered.
    pub fn reconcile_fixed_exit(&mut self) {
        if self.tuning.fixed_route.is_some() && self.fixed_exit().is_none() {
            self.tuning.fixed_route = None;
        }
        if !self.tuning.failover_enabled
            && self.tuning.fixed_route.is_none()
            && let Some((member, line)) = self.fixed_exit()
        {
            self.tuning.fixed_route = Some(superba_core::RouterFixedRoute {
                member_id: self.members[member].id.clone(),
                line_id: self.members[member].lines[line].id.clone(),
            });
        }
    }

    /// Check every rule; the router refuses to start (or reload) otherwise.
    pub fn validate(&self) -> Result<()> {
        if let Some(catalog) = &self.catalog {
            catalog.validate(self)?;
        }
        if self.format != CONFIG_FORMAT {
            return Err(Error::invalid(format!(
                "router config format {} is not supported (expected {CONFIG_FORMAT})",
                self.format
            )));
        }
        if self.members.is_empty() {
            return Err(Error::invalid("the router needs at least one member"));
        }
        if self.tuning.fixed_route.is_some() && self.fixed_exit().is_none() {
            return Err(Error::invalid(
                "the fixed router exit must name an existing member and line",
            ));
        }
        for id in &self.tool_only_members {
            if self.member(id).is_none() {
                return Err(Error::invalid(format!("tool-only provider {id} is absent")));
            }
        }
        for ip in &self.wsl_listen {
            if !wsl_listen_allowed(*ip) {
                return Err(Error::invalid(format!(
                    "{ip} is not a WSL adapter address (only private IPv4 addresses are accepted)"
                )));
            }
        }
        let mut synthetic_ids = BTreeSet::new();
        for model in &self.synthetic_models {
            if !synthetic_ids.insert((&model.slug, &model.member)) {
                return Err(Error::invalid(
                    "duplicate synthetic model record for a provider",
                ));
            }
            if model.slug.is_empty()
                || model.model.is_empty()
                || self.member(&model.member).is_none()
                || self
                    .openai_member(&model.member)
                    .is_none_or(|m| m.responses.is_none())
            {
                return Err(Error::invalid(
                    "synthetic models require an existing Responses member and nonempty model ids",
                ));
            }
        }
        let mut ids = BTreeSet::new();
        for m in &self.members {
            check_id("member", &m.id)?;
            if !ids.insert(m.id.as_str()) {
                return Err(Error::invalid(format!("duplicate member id {:?}", m.id)));
            }
            if m.lines.is_empty() {
                return Err(Error::invalid(format!("member {:?} has no line", m.id)));
            }
            let mut line_ids = BTreeSet::new();
            for l in &m.lines {
                check_id("line", &l.id)?;
                if !line_ids.insert(l.id.as_str()) {
                    return Err(Error::invalid(format!(
                        "duplicate line id {:?} in member {:?}",
                        l.id, m.id
                    )));
                }
                if m.kind == MemberKind::Official {
                    check_official_url(&l.base_url)?;
                } else {
                    check_upstream_url(&l.base_url)?;
                }
                if let Some(p) = &l.probe_url {
                    check_upstream_url(p)?;
                }
            }
            for (from, to) in &m.model_map {
                if from.trim().is_empty() || to.trim().is_empty() {
                    return Err(Error::invalid(format!(
                        "member {:?}: empty model name in the model mapping",
                        m.id
                    )));
                }
            }
        }
        let t = &self.tuning;
        if t.connect_timeout_ms == 0
            || t.breaker_base_ms == 0
            || t.probe_timeout_ms == 0
            || t.pool_idle_timeout_secs == 0
            || t.inbound_idle_timeout_secs == 0
            || t.pressure_idle_timeout_ms == 0
        {
            return Err(Error::invalid("router timeouts must be positive"));
        }
        if t.breaker_max_ms < t.breaker_base_ms || t.breaker_threshold == 0 {
            return Err(Error::invalid("invalid router breaker settings"));
        }
        if t.sticky_capacity == 0 || t.max_body_bytes == 0 {
            return Err(Error::invalid("router capacities must be positive"));
        }
        if let Some(stats) = &self.stats {
            stats.validate()?;
        }
        if let Some(claude) = &self.claude {
            claude.validate(self)?;
        }
        if let Some(openai) = &self.openai {
            openai.validate(self)?;
        }
        if let Some(gemini) = &self.gemini {
            gemini.validate(self)?;
        }
        Ok(())
    }

    /// Read `config.json`.
    pub fn load(paths: &RouterPaths) -> Result<Self> {
        let path = paths.config_json();
        let bytes = fs::read(&path).map_err(|e| Error::io(&path, &e))?;
        let cfg: RouterConfig =
            serde_json::from_slice(&bytes).map_err(|e| Error::parse("router config.json", e))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Validate and write `config.json` atomically (0600).
    pub fn save(&self, paths: &RouterPaths) -> Result<()> {
        self.validate()?;
        let mut bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| Error::other(format!("serialize router config: {e}")))?;
        bytes.push(b'\n');
        write_private(&paths.config_json(), &bytes)
    }
}

/// The router's secrets: the token Codex presents and each member's own key.
#[derive(Clone, Debug)]
pub struct RouterSecrets {
    /// Token Codex sends as its API key (`auth.json` `OPENAI_API_KEY`).
    pub router_token: Secret,
    /// Member id → that member's API key.
    pub member_keys: BTreeMap<String, Secret>,
    /// Official member id → its stored ChatGPT login ([`crate::official`]).
    pub official_tokens: BTreeMap<String, crate::official::OfficialTokens>,
}

#[derive(Serialize, Deserialize)]
struct SecretsWire {
    format: u32,
    router_token: String,
    member_keys: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    official_tokens: BTreeMap<String, crate::official::OfficialTokens>,
}

impl Drop for SecretsWire {
    fn drop(&mut self) {
        self.router_token.zeroize();
        for v in self.member_keys.values_mut() {
            v.zeroize();
        }
        for t in self.official_tokens.values_mut() {
            t.access_token.zeroize();
            t.refresh_token.zeroize();
        }
    }
}

impl RouterSecrets {
    /// Fresh secrets with a new random router token.
    pub fn generate(member_keys: BTreeMap<String, Secret>) -> Self {
        Self {
            router_token: Secret::new(token::generate(token::ROUTER_TOKEN_PREFIX)),
            member_keys,
            official_tokens: BTreeMap::new(),
        }
    }

    /// The key of `member`.
    pub fn key(&self, member: &str) -> Option<&Secret> {
        self.member_keys.get(member)
    }

    /// Check that the token is set and every member of `config` has a key.
    pub fn validate_for(&self, config: &RouterConfig) -> Result<()> {
        if self.router_token.is_empty() {
            return Err(Error::Secret {
                message: "the router token is empty".into(),
            });
        }
        // An official member needs no key: it uses the mixed-login client's
        // token or its stored login.
        for m in config
            .members
            .iter()
            .filter(|m| m.kind != MemberKind::Official)
        {
            match self.member_keys.get(&m.id) {
                Some(k) if !k.is_empty() => {}
                _ => {
                    return Err(Error::Secret {
                        message: format!("no API key for router member {:?}", m.id),
                    });
                }
            }
        }
        Ok(())
    }

    /// Read `secrets.json`.
    pub fn load(paths: &RouterPaths) -> Result<Self> {
        let path = paths.secrets_json();
        let bytes = fs::read(&path).map_err(|e| Error::io(&path, &e))?;
        let mut wire: SecretsWire = serde_json::from_slice(&bytes).map_err(|_| Error::Secret {
            // The parser message could quote file content: keep it out.
            message: "router secrets.json is not valid".into(),
        })?;
        if wire.format != CONFIG_FORMAT {
            return Err(Error::Secret {
                message: format!("router secrets format {} is not supported", wire.format),
            });
        }
        Ok(Self {
            router_token: Secret::new(wire.router_token.as_str()),
            member_keys: wire
                .member_keys
                .iter()
                .map(|(k, v)| (k.clone(), Secret::new(v.as_str())))
                .collect(),
            official_tokens: std::mem::take(&mut wire.official_tokens),
        })
    }

    /// Write `secrets.json` atomically with mode 0600.
    pub fn save(&self, paths: &RouterPaths) -> Result<()> {
        let wire = SecretsWire {
            format: CONFIG_FORMAT,
            router_token: self.router_token.expose().to_owned(),
            member_keys: self
                .member_keys
                .iter()
                .map(|(k, v)| (k.clone(), v.expose().to_owned()))
                .collect(),
            official_tokens: self.official_tokens.clone(),
        };
        let mut bytes = serde_json::to_vec_pretty(&wire)
            .map_err(|e| Error::other(format!("serialize router secrets: {e}")))?;
        bytes.push(b'\n');
        let result = write_private(&paths.secrets_json(), &bytes);
        bytes.zeroize();
        result
    }
}

fn check_id(what: &str, id: &str) -> Result<()> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b));
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "{what} id {id:?} must be 1-64 characters of a-z, 0-9, '.', '_' or '-'"
        )))
    }
}

/// Upstream rule of an official member: `https://chatgpt.com/backend-api/codex`,
/// or a loopback URL (tests and local forwarders).
pub fn check_official_url(url: &Url) -> Result<()> {
    let loopback = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        None => false,
    };
    let official = url.scheme() == "https"
        && url.host_str() == Some("chatgpt.com")
        && url.port().is_none()
        && url.path().trim_end_matches('/') == "/backend-api/codex";
    if (official || (loopback && matches!(url.scheme(), "http" | "https")))
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
    {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "{url}: an official ChatGPT member must use {}",
            crate::official::OFFICIAL_CODEX_BASE
        )))
    }
}

/// Upstream rule: https, or plain http to a loopback host (tests, local
/// forwarders); no credentials, query or fragment; never a ChatGPT host.
pub fn check_upstream_url(url: &Url) -> Result<()> {
    let loopback = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        None => return Err(Error::invalid(format!("{url}: no host"))),
    };
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        other => {
            return Err(Error::invalid(format!(
                "{url}: scheme {other} not allowed (https required)"
            )));
        }
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::invalid(format!(
            "{}: must not contain credentials",
            url.host_str().unwrap_or_default()
        )));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(Error::invalid(format!(
            "{url}: must not contain a query or fragment"
        )));
    }
    let host = url
        .host_str()
        .unwrap_or_default()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if CHATGPT_HOSTS
        .iter()
        .any(|h| host == *h || host.ends_with(&format!(".{h}")))
    {
        return Err(Error::invalid(
            "the official ChatGPT login is never routed through the local router",
        ));
    }
    Ok(())
}

/// `base` with `rest` (an absolute path) appended to its path, plus `query`.
pub(crate) fn join_path(base: &Url, rest: &str, query: Option<&str>) -> Url {
    let mut url = base.clone();
    let mut path = base.path().trim_end_matches('/').to_owned();
    if !rest.starts_with('/') {
        path.push('/');
    }
    path.push_str(rest);
    url.set_path(&path);
    url.set_query(query);
    url
}

/// Write `bytes` to `path` atomically (temp file + rename) with mode 0600 on Unix.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| Error::invalid(format!("{} has no parent", path.display())))?;
    create_private_dir(dir)?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("router");
    let tmp = dir.join(format!(
        ".{file_name}.{}.tmp",
        token::generate("").get(..12).unwrap_or("tmp")
    ));
    let write = || -> std::io::Result<()> {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    };
    write().map_err(|e| {
        let _ = fs::remove_file(&tmp);
        Error::io(path, &e)
    })
}

/// Create `dir` (and parents); the leaf gets mode 0700 on Unix.
pub(crate) fn create_private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).map_err(|e| Error::io(dir, &e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}
