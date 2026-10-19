//! Router mode as seen by the switch service, the GUI and `spb` (v2 decision 6):
//! the persisted choice ([`RouterSettings`]) and a status snapshot
//! ([`RouterView`]). The router itself lives in `superba-router`; this module only
//! holds the data the other crates exchange.
//!
//! Router mode is opt-in. While it is on, the local Codex home points at the
//! loopback router in API-key mode (`openai_base_url =
//! http://127.0.0.1:<port>/backend-api/codex`, a local router token in
//! `auth.json`), and the router forwards each request to its members, every member
//! with its own key. Remote hosts and the official ChatGPT login are never routed.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::i18n::{Catalog, Locale, LocalizedText};
use crate::ids::ProfileId;
use crate::requests::{Multiplier, PriceEntry, RouterStatsStatus};

/// Persisted router choice (part of [`crate::Settings`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RouterSettings {
    /// Codex is pointed at the router while a key-based profile is applied.
    pub enabled: bool,
    /// Member profiles in priority order (each with its own site, lines and key).
    /// Switching to a profile while the router is on moves it to the front.
    pub members: Vec<ProfileId>,
    /// Request log, stats line and prices (applied with a router reload, never a
    /// Codex restart).
    #[serde(skip_serializing_if = "RouterStatsSettings::is_default")]
    pub stats: RouterStatsSettings,
    /// Namespaces and visibility of the merged Codex catalog.
    pub catalog: crate::RouterCatalogSettings,
}

/// Request recording, the optional stats line and the prices of cost estimates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RouterStatsSettings {
    /// Record request numbers (tokens, timings, status; never headers or
    /// bodies). On by default while the router is on.
    pub record: bool,
    /// Keep request rows this many days.
    pub usage_days: u32,
    /// Keep at most this many request rows.
    pub max_rows: u32,
    /// The stats line under a reply.
    pub line: StatsLineSettings,
    /// Price ratios and overrides.
    pub prices: PriceSettings,
}

impl RouterStatsSettings {
    /// Default row retention in days.
    pub const DEFAULT_USAGE_DAYS: u32 = 90;
    /// Default row cap.
    pub const DEFAULT_MAX_ROWS: u32 = 200_000;

    /// True for the defaults (serde skip helper).
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

impl Default for RouterStatsSettings {
    fn default() -> Self {
        Self {
            record: true,
            usage_days: Self::DEFAULT_USAGE_DAYS,
            max_rows: Self::DEFAULT_MAX_ROWS,
            line: StatsLineSettings::default(),
            prices: PriceSettings::default(),
        }
    }
}

/// Which clients get the stats line (`codex exec` never does).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StatsLineClient {
    /// The desktop app (`Codex Desktop`).
    Desktop,
    /// The interactive terminal Codex (`codex_cli_rs`).
    Tui,
}

impl StatsLineClient {
    /// The `originator` header value of this client.
    pub fn originator(self) -> &'static str {
        match self {
            StatsLineClient::Desktop => crate::codex::ORIGINATOR_DESKTOP,
            StatsLineClient::Tui => crate::codex::ORIGINATOR_TUI,
        }
    }
}

/// Metrics of the default stats line template.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StatsMetric {
    /// Output tokens per second of the answering request (`{tps}`).
    Tps,
    /// First token of the answering request (`{ttft}`).
    FirstToken,
    /// Turn input / output totals (`{input}` / `{output}`).
    TurnTokens,
    /// Turn cached tokens (`{cached}`).
    Cached,
    /// Turn cost estimate (`{cost}`).
    Cost,
    /// Requests in the turn (`{requests}`).
    Requests,
}

/// Quote style of the stats line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StatsLineStyle {
    /// `> ⚡ ...`
    #[default]
    Quote,
    /// `> *⚡ ...*`
    QuoteItalic,
}

