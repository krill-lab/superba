//! Verified facts about Codex 0.162 and the desktop app 26.1013, as constants and
//! small pure helpers. Keep this the single source of these strings.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Error, Result};

/// `CODEX_HOME` (defaults to `~/.codex`).
pub const ENV_CODEX_HOME: &str = "CODEX_HOME";
/// `CODEX_SQLITE_HOME` (directory of `state_5.sqlite`, defaults to `CODEX_HOME`).
pub const ENV_CODEX_SQLITE_HOME: &str = "CODEX_SQLITE_HOME";
/// PAT whoami origin. Read once at process start; no config key exists for it. Only
/// the PAT path reads it, so it may stay set permanently.
pub const ENV_AUTHAPI_BASE_URL: &str = "CODEX_AUTHAPI_BASE_URL";
/// Overrides Superba's data directory (tests, e2e, portable installs).
pub const ENV_SUPERBA_DATA_DIR: &str = "SUPERBA_DATA_DIR";
/// Explicit desktop app executable or install directory (e2e with an extracted app,
/// portable installs); takes precedence over install detection.
pub const ENV_SUPERBA_APP_PATH: &str = "SUPERBA_APP_PATH";
/// Extra arguments appended when Superba launches the app, whitespace separated
/// (e2e: `--no-sandbox --disable-gpu`).
pub const ENV_SUPERBA_APP_ARGS: &str = "SUPERBA_APP_ARGS";
/// Extra feed mirror base URLs, whitespace separated (tests and e2e add a local
/// mirror; never removes signature or checksum verification).
pub const ENV_SUPERBA_FEED_MIRRORS: &str = "SUPERBA_FEED_MIRRORS";

/// Codex executable (or a directory holding `codex`) the desktop app starts its
/// app-server with. Read by the app main process at launch, so changing it needs a
/// full app restart. A missing path silently falls back to the bundled Codex, and
/// the app sets it on every app-server it spawns, so only the process image
/// proves a pin.
pub const ENV_CODEX_CLI_PATH: &str = "CODEX_CLI_PATH";
/// Directory the app prepends to `PATH` when it starts `codex` on an SSH host
/// (`${CODEX_INSTALL_DIR:-$HOME/.local/bin}`).
pub const ENV_CODEX_INSTALL_DIR: &str = "CODEX_INSTALL_DIR";

/// Minimum app-server version of the desktop app when the floor cannot be read
/// from the installed `app.asar` (`0.141.0` in 26.1007 and 26.1013).
pub const APP_SERVER_FLOOR_FALLBACK: CodexVersion = CodexVersion::new(0, 141, 0);

/// `originator` header of requests from the desktop app's app-server.
pub const ORIGINATOR_DESKTOP: &str = "Codex Desktop";
/// `originator` header of the interactive CLI (TUI).
pub const ORIGINATOR_TUI: &str = "codex_cli_rs";
/// `originator` header of `codex exec` (never gets a stats line).
pub const ORIGINATOR_EXEC: &str = "codex_exec";

/// Marker that starts the router's stats line: `⚡` followed by U+2060 WORD
/// JOINER (invisible in the app, survives the rollout and re-sends, never produced
/// by the model). The router strips the line it marks from every outgoing request.
pub const STATS_LINE_MARKER: &str = "\u{26a1}\u{2060}";

/// The only provider id Superba writes. The app gates features on it and filters the
/// thread list by it.
pub const PROVIDER_OPENAI: &str = "openai";
/// Provider id written by the legacy Krill tutorial (`[model_providers.krill]`);
/// sessions under it are migrated to [`PROVIDER_OPENAI`] on takeover.
pub const LEGACY_KRILL_PROVIDER: &str = "krill";

/// Input item Codex appends to a remote compaction request on the ordinary
/// Responses endpoint, HTTP or WebSocket (`{"type":"compaction_trigger"}`;
/// Codex 0.149+ for the built-in `openai` provider, the only one Superba
/// writes; there is no setting that makes that provider compact locally —
/// see [`crate::compaction`]).
pub const COMPACTION_TRIGGER_ITEM: &str = "compaction_trigger";
/// Item holding a compacted history: the single output item of a compaction
/// request, sent back as input in every later request of the conversation
/// (`{"type":"compaction","encrypted_content":"…"}`, readable only by the
/// provider that made it).
pub const COMPACTION_ITEM: &str = "compaction";

