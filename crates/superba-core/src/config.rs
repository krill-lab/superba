//! Types of the configuration engine ([`crate::ConfigEngine`]): resolved profiles,
//! file plans, apply reports, baseline, journal recovery.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::auth::{AuthMode, AuthShape};
use crate::env::{EnvSnapshot, RcLine};
use crate::error::{Error, Result};
use crate::i18n::Notice;
use crate::ids::{LineId, ProfileId, SiteId};
use crate::origin::{CodexPath, Origin, chatgpt_base_url};
use crate::profile::ModelDefaults;
use crate::secret::{Secret, SecretBytes};
use crate::site::SiteKind;
use crate::time::Timestamp;

/// Concrete endpoints for a token-auth profile on one line (plan 2.3 table).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedEndpoints {
    /// Line origin (Krill) or the compatible site's origin.
    pub origin: Origin,
    /// Value of top-level `openai_base_url`.
    pub openai_base_url: Url,
    /// Value of `chatgpt_base_url` (PAT only; `None` = restore baseline value).
    pub chatgpt_base_url: Option<Url>,
    /// Value for `CODEX_AUTHAPI_BASE_URL` (PAT only).
    pub authapi_origin: Option<Origin>,
    /// The Codex path used.
    pub codex_path: CodexPath,
}

impl ResolvedEndpoints {
    /// Endpoints for a Krill-style line: `openai_base_url = origin + path`;
    /// PAT additionally gets `chatgpt_base_url = origin/backend-api/` and the
    /// authapi origin. PAT requires [`CodexPath::BackendApiCodex`] (`/codex/v1` is
    /// an API-key-only fallback); other combinations return [`Error::Invalid`].
    pub fn for_line(origin: &Origin, path: CodexPath, mode: AuthMode) -> Result<Self> {
        if mode.writes_chatgpt_base_url() && path != CodexPath::BackendApiCodex {
            return Err(Error::invalid(format!(
                "{mode:?} requires the /backend-api/codex path, not {path:?}"
            )));
        }
        if mode.writes_chatgpt_base_url() && !origin.is_https() {
            // Codex 0.162 rejects a plain-http chatgpt_base_url at the first turn.
            return Err(Error::invalid(format!(
                "{mode:?} requires an https line, not {origin}"
            )));
        }
        Ok(Self {
            origin: origin.clone(),
            openai_base_url: path.base_url(origin),
            chatgpt_base_url: mode
                .writes_chatgpt_base_url()
                .then(|| chatgpt_base_url(origin)),
            authapi_origin: mode.needs_authapi_env().then(|| origin.clone()),
            codex_path: path,
        })
    }

    /// Endpoints for a compatible site with a full base URL (API key only).
    pub fn for_base_url(base: &Url) -> Result<Self> {
        let origin = Origin::parse(&base.origin().ascii_serialization())?;
        Ok(Self {
            origin,
            openai_base_url: base.clone(),
            chatgpt_base_url: None,
            authapi_origin: None,
            codex_path: CodexPath::AsIs,
        })
    }
}

/// How the provider key is handled when restoring the original configuration
/// whose provider is not `openai` (plan 10.4). `Baseline` is the only case in which
/// Superba writes a `model_provider` other than `"openai"` (the user's own value).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderRestore {
    /// Option 1 (recommended): restore line and auth only, keep `model_provider="openai"`
    /// so every session stays visible.
    KeepOpenai,
    /// Options 2/3: restore `model_provider` byte-for-byte; `reverse_migrate`
    /// selects option 2 (move sessions back) vs option 3 (sessions get hidden).
    Baseline {
        /// Reverse-migrate sessions to the baseline provider.
        reverse_migrate: bool,
    },
}