/// The optional stats line appended to the final answer of a turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatsLineSettings {
    /// Off by default.
    pub enabled: bool,
    /// Codex clients that get it (default: the desktop app only).
    pub clients: Vec<StatsLineClient>,
    /// Harnesses that get it while it is on (default: Codex and Claude Code).
    /// Codex follows [`StatsLineSettings::clients`] as well; Claude Code gets
    /// it on the final answer of its main conversation.
    pub harnesses: Vec<crate::requests::RequestHarness>,
    /// Metrics of the default template.
    pub metrics: Vec<StatsMetric>,
    /// Custom template (`{tps}`, `{ttft}`, `{input}`, `{output}`, `{cached}`,
    /// `{cost}`, `{requests}`), kept exactly as the user wrote it. `None` is the
    /// default marker: nothing is materialised, the default is built from
    /// `metrics` in the UI language each time the settings reach the router, so
    /// it follows a later language change (see [`Self::resolved_template`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// Quote style.
    pub style: StatsLineStyle,
}

impl StatsLineSettings {
    /// Placeholders a stats line template may use.
    pub const PLACEHOLDERS: [&'static str; 7] = [
        "tps", "ttft", "input", "output", "cached", "cost", "requests",
    ];

    /// What a user types to go back to the default template (`spb router
    /// stats-line --template default`, the GUI's empty field).
    pub const DEFAULT_TEMPLATE_KEYWORD: &'static str = "default";

    /// The custom template, `None` while the default is in use (no template, an
    /// empty one or [`Self::DEFAULT_TEMPLATE_KEYWORD`]).
    pub fn custom_template(&self) -> Option<&str> {
        self.template.as_deref().filter(|t| !is_default_input(t))
    }

    /// Set the template from user input: empty text or
    /// [`Self::DEFAULT_TEMPLATE_KEYWORD`] stores the default marker (`None`),
    /// anything else is kept verbatim.
    pub fn set_template(&mut self, text: &str) {
        self.template = (!is_default_input(text)).then(|| text.to_owned());
    }

    /// The template the router gets for `locale` (the UI language): the custom
    /// one verbatim, else [`Self::default_template`] of the chosen metrics in
    /// that language.
    pub fn resolved_template(&self, locale: Locale) -> String {
        self.template_text(&Catalog::new(locale))
    }

    /// The template in effect: the custom one, else [`Self::default_template`]
    /// of the chosen metrics in `catalog`'s language. The router fills the
    /// placeholders and never translates, so Superba passes this text.
    pub fn template_text(&self, catalog: &Catalog) -> String {
        match self.custom_template() {
            Some(t) => t.to_owned(),
            None => Self::default_template(&self.metrics, catalog),
        }
    }

    /// `⚡ ` followed by one fragment per metric (in the fixed order tps, first
    /// token, turn tokens, cached, cost, requests) joined with ` · `, e.g.
    /// `⚡ {tps} tok/s · 首字 {ttft} · 本轮 {input} in / {output} out`.
    pub fn default_template(metrics: &[StatsMetric], catalog: &Catalog) -> String {
        const ORDER: [(StatsMetric, &str, &str); 6] = [
            (StatsMetric::Tps, "stats_template.tps", "{tps} tok/s"),
            (
                StatsMetric::FirstToken,
                "stats_template.ttft",
                "first token {ttft}",
            ),
            (
                StatsMetric::TurnTokens,
                "stats_template.turn_tokens",
                "turn {input} in / {output} out",
            ),
            (
                StatsMetric::Cached,
                "stats_template.cached",
                "cached {cached}",
            ),
            (StatsMetric::Cost, "stats_template.cost", "≈ {cost}"),
            (
                StatsMetric::Requests,
                "stats_template.requests",
                "{requests} req",
            ),
        ];
        let parts: Vec<String> = ORDER
            .iter()
            .filter(|(m, _, _)| metrics.contains(m))
            .map(|(_, key, fallback)| catalog.tr(key, fallback))
            .collect();
        let parts = if parts.is_empty() {
            vec![catalog.tr("stats_template.tps", "{tps} tok/s")]
        } else {
            parts
        };
        let sep = catalog.tr("stats_template.separator", " · ");
        format!("⚡ {}", parts.join(&sep))
    }
}