/// Owned top-level `config.toml` keys (plan 9.1). Never write `[model_providers.*]`.
pub mod config_keys {
    /// `model_provider` (always `"openai"` when Superba manages the home).
    pub const MODEL_PROVIDER: &str = "model_provider";
    /// `openai_base_url` (`<line>/backend-api/codex`, `<line>/codex/v1` or a compatible base).
    pub const OPENAI_BASE_URL: &str = "openai_base_url";
    /// `chatgpt_base_url` (`<line>/backend-api/`, PAT only).
    pub const CHATGPT_BASE_URL: &str = "chatgpt_base_url";
    /// `model` (only when the profile sets it).
    pub const MODEL: &str = "model";
    /// `model_reasoning_effort` (only when the profile sets it).
    pub const MODEL_REASONING_EFFORT: &str = "model_reasoning_effort";
    /// Top-level `profile = "..."` override (diagnostics only; never written).
    pub const PROFILE: &str = "profile";
    /// `cli_auth_credentials_store` (diagnostics only; never written).
    pub const CLI_AUTH_CREDENTIALS_STORE: &str = "cli_auth_credentials_store";
    /// `[model_providers]` table (read for legacy detection; never written).
    pub const MODEL_PROVIDERS: &str = "model_providers";
    /// `model_catalog_json = "<abs path>"`: a static `{"models":[...]}` catalog that
    /// replaces Codex's model list (read once at start; a parse error stops the
    /// app-server, so the file is always preflighted). Owned by the profile's
    /// models ([`crate::ModelDefaults`], per-model overrides, speed tiers).
    pub const MODEL_CATALOG_JSON: &str = "model_catalog_json";
    /// `model_context_window`: one context window for every model (the "all
    /// models" row; Codex clamps it to each model's `max_context_window`).
    pub const MODEL_CONTEXT_WINDOW: &str = "model_context_window";
    /// `model_auto_compact_token_limit`: one compaction threshold for every model
    /// (Codex clamps it to 90 % of the window).
    pub const MODEL_AUTO_COMPACT_TOKEN_LIMIT: &str = "model_auto_compact_token_limit";
    /// `service_tier`: `"default"` (explicit Standard), `"fast"` or `"ultrafast"`
    /// ([`crate::SpeedTier::config_value`]).
    pub const SERVICE_TIER: &str = "service_tier";
}

/// A literal TOML value in a static key table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TomlLiteral {
    /// String value.
    Str(&'static str),
    /// Boolean value.
    Bool(bool),
}

/// Keys the Krill PAT tutorial recommends (plan 9.1), as dotted paths. Written only
/// in PAT mode; restored to the
/// baseline when leaving PAT mode.
pub const PAT_RECOMMENDED_KEYS: &[(&str, TomlLiteral)] = &[
    ("web_search", TomlLiteral::Str("live")),
    (
        "suppress_unstable_features_warning",
        TomlLiteral::Bool(true),
    ),
    ("features.fast_mode", TomlLiteral::Bool(true)),
    ("features.context_management", TomlLiteral::Bool(true)),
    ("features.token_budget.enabled", TomlLiteral::Bool(true)),
    (
        "features.token_budget.use_history_notes_extension",
        TomlLiteral::Bool(true),
    ),
    ("analytics.enabled", TomlLiteral::Bool(false)),
];

/// `auth.json` field names.
pub mod auth_fields {
    /// PAT field.
    pub const PERSONAL_ACCESS_TOKEN: &str = "personal_access_token";
    /// API key field (also set in PAT mode).
    pub const OPENAI_API_KEY: &str = "OPENAI_API_KEY";
    /// Mode discriminator (`"apikey"` for API-key mode).
    pub const AUTH_MODE: &str = "auth_mode";
    /// Value of [`AUTH_MODE`] for API-key mode.
    pub const AUTH_MODE_APIKEY: &str = "apikey";
    /// ChatGPT login token object.
    pub const TOKENS: &str = "tokens";
    /// ChatGPT login refresh timestamp.
    pub const LAST_REFRESH: &str = "last_refresh";
}

/// IPC message type that restarts an app-server (handled by the app main process).
pub const IPC_APP_SERVER_RESTART: &str = "codex-app-server-restart";
/// `hostId` of the local app-server.
pub const LOCAL_HOST_ID: &str = "local";
/// Prefix of the main renderer page URL (the CDP target to evaluate in).
pub const RENDERER_URL_PREFIX: &str = "app://-/index.html";
/// Substrings of renderer URLs that are NOT the main window.
pub const RENDERER_URL_EXCLUDES: &[&str] = &["initialRoute=/avatar-overlay", "quick-chat-prewarm"];
/// Response header set by the Krill edge when it intercepts a path (empty 200).
pub const EDGE_BLOCK_HEADER: &str = "l-block";

/// True if a CDP target URL is the app's main renderer page.
pub fn is_main_renderer_url(url: &str) -> bool {
    url.starts_with(RENDERER_URL_PREFIX) && !RENDERER_URL_EXCLUDES.iter().any(|x| url.contains(x))
}