/// What the engine should make the Codex home look like.
// Built once per plan; the size difference between variants does not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ResolvedMode {
    /// Krill or compatible site with token auth (PAT / API key / future token modes).
    Token {
        /// Site kind (selects owned keys).
        site_kind: SiteKind,
        /// Auth mode (one with `uses_key()`).
        auth: AuthMode,
        /// Endpoints.
        endpoints: ResolvedEndpoints,
        /// The key, resolved from the profile's `KeyRef` just before planning.
        key: Secret,
    },
    /// Official ChatGPT: remove owned keys, restore the stashed login (if any).
    Official,
    /// Restore the baseline captured at first takeover.
    Original {
        /// Provider handling (plan 10.4).
        provider: ProviderRestore,
    },
    /// Restore the owned keys and auth.json as they were right after generation
    /// `generation` was applied (point-in-time restore from `backups/switch`).
    Generation {
        /// Generation number (see [`GenerationRecord`]).
        generation: u64,
    },
}

/// How to treat an owned key the user edited after Superba last wrote it
/// (plan 9.3: drift is shown, never silently reverted).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyResolution {
    /// Keep the user's current value.
    KeepCurrent,
    /// Overwrite it with the planned / baseline value.
    Overwrite,
}

/// A profile with everything resolved (line, endpoints, key): the input of
/// [`crate::ConfigEngine::plan`] and [`crate::ConfigEngine::render`].
#[derive(Clone, Debug)]
pub struct ResolvedProfile {
    /// Source profile (`None` for ad-hoc switches such as restore).
    pub profile_id: Option<ProfileId>,
    /// Site.
    pub site_id: Option<SiteId>,
    /// Line.
    pub line_id: Option<LineId>,
    /// Mode.
    pub mode: ResolvedMode,
    /// Model keys to manage.
    pub model: ModelDefaults,
    /// `model_catalog_json` to write: the preflighted, content-addressed catalog
    /// file of this home (`None` = no catalog file; the key is released).
    pub model_catalog: Option<std::path::PathBuf>,
    /// Codex settings to write as owned keys (token modes only), already filtered
    /// for this home by `superba-config`'s settings engine
    /// ([`crate::SettingsPlan::apply`]). Krill's recommended keys are ordinary
    /// settings of the profile ([`crate::CodexSettings::krill_recommended`]).
    pub settings: crate::settings::CodexSettings,
    /// Answers for drifted owned keys (dotted path -> resolution). Keys in drift
    /// without an answer make planning return a [`crate::PlanChoice::OwnedKeyConflict`].
    pub key_resolutions: BTreeMap<String, KeyResolution>,
}

impl ResolvedProfile {
    /// Auth mode this profile results in, when known up front.
    pub fn auth_mode(&self) -> Option<AuthMode> {
        match &self.mode {
            ResolvedMode::Token { auth, .. } => Some(*auth),
            ResolvedMode::Official => Some(AuthMode::OfficialChatGpt),
            ResolvedMode::Original { .. } | ResolvedMode::Generation { .. } => None,
        }
    }
}

/// Which Codex file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexFileKind {
    /// `config.toml`.
    ConfigToml,
    /// `auth.json`.
    AuthJson,
}

/// Current bytes of the two Codex files (`None` = absent). Input of the pure render.
/// Both are [`SecretBytes`]: `config.toml` can hold MCP server tokens.
#[derive(Clone, Debug, Default)]
pub struct CodexFiles {
    /// `config.toml`.
    pub config_toml: Option<SecretBytes>,
    /// `auth.json`.
    pub auth_json: Option<SecretBytes>,
}

/// Planned change of one file.
///
/// Serialize-only on purpose: the new contents are never serialized, so a
/// deserialized change could not be executed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileChange {
    /// Leave as is.
    #[default]
    Unchanged,
    /// Write these bytes (contents are never serialized).
    Write {
        /// sha256 hex of the new contents.
        sha256: String,
        /// New contents.
        #[serde(skip)]
        contents: SecretBytes,
    },
    /// Delete the file (e.g. auth.json when restoring "no login").
    Remove,
}

/// One owned-key change (values rendered as TOML, e.g. `"openai"` or `true`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyChange {
    /// Dotted key path (`openai_base_url`, `features.fast_mode`).
    pub path: String,
    /// Value before (`None` = absent).
    pub before: Option<String>,
    /// Value after (`None` = removed).
    pub after: Option<String>,
}

/// auth.json transition (shapes only, never secrets).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthChange {
    /// Before.
    pub before: AuthShape,
    /// After.
    pub after: AuthShape,
    /// The current ChatGPT login is stashed (encrypted) before being replaced.
    pub stash_login: bool,
}

