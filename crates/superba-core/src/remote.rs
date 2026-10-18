//! Remote host types ([`crate::RemoteHosts`]).

use serde::{Deserialize, Serialize};

use crate::auth::AuthShape;
use crate::codex::CodexVersion;
use crate::config::{AuthChange, FileWrite, KeyChange, PlannedFile, ResolvedProfile};
use crate::env::{EnvApplied, EnvObservation, EnvPlan, ShellKind};
use crate::error::Error;
use crate::i18n::{LocalizedText, Notice};
use crate::ids::{ProfileId, TargetId};
use crate::plan::BlockReason;
use crate::profile::HostSettings;
use crate::secret::SecretBytes;
use crate::settings::SkipReason;
use crate::target::RemoteHost;
use crate::time::Timestamp;
use crate::versions::CodexInstallKind;

/// Remote operating system (`uname -s`). Non-POSIX hosts are skipped.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteOs {
    /// Linux.
    Linux,
    /// macOS.
    Darwin,
    /// Anything else (skipped with an explanation).
    Other(String),
}

/// Probe result of a remote host (one ssh round trip running a POSIX script).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteState {
    /// Target.
    pub target_id: TargetId,
    /// When probed.
    pub probed_at: Timestamp,
    /// OS.
    pub os: RemoteOs,
    /// Login shell.
    pub shell: ShellKind,
    /// Remote `CODEX_HOME` (POSIX path).
    pub codex_home: String,
    /// Codex binary used by the app-server, if found.
    #[serde(default)]
    pub codex_path: Option<String>,
    /// Its version (`None` = unknown: needs user confirmation for PAT).
    #[serde(default)]
    pub codex_version: Option<CodexVersion>,
    /// sha256 of config.toml.
    #[serde(default)]
    pub config_sha256: Option<String>,
    /// sha256 of auth.json.
    #[serde(default)]
    pub auth_sha256: Option<String>,
    /// auth.json shape.
    pub auth: AuthShape,
    /// `model_provider`.
    #[serde(default)]
    pub model_provider: Option<String>,
    /// `openai_base_url`.
    #[serde(default)]
    pub openai_base_url: Option<String>,
    /// `/etc/machine-id` (or `hostid`).
    #[serde(default)]
    pub machine_id: Option<String>,
    /// `machine_id` recorded in the remote `remote-state.json` marker, if present.
    #[serde(default)]
    pub marker_machine_id: Option<String>,
    /// `CODEX_AUTHAPI_BASE_URL` as seen by a non-interactive ssh shell.
    #[serde(default)]
    pub authapi_env: EnvObservation,
    /// pids of running `codex app-server --listen` processes (to recognise the new
    /// one after a restart without comparing clocks across machines).
    #[serde(default)]
    pub app_server_pids: Vec<u32>,
    /// Owned keys Superba wrote on this host last time (dotted paths, from the
    /// local per-host state; empty before the first write). The settings engine
    /// never counts a value there as the user's.
    #[serde(default)]
    pub owned_paths: Vec<String>,
    /// The `config.toml` bytes the probe read (`None` = absent). In memory only:
    /// the file may hold MCP tokens, so it is never serialized.
    #[serde(skip)]
    pub config_toml: Option<SecretBytes>,
    /// Machine architecture (`uname -m`: `x86_64`, `aarch64`, `arm64`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    /// Kernel release (`uname -r`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel: Option<String>,
    /// Free space under `$HOME` in KiB (`df -Pk "$HOME"`); installs need ~600 MB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_free_kib: Option<u64>,
    /// How the `codex` the app would start is installed (from its resolved path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_install: Option<CodexInstallKind>,
    /// `readlink -f` of that `codex`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_resolved_path: Option<String>,
    /// WSL: the distribution's IPv4 default gateway (`/proc/net/route`); behind
    /// WSL 2 NAT this is the Windows host's address on the WSL adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
    /// WSL: `/usr/bin/bash` exists, so the desktop app can run its WSL
    /// app-server there (`None` = not checked: SSH hosts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_bash: Option<bool>,
}

impl RemoteState {
    /// True if the home is likely shared with another machine (NFS): the marker was
    /// written by a different machine id (plan 8.6).
    pub fn shared_home_suspected(&self) -> bool {
        matches!((&self.machine_id, &self.marker_machine_id), (Some(a), Some(b)) if a != b)
    }
}

/// A computed change for one remote host (serialize-only, like
/// [`crate::ApplyPlan`]: contents and key are skipped).
#[derive(Clone, Debug, Serialize)]
pub struct RemotePlan {
    /// Target.
    pub target_id: TargetId,
    /// State the plan was computed from.
    pub state: RemoteState,
    /// File changes (paths are remote POSIX paths).
    pub files: Vec<PlannedFile>,
    /// Owned key changes.
    pub key_changes: Vec<KeyChange>,
    /// auth.json transition.
    #[serde(default)]
    pub auth_change: Option<AuthChange>,
    /// Remote env change (PAT only).
    #[serde(default)]
    pub env: Option<EnvPlan>,
    /// Reasons this host cannot be switched (others continue).
    #[serde(default)]
    pub blocked: Vec<BlockReason>,
    /// Warnings (unknown version, shared home, pkill risk ...).
    #[serde(default)]
    pub warnings: Vec<Notice>,
    /// Key families the plan writes ([`RemoteApplyScope::Both`] for a 0.2-style
    /// plan); [`crate::RemoteHosts::apply`] re-renders with the same scope.
    #[serde(default)]
    pub scope: RemoteApplyScope,
    /// Desired state, kept for re-rendering at apply time (never serialized).
    #[serde(skip)]
    pub desired: Option<ResolvedProfile>,
}