/// JavaScript to evaluate in the main renderer to restart an app-server.
///
/// Local: `host_id = "local"`, `kill_remote = false`. SSH remote: the app `hostId`
/// and `kill_remote = true` (the app pkills the remote `codex app-server --listen`
/// and respawns it over ssh). Evaluates to `true` when the bridge accepted the call.
pub fn hot_restart_js(host_id: &str, kill_remote: bool) -> String {
    let mut msg = format!(
        "{{type:{},hostId:{},intent:'restart'",
        js_string(IPC_APP_SERVER_RESTART),
        js_string(host_id)
    );
    if kill_remote {
        msg.push_str(",killCodexProcess:true");
    }
    msg.push('}');
    format!(
        "(() => {{ const b = window.electronBridge; \
         if (!b || typeof b.sendMessageFromView !== 'function') return false; \
         b.sendMessageFromView({msg}); return true; }})()"
    )
}

/// JavaScript that evaluates to `true` when the restart bridge is present.
pub const BRIDGE_PROBE_JS: &str =
    "typeof window.electronBridge?.sendMessageFromView === 'function'";

/// Quote a string as a JavaScript (and JSON) string literal.
pub fn js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A Codex version such as `0.162.0` or `0.162.0-alpha.17.2`.
///
/// Ordering follows semver (a pre-release sorts before its release). Version gates
/// use [`CodexVersion::meets`], which compares only `major.minor.patch`, because
/// the app bundles pre-release builds (e.g. `0.162.0-alpha.17.2`) that already have
/// the features of the release.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CodexVersion {
    /// Major.
    pub major: u64,
    /// Minor.
    pub minor: u64,
    /// Patch.
    pub patch: u64,
    /// Pre-release identifiers (`alpha.17.2`), if any.
    pub pre: Option<String>,
}

impl CodexVersion {
    /// Construct a release version.
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
            pre: None,
        }
    }

    /// Find and parse the first whitespace/punctuation-delimited token of the form
    /// `[v]X.Y.Z[-pre][+build]` in free text such as `codex-cli 0.162.0`,
    /// `0.162.0-alpha.17.2` or `v1.2.3`. Malformed tokens (`1.2.3.4`) never match
    /// partially.
    pub fn parse(text: &str) -> Result<Self> {
        text.split(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    '(' | ')' | '[' | ']' | ',' | ';' | '"' | '\'' | '/' | '='
                )
        })
        .find_map(Self::parse_token)
        .ok_or_else(|| Error::parse("codex version", format!("no version in {text:?}")))
    }

    fn parse_token(token: &str) -> Option<Self> {
        let token = token.trim_end_matches(['.', ':']);
        let token = token.strip_prefix(['v', 'V']).unwrap_or(token);
        let token = token.split_once('+').map_or(token, |(t, _)| t);
        let (core, pre) = match token.split_once('-') {
            Some((c, p)) => {
                let valid = !p.is_empty()
                    && p.split('.').all(|id| {
                        !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                    });
                if !valid {
                    return None;
                }
                (c, Some(p.to_owned()))
            }
            None => (token, None),
        };
        let parts: Vec<&str> = core.split('.').collect();
        let [major, minor, patch] = parts.as_slice() else {
            return None;
        };
        let num = |s: &str| -> Option<u64> {
            (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| s.parse().ok())
                .flatten()
        };
        Some(Self {
            major: num(major)?,
            minor: num(minor)?,
            patch: num(patch)?,
            pre,
        })
    }

    /// `(major, minor, patch)`.
    pub fn triple(&self) -> (u64, u64, u64) {
        (self.major, self.minor, self.patch)
    }

    /// True if `major.minor.patch >= min.major.minor.patch` (pre-release ignored).
    pub fn meets(&self, min: &CodexVersion) -> bool {
        self.triple() >= min.triple()
    }
}

impl Ord for CodexVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.triple()
            .cmp(&other.triple())
            .then_with(|| match (&self.pre, &other.pre) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => cmp_pre(a, b),
            })
    }
}

impl PartialOrd for CodexVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn cmp_pre(a: &str, b: &str) -> Ordering {
    let mut ia = a.split('.');
    let mut ib = b.split('.');
    loop {
        match (ia.next(), ib.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let o = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(nx), Ok(ny)) => nx.cmp(&ny),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    (Err(_), Err(_)) => x.cmp(y),
                };
                if o != Ordering::Equal {
                    return o;
                }
            }
        }
    }
}

impl fmt::Display for CodexVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(p) = &self.pre {
            write!(f, "-{p}")?;
        }
        Ok(())
    }
}

impl FromStr for CodexVersion {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        CodexVersion::parse(s)
    }
}

impl Serialize for CodexVersion {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for CodexVersion {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        CodexVersion::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// Capabilities gated by Codex version (plan 9.8). The table of minimum versions is
/// owned by `superba-switch`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum VersionCapability {
    /// Honours top-level `openai_base_url` (else keys would go to api.openai.com).
    TopLevelOpenaiBaseUrl,
    /// Supports `personal_access_token` + `CODEX_AUTHAPI_BASE_URL`.
    PersonalAccessToken,
    /// Calls `<chatgpt_base_url>wham/accounts/check` (needs `chatgpt_base_url`).
    WhamAccountsCheck,
}
