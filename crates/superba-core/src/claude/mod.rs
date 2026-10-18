//! Claude Code as a second harness (user bug 79): using GPT, DeepSeek and other
//! models through Claude Code. Superba writes a set of keys it owns into
//! Claude Code's user `settings.json` (the Anthropic endpoint, the key, the
//! model slots and their capabilities, the `/model` picker rows) and never
//! touches the official Claude account (`~/.claude.json` `oauthAccount`, the
//! keychain).
//!
//! - [`ClaudeHome`]: where Claude Code keeps `settings.json`
//!   (`CLAUDE_CONFIG_DIR`, else `~/.claude`).
//! - [`AnthropicEndpoint`] / [`presets`]: a provider's Anthropic Messages
//!   endpoint (base URL and how the key is sent) and the verified vendor
//!   endpoints.
//! - [`models`]: slots, capabilities, picker rows and the per-provider model
//!   configuration ([`ClaudeConfig`]).
//! - [`render`]: the keys Superba owns and the values a configuration wants
//!   there ([`ClaudeDesired`]).
//! - [`views`]: what the service returns to the GUI and `spb claude`.
//!
//! Facts (Claude Code 2.1.295): `settings.json` `env` is applied to every
//! session and read only when a session starts; the built-in gateway model
//! discovery keeps only ids matching `claude|anthropic`, so other models reach
//! `/model` only through `modelPicker.options` and the slot variables;
//! `modelPricing` is honoured from managed settings only.

pub mod models;
pub mod presets;
pub mod render;
pub mod views;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::paths::EnvSource;

pub use models::*;
pub use presets::*;
pub use render::*;
pub use views::*;

/// `CLAUDE_CONFIG_DIR`: Claude Code's configuration directory (default
/// `~/.claude`).
pub const ENV_CLAUDE_CONFIG_DIR: &str = "CLAUDE_CONFIG_DIR";
/// The user settings file in [`ClaudeHome`].
pub const SETTINGS_FILE: &str = "settings.json";
/// The managed settings file in [`managed_settings_dir`].
pub const MANAGED_SETTINGS_FILE: &str = "managed-settings.json";
/// The `anthropic-version` header Superba sends to Anthropic endpoints.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Claude Code's configuration directory: `CLAUDE_CONFIG_DIR` when set and
/// not empty, else `<home>/.claude` (`%USERPROFILE%\.claude` on Windows).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ClaudeHome {
    root: PathBuf,
    /// The directory came from `CLAUDE_CONFIG_DIR`.
    from_env: bool,
    home_dir: PathBuf,
}

impl ClaudeHome {
    /// Resolve like Claude Code does.
    pub fn resolve(env: &dyn EnvSource, home_dir: &Path) -> Self {
        match env.var_os(ENV_CLAUDE_CONFIG_DIR).filter(|v| !v.is_empty()) {
            Some(dir) => Self {
                root: PathBuf::from(dir),
                from_env: true,
                home_dir: home_dir.to_path_buf(),
            },
            None => Self {
                root: home_dir.join(".claude"),
                from_env: false,
                home_dir: home_dir.to_path_buf(),
            },
        }
    }

    /// The directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory came from `CLAUDE_CONFIG_DIR`.
    pub fn from_env(&self) -> bool {
        self.from_env
    }

    /// `settings.json` (user scope).
    pub fn settings_json(&self) -> PathBuf {
        self.root.join(SETTINGS_FILE)
    }

    /// `.credentials.json`, where Claude Code keeps a claude.ai login on Linux
    /// and Windows (read for its presence only).
    pub fn credentials_json(&self) -> PathBuf {
        self.root.join(".credentials.json")
    }

    /// The global state file with the claude.ai account (`oauthAccount`):
    /// `<CLAUDE_CONFIG_DIR>/.claude.json`, else `~/.claude.json`. Superba only
    /// looks whether an account is there; it never writes the file.
    pub fn global_state_json(&self) -> PathBuf {
        if self.from_env {
            self.root.join(".claude.json")
        } else {
            self.home_dir.join(".claude.json")
        }
    }
}

/// The `settings.json` path as text for `home` (the user's home directory)
/// and an optional `CLAUDE_CONFIG_DIR`, with the separator of the system it
/// lives on (`windows`). Used for display and for homes of other systems.
pub fn settings_path_text(home: &str, config_dir: Option<&str>, windows: bool) -> String {
    let sep = if windows { '\\' } else { '/' };
    let dir = match config_dir.map(str::trim).filter(|d| !d.is_empty()) {
        Some(d) => d.trim_end_matches(['/', '\\']).to_owned(),
        None => format!("{}{sep}.claude", home.trim_end_matches(['/', '\\'])),
    };
    format!("{dir}{sep}{SETTINGS_FILE}")
}