/// Result of applying a [`RemotePlan`]; enough to roll the host back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteApplyReport {
    /// Target.
    pub target_id: TargetId,
    /// When.
    pub applied_at: Timestamp,
    /// Files written (remote paths; backups are remote paths too).
    pub files: Vec<FileWrite>,
    /// Env edits performed.
    #[serde(default)]
    pub env: Option<EnvApplied>,
}

/// What [`crate::RemoteHosts::verify`] checks against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteVerifyRequest {
    /// What was written.
    pub report: RemoteApplyReport,
    /// app-server pids before the restart ([`RemoteState::app_server_pids`]).
    pub previous_app_server_pids: Vec<u32>,
    /// Expected `CODEX_AUTHAPI_BASE_URL` of the new app-server (PAT), if any.
    pub expected_authapi: Option<String>,
    /// How long to wait for the new app-server (local elapsed time only).
    pub timeout_ms: u64,
}

/// Post-restart verification of a remote host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteVerification {
    /// An app-server started after T0 is running.
    pub new_app_server: bool,
    /// `CODEX_AUTHAPI_BASE_URL` in the new app-server's environment.
    pub authapi_env: EnvObservation,
    /// Config hashes match what was written.
    pub files_match: bool,
    /// Short diagnostic.
    #[serde(default)]
    pub detail: Option<String>,
}

/// Which key families a remote apply writes (v0.3: a switch changes a following
/// host's connection only; a sync pushes settings only).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RemoteApplyScope {
    /// Routing and auth keys (and the PAT env); the host's own settings stay.
    Connection,
    /// Codex settings only; routing and auth stay.
    Settings,
    /// Both (the 0.2 behaviour).
    #[default]
    Both,
}

/// Result of installing or rolling back Codex on a remote host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteInstallReport {
    /// Host.
    pub target_id: TargetId,
    /// Version now active.
    pub version: CodexVersion,
    /// `~/.local/bin/codex` (or `$CODEX_INSTALL_DIR/codex`) on the host.
    pub executable: String,
    /// The version it replaced (kept for rollback).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<CodexVersion>,
    /// When.
    pub at: Timestamp,
}

/// One remote host on the Remote hosts page (hosts the app knows only).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostView {
    /// Host.
    pub host: RemoteHost,
    /// Last probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<RemoteState>,
    /// Why the last probe failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_error: Option<Error>,
    /// Follow toggle and last sync.
    #[serde(default)]
    pub settings: HostSettings,
    /// Rows a sync would change now, when known (sidebar badge).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_sync: Option<u32>,
    /// The running app can hot-restart this host's app-server (`hostId` known).
    #[serde(default)]
    pub hot_restart: bool,
}

/// What a sync does with one key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
#[non_exhaustive]
pub enum HostSyncAction {
    /// Write the local value.
    Write,
    /// Not written (the host's Codex does not know the key, ...).
    Skip {
        /// Why.
        reason: SkipReason,
    },
    /// Remove a key Superba wrote there earlier that the local profile no longer
    /// sets.
    Remove,
    /// The host's value changed since the last sync (unticked by default).
    Conflict,
}

impl HostSyncAction {
    /// Display text.
    pub fn text(&self) -> LocalizedText {
        match self {
            HostSyncAction::Write => LocalizedText::new("host_sync.write", "Write"),
            HostSyncAction::Skip { reason } => {
                LocalizedText::new("host_sync.skip", "Skip: {reason}")
                    .arg_text("reason", reason.text())
            }
            HostSyncAction::Remove => LocalizedText::new("host_sync.remove", "Remove"),
            HostSyncAction::Conflict => LocalizedText::new(
                "host_sync.conflict",
                "Changed on the host since the last sync",
            ),
        }
    }
}

/// One row of a host sync preview.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSyncRow {
    /// Dotted path.
    pub path: String,
    /// Local value (TOML representation), `None` = not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<String>,
    /// Host value (TOML representation), `None` = not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    /// Action.
    pub action: HostSyncAction,
    /// Ticked by default (writes and removes; conflicts and skips are not).
    #[serde(default)]
    pub selected: bool,
}

/// Preview of syncing the local settings to one host
/// ([`crate::SwitchService::host_sync_plan`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSyncPlan {
    /// Host.
    pub target_id: TargetId,
    /// Local profile whose settings are synced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<ProfileId>,
    /// The host's Codex version the rows were filtered with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_version: Option<CodexVersion>,
    /// Rows, by path.
    #[serde(default)]
    pub rows: Vec<HostSyncRow>,
    /// Why the host cannot be synced now.
    #[serde(default)]
    pub blocked: Vec<BlockReason>,
    /// Notes (unknown version, no hot restart: reconnect in the app).
    #[serde(default)]
    pub notes: Vec<Notice>,
}

/// Which rows of a [`HostSyncPlan`] to apply.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HostSyncSelection {
    /// Paths to apply; `None` = the rows selected by default.
    #[serde(default)]
    pub paths: Option<Vec<String>>,
}

/// Result of a host sync.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSyncReport {
    /// Host.
    pub target_id: TargetId,
    /// When.
    pub at: Timestamp,
    /// Paths written.
    #[serde(default)]
    pub written: Vec<String>,
    /// Paths removed.
    #[serde(default)]
    pub removed: Vec<String>,
    /// The host's app-server was hot-restarted.
    #[serde(default)]
    pub restarted: bool,
    /// Notes.
    #[serde(default)]
    pub notes: Vec<Notice>,
}