/// Input of the pure [`crate::ConfigEngine::render`].
#[derive(Clone, Copy, Debug)]
pub struct RenderInput<'a> {
    /// Current files.
    pub files: &'a CodexFiles,
    /// Desired state.
    pub desired: &'a ResolvedProfile,
    /// Baseline (needed to restore keys / the original config).
    pub baseline: Option<&'a Baseline>,
    /// Decrypted stashed ChatGPT login (needed for [`ResolvedMode::Official`]).
    pub login_stash: Option<&'a SecretBytes>,
    /// Pre-images from before the first takeover (decrypted first-write backups).
    /// Lets a full restore reproduce the original files byte for byte when nothing
    /// but owned keys changed since (plan 10.4 option 3).
    pub original_files: Option<&'a CodexFiles>,
    /// Owned key values Superba wrote last time (drift detection, plan 9.3).
    pub last_written: Option<&'a [KeySnapshot]>,
}

/// Output of the pure render.
#[derive(Clone, Debug, Default)]
pub struct RenderedFiles {
    /// `config.toml` change.
    pub config_toml: FileChange,
    /// `auth.json` change.
    pub auth_json: FileChange,
    /// Owned keys that change.
    pub key_changes: Vec<KeyChange>,
    /// auth.json transition, if it changes.
    pub auth_change: Option<AuthChange>,
    /// Non-fatal observations (legacy provider table kept, unknown keys ...).
    pub notes: Vec<Notice>,
    /// Owned keys after this render with the values it leaves (the next
    /// `last_written`): every key the desired state sets or removes, unchanged
    /// ones included. Released keys are not in it.
    pub owned: Vec<KeySnapshot>,
    /// Keys this render takes over that were not owned before, with the value
    /// they had just before (`None` = absent). A caller keeping its own baseline
    /// records them, so releasing such a key later restores the user's value.
    pub newly_owned: Vec<KeySnapshot>,
}

/// One file in an [`ApplyPlan`] (serialize-only, see [`FileChange`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PlannedFile {
    /// Which file.
    pub kind: CodexFileKind,
    /// Absolute path (POSIX path string for remote targets).
    pub path: PathBuf,
    /// sha256 hex at plan time (`None` = absent). Re-checked before rename.
    pub before_sha256: Option<String>,
    /// Change.
    pub change: FileChange,
}

/// A computed, not yet applied configuration change for one Codex home.
/// Planning never writes; `apply` re-reads the files and re-renders if they changed.
///
/// Executable plans are serialize-only (previews, `--json`); they carry contents
/// and the key in fields that are skipped, so they must never be deserialized and
/// executed. Journals persist their own records.
#[derive(Clone, Debug, Serialize)]
pub struct ApplyPlan {
    /// Unique id (also the journal id).
    pub id: String,
    /// Codex home root.
    pub codex_home: PathBuf,
    /// Generation number after apply.
    pub generation: u64,
    /// Profile being applied.
    pub profile_id: Option<ProfileId>,
    /// Site being applied (recorded in the journal and `state.json`).
    pub site_id: Option<SiteId>,
    /// Line being applied.
    pub line_id: Option<LineId>,
    /// Auth mode being applied (`None` for restores whose mode is not known up front).
    pub auth_mode: Option<AuthMode>,
    /// Files.
    pub files: Vec<PlannedFile>,
    /// Owned key changes (diff preview).
    pub key_changes: Vec<KeyChange>,
    /// auth.json transition.
    pub auth_change: Option<AuthChange>,
    /// Notes for the preview.
    #[serde(default)]
    pub notes: Vec<Notice>,
    /// The desired state, kept so `apply` can re-render (contains the key; never
    /// serialized).
    #[serde(skip)]
    pub desired: Option<ResolvedProfile>,
}

impl ApplyPlan {
    /// True if nothing would be written.
    pub fn is_noop(&self) -> bool {
        self.files
            .iter()
            .all(|f| matches!(f.change, FileChange::Unchanged))
    }
}