/// Template input that means "the default": empty (also just the leading `⚡`
/// the editor shows as a fixed prefix) or [`StatsLineSettings::DEFAULT_TEMPLATE_KEYWORD`].
fn is_default_input(text: &str) -> bool {
    let t = text.trim();
    let t = t.strip_prefix('⚡').unwrap_or(t);
    let t = t.strip_prefix('\u{2060}').unwrap_or(t).trim();
    t.is_empty() || t.eq_ignore_ascii_case(StatsLineSettings::DEFAULT_TEMPLATE_KEYWORD)
}

impl Default for StatsLineSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            clients: vec![StatsLineClient::Desktop],
            harnesses: vec![
                crate::requests::RequestHarness::Codex,
                crate::requests::RequestHarness::ClaudeCode,
            ],
            metrics: vec![
                StatsMetric::Tps,
                StatsMetric::FirstToken,
                StatsMetric::TurnTokens,
            ],
            template: None,
            style: StatsLineStyle::Quote,
        }
    }
}

/// Price adjustments of the cost estimates (the price table itself comes from the
/// feeds).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PriceSettings {
    /// Ratio of the list price a provider charges (relays often sell below list
    /// price), by profile; missing = 1.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub ratio_by_profile: BTreeMap<ProfileId, Multiplier>,
    /// Absolute per-model prices by profile, then model slug (USD per 1M tokens).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub overrides: BTreeMap<ProfileId, BTreeMap<String, PriceEntry>>,
}

/// What [`crate::SwitchService::router_install`] configures.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouterInstallRequest {
    /// Member profiles in priority order; empty = keep the current members.
    #[serde(default)]
    pub members: Vec<ProfileId>,
}

/// Why a provider cannot serve Claude Code (directly or through the local
/// router). Until protocol conversion exists, Claude Code reaches only
/// upstreams that speak Anthropic Messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ClaudeIneligible {
    /// The provider has no Anthropic Messages endpoint.
    NoAnthropicEndpoint,
    /// No native endpoint or enabled conversion can serve this member.
    NoProtocolRoute,
}

impl ClaudeIneligible {
    /// Display text.
    pub fn text(self) -> LocalizedText {
        match self {
            ClaudeIneligible::NoAnthropicEndpoint => LocalizedText::new(
                "claude_routing.no_anthropic_endpoint",
                "This provider has no Anthropic endpoint for a direct connection",
            ),
            ClaudeIneligible::NoProtocolRoute => LocalizedText::new(
                "claude_routing.no_protocol_route",
                "This provider has no Anthropic endpoint or enabled protocol conversion",
            ),
        }
    }
}

/// Why a provider cannot serve a request on the router's OpenAI ingress
/// (`/openai/v1`, the other coding tools), including enabled conversions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum OpenAiIneligible {
    /// The provider serves no OpenAI Responses endpoint through the router
    /// (it is reserved for Claude Code, or its Responses endpoint is off).
    NoResponsesEndpoint,
    /// The provider has no Chat Completions endpoint.
    NoChatEndpoint,
}

impl OpenAiIneligible {
    /// Display text.
    pub fn text(self) -> LocalizedText {
        match self {
            OpenAiIneligible::NoResponsesEndpoint => LocalizedText::new(
                "openai_routing.no_responses_endpoint",
                "This provider has no OpenAI Responses endpoint or enabled protocol conversion",
            ),
            OpenAiIneligible::NoChatEndpoint => LocalizedText::new(
                "openai_routing.no_chat_endpoint",
                "This provider has no Chat Completions endpoint or enabled protocol conversion",
            ),
        }
    }
}