/// The directory of Claude Code's managed (enterprise) settings on this
/// system, which override user settings: `/etc/claude-code`,
/// `/Library/Application Support/ClaudeCode` (macOS),
/// `C:\Program Files\ClaudeCode` (Windows).
pub fn managed_settings_dir() -> PathBuf {
    PathBuf::from(managed_settings_dir_for(
        cfg!(target_os = "macos"),
        cfg!(windows),
    ))
}

/// [`managed_settings_dir`] for a given system.
pub fn managed_settings_dir_for(macos: bool, windows: bool) -> &'static str {
    if windows {
        r"C:\Program Files\ClaudeCode"
    } else if macos {
        "/Library/Application Support/ClaudeCode"
    } else {
        "/etc/claude-code"
    }
}

/// How the key is sent to an Anthropic endpoint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnthropicAuth {
    /// `Authorization: Bearer <key>` (`ANTHROPIC_AUTH_TOKEN`). Preferred:
    /// Claude Code asks before it uses an `ANTHROPIC_API_KEY`.
    #[default]
    Bearer,
    /// `x-api-key: <key>` (`ANTHROPIC_API_KEY`).
    ApiKey,
}

impl AnthropicAuth {
    /// The environment variable Claude Code reads the key from.
    pub fn env_var(self) -> &'static str {
        match self {
            AnthropicAuth::Bearer => render::ENV_AUTH_TOKEN,
            AnthropicAuth::ApiKey => render::ENV_API_KEY,
        }
    }

    /// Short name (`bearer`, `x-api-key`).
    pub fn as_str(self) -> &'static str {
        match self {
            AnthropicAuth::Bearer => "bearer",
            AnthropicAuth::ApiKey => "x-api-key",
        }
    }

    /// Parse [`AnthropicAuth::as_str`] (also `api-key`, `api_key`, `token`).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "bearer" | "token" | "auth_token" | "auth-token" => Some(AnthropicAuth::Bearer),
            "x-api-key" | "api-key" | "api_key" | "apikey" => Some(AnthropicAuth::ApiKey),
            _ => None,
        }
    }
}

/// A provider's Anthropic Messages endpoint: Claude Code calls
/// `<base_url>/v1/messages` (`ANTHROPIC_BASE_URL`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AnthropicEndpoint {
    /// Base URL without `/v1` (`https://api.deepseek.com/anthropic`).
    pub base_url: Url,
    /// How the key is sent.
    #[serde(default)]
    pub auth: AnthropicAuth,
    /// The preset it was taken from ([`presets::AnthropicPreset::id`]), for
    /// display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
}

impl AnthropicEndpoint {
    /// An endpoint at `base_url` with Bearer auth.
    pub fn new(base_url: Url) -> Self {
        Self {
            base_url,
            auth: AnthropicAuth::Bearer,
            preset: None,
        }
    }

    /// The base URL as Claude Code wants it: no trailing slash.
    pub fn base_text(&self) -> String {
        self.base_url.as_str().trim_end_matches('/').to_owned()
    }

    /// `<base>/<path>` (`v1/messages`, `v1/models`).
    pub fn join(&self, path: &str) -> Url {
        let base = self.base_text();
        let joined = format!("{base}/{}", path.trim_start_matches('/'));
        Url::parse(&joined).unwrap_or_else(|_| self.base_url.clone())
    }

    /// Check a typed base URL: http(s), no query or fragment, no `/v1/messages`
    /// suffix (Claude Code adds it), plain http only on loopback.
    pub fn parse_base(input: &str) -> std::result::Result<Url, &'static str> {
        let t = input.trim();
        if t.is_empty() {
            return Err("claude.endpoint.invalid.empty");
        }
        let url = Url::parse(t).map_err(|_| "claude.endpoint.invalid.url")?;
        match url.scheme() {
            "https" => {}
            "http" => {
                let loopback = matches!(
                    url.host(),
                    Some(url::Host::Ipv4(ip)) if ip.is_loopback()
                ) || matches!(url.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback())
                    || url.host_str() == Some("localhost");
                if !loopback {
                    return Err("claude.endpoint.invalid.http");
                }
            }
            _ => return Err("claude.endpoint.invalid.url"),
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err("claude.endpoint.invalid.query");
        }
        let path = url.path().trim_end_matches('/');
        if path.ends_with("/v1/messages") || path.ends_with("/messages") {
            return Err("claude.endpoint.invalid.messages");
        }
        Ok(url)
    }
}

/// How Claude Code reaches its provider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeRoute {
    /// Straight to the provider's Anthropic endpoint.
    #[default]
    Direct,
    /// Through Superba's local router (its Anthropic ingress).
    Router,
}