/// One file written by `apply`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileWrite {
    /// Which file.
    pub kind: CodexFileKind,
    /// Path.
    pub path: PathBuf,
    /// Hash before (`None` = did not exist).
    pub before_sha256: Option<String>,
    /// Hash after (`None` = removed).
    pub after_sha256: Option<String>,
    /// Pre-image backup (for rollback).
    #[serde(default)]
    pub backup: Option<PathBuf>,
}

/// Result of a successful `apply`; the input of `rollback` and of hash verification.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyReport {
    /// Plan / journal id.
    pub journal_id: String,
    /// Codex home root.
    pub codex_home: PathBuf,
    /// New generation.
    pub generation: u64,
    /// When.
    pub applied_at: Timestamp,
    /// Files written.
    pub files: Vec<FileWrite>,
}

/// Superba's record of what is applied to a Codex home.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveState {
    /// Applied profile.
    pub profile_id: Option<ProfileId>,
    /// Site.
    pub site_id: Option<SiteId>,
    /// Line.
    pub line_id: Option<LineId>,
    /// Auth mode.
    pub auth_mode: Option<AuthMode>,
    /// Generation (incremented by every successful apply).
    pub generation: u64,
    /// When applied.
    pub applied_at: Timestamp,
}

/// History entry: which profile/site was active from `applied_at` on (used to
/// attribute sessions to generations for reverse migration, plan 10.4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationRecord {
    /// Generation.
    pub generation: u64,
    /// Profile.
    pub profile_id: Option<ProfileId>,
    /// Site kind, if known.
    pub site_kind: Option<SiteKind>,
    /// When it became active.
    pub applied_at: Timestamp,
}

/// Observed state of a Codex home ([`crate::ConfigEngine::read_state`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigState {
    /// Codex home root.
    pub codex_home: PathBuf,
    /// sha256 of config.toml (`None` = absent).
    pub config_sha256: Option<String>,
    /// sha256 of auth.json (`None` = absent).
    pub auth_sha256: Option<String>,
    /// config.toml parse error, if any (writers refuse to touch the file).
    #[serde(default)]
    pub parse_error: Option<String>,
    /// `model_provider` (`None` = absent, Codex default `openai`).
    pub model_provider: Option<String>,
    /// `[model_providers.<model_provider>]` table exists.
    pub provider_table_present: bool,
    /// `openai_base_url`.
    pub openai_base_url: Option<String>,
    /// `chatgpt_base_url`.
    pub chatgpt_base_url: Option<String>,
    /// `model`.
    pub model: Option<String>,
    /// `model_reasoning_effort`.
    pub reasoning_effort: Option<String>,
    /// Top-level `profile = ...` override (diagnostic).
    pub profile_override: Option<String>,
    /// `cli_auth_credentials_store` (diagnostic).
    pub cli_auth_credentials_store: Option<String>,
    /// auth.json shape.
    pub auth: AuthShape,
    /// What Superba last applied here, if anything.
    pub active: Option<ActiveState>,
    /// Owned keys whose live value differs from what Superba last wrote (user edits;
    /// shown, never silently reverted).
    #[serde(default)]
    pub drift: Vec<KeyChange>,
    /// A journal entry is pending (crash during apply); `recover` must run first.
    pub journal_pending: bool,
}

/// Original value of one owned key before the first takeover.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeySnapshot {
    /// Dotted key path.
    pub path: String,
    /// TOML-rendered value (`None` = absent).
    pub value: Option<String>,
}

