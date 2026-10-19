//! Coding-agent harnesses besides Codex and Claude Code: dsh (DeepSeek
//! Harness), opencode (OpenCode 1 and 2), pi, ZCode and WorkBuddy.
//!
//! Superba writes **one provider it owns** (id [`ENTRY_ID`]) with a model set
//! into each harness's own configuration files, reads back what is there and
//! restores what was there before Superba. Only the entries Superba owns
//! change; comments and every other key stay where they are
//! (`superba_config::harness`).
//!
//! - [`HarnessId`]: which harness; [`HarnessProtocol`]: the wire protocol it
//!   speaks on one connection. Direct mode requires a matching native endpoint;
//!   routed mode can use the local router's enabled protocol conversions.
//! - [`HarnessSettings`] / [`HarnessConfig`]: what Superba wants each harness
//!   to use (`<data>/harnesses.json`), and the Chat Completions / Anthropic
//!   endpoints declared for providers that do not have them built in.
//! - [`ProtocolEndpoint`] / [`provider_endpoints`]: the endpoints a provider
//!   offers per protocol; [`routed_endpoint`]: the local router's ingress
//!   ("routed" mode: a base URL plus a per-harness token; the `/openai/v1`
//!   ingress itself is a router follow-up).
//! - [`HarnessTarget`]: everything one apply writes (holds the key; never
//!   serialized).
//! - [`views`]: what the service returns to `spb harness` (and later the GUI).
//!
//! Shared shape with Claude Code (`superba_core::claude` on the `cc1`
//! branch): plan / apply / restore with compare-and-swap, owned entries with
//! their values from before Superba, drift kept unless taken over, backups in
//! the file editor's store. After the merge both can sit behind one
//! "harness writer" abstraction (see `superba_config::harness`).

pub mod views;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::i18n::LocalizedText;
use crate::ids::ProfileId;
use crate::models::ModelSpec;
use crate::secret::{KeyRef, Secret};
use crate::site::{Site, SiteKind};

pub use views::*;

/// The id of the one provider entry Superba owns in every harness
/// (`providers.superba`, `superba/<model>`, `providerId: superba`).
pub const ENTRY_ID: &str = "superba";

/// The router's OpenAI-compatible ingress (Responses and Chat Completions),
/// joined onto the router's base URL in routed mode (multi-harness §1; the
/// ingress is a router follow-up).
pub const ROUTER_OPENAI_PATH: &str = "/openai/v1";
/// The router's Anthropic Messages ingress (the client appends
/// `/v1/messages`).
pub const ROUTER_ANTHROPIC_PATH: &str = "/claude";

/// The variable dsh reads Superba's key from (`apiKeyEnv`), written to
/// `$DSH_HOME/.env`: the headless profile reads neither `.credentials.yaml`
/// nor anything else, and `DSH_*` names are refused in `.env` files.
pub const DSH_KEY_ENV: &str = "SUPERBA_API_KEY";
/// `DSH_HOME`: dsh's home (default `~/.dsh`; empty = unset).
pub const ENV_DSH_HOME: &str = "DSH_HOME";
/// `OPENCODE_CONFIG_DIR`: replaces opencode's global configuration directory.
pub const ENV_OPENCODE_CONFIG_DIR: &str = "OPENCODE_CONFIG_DIR";
/// `XDG_CONFIG_HOME` (opencode uses `~/.config` on every system otherwise).
pub const ENV_XDG_CONFIG_HOME: &str = "XDG_CONFIG_HOME";
/// `PI_CODING_AGENT_DIR`: replaces pi's agent directory (`~/.pi/agent`).
pub const ENV_PI_AGENT_DIR: &str = "PI_CODING_AGENT_DIR";
/// `PI_SKIP_VERSION_CHECK`: pi's start-up "new version" check is skipped.
pub const ENV_PI_SKIP_VERSION_CHECK: &str = "PI_SKIP_VERSION_CHECK";
/// `ZCODE_PERSONAL_PROVIDER_CONFIG_FILE`: where ZCode's agent reads the
/// personal providers (the desktop sets it to `~/.zcode/v2/provider_config.json`).
pub const ENV_ZCODE_PROVIDER_FILE: &str = "ZCODE_PERSONAL_PROVIDER_CONFIG_FILE";
/// `WORKBUDDY_CONFIG_DIR`: replaces WorkBuddy's configuration directory.
pub const ENV_WORKBUDDY_CONFIG_DIR: &str = "WORKBUDDY_CONFIG_DIR";

