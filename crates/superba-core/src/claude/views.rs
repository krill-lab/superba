//! What the service returns for Claude Code ([`crate::SwitchService`]'s
//! `claude_*` methods): the page state, model lists, apply / restore plans,
//! diagnostics and the endpoint check.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use url::Url;

use super::{AnthropicEndpoint, ClaudeCapability, ClaudeConfig};
use crate::i18n::Notice;
use crate::ids::ProfileId;
use crate::models::{ModelListKind, ModelSourceStatus};
use crate::plan::DoctorFinding;
use crate::time::Timestamp;

/// The `claude` program found on this computer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeInstall {
    /// Its path.
    pub binary: PathBuf,
    /// `claude --version` (`2.1.295`), when it answered.
    #[serde(default)]
    pub version: Option<String>,
}

/// The shared configuration file status.
pub use crate::harness::HarnessFileStatus as ClaudeFileStatus;

/// What Superba applied last (no values: the file holds a key).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeApplied {
    /// Counter of applies.
    pub generation: u64,
    /// When.
    pub at: Timestamp,
    /// The provider applied.
    #[serde(default)]
    pub provider: Option<ProfileId>,
    /// The paths Superba wrote a value to (it also owns the keys it
    /// removed).
    #[serde(default)]
    pub owned: Vec<String>,
    /// Owned paths someone changed since (they stay that person's until the
    /// next apply takes them over, which it asks first).
    #[serde(default)]
    pub drift: Vec<String>,
}

/// The state of both Claude Code pages.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClaudeView {
    /// Claude Code's configuration directory.
    pub config_dir: PathBuf,
    /// It came from `CLAUDE_CONFIG_DIR`.
    #[serde(default)]
    pub config_dir_from_env: bool,
    /// `settings.json`.
    pub settings_path: PathBuf,
    /// Its state.
    pub file: ClaudeFileStatus,
    /// The `claude` program, when found.
    #[serde(default)]
    pub install: Option<ClaudeInstall>,
    /// Superba's configuration for Claude Code.
    pub config: ClaudeConfig,
    /// The provider Claude Code uses (the configured one, else Codex's
    /// applied key-based provider).
    #[serde(default)]
    pub provider: Option<ProfileId>,
    /// [`Self::provider`] follows Codex.
    #[serde(default)]
    pub follows_codex: bool,
    /// The provider's Anthropic endpoint (its own, else the suggested
    /// preset's).
    #[serde(default)]
    pub endpoint: Option<AnthropicEndpoint>,
    /// The endpoint is the suggested preset, not one stored with the
    /// provider.
    #[serde(default)]
    pub endpoint_suggested: bool,
    /// The provider has a stored key.
    #[serde(default)]
    pub key_available: bool,
    /// What Superba applied last.
    #[serde(default)]
    pub applied: Option<ClaudeApplied>,
    /// Owned paths whose value in the file differs from what the
    /// configuration wants now ("not applied yet").
    #[serde(default)]
    pub pending: Vec<String>,
    /// Claude Code's base URL through the local router (`None` = the router
    /// does not serve Claude Code yet).
    #[serde(default)]
    pub router_base_url: Option<Url>,
    /// Remarks.
    #[serde(default)]
    pub notes: Vec<Notice>,
}

/// Which provider's models to list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeModelsRequest {
    /// The provider (`None` = the one Claude Code uses).
    #[serde(default)]
    pub provider: Option<ProfileId>,
    /// Ask the provider again instead of the cache.
    #[serde(default)]
    pub refresh: bool,
}

/// One model Claude Code can use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeModelRow {
    /// Model id.
    pub id: String,
    /// Display name from a list.
    #[serde(default)]
    pub display_name: Option<String>,
    /// The lists it came from (empty = added by hand).
    #[serde(default)]
    pub lists: Vec<ModelListKind>,
    /// Added by hand.
    #[serde(default)]
    pub manual: bool,
    /// The Anthropic endpoint can serve it: `false` for an id only the
    /// Codex list names when the endpoint does not serve that list ("Codex
    /// only"; not offered for slots).
    #[serde(default = "yes")]
    pub usable: bool,
    /// Context window from the Codex metadata.
    #[serde(default)]
    pub context_window: Option<u64>,
    /// Reasoning levels from the Codex metadata.
    #[serde(default)]
    pub reasoning_levels: Vec<String>,
    /// The suggested capabilities.
    #[serde(default)]
    pub suggested: BTreeSet<ClaudeCapability>,
}

fn yes() -> bool {
    true
}

/// The models of a provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeModelsView {
    /// The provider.
    pub provider: ProfileId,
    /// Rows: the lists merged by id (Codex metadata wins), then the ids added
    /// by hand.
    pub rows: Vec<ClaudeModelRow>,
    /// Each list asked, with its result.
    #[serde(default)]
    pub sources: Vec<ModelSourceStatus>,
    /// When.
    pub at: Timestamp,
}

/// Claude Code uses the same plan, changes and report as every harness.
pub use crate::harness::{
    HarnessApplyReport as ClaudeApplyReport, HarnessChange as ClaudeChange,
    HarnessPlan as ClaudePlan, HarnessPlanKind as ClaudePlanKind,
};

/// One environment variable outside `settings.json` that changes what
/// Claude Code uses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeEnvConflict {
    /// The variable.
    pub name: String,
    /// Where it is set: `process` (Superba's own environment, inherited from
    /// the shell) or a shell file's path.
    pub source: String,
}

/// Diagnostics of Claude Code (the Claude Code section of Diagnostics).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClaudeDiagnostics {
    /// `claude` found.
    #[serde(default)]
    pub install: Option<ClaudeInstall>,
    /// `settings.json`.
    pub settings_path: PathBuf,
    /// Its state.
    pub file: ClaudeFileStatus,
    /// Managed settings files and the keys there that override Superba's.
    #[serde(default)]
    pub managed: Vec<(PathBuf, Vec<String>)>,
    /// Variables outside `settings.json` that conflict.
    #[serde(default)]
    pub env_conflicts: Vec<ClaudeEnvConflict>,
    /// A claude.ai account is logged in (`oauthAccount` / credentials).
    #[serde(default)]
    pub official_login: bool,
    /// `apiKeyHelper` is set (its key wins over Superba's).
    #[serde(default)]
    pub api_key_helper: bool,
    /// The checks, ready to show.
    #[serde(default)]
    pub findings: Vec<DoctorFinding>,
}

/// One request of the endpoint check: a 1-token `/v1/messages`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeProbe {
    /// Model asked.
    pub model: String,
    /// URL (no key).
    pub url: String,
    /// HTTP status (`None` = no answer).
    #[serde(default)]
    pub status: Option<u16>,
    /// Time to the answer.
    pub latency_ms: u64,
    /// The endpoint answered with a message.
    pub ok: bool,
    /// The error, when not ok.
    #[serde(default)]
    pub message: Option<String>,
}