/// Pre-takeover state of a Codex home (captured once, extended lazily when a key is
/// first touched). Restore uses it key by key (plan 9.3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Baseline {
    /// When captured.
    pub captured_at: Timestamp,
    /// Codex home root.
    pub codex_home: PathBuf,
    /// sha256 of the original config.toml (`None` = absent).
    pub config_sha256: Option<String>,
    /// sha256 of the original auth.json (`None` = absent).
    pub auth_sha256: Option<String>,
    /// Original owned keys.
    pub keys: Vec<KeySnapshot>,
    /// Original `model_provider` (e.g. `krill`); `None` = absent (= openai).
    pub provider_key: Option<String>,
    /// `[model_providers.<provider_key>]` existed.
    pub provider_table_present: bool,
    /// Original auth shape.
    pub auth: AuthShape,
    /// A ChatGPT login was stashed at takeover.
    pub login_stashed: bool,
    /// Session counts per provider at takeover.
    #[serde(default)]
    pub session_distribution: BTreeMap<String, u32>,
    /// Persisted env values at takeover.
    #[serde(default)]
    pub env: Vec<EnvSnapshot>,
    /// Legacy tutorial rc lines that were commented out.
    #[serde(default)]
    pub legacy_env_lines: Vec<RcLine>,
    /// Pre-images of files before Superba's first write.
    #[serde(default)]
    pub first_write_backups: Vec<PathBuf>,
    /// Superba owns nothing in this home any more: a restore to the original or the
    /// official configuration completed. The baseline still describes the state
    /// before the last takeover (so "restore original" stays available), but the
    /// next takeover starts a new one: planners capture fresh extras (env, legacy
    /// rc lines, sessions) and the config engine archives this baseline and its
    /// first-write backups and captures new ones.
    #[serde(default)]
    pub released: bool,
}

/// Parts of the baseline gathered by other components and handed to
/// [`crate::ConfigEngine::snapshot_baseline`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineExtras {
    /// From [`crate::SessionMigrator::scan`].
    pub session_distribution: BTreeMap<String, u32>,
    /// From [`crate::EnvPersister::inspect`].
    pub env: Vec<EnvSnapshot>,
    /// Legacy lines that the takeover will comment out.
    pub legacy_env_lines: Vec<RcLine>,
}

/// What recovery did with one interrupted journal entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalAction {
    /// Nothing had been written; entry dropped.
    Discarded,
    /// Partially written; finished forward to the new state.
    Completed,
    /// Partially written; rolled back to the old state.
    RolledBack,
    /// Files match neither state (someone else wrote); left alone and reported.
    Drift,
}

/// One recovered journal entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveredJournal {
    /// Journal id.
    pub journal_id: String,
    /// Action taken.
    pub action: JournalAction,
    /// Files involved.
    pub files: Vec<PathBuf>,
}

/// Result of [`crate::ConfigEngine::recover`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryReport {
    /// Entries handled.
    pub entries: Vec<RecoveredJournal>,
    /// Start-up repairs to tell the user about (a model catalog file a changed
    /// Codex no longer accepts was rebuilt or dropped).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<Notice>,
}

/// Metadata of the stashed ChatGPT login (for the "may need to sign in again"
/// warning, plan 9.4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginStashInfo {
    /// When stashed.
    pub stashed_at: Timestamp,
    /// `last_refresh` from the login.
    #[serde(default)]
    pub last_refresh: Option<Timestamp>,
    /// id_token `exp`.
    #[serde(default)]
    pub id_token_exp: Option<Timestamp>,
    /// Account email.
    #[serde(default)]
    pub email: Option<String>,
}

/// Where a stashed ChatGPT login belongs: a local Codex home or a remote host (the
/// remote login is fetched and stashed locally, encrypted, plan 8.4).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum StashScope {
    /// A local Codex home (root path).
    Local(PathBuf),
    /// A remote target.
    Remote(crate::ids::TargetId),
}

/// The token credentials currently in a Codex home (for post-switch self-tests of
/// restored configurations). Never serialized.
#[derive(Clone, Debug)]
pub struct ActiveCredentials {
    /// Mode found in auth.json.
    pub auth_mode: AuthMode,
    /// The key.
    pub key: Secret,
    /// `openai_base_url` from config.toml.
    pub openai_base_url: Option<Url>,
    /// `chatgpt_base_url` from config.toml.
    pub chatgpt_base_url: Option<Url>,
}

/// Check that a plan targets the expected home (helper for implementations).
pub fn ensure_same_home(plan: &ApplyPlan, home: &crate::paths::CodexHome) -> Result<()> {
    if plan.codex_home != home.root() {
        return Err(Error::invalid(format!(
            "plan {} is for {}, not {}",
            plan.id,
            plan.codex_home.display(),
            home.root().display()
        )));
    }
    Ok(())
}