/// The environment a launch of pi that Superba controls gets when Superba
/// manages pi's updates: the start-up version check is skipped (pi has no
/// settings key for it). Empty otherwise. Seam for the tools lifecycle work
/// (`spb tools`), which launches and updates pi.
pub fn pi_launch_env(superba_manages_updates: bool) -> Vec<(&'static str, &'static str)> {
    if superba_manages_updates {
        vec![(ENV_PI_SKIP_VERSION_CHECK, "1")]
    } else {
        Vec::new()
    }
}

/// A harness Superba configures here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessId {
    /// Claude Code (configured on its own page, using the shared writer).
    ClaudeCode,
    /// dsh, the DeepSeek Harness (`$DSH_HOME/profiles/<p>/cordis.patch.yml`).
    Dsh,
    /// opencode, OpenCode 1 or 2 (`~/.config/opencode/opencode.json[c]`).
    #[serde(rename = "opencode")]
    OpenCode,
    /// pi (`~/.pi/agent/models.json` + `settings.json`).
    Pi,
    /// ZCode (`~/.zcode/v2/provider_config.json`).
    #[serde(rename = "zcode")]
    ZCode,
    /// WorkBuddy, China and international builds
    /// (`~/.workbuddy/models.json`, `~/.workbuddy-ai/models.json`).
    #[serde(rename = "workbuddy")]
    WorkBuddy,
}

impl HarnessId {
    /// Lightweight tools, in display order. Claude Code has its own page.
    pub const ALL: [HarnessId; 5] = [
        HarnessId::Dsh,
        HarnessId::OpenCode,
        HarnessId::Pi,
        HarnessId::ZCode,
        HarnessId::WorkBuddy,
    ];

    /// `dsh`, `opencode`, `pi`, `zcode`, `workbuddy`.
    pub fn as_str(self) -> &'static str {
        match self {
            HarnessId::ClaudeCode => "claude_code",
            HarnessId::Dsh => "dsh",
            HarnessId::OpenCode => "opencode",
            HarnessId::Pi => "pi",
            HarnessId::ZCode => "zcode",
            HarnessId::WorkBuddy => "workbuddy",
        }
    }

    /// Parse [`HarnessId::as_str`] (case-insensitive; a few aliases).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude_code" => Some(HarnessId::ClaudeCode),
            "dsh" | "deepseek-harness" | "deepseek" => Some(HarnessId::Dsh),
            "opencode" | "open-code" => Some(HarnessId::OpenCode),
            "pi" => Some(HarnessId::Pi),
            "zcode" | "z-code" => Some(HarnessId::ZCode),
            "workbuddy" | "work-buddy" => Some(HarnessId::WorkBuddy),
            _ => None,
        }
    }

    /// The product's name (not translated).
    pub fn display_name(self) -> &'static str {
        match self {
            HarnessId::ClaudeCode => "Claude Code",
            HarnessId::Dsh => "dsh",
            HarnessId::OpenCode => "opencode",
            HarnessId::Pi => "pi",
            HarnessId::ZCode => "ZCode",
            HarnessId::WorkBuddy => "WorkBuddy",
        }
    }

    /// The protocols the harness can speak to a provider, preferred first.
    pub fn protocols(self) -> &'static [HarnessProtocol] {
        use HarnessProtocol as P;
        match self {
            HarnessId::ClaudeCode => &[P::Anthropic],
            HarnessId::WorkBuddy => &[P::Chat],
            HarnessId::Dsh | HarnessId::OpenCode | HarnessId::Pi | HarnessId::ZCode => {
                &[P::Responses, P::Chat, P::Anthropic]
            }
        }
    }

    /// The harness speaks `protocol`.
    pub fn speaks(self, protocol: HarnessProtocol) -> bool {
        self.protocols().contains(&protocol)
    }
}

impl std::fmt::Display for HarnessId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The protocol a harness speaks to its provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessProtocol {
    /// OpenAI Responses (`POST <base>/responses`).
    Responses,
    /// OpenAI Chat Completions (`POST <base>/chat/completions`).
    Chat,
    /// Anthropic Messages (`POST <root>/v1/messages`).
    Anthropic,
}

impl HarnessProtocol {
    /// Every protocol.
    pub const ALL: [HarnessProtocol; 3] = [
        HarnessProtocol::Responses,
        HarnessProtocol::Chat,
        HarnessProtocol::Anthropic,
    ];