/// Health of one router line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RouterLineHealth {
    /// Serving normally.
    Healthy,
    /// Recently failed; skipped until its backoff ends.
    Open,
    /// Backoff over; the next request decides.
    HalfOpen,
}

impl RouterLineHealth {
    /// Display text.
    pub fn text(self) -> LocalizedText {
        match self {
            RouterLineHealth::Healthy => LocalizedText::new("router.line.healthy", "Healthy"),
            RouterLineHealth::Open => LocalizedText::new("router.line.open", "Skipped (failing)"),
            RouterLineHealth::HalfOpen => LocalizedText::new("router.line.half_open", "Retrying"),
        }
    }
}

/// One line of a router member.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouterLineView {
    /// Line id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Health.
    pub health: RouterLineHealth,
    /// Last time to first byte.
    #[serde(default)]
    pub ttfb_ms: Option<u64>,
    /// Requests sent through it.
    #[serde(default)]
    pub requests: u64,
    /// Failovers away from it.
    #[serde(default)]
    pub failovers_away: u64,
    /// Last error (redacted).
    #[serde(default)]
    pub last_error: Option<String>,
}

/// One router member.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouterMemberView {
    /// Member profile.
    pub profile_id: ProfileId,
    /// Display name.
    pub name: String,
    /// Lines in priority order (empty when the router is not running).
    #[serde(default)]
    pub lines: Vec<RouterLineView>,
    /// Native, converted or unavailable routes on the OpenAI/Gemini ingresses.
    #[serde(default)]
    pub protocols: Vec<RouterProtocolRoute>,
    /// Most recently selected upstream for this member. Concurrent conversations
    /// can use different lines; this is an observation, not its preferred line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_route: Option<RouterLineRoute>,
}

/// How a member serves one client protocol through the local router.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouterProtocolRoute {
    /// The protocol the client sends.
    pub protocol: crate::RequestProtocol,
    /// Selected upstream protocol; absent when no enabled route exists.
    pub upstream: Option<crate::RequestProtocol>,
}

impl RouterProtocolRoute {
    /// Whether this route uses protocol conversion.
    pub fn translated(&self) -> bool {
        self.upstream.is_some_and(|p| p != self.protocol)
    }

    /// Client-facing ingress label.
    pub fn ingress(&self) -> &'static str {
        match self.protocol {
            crate::RequestProtocol::Responses => "/openai/v1 · Responses",
            crate::RequestProtocol::ChatCompletions => "/openai/v1 · Chat Completions",
            crate::RequestProtocol::Gemini => "/gemini/v1beta",
            crate::RequestProtocol::Messages => "/claude",
        }
    }

    /// Connection method or reason this member cannot serve the protocol.
    pub fn connection_text(&self) -> LocalizedText {
        if self.translated() {
            LocalizedText::new("router.protocol.translated", "Via protocol conversion")
        } else if self.upstream.is_some() {
            LocalizedText::new("router.protocol.native", "Native protocol")
        } else {
            match self.protocol {
                crate::RequestProtocol::Responses => OpenAiIneligible::NoResponsesEndpoint.text(),
                crate::RequestProtocol::ChatCompletions => OpenAiIneligible::NoChatEndpoint.text(),
                crate::RequestProtocol::Messages => ClaudeIneligible::NoProtocolRoute.text(),
                crate::RequestProtocol::Gemini => LocalizedText::new(
                    "router.protocol.no_gemini",
                    "This provider has no Gemini endpoint or enabled protocol conversion",
                ),
            }
        }
    }
}

/// The latest request whose upstream response headers arrived, since reload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouterLineRoute {
    /// Line actually used.
    pub line_id: String,
    /// Earlier lines with failures that explain using a backup. Kept while
    /// that conversation stays on the backup, even after a probe recovers them.
    pub failed_lines: Vec<String>,
}