    /// `responses`, `chat`, `anthropic`.
    pub fn as_str(self) -> &'static str {
        match self {
            HarnessProtocol::Responses => "responses",
            HarnessProtocol::Chat => "chat",
            HarnessProtocol::Anthropic => "anthropic",
        }
    }

    /// Parse [`HarnessProtocol::as_str`] and the harnesses' own names
    /// (`openai-responses`, `openai-completions`, `chat-completions`,
    /// `anthropic-messages`, `messages`).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "responses" | "openai-responses" | "response" => Some(HarnessProtocol::Responses),
            "chat"
            | "chat-completions"
            | "openai-completions"
            | "openai-chat-completions"
            | "completions" => Some(HarnessProtocol::Chat),
            "anthropic" | "anthropic-messages" | "messages" | "claude" => {
                Some(HarnessProtocol::Anthropic)
            }
            _ => None,
        }
    }

    /// An OpenAI protocol (base URL ends in `/v1`).
    pub fn is_openai(self) -> bool {
        !matches!(self, HarnessProtocol::Anthropic)
    }

    /// "OpenAI Responses", "Chat Completions", "Anthropic Messages".
    pub fn label(self) -> &'static str {
        match self {
            HarnessProtocol::Responses => "OpenAI Responses",
            HarnessProtocol::Chat => "Chat Completions",
            HarnessProtocol::Anthropic => "Anthropic Messages",
        }
    }
}

impl std::fmt::Display for HarnessProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How the harness reaches its models.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessRoute {
    /// Straight to the provider's endpoint with the provider's key.
    #[default]
    Direct,
    /// Through Superba's local router: its ingress for the protocol and a
    /// per-harness router token ([`HarnessSettings::router_url`] /
    /// [`HarnessSettings::router_token`]; later from the router crate).
    Routed,
}

impl HarnessRoute {
    /// `direct`, `routed`.
    pub fn as_str(self) -> &'static str {
        match self {
            HarnessRoute::Direct => "direct",
            HarnessRoute::Routed => "routed",
        }
    }
}

/// WorkBuddy's two builds (separate apps and configuration directories).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkBuddyEdition {
    /// WorkBuddy (China): `~/.workbuddy`.
    Cn,
    /// WorkBuddy AI (international): `~/.workbuddy-ai`.
    Intl,
}

impl WorkBuddyEdition {
    /// Both.
    pub const ALL: [WorkBuddyEdition; 2] = [WorkBuddyEdition::Cn, WorkBuddyEdition::Intl];

    /// `cn`, `intl`.
    pub fn as_str(self) -> &'static str {
        match self {
            WorkBuddyEdition::Cn => "cn",
            WorkBuddyEdition::Intl => "intl",
        }
    }

    /// Parse [`WorkBuddyEdition::as_str`] (also `china`, `international`).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cn" | "china" | "zh" => Some(WorkBuddyEdition::Cn),
            "intl" | "international" | "ai" | "global" => Some(WorkBuddyEdition::Intl),
            _ => None,
        }
    }

    /// The configuration directory's name under the home directory.
    pub fn dir_name(self) -> &'static str {
        match self {
            WorkBuddyEdition::Cn => ".workbuddy",
            WorkBuddyEdition::Intl => ".workbuddy-ai",
        }
    }
}

/// opencode's two products under one name, with different configuration
/// shapes (OpenCode 1 drops an OpenCode 2 `providers` block).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpencodeMajor {
    /// OpenCode 1 (`provider{npm, options}`, restart needed).
    V1,
    /// OpenCode 2 (`providers{package, settings}`, hot reload).
    V2,
}

impl OpencodeMajor {
    /// `1`, `2`.
    pub fn as_str(self) -> &'static str {
        match self {
            OpencodeMajor::V1 => "1",
            OpencodeMajor::V2 => "2",
        }
    }

    /// Parse `1` / `2` (also `v1`, `v2`).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().trim_start_matches(['v', 'V']) {
            "1" => Some(OpencodeMajor::V1),
            "2" => Some(OpencodeMajor::V2),
            _ => None,
        }
    }

    /// The major of `opencode --version` (`1.19.3`, `opencode v2.0.32`).
    pub fn of_version(text: &str) -> Option<Self> {
        let t = text.trim();
        let t = t.strip_prefix("opencode").unwrap_or(t).trim();
        let t = t.trim_start_matches(['v', 'V']);
        match t.split('.').next()?.trim() {
            "1" => Some(OpencodeMajor::V1),
            "2" => Some(OpencodeMajor::V2),
            _ => None,
        }
    }
}

/// dsh's telemetry uploads (session-log upload on the DeepSeek route, OTel
/// feedback upload): left alone, or switched off with two patch entries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DshTelemetry {
    /// Superba does not touch dsh's telemetry entries.
    #[default]
    Unmanaged,
    /// `session-telemetry-otel` disabled and `session-log-deepseek`
    /// `enabled: false` in every profile patch.
    Off,
}

/// Per-harness options.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessOptions {
    /// dsh: telemetry entries.
    #[serde(default)]
    pub dsh_telemetry: DshTelemetry,
    /// opencode: the configuration shape to write (`None` = the installed
    /// major, else OpenCode 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opencode_major: Option<OpencodeMajor>,
    /// WorkBuddy: the builds to configure (empty = those whose
    /// configuration directory exists, else the China build).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workbuddy_editions: Vec<WorkBuddyEdition>,
}

/// What Superba wants one harness to use (`<data>/harnesses.json`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessSettings {
    /// The Superba provider (its models; its endpoint and key in direct
    /// mode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProfileId>,
    /// The protocol (`None` = the first the harness speaks that the provider
    /// offers: Responses, Chat Completions, Anthropic).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<HarnessProtocol>,
    /// The models (empty = every model of the provider's list).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    /// The default model (`None` = the first model).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// Direct or through the local router.
    #[serde(default)]
    pub route: HarnessRoute,
    /// Routed mode: the router's base URL (`http://127.0.0.1:<port>`; the
    /// protocol's ingress path is added). Explicit until the router crate
    /// hands out per-harness routes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router_url: Option<Url>,
    /// Routed mode: the per-harness router token in Superba's key store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router_token: Option<KeyRef>,
    /// Per-harness options.
    #[serde(default)]
    pub options: HarnessOptions,
}

/// Endpoints declared for a provider that does not have them built in
/// (compatible sites: Superba knows only their Responses base URL).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderProtocols {
    /// The Chat Completions base (`…/v1`; the client appends
    /// `/chat/completions`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<Url>,
    /// The Anthropic Messages root (the client appends `/v1/messages`).
    /// After the Claude Code merge this comes from the provider's
    /// `anthropic` endpoint instead (`superba_core::claude::effective_endpoint`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic: Option<Url>,
}

impl ProviderProtocols {
    /// Nothing declared.
    pub fn is_empty(&self) -> bool {
        self.chat.is_none() && self.anthropic.is_none()
    }
}

/// Current `harnesses.json` schema.
pub const HARNESS_CONFIG_SCHEMA: u32 = 1;

/// Superba's configuration of every harness (`<data>/harnesses.json`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessConfig {
    /// Format.
    #[serde(default)]
    pub schema: u32,
    /// Per harness.
    #[serde(default)]
    pub harnesses: BTreeMap<HarnessId, HarnessSettings>,
    /// Declared endpoints per provider.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub providers: BTreeMap<ProfileId, ProviderProtocols>,
}

impl HarnessConfig {
    /// The settings of `harness` (default when none are saved).
    pub fn settings(&self, harness: HarnessId) -> HarnessSettings {
        self.harnesses.get(&harness).cloned().unwrap_or_default()
    }

    /// The declared endpoints of `provider`.
    pub fn declared(&self, provider: &ProfileId) -> Option<&ProviderProtocols> {
        self.providers.get(provider).filter(|p| !p.is_empty())
    }
}

/// Where an endpoint comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointSource {
    /// The provider's own endpoint or suggested preset.
    Provider,
    /// Krill's line and the shared all-models preset.
    Krill,
    /// Declared for the provider ([`ProviderProtocols`]).
    Declared,
    /// The local router's ingress.
    Router,
}

/// One protocol endpoint. `base_url` is the OpenAI base (`…/v1`: the client
/// appends `/responses` or `/chat/completions`) or the Anthropic root (the
/// client appends `/v1/messages`); each writer adapts it to its harness.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProtocolEndpoint {
    /// The protocol.
    pub protocol: HarnessProtocol,
    /// Base URL (see the type).
    pub base_url: Url,
    /// Where it comes from.
    pub source: EndpointSource,
}

impl ProtocolEndpoint {
    /// The base URL as harnesses want it: no trailing slash.
    pub fn base_text(&self) -> String {
        self.base_url.as_str().trim_end_matches('/').to_owned()
    }
}