/// Router status snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouterView {
    /// The persisted choice.
    pub settings: RouterSettings,
    /// A router configuration exists.
    pub configured: bool,
    /// The service is installed with the service manager.
    pub installed: bool,
    /// The router answers its admin API.
    pub running: bool,
    /// Loopback port.
    #[serde(default)]
    pub port: Option<u16>,
    /// The local Codex home currently points at the router.
    pub codex_routed: bool,
    /// Members with their lines.
    #[serde(default)]
    pub members: Vec<RouterMemberView>,
    /// Requests forwarded since the router started.
    #[serde(default)]
    pub requests: u64,
    /// Failovers since the router started.
    #[serde(default)]
    pub failovers: u64,
    /// Why the router is not running / not reachable (no secrets).
    #[serde(default)]
    pub detail: Option<String>,
    /// Request recording state (`None` when the router does not report it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<RouterStatsStatus>,
    /// The router's local budget and shortages (`None` when it is not running
    /// or does not report them).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<RouterLocalView>,
    /// Claude Code on the router (its Anthropic ingress).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<RouterClaudeView>,
    /// What an action on the router also did (Claude Code moved back to its
    /// provider, ...).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<crate::i18n::Notice>,
}

/// Claude Code on the local router: whether it goes through it and which
/// providers serve it there.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RouterClaudeView {
    /// Claude Code's route is the local router.
    pub route_router: bool,
    /// Claude Code's `settings.json` points at the router now.
    pub routed: bool,
    /// The router's `ANTHROPIC_BASE_URL` (`http://127.0.0.1:<port>/claude`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Providers for Claude Code in its order (its own provider first).
    pub members: Vec<RouterClaudeMember>,
}

/// One provider for Claude Code on the router.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouterClaudeMember {
    /// The provider.
    pub profile_id: ProfileId,
    /// Display name.
    pub name: String,
    /// Its Anthropic endpoint (base URL), when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Why it cannot serve Claude Code (`None`: it serves it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ineligible: Option<ClaudeIneligible>,
    /// An enabled conversion serves this provider's Claude Code requests.
    #[serde(default)]
    pub translated: bool,
    /// It is Claude Code's own provider.
    #[serde(default)]
    pub preferred: bool,
    /// It serves only Claude Code (not a Codex member of the router).
    #[serde(default)]
    pub claude_only: bool,
}

impl RouterClaudeMember {
    /// The connection method or reason it cannot serve Claude Code.
    pub fn connection_text(&self) -> LocalizedText {
        match self.ineligible {
            Some(reason) => reason.text(),
            None if self.translated => {
                LocalizedText::new("claude_routing.translated", "Via protocol conversion")
            }
            None => LocalizedText::new("", "{endpoint}")
                .arg("endpoint", self.endpoint.as_deref().unwrap_or_default()),
        }
    }
}

impl RouterView {
    /// The router is on and Codex goes through it.
    pub fn active(&self) -> bool {
        self.settings.enabled && self.running && self.codex_routed
    }
}

/// The router's side on this computer: how many requests it serves at once
/// (sized from its open-file limit) and how often it ran short. A shortage is
/// answered 503 `local_router_busy` and never counted against a line.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RouterLocalView {
    /// The router's soft open-file limit (`None`: unlimited, as on Windows).
    pub open_files_limit: Option<u64>,
    /// Requests and WebSocket sessions it serves at once.
    pub max_concurrent: u64,
    /// Requests and WebSocket sessions in progress.
    pub in_flight: u64,
    /// Requests answered `local_router_busy` because all places were taken.
    pub busy: u64,
    /// "Too many open files" errors it met (accepting, connecting upstream,
    /// the request log).
    pub file_limit_errors: u64,
    /// Unix milliseconds of the last shortage of either kind.
    pub last_problem_ms: Option<u64>,
    /// The last "too many open files": what the router was doing and the OS
    /// error (English, no secret).
    pub last_file_limit: Option<String>,
}