fn join(origin: &Url, path: &str) -> Option<Url> {
    let base = origin.as_str().trim_end_matches('/');
    Url::parse(&format!("{base}{path}")).ok()
}

/// The endpoints a provider offers, one per protocol it serves: Krill's line
/// serves all three; a compatible site its Responses base URL plus what was
/// declared for it. Anthropic uses the provider's effective Claude Code endpoint
/// unless a harness endpoint was explicitly declared. `origin` is the resolved
/// line origin (Krill) and `responses_base` its resolved Responses base URL.
pub fn provider_endpoints(
    site: &Site,
    profile: &crate::Profile,
    origin: &Url,
    responses_base: &Url,
    declared: Option<&ProviderProtocols>,
) -> Vec<ProtocolEndpoint> {
    let mut out = Vec::new();
    let mut push = |protocol, base_url: Option<Url>, source| {
        if let Some(base_url) = base_url {
            out.push(ProtocolEndpoint {
                protocol,
                base_url,
                source,
            });
        }
    };
    let declared_chat = declared.and_then(|d| d.chat.clone());
    let declared_anthropic = declared.and_then(|d| d.anthropic.clone());
    if site.kind == SiteKind::Krill {
        // The same all-models root as Claude Code's default Krill preset,
        // with the OpenAI /v1 suffix instead of /v1/messages.
        let v1 = crate::claude::anthropic_preset(crate::claude::PRESET_KRILL)
            .and_then(|preset| preset.endpoint(Some(origin)))
            .and_then(|endpoint| join(&endpoint.base_url, "/v1"));
        push(
            HarnessProtocol::Responses,
            v1.clone(),
            EndpointSource::Krill,
        );
        match declared_chat {
            Some(u) => push(HarnessProtocol::Chat, Some(u), EndpointSource::Declared),
            None => push(HarnessProtocol::Chat, v1, EndpointSource::Krill),
        }
    } else {
        push(
            HarnessProtocol::Responses,
            Some(responses_base.clone()),
            EndpointSource::Provider,
        );
        push(
            HarnessProtocol::Chat,
            declared_chat,
            EndpointSource::Declared,
        );
    }
    let (anthropic, source) = match declared_anthropic {
        Some(url) => (Some(url), EndpointSource::Declared),
        None => (
            crate::claude::effective_endpoint(site, profile).map(|e| e.base_url),
            if site.kind == SiteKind::Krill && profile.anthropic.is_none() {
                EndpointSource::Krill
            } else {
                EndpointSource::Provider
            },
        ),
    };
    push(HarnessProtocol::Anthropic, anthropic, source);
    out
}

/// The local router's ingress for `protocol` at `router_url` (its base URL):
/// `/openai/v1` for the OpenAI protocols, `/claude` for Anthropic Messages.
pub fn routed_endpoint(router_url: &Url, protocol: HarnessProtocol) -> Option<ProtocolEndpoint> {
    let path = if protocol.is_openai() {
        ROUTER_OPENAI_PATH
    } else {
        ROUTER_ANTHROPIC_PATH
    };
    Some(ProtocolEndpoint {
        protocol,
        base_url: join(router_url, path)?,
        source: EndpointSource::Router,
    })
}

/// The protocol to use: the wanted one when the harness speaks it, else the
/// harness's first preference that `offered` contains.
pub fn pick_protocol(
    harness: HarnessId,
    wanted: Option<HarnessProtocol>,
    offered: &[HarnessProtocol],
) -> Option<HarnessProtocol> {
    match wanted {
        Some(p) => (harness.speaks(p) && offered.contains(&p)).then_some(p),
        None => harness
            .protocols()
            .iter()
            .copied()
            .find(|p| offered.contains(p)),
    }
}

/// Check a base URL typed for an endpoint or the router: http(s), no query
/// or fragment, plain http only on loopback. Returns the message key of the
/// problem.
pub fn check_base_url(input: &str) -> std::result::Result<Url, &'static str> {
    let t = input.trim();
    if t.is_empty() {
        return Err("harness.url.empty");
    }
    let url = Url::parse(t).map_err(|_| "harness.url.invalid")?;
    match url.scheme() {
        "https" => {}
        "http" => {
            let loopback = match url.host() {
                Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
                Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
                Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
                None => false,
            };
            if !loopback {
                return Err("harness.url.http");
            }
        }
        _ => return Err("harness.url.invalid"),
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("harness.url.query");
    }
    Ok(url)
}

/// [`check_base_url`]'s message for people.
pub fn base_url_problem(key: &'static str) -> LocalizedText {
    let fallback = match key {
        "harness.url.empty" => "Enter a URL.",
        "harness.url.http" => {
            "Plain http is accepted only for this computer (127.0.0.1, localhost)."
        }
        "harness.url.query" => "The URL must not have a query or fragment.",
        _ => "This is not an http(s) URL.",
    };
    LocalizedText::new(key, fallback)
}

/// One model as a harness gets it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessModel {
    /// Model id (sent as `model`).
    pub id: String,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Context window in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// Reasoning efforts it supports (`low`, `medium`, `high`, `xhigh`);
    /// empty = not a reasoning model (or unknown: nothing is declared).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_levels: Vec<String>,
    /// Default effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_reasoning: Option<String>,
    /// Takes images.
    #[serde(default)]
    pub images: bool,
}

impl HarnessModel {
    /// A model known only by id.
    pub fn id_only(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            ..Self::default()
        }
    }

    /// From the provider's model list.
    pub fn from_spec(spec: &ModelSpec) -> Self {
        let mut levels: Vec<String> = Vec::new();
        for l in &spec.reasoning_levels {
            let e = l.effort.trim().to_ascii_lowercase();
            if !e.is_empty() && !levels.contains(&e) {
                levels.push(e);
            }
        }
        Self {
            id: spec.slug.clone(),
            name: spec
                .display_name
                .clone()
                .filter(|n| !n.trim().is_empty() && *n != spec.slug),
            context_window: spec.context_window.filter(|w| *w > 0),
            default_reasoning: spec
                .default_reasoning_level
                .as_ref()
                .map(|d| d.trim().to_ascii_lowercase())
                .filter(|d| levels.contains(d)),
            reasoning_levels: levels,
            images: spec
                .input_modalities
                .iter()
                .any(|m| m.eq_ignore_ascii_case("image")),
        }
    }

    /// Supports reasoning efforts.
    pub fn reasons(&self) -> bool {
        !self.reasoning_levels.is_empty()
    }

    /// The effort levels other than `none` / `minimal` (the ones every
    /// harness offers by name).
    pub fn named_levels(&self) -> Vec<String> {
        self.reasoning_levels
            .iter()
            .filter(|l| !matches!(l.as_str(), "none" | "minimal"))
            .cloned()
            .collect()
    }
}

/// Everything one apply writes into a harness. Holds the key: never
/// serialized, redacted in `Debug`.
#[derive(Clone, Debug)]
pub struct HarnessTarget {
    /// Claude Code's specialized model slots and environment entries.
    pub claude: Option<crate::claude::ClaudeDesired>,
    /// The harness.
    pub harness: HarnessId,
    /// The Superba provider it was resolved from.
    pub provider: Option<ProfileId>,
    /// The provider's name, for the harness's provider display name.
    pub provider_name: String,
    /// The endpoint.
    pub endpoint: ProtocolEndpoint,
    /// The key (the provider's, or the router token in routed mode).
    pub key: Secret,
    /// The models, in order.
    pub models: Vec<HarnessModel>,
    /// The default model (one of [`Self::models`]).
    pub default_model: Option<String>,
    /// Direct or routed.
    pub route: HarnessRoute,
    /// Options.
    pub options: HarnessOptions,
}

impl HarnessTarget {
    /// The display name of Superba's provider entry: "Superba · Krill",
    /// "Superba · Krill (router)".
    pub fn display_name(&self) -> String {
        let name = self.provider_name.trim();
        let base = if name.is_empty() {
            "Superba".to_owned()
        } else {
            format!("Superba · {name}")
        };
        match self.route {
            HarnessRoute::Direct => base,
            HarnessRoute::Routed => format!("{base} (router)"),
        }
    }

    /// The default model: the chosen one, else the first.
    pub fn default_model_id(&self) -> Option<&str> {
        self.default_model
            .as_deref()
            .filter(|d| self.models.iter().any(|m| m.id == *d))
            .or_else(|| self.models.first().map(|m| m.id.as_str()))
    }
}

/// Characters no harness takes in a key value (they break the file or the
/// header): control characters, quotes and backslashes are refused before
/// anything is written.
pub fn check_key_text(key: &Secret) -> std::result::Result<(), &'static str> {
    let k = key.expose();
    if k.is_empty() {
        return Err("harness.key.empty");
    }
    if k.chars()
        .any(|c| c.is_control() || c == '"' || c == '\\' || c.is_whitespace())
    {
        return Err("harness.key.chars");
    }
    Ok(())
}