impl RouterLocalView {
    /// The router ran short at least once since it started.
    pub fn ran_short(&self) -> bool {
        self.busy > 0 || self.file_limit_errors > 0
    }
}

/// State reported by one watchdog tick ([`crate::SwitchService::router_watch`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RouterWatchState {
    /// Router mode is off or Codex does not go through the router: nothing to watch.
    Inactive,
    /// The router answers.
    Healthy,
    /// The router did not answer; still within the grace period.
    Suspect {
        /// What the check saw (no secrets).
        detail: String,
    },
    /// The router stayed down: router mode was turned off and Codex switched back
    /// to direct mode.
    FellBack {
        /// What the last check saw (no secrets).
        detail: String,
    },
    /// The router listens on another port than Codex was pointed at (another
    /// program held the old one, user bug 65): a switch pointed Codex at the
    /// new port; router mode stays on.
    Moved {
        /// The port Codex used.
        from: u16,
        /// The port the router listens on now.
        to: u16,
    },
}

/// Result of one watchdog tick.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RouterWatch {
    /// State.
    pub state: RouterWatchState,
    /// The fallback switch, or the switch to the router's new port, when one
    /// ran (`None` also when it was not needed).
    #[serde(default)]
    pub outcome: Option<crate::plan::SwitchOutcome>,
    /// Why the fallback or the move to a new port failed, if it did (it is
    /// retried at the next tick).
    #[serde(default)]
    pub error: Option<crate::Error>,
    /// What else the tick did (Claude Code moved back to its provider).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<crate::i18n::Notice>,
}

/// `spb router doctor` and the Router page's Diagnostics: everything needed to
/// see why the local router does not start, as one block a user can paste into
/// a support request. Built by the switch service; never holds a key or token.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouterDoctorReport {
    /// Heading (Superba version, platform).
    pub title: LocalizedText,
    /// Problems found, most important first, each saying what to do.
    #[serde(default)]
    pub findings: Vec<crate::i18n::Notice>,
    /// Facts in display order.
    #[serde(default)]
    pub sections: Vec<RouterDoctorSection>,
    /// The command that runs the router in a terminal exactly as the service
    /// does (program and arguments), for `spb router doctor --foreground`.
    #[serde(default)]
    pub foreground: Vec<String>,
}

/// One part of a [`RouterDoctorReport`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouterDoctorSection {
    /// Heading.
    pub title: LocalizedText,
    /// Label / value rows.
    #[serde(default)]
    pub rows: Vec<RouterDoctorRow>,
    /// Verbatim lines (command output, the end of the log).
    #[serde(default)]
    pub lines: Vec<String>,
}

/// One fact.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouterDoctorRow {
    /// What it is.
    pub label: LocalizedText,
    /// Its value (paths and command output are passed verbatim as arguments).
    pub value: LocalizedText,
}

impl RouterDoctorReport {
    /// Plain text with `render` resolving the texts: the heading, the findings
    /// (`!` for problems, `-` for notes), then each section with its rows and
    /// lines indented. Meant to be copied as a whole.
    pub fn render_text(&self, render: &dyn Fn(&LocalizedText) -> String) -> String {
        let mut out = render(&self.title);
        out.push('\n');
        if !self.findings.is_empty() {
            out.push('\n');
            for f in &self.findings {
                let mark = match f.severity {
                    crate::i18n::Severity::Info => '-',
                    _ => '!',
                };
                out.push_str(&format!("{mark} {}\n", render(&f.text)));
            }
        }
        for section in &self.sections {
            out.push_str(&format!("\n[{}]\n", render(&section.title)));
            for row in &section.rows {
                out.push_str(&format!(
                    "  {}: {}\n",
                    render(&row.label),
                    render(&row.value)
                ));
            }
            for line in &section.lines {
                out.push_str("    ");
                out.push_str(line);
                out.push('\n');
            }
        }
        out
    }
}
