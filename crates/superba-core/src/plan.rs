//! Switch planning and execution types shared by `superba-switch` (producer) and
//! the GUI / `spb` (consumers): restart decisions (plan 7.1), the plan, the
//! execution phases and events (plan 7.2), decisions asked from the user, the
//! outcome, and the overview snapshot.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::app::{
    AppInstall, AppSession, CodexBinary, CodexProcessKind, ProcessInfo, ThreadActivity, ThreadRef,
    TurnActivity,
};
use crate::auth::AuthMode;
use crate::caps::{AccountSummary, KeyAccess, ModelCheck};
use crate::codex::{CodexVersion, VersionCapability};
use crate::config::{
    ActiveState, ApplyPlan, ConfigState, KeyChange, KeyResolution, ProviderRestore,
};
use crate::env::{EnvPlan, RcLine};
use crate::error::Error;
use crate::i18n::{LocalizedText, Notice, Severity};
use crate::ids::{LineId, ProfileId, SiteId, TargetId};
use crate::remote::{RemotePlan, RemoteState};
use crate::secret::{KeyRef, Secret};
use crate::sessions::{MigrationPlan, MigrationRequest};
use crate::target::RemoteHost;
use crate::time::Timestamp;

// ---------------------------------------------------------------------------
// Restart decision (plan 7.1)
// ---------------------------------------------------------------------------

/// How a consumer picks up a switch. Computed at plan time, first matching rule of
/// the plan 7.1 table wins (owned by `superba-switch`):
///
/// 1. version gate fails or target line unusable -> [`RestartKind::Blocked`]
/// 2. session migration needed -> `FullAppRestart(SessionMigration)`
/// 3. auth family changes -> `FullAppRestart(CrossAuthFamily)` (until S18)
/// 4. app not running -> [`RestartKind::None`]
/// 5. no CDP / no bridge -> `FullAppRestart(NoCdp | NoBridge)`
/// 6. PAT target, app env lacks `CODEX_AUTHAPI_BASE_URL` -> `FullAppRestart(AuthApiMissing)`
/// 7. PAT target, that origin's whoami unhealthy -> `FullAppRestart(AuthApiUnhealthy)`
///    (+ "hot switch with API key instead" alternative)
/// 8. otherwise -> `AppServerHot { host_id: "local" }`
///
/// CLI TUI/exec -> [`RestartKind::NextLaunch`]; CLI daemon -> [`RestartKind::DaemonRestart`];
/// remote hosts -> `AppServerHot { host_id, kill_remote: true }`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RestartKind {
    /// The switch must not run.
    Blocked {
        /// Reasons.
        reasons: Vec<BlockReason>,
    },
    /// Nothing to restart (consumer not running): write files and offer to open the app.
    None,
    /// Restart the app-server through the app's IPC (~1 s; window and threads stay).
    AppServerHot {
        /// `local` or a remote `hostId`.
        host_id: String,
        /// Remote: pass `killCodexProcess: true`.
        kill_remote: bool,
    },
    /// Quit and relaunch the whole app (explicit confirmation, no countdown).
    FullAppRestart {
        /// Why hot restart is not possible.
        reason: FullRestartReason,
    },
    /// Picks the change up when started next time (CLI processes; never killed).
    NextLaunch,
    /// Restart the CLI daemon (optional, needs confirmation).
    DaemonRestart,
}

impl RestartKind {
    /// Hot app-server restart.
    pub fn is_hot(&self) -> bool {
        matches!(self, RestartKind::AppServerHot { .. })
    }

    /// Blocked.
    pub fn is_blocked(&self) -> bool {
        matches!(self, RestartKind::Blocked { .. })
    }

    /// Needs an explicit confirmation (no auto countdown).
    pub fn needs_confirmation(&self) -> bool {
        matches!(
            self,
            RestartKind::FullAppRestart { .. } | RestartKind::DaemonRestart
        )
    }

    /// Interrupts in-flight turns, MCP servers and agent terminal commands.
    pub fn interrupts_turns(&self) -> bool {
        matches!(
            self,
            RestartKind::AppServerHot { .. }
                | RestartKind::FullAppRestart { .. }
                | RestartKind::DaemonRestart
        )
    }
}

/// Why a full app restart is required.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FullRestartReason {
    /// The app was not launched with a CDP port (not opened through Superba).
    NoCdp,
    /// CDP is up but `electronBridge.sendMessageFromView` is missing.
    NoBridge,
    /// PAT target but the app main process has no `CODEX_AUTHAPI_BASE_URL`.
    AuthApiMissing,
    /// PAT target but whoami on the app's auth API origin is unhealthy.
    AuthApiUnhealthy,
    /// Switching between ChatGPT login and token auth.
    CrossAuthFamily,
    /// Sessions must be migrated (all Codex processes must stop).
    SessionMigration,
    /// This platform build only supports full restarts (S2/S3 fallback).
    PlatformNoHot,
    /// The user asked for it (`SwitchIntent::RestartApp`: reopen the app through
    /// Superba so it gets CDP and the current line's auth address).
    Requested,
    /// The main app must inherit the changed router certificate trust bundle.
    CertificateTrustChanged,
}

impl FullRestartReason {
    /// Display text for the confirmation dialog.
    pub fn text(self) -> LocalizedText {
        let (key, en) = match self {
            Self::NoCdp => (
                "restart.reason.no_cdp",
                "ChatGPT was not opened from Superba, so it cannot be hot-switched.",
            ),
            Self::NoBridge => (
                "restart.reason.no_bridge",
                "This ChatGPT version does not expose the restart bridge.",
            ),
            Self::AuthApiMissing => (
                "restart.reason.authapi_missing",
                "ChatGPT was started without the auth API address; a hot switch would lose the login.",
            ),
            Self::AuthApiUnhealthy => (
                "restart.reason.authapi_unhealthy",
                "The auth address of the line ChatGPT was started with is unavailable; a hot switch would lose the login.",
            ),
            Self::CrossAuthFamily => (
                "restart.reason.cross_auth_family",
                "Switching between the official login and an API key.",
            ),
            Self::SessionMigration => (
                "restart.reason.session_migration",
                "Old sessions must be migrated first, which requires closing every Codex process.",
            ),
            Self::PlatformNoHot => (
                "restart.reason.platform_no_hot",
                "Hot switching is not supported on this platform yet.",
            ),
            Self::CertificateTrustChanged => (
                "restart.reason.certificate_trust_changed",
                "ChatGPT must restart to use the updated local router certificate settings.",
            ),
            Self::Requested => (
                "restart.reason.requested",
                "You asked to reopen ChatGPT through Superba.",
            ),
        };
        LocalizedText::new(key, en)
    }
}

/// Why a switch (or one host of it) cannot run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum BlockReason {
    /// A consumer's Codex is too old for a required capability (plan 9.8).
    VersionTooOld {
        /// Consumer label (`ChatGPT app`, `/usr/local/bin/codex`, host name).
        consumer: String,
        /// Version found.
        found: CodexVersion,
        /// Minimum required.
        required: CodexVersion,
        /// Capability.
        capability: VersionCapability,
    },
    /// The line is unreachable or has no usable Codex path.
    LineUnavailable {
        /// Line.
        line_id: LineId,
    },
    /// The auth mode is not usable on this line (e.g. PAT endpoints edge-blocked).
    AuthModeUnavailable {
        /// Line.
        line_id: LineId,
        /// Mode.
        auth_mode: AuthMode,
    },
    /// The key was rejected on the line.
    KeyRejected {
        /// Line.
        line_id: LineId,
        /// Access result.
        access: KeyAccess,
    },
    /// The profile needs a key and none is stored.
    MissingKey,
    /// An interrupted switch must be recovered first.
    JournalPending,
    /// A file cannot be written.
    FileNotWritable {
        /// Path.
        path: PathBuf,
    },
    /// config.toml / auth.json cannot be parsed; Superba refuses to touch it.
    Unparseable {
        /// Path.
        path: PathBuf,
        /// Parser message.
        message: String,
    },
    /// Unix: running as root through `sudo` (files in the user's home would end
    /// up owned by root). An elevated Windows token of the signed-in user is fine.
    Elevated,
    /// Windows: running as another account than the user signed in at this
    /// computer (SYSTEM, a service account, another user through `runas` or a
    /// scheduled task): Superba's and Codex's files would go to that account's
    /// profile instead of the user's.
    OtherUser {
        /// The account Superba runs as (`NT AUTHORITY\SYSTEM`).
        account: String,
        /// The signed-in user (`PC\alice`), when there is one.
        desktop_user: Option<String>,
    },
    /// Another Superba instance or tool holds a lock.
    Locked {
        /// Lock description.
        what: String,
    },
    /// Restore requested but no baseline exists.
    NoBaseline,
    /// The target cannot be handled (non-POSIX remote, unreachable host ...).
    UnsupportedTarget {
        /// Explanation.
        detail: String,
    },
    /// Router mode is on but the local router is not configured or not answering.
    RouterUnavailable {
        /// Why (no secrets).
        detail: String,
    },
    /// The desktop app runs this computer's Codex inside a WSL distribution that
    /// does not reach Windows on `127.0.0.1` (WSL 2 behind NAT, "Run Codex in
    /// WSL"): router mode would cut the app off (user bug 54 (b)).
    RouterFromWslNat {
        /// The distribution.
        distro: String,
    },
    /// The model catalog file the profile needs (per-model overrides, the
    /// provider's model list, Ultrafast or tier-suffix model names) could not be
    /// built, or a local Codex refused it in the preflight. Nothing is written.
    ModelCatalog {
        /// Why (Codex's own message for a refused file).
        reason: LocalizedText,
    },
}

impl BlockReason {
    /// Display text.
    pub fn text(&self) -> LocalizedText {
        match self {
            BlockReason::VersionTooOld {
                consumer,
                found,
                required,
                ..
            } => LocalizedText::new(
                "block.version_too_old",
                "{consumer} runs Codex {found}; {required} or newer is required.",
            )
            .arg("consumer", consumer)
            .arg("found", found)
            .arg("required", required),
            BlockReason::LineUnavailable { line_id } => {
                LocalizedText::new("block.line_unavailable", "Line {line} is unavailable.")
                    .arg("line", line_id)
            }
            BlockReason::AuthModeUnavailable { line_id, auth_mode } => LocalizedText::new(
                "block.auth_mode_unavailable",
                "{mode} is not available on line {line}.",
            )
            .arg_text("mode", auth_mode.text())
            .arg("line", line_id),
            BlockReason::KeyRejected { line_id, access } => LocalizedText::new(
                "block.key_rejected",
                "The key cannot be used on line {line}: {reason}",
            )
            .arg("line", line_id)
            .arg_text("reason", access.text()),
            BlockReason::MissingKey => {
                LocalizedText::new("block.missing_key", "Enter a key for this profile first.")
            }
            BlockReason::JournalPending => LocalizedText::new(
                "block.journal_pending",
                "A previous switch was interrupted; recover it first.",
            ),
            BlockReason::FileNotWritable { path } => {
                LocalizedText::new("block.file_not_writable", "Cannot write {path}.")
                    .arg("path", crate::paths::display_path(path))
            }
            BlockReason::Unparseable { path, message } => {
                LocalizedText::new("block.unparseable", "Cannot parse {path}: {message}")
                    .arg("path", crate::paths::display_path(path))
                    .arg("message", message)
            }
            BlockReason::Elevated => {
                LocalizedText::new("block.elevated", "Do not run Superba with sudo.")
            }
            BlockReason::OtherUser {
                account,
                desktop_user: Some(user),
            } => LocalizedText::new(
                "block.other_user",
                "Superba is running as {account}, not as {user}, the user signed in at this computer: its files would go to the wrong user profile. Start Superba from {user}'s desktop.",
            )
            .arg("account", account)
            .arg("user", user),
            BlockReason::OtherUser {
                account,
                desktop_user: None,
            } => LocalizedText::new(
                "block.other_user_no_desktop",
                "Superba is running as {account}, which is not a user signed in at this computer: its files would go to the wrong user profile. Start Superba from your own desktop.",
            )
            .arg("account", account),
            BlockReason::Locked { what } => {
                LocalizedText::new("block.locked", "Busy: {what} is locked by another process.")
                    .arg("what", what)
            }
            BlockReason::NoBaseline => LocalizedText::new(
                "block.no_baseline",
                "No original configuration has been recorded.",
            ),
            BlockReason::UnsupportedTarget { detail } => {
                LocalizedText::new("block.unsupported_target", "Unsupported target: {detail}")
                    .arg("detail", detail)
            }
            BlockReason::RouterUnavailable { detail } => LocalizedText::new(
                "block.router_unavailable",
                "Router mode is on but the local router is not available ({detail}). Install it again or turn router mode off.",
            )
            .arg("detail", detail),
            BlockReason::RouterFromWslNat { distro } => LocalizedText::new(
                "block.router_wsl_nat",
                "The ChatGPT app runs Codex inside WSL ({distro}, WSL 2 behind NAT: \"Run Codex in WSL\"), where 127.0.0.1 is the distribution and not this computer, so it cannot use the local router. Turn on WSL's mirrored networking (networkingMode=mirrored in %USERPROFILE%\\.wslconfig, then wsl --shutdown) or turn off \"Run Codex in WSL\" in the app.",
            )
            .arg("distro", distro),
            BlockReason::ModelCatalog { reason } => LocalizedText::new(
                "block.model_catalog",
                "No model catalog file could be made for Codex: {reason}",
            )
            .arg_text("reason", reason.clone()),
        }
    }
}

/// A group of consumers of a target's configuration.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConsumerKind {
    /// The local desktop app (main process + app-server).
    DesktopApp,
    /// Local CLI TUI / exec processes.
    CliProcesses,
    /// The local CLI daemon.
    CliDaemon,
    /// IDE extension app-servers.
    IdeAppServers,
    /// A remote host's app-server.
    Remote {
        /// Target.
        target_id: TargetId,
    },
}

/// Restart decision for one consumer group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsumerRestart {
    /// Consumer group.
    pub consumer: ConsumerKind,
    /// Decision.
    pub restart: RestartKind,
    /// Affected pids (for "2 codex processes in terminals keep the old config").
    #[serde(default)]
    pub pids: Vec<u32>,
}

// ---------------------------------------------------------------------------
// Requests and plans
// ---------------------------------------------------------------------------

/// Which targets a switch applies to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetSelection {
    /// The local machine (app + CLI).
    pub local: bool,
    /// Remote hosts to switch together with the local machine.
    #[serde(default)]
    pub remotes: Vec<TargetId>,
}

impl Default for TargetSelection {
    fn default() -> Self {
        Self {
            local: true,
            remotes: Vec::new(),
        }
    }
}

/// Restore destinations (plan 9.6).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RestoreTarget {
    /// The pre-takeover configuration (plan 10.4 provider choice).
    Original {
        /// Provider handling.
        provider: ProviderRestore,
    },
    /// The official ChatGPT login.
    Official,
    /// Point-in-time restore: the state right after generation `generation`.
    Generation {
        /// Generation number.
        generation: u64,
    },
}

/// What the user asked for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SwitchIntent {
    /// Switch to a profile, optionally overriding its line and auth mode for this
    /// switch (quick line switch, manual auth under Advanced). The switch service
    /// stores the overrides into the profile after a successful switch.
    Profile {
        /// Profile.
        profile_id: ProfileId,
        /// Line override.
        #[serde(default)]
        line_id: Option<LineId>,
        /// Auth mode override.
        #[serde(default)]
        auth_mode: Option<AuthMode>,
    },
    /// Restore the original configuration or the official login.
    Restore {
        /// Destination.
        target: RestoreTarget,
    },
    /// Sessions page: migrate sessions between providers (full restart).
    MigrateSessions {
        /// Request.
        request: MigrationRequest,
    },
    /// Sessions page: roll a migration back (full restart).
    RollbackMigration {
        /// Migration id.
        migration_id: String,
    },
    /// Remove every change Superba made (plan 9.6 uninstall): restore the original
    /// configuration, env values and legacy rc lines, remote markers, launcher
    /// entries; optionally delete backups.
    Uninstall {
        /// Also delete Superba's backups.
        delete_backups: bool,
        /// Provider handling for a non-`openai` baseline (plan 10.4); required when
        /// the plan raises [`PlanChoice::RestoreProvider`].
        #[serde(default)]
        provider: Option<ProviderRestore>,
    },
    /// Targets page: fully restart the desktop app through Superba (CDP + auth env)
    /// without changing the configuration (explicit confirmation).
    RestartApp {
        /// Install to (re)launch; `None` = preferred / running one.
        #[serde(default)]
        install_id: Option<String>,
    },
    /// Roll one remote host back to its state before the last switch (recorded in
    /// the switch journal), plan 8.6 "roll back this host".
    RollbackRemote {
        /// Target.
        target_id: TargetId,
    },
    /// Turn router mode on or off for the applied key-based profile (else the
    /// router's first member): on points the local Codex home at the local router
    /// (API key mode), off back at a direct line. The choice is stored only after
    /// the switch succeeded; remote hosts are not part of it. Turning it on needs
    /// an installed router ([`crate::SwitchService::router_install`]).
    Router {
        /// On or off.
        enable: bool,
    },
    /// Updates page: run the desktop app with another Codex (`CODEX_CLI_PATH` at
    /// launch). Always a full app restart; the pin is stored after the relaunch
    /// was verified.
    AppCodexPin {
        /// The new pin.
        pin: crate::versions::AppCodexPin,
    },
    /// Let another tool manage the local Codex configuration (GH#5): Superba
    /// releases every key, the credential and its environment values it still
    /// owns without writing `config.toml` or `auth.json` (environment values it
    /// set are restored where nobody changed them). The generation recording the
    /// hand-off keeps encrypted copies of both files; a later profile switch
    /// takes over again from a fresh baseline.
    HandOff {
        /// Who takes over (the attribution shown to the user).
        #[serde(default)]
        to: crate::ownership::ChangeSource,
    },
    /// Remote hosts page: push the local profile's settings, filtered by the
    /// host's Codex version, to one host (settings only, never routing or auth),
    /// then hot-restart its app-server.
    HostSync {
        /// Host.
        target_id: TargetId,
        /// Rows to apply.
        #[serde(default)]
        selection: crate::remote::HostSyncSelection,
    },
}

/// Answer to the forced takeover question for sessions under a legacy provider
/// (plan 10.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacySessionChoice {
    /// Migrate them to `openai` (recommended; full restart).
    Migrate,
    /// Leave them; they stay hidden until migrated from the sessions page.
    Skip,
}

/// Input of [`crate::SwitchService::plan`]. Questions the plan raises
/// ([`PlanChoice`]) are answered by re-planning with the matching field set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchRequest {
    /// What to do.
    pub intent: SwitchIntent,
    /// Where.
    #[serde(default)]
    pub targets: TargetSelection,
    /// Replacement model when the current one is not in the target catalog.
    #[serde(default)]
    pub model_override: Option<String>,
    /// Answer to [`PlanChoice::LegacySessions`].
    #[serde(default)]
    pub legacy_sessions: Option<LegacySessionChoice>,
    /// Answer to [`PlanChoice::ForeignEnv`]: take over the user's own value.
    #[serde(default)]
    pub take_over_foreign_env: Option<bool>,
    /// Answer to [`PlanChoice::UnknownVersion`]: continue although a version is unknown.
    #[serde(default)]
    pub accept_unknown_versions: bool,
    /// Answers to [`PlanChoice::OwnedKeyConflict`] (dotted key path -> resolution).
    #[serde(default)]
    pub key_resolutions: BTreeMap<String, KeyResolution>,
    /// Answer to [`PlanChoice::SharedHome`]: write a home shared by several hosts once
    /// and restart all of them.
    #[serde(default)]
    pub accept_shared_home: bool,
    /// Thread ids unticked in a migration preview.
    #[serde(default)]
    pub migration_exclude: Vec<String>,
    /// The router's model list for Codex changed (a router model list mode,
    /// a model that passed its test): restart the app's Codex core even when
    /// no file changes, so it fetches the list again at once (user bug 106).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub refresh_models: bool,
}

impl SwitchRequest {
    /// A plain local switch to a profile.
    pub fn profile(profile_id: ProfileId) -> Self {
        Self {
            intent: SwitchIntent::Profile {
                profile_id,
                line_id: None,
                auth_mode: None,
            },
            targets: TargetSelection::default(),
            model_override: None,
            legacy_sessions: None,
            take_over_foreign_env: None,
            accept_unknown_versions: false,
            key_resolutions: BTreeMap::new(),
            accept_shared_home: false,
            migration_exclude: Vec::new(),
            refresh_models: false,
        }
    }

    /// A local re-apply of `profile_id` that restarts Codex's core so it reads
    /// the router's changed model list (user bug 106).
    pub fn refresh_models(profile_id: ProfileId) -> Self {
        let mut request = Self::profile(profile_id);
        request.targets = TargetSelection {
            local: true,
            remotes: Vec::new(),
        };
        request.refresh_models = true;
        request
    }
}

/// A question the user must answer before the plan can execute.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PlanChoice {
    /// Sessions under a non-`openai` provider would become hidden (plan 10.3).
    LegacySessions {
        /// Provider -> session count.
        providers: BTreeMap<String, u32>,
    },
    /// Restoring an original config whose provider is not `openai` (plan 10.4);
    /// answered through [`RestoreTarget::Original`]'s provider field.
    RestoreProvider {
        /// Baseline provider id.
        baseline_provider: String,
        /// Sessions created while Superba was active.
        created_during: u32,
        /// Sessions migrated in at takeover.
        migrated_in: u32,
    },
    /// The configured model is not in the target catalog (plan 7.3).
    ReplacementModel {
        /// Current model.
        current: String,
        /// Suggestions.
        suggestions: Vec<String>,
    },
    /// The user has their own non-Krill `CODEX_AUTHAPI_BASE_URL` lines (plan 9.9).
    ForeignEnv {
        /// Lines.
        lines: Vec<RcLine>,
    },
    /// A consumer's Codex version is unknown (remote) and PAT/base-URL support cannot
    /// be verified (plan 9.8).
    UnknownVersion {
        /// Consumer label.
        consumer: String,
    },
    /// Owned keys (or the `auth.json` credential) were changed outside Superba
    /// since it wrote them (plan 9.3, GH#5); answered per key through
    /// [`SwitchRequest::key_resolutions`] (`auth.json` as
    /// [`crate::ownership::AUTH_JSON_PATH`]): `Overwrite` takes them over,
    /// `KeepCurrent` keeps the other tool's value. Not answering is cancelling.
    OwnedKeyConflict {
        /// Key path, the value Superba wrote (`before`) and the live value (`after`).
        conflicts: Vec<KeyChange>,
        /// The credential, when another tool or a sign-in replaced it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth: Option<crate::ownership::AuthDrift>,
        /// Who most likely made the change.
        #[serde(default)]
        source: crate::ownership::ChangeSource,
    },
    /// Several remote hosts share one home directory (NFS, plan 8.6).
    SharedHome {
        /// Hosts sharing it.
        targets: Vec<TargetId>,
    },
}

/// A one-click alternative plan offered next to the main one (e.g. "hot switch with
/// an API key instead" when the auth API origin is unhealthy).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanAlternative {
    /// Button label.
    pub label: LocalizedText,
    /// Request to plan instead.
    pub request: SwitchRequest,
}

/// Summary of the destination for headers ("Krill · Overseas · PAT").
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanDestination {
    /// Profile.
    pub profile_id: Option<ProfileId>,
    /// Site.
    pub site_id: Option<SiteId>,
    /// Line.
    pub line_id: Option<LineId>,
    /// Auth mode.
    pub auth_mode: Option<AuthMode>,
    /// Display label.
    pub label: LocalizedText,
}

/// The full plan computed before anything is written (plan 3.1 `SwitchPlan`).
/// Serialize-only (it embeds executable [`ApplyPlan`]s); pass it back to
/// `execute` as the value `plan` returned.
#[derive(Clone, Debug, Serialize)]
pub struct SwitchPlan {
    /// Plan id.
    pub id: String,
    /// When computed.
    pub created_at: Timestamp,
    /// Request it answers.
    pub request: SwitchRequest,
    /// Currently applied state.
    pub from: Option<ActiveState>,
    /// Destination.
    pub to: PlanDestination,
    /// Restart decision for the local desktop app; drives the UI:
    /// hot -> countdown bar, full -> confirmation dialog, none -> apply, blocked -> error.
    pub primary: RestartKind,
    /// Decisions for every consumer group (local and remote).
    pub restarts: Vec<ConsumerRestart>,
    /// Local config/auth change.
    #[serde(default)]
    pub config: Option<ApplyPlan>,
    /// Local env change.
    #[serde(default)]
    pub env: Option<EnvPlan>,
    /// Codex's additive certificate bundle for the local router, including its
    /// restoration when leaving router mode.
    #[serde(default)]
    pub certificate_env: Option<EnvPlan>,
    /// Session migration.
    #[serde(default)]
    pub migration: Option<MigrationPlan>,
    /// Remote host plans.
    #[serde(default)]
    pub remotes: Vec<RemotePlan>,
    /// Threads with turns in flight at plan time (re-checked before applying).
    #[serde(default)]
    pub active_threads: Vec<ThreadActivity>,
    /// Model catalog check.
    #[serde(default)]
    pub model_check: Option<ModelCheck>,
    /// Questions to answer by re-planning; execution refuses while non-empty.
    #[serde(default)]
    pub choices: Vec<PlanChoice>,
    /// Alternatives.
    #[serde(default)]
    pub alternatives: Vec<PlanAlternative>,
    /// Risk notes and explanations (CLI processes unaffected, auth address, ...).
    #[serde(default)]
    pub warnings: Vec<Notice>,
    /// Uninstall / restore clean-up items to list before confirming (marker blocks,
    /// launcher entries, remote markers, backups).
    #[serde(default)]
    pub cleanup: Vec<LocalizedText>,
}

/// Key of the plan notice that says before confirming that ChatGPT is ended
/// by force: on Windows a ChatGPT Superba did not open has no debugging port,
/// so it cannot be asked to quit (closing its windows leaves it running in the
/// notification area; user bug 76).
pub const FORCE_QUIT_NOTICE: &str = "switch.warn.windows_force_quit";

impl SwitchPlan {
    /// The plan says up front that the desktop app is ended by force
    /// ([`FORCE_QUIT_NOTICE`]): confirming it answers the force-quit question
    /// ([`ExecuteOptions::force_quit`]).
    pub fn quits_by_force(&self) -> bool {
        self.warnings
            .iter()
            .any(|w| w.text.key == FORCE_QUIT_NOTICE)
    }

    /// The local restart decision is `Blocked`.
    pub fn is_blocked(&self) -> bool {
        self.primary.is_blocked()
    }

    /// Hot plan without questions: the UI may start the countdown.
    pub fn countdown_allowed(&self) -> bool {
        self.primary.is_hot() && self.choices.is_empty() && self.migration.is_none()
    }

    /// Needs the explicit confirmation dialog.
    pub fn needs_confirmation(&self) -> bool {
        self.primary.needs_confirmation() || self.migration.is_some()
    }
}

// ---------------------------------------------------------------------------
// Execution (plan 7.2)
// ---------------------------------------------------------------------------

/// Options for [`crate::SwitchService::execute`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecuteOptions {
    /// "Write config only, I will restart myself" (ConfirmFull). No restart, no
    /// session migration.
    pub write_only: bool,
    /// Do not wait for running turns (`spb --force`); the decider is not asked.
    pub force_turns: bool,
    /// Allow a full restart without asking (the GUI confirmed it already;
    /// `spb --allow-full-restart`). If false and the plan needs one, execution fails
    /// with [`Error::Blocked`].
    pub allow_full_restart: bool,
    /// Quiet period after the last turn ends before switching (default 2000 ms).
    pub turn_quiet_ms: u64,
    /// Graceful quit timeout before asking to force (default 15000 ms).
    pub quit_timeout_ms: u64,
    /// End the desktop app by force without asking again: the user confirmed
    /// a plan that said so up front ([`SwitchPlan::quits_by_force`], user bug
    /// 76). Ignored when the app can be asked to quit.
    #[serde(default)]
    pub force_quit: bool,
}

impl Default for ExecuteOptions {
    fn default() -> Self {
        Self {
            write_only: false,
            force_turns: false,
            allow_full_restart: false,
            turn_quiet_ms: 2_000,
            quit_timeout_ms: 15_000,
            force_quit: false,
        }
    }
}

/// States of the switch state machine (plan 7.2).
///
/// `Idle`, `Planning`, `Blocked`, `Armed` and `ConfirmFull` live in the UI (before
/// `execute` is called: cancelling there writes nothing). The remaining states are
/// reported by `execute` through [`SwitchEvent::Phase`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwitchPhase {
    /// UI: nothing in progress.
    Idle,
    /// UI: computing a plan.
    Planning,
    /// UI: plan blocked.
    Blocked,
    /// UI: hot plan, countdown running.
    Armed,
    /// UI: full restart / migration needs confirmation.
    ConfirmFull,
    /// Checking for running turns.
    TurnCheck,
    /// Waiting for turns to finish (countdown paused).
    WaitingTurn,
    /// Quitting the app and processes that must stop.
    Quitting,
    /// Quit timed out; asking whether to force.
    ForceQuitConfirm,
    /// Writing files / env / migrating (journaled).
    Applying,
    /// Undoing a failed apply.
    RollingBack,
    /// Sending the hot restart IPC.
    Restarting,
    /// IPC failed; offering a full restart, undo, or keep.
    FullRestartOffer,
    /// Relaunching the app (full restart).
    Launching,
    /// Running the four verification checks.
    Verifying,
    /// Navigating back to the thread the user had open.
    NavRestore,
    /// Verification failed; retrying / deciding.
    Recovery,
    /// Rolled back files and restarted again.
    RollbackRestart,
    /// Finished.
    Done,
}

/// Progress steps shown as `write config ✓ · restart core … · verify auth · verify line`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SwitchStep {
    /// Write config.toml / auth.json.
    WriteConfig,
    /// Persist environment variables.
    WriteEnv,
    /// Migrate sessions.
    MigrateSessions,
    /// Quit the app.
    QuitApp,
    /// Hot-restart the app-server.
    RestartCore,
    /// Stop Codex processes the user asked Superba to stop.
    StopProcesses,
    /// Launch the app.
    LaunchApp,
    /// Verify: new app-server process.
    VerifyProcess,
    /// Verify: file hashes.
    VerifyFiles,
    /// Verify: whoami / auth.
    VerifyAuth,
    /// Verify: authenticated models on the new line.
    VerifyLine,
    /// Verify: renderer not on the login page.
    VerifyRenderer,
    /// A remote host (all of its sub-steps).
    Remote {
        /// Target.
        target_id: TargetId,
    },
}

/// Status of a [`SwitchStep`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StepStatus {
    /// Not started.
    Pending,
    /// In progress.
    Running,
    /// Finished successfully.
    Done,
    /// Skipped (not needed).
    Skipped,
    /// Failed.
    Failed {
        /// Reason.
        message: String,
    },
}

/// Progress events emitted during execution, in order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SwitchEvent {
    /// A WSL operation, including read-only planning before execution starts.
    RemoteProgress {
        /// Distribution being contacted.
        target_id: TargetId,
        /// Current stage (or `None` when finished).
        message: Option<LocalizedText>,
        /// Safe to interrupt before any remote files are published.
        can_stop: bool,
    },
    /// Entered a state.
    Phase {
        /// State.
        phase: SwitchPhase,
        /// When.
        at: Timestamp,
    },
    /// A step changed status.
    Step {
        /// Step.
        step: SwitchStep,
        /// Status.
        status: StepStatus,
        /// When.
        at: Timestamp,
    },
    /// Live list of busy threads while waiting (local and selected remotes).
    Turns {
        /// Per-target activity.
        activity: Vec<TurnActivity>,
        /// Time waited so far.
        waited_ms: u64,
    },
    /// Log line for the activity log.
    Log {
        /// Severity.
        level: Severity,
        /// Message.
        text: LocalizedText,
    },
    /// Sessions are being migrated or restored (several reports a second while
    /// they move, a last one at [`crate::SessionStage::Done`]).
    Sessions {
        /// Progress.
        progress: crate::SessionProgress,
    },
}

/// A question asked by `execute` through [`crate::SwitchDecider`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum DecisionRequest {
    /// Turns are running (`WaitingTurn`). Allowed: Wait, Force, Cancel. Asked again
    /// after 15 minutes of waiting.
    WaitForTurns {
        /// Busy threads.
        busy: Vec<ThreadActivity>,
        /// Time waited so far.
        waited_ms: u64,
    },
    /// Graceful quit timed out (`ForceQuitConfirm`). Allowed: Force, Cancel.
    ForceQuit {
        /// Processes still alive.
        pids: Vec<u32>,
        /// What the quit did and which of the app's processes still run.
        #[serde(default)]
        report: crate::QuitReport,
    },
    /// The app did not quit even by force (user bug 53): `report` names its
    /// processes still running and what was tried. Allowed: Force (end
    /// exactly these processes), Continue (try the forced quit again),
    /// Cancel (stop the switch; nothing was written yet).
    AppStillRunning {
        /// What was tried and what still runs.
        report: crate::QuitReport,
    },
    /// The hot restart IPC failed (`FullRestartOffer`). Allowed: FullRestart, Undo,
    /// KeepWritten.
    IpcFailed {
        /// Error.
        error: Error,
    },
    /// Verification failed after one automatic retry (`Recovery`). Allowed:
    /// Rollback, FullRestart, KeepWritten.
    VerificationFailed {
        /// Results.
        verification: Verification,
    },
    /// Turn activity on some remote hosts could not be determined. Allowed:
    /// Continue, Cancel.
    UnknownRemoteActivity {
        /// Hosts.
        targets: Vec<TargetId>,
    },
    /// Session migration needs these processes stopped (TUI, exec, IDE
    /// app-servers, plan 10.2). The user closes them and continues (re-check),
    /// or asks Superba to stop them ([`Decision::StopProcesses`]: each is asked
    /// to exit where the platform allows; what does not exit is listed again in
    /// [`DecisionRequest::ForceStop`]); Superba never stops one without that
    /// answer. Allowed: StopProcesses, Continue, Cancel.
    CloseConsumers {
        /// Processes still using the Codex home.
        processes: Vec<ProcessInfo>,
        /// Turns running in them (stopping one now interrupts its turn).
        #[serde(default)]
        busy: Vec<ThreadActivity>,
    },
    /// After [`Decision::StopProcesses`], these did not exit (or the platform
    /// cannot ask them to: Windows console programs and pipe app-servers). End
    /// them by force? Allowed: Force, Continue (re-check), Cancel.
    ForceStop {
        /// Processes still running.
        processes: Vec<ProcessInfo>,
        /// Turns running in them (their output is lost).
        #[serde(default)]
        busy: Vec<ThreadActivity>,
    },
}

impl DecisionRequest {
    /// Decisions valid for this request.
    pub fn allowed(&self) -> &'static [Decision] {
        match self {
            DecisionRequest::WaitForTurns { .. } => {
                &[Decision::Wait, Decision::Force, Decision::Cancel]
            }
            DecisionRequest::ForceQuit { .. } => &[Decision::Force, Decision::Cancel],
            DecisionRequest::AppStillRunning { .. } => {
                &[Decision::Force, Decision::Continue, Decision::Cancel]
            }
            DecisionRequest::IpcFailed { .. } => {
                &[Decision::FullRestart, Decision::Undo, Decision::KeepWritten]
            }
            DecisionRequest::VerificationFailed { .. } => &[
                Decision::Rollback,
                Decision::FullRestart,
                Decision::KeepWritten,
            ],
            DecisionRequest::UnknownRemoteActivity { .. } => {
                &[Decision::Continue, Decision::Cancel]
            }
            DecisionRequest::CloseConsumers { .. } => &[
                Decision::StopProcesses,
                Decision::Continue,
                Decision::Cancel,
            ],
            DecisionRequest::ForceStop { .. } => {
                &[Decision::Force, Decision::Continue, Decision::Cancel]
            }
        }
    }

    /// The safe default (what a non-interactive caller without flags gets).
    pub fn safe_default(&self) -> Decision {
        match self {
            DecisionRequest::WaitForTurns { .. } => Decision::Wait,
            DecisionRequest::ForceQuit { .. }
            | DecisionRequest::AppStillRunning { .. }
            | DecisionRequest::UnknownRemoteActivity { .. }
            | DecisionRequest::CloseConsumers { .. }
            | DecisionRequest::ForceStop { .. } => Decision::Cancel,
            DecisionRequest::IpcFailed { .. } => Decision::Undo,
            DecisionRequest::VerificationFailed { .. } => Decision::Rollback,
        }
    }
}

/// A user's answer to a [`DecisionRequest`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Keep waiting.
    Wait,
    /// Proceed anyway (lose in-flight output / force-kill).
    Force,
    /// Abort; roll back anything written.
    Cancel,
    /// Do a full app restart instead.
    FullRestart,
    /// Undo the switch (roll files back).
    Undo,
    /// Keep the written config; the user restarts later.
    KeepWritten,
    /// Roll back to the previous config and restart.
    Rollback,
    /// Continue.
    Continue,
    /// Stop the listed processes for the user: ask each to exit (graceful)
    /// where the platform allows, then ask again before ending any by force.
    StopProcesses,
}

/// Result of one verification check.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CheckResult {
    /// Not run yet (a verification with pending checks has not passed).
    #[default]
    Pending,
    /// Passed.
    Passed,
    /// Failed.
    Failed {
        /// Reason.
        detail: String,
    },
    /// Deliberately not run because it does not apply to this path (e.g. renderer
    /// check without CDP, auth self-test for an official login).
    Skipped {
        /// Why.
        reason: String,
    },
}

impl CheckResult {
    /// Passed or deliberately skipped.
    pub fn ok(&self) -> bool {
        matches!(self, CheckResult::Passed | CheckResult::Skipped { .. })
    }
}

/// The four post-switch checks (plan 7.2 `Verifying`) plus the line self-test.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verification {
    /// New app-server started after T0, old one gone, no crash within 5 s.
    pub process: CheckResult,
    /// config.toml / auth.json hashes equal what was written.
    pub files: CheckResult,
    /// whoami (PAT: on the app's auth API origin).
    pub auth: CheckResult,
    /// Authenticated models on the new line.
    pub line: CheckResult,
    /// Renderer not on the login page.
    pub renderer: CheckResult,
}

impl Verification {
    /// All checks passed or were deliberately skipped (none pending or failed).
    pub fn passed(&self) -> bool {
        [
            &self.process,
            &self.files,
            &self.auth,
            &self.line,
            &self.renderer,
        ]
        .iter()
        .all(|c| c.ok())
    }
}

/// Final status of an execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeStatus {
    /// Switched and verified.
    Switched,
    /// Config written, restart left to the user.
    WrittenOnly,
    /// Cancelled before writing anything.
    Cancelled,
    /// Rolled back to the previous state.
    RolledBack,
    /// Local switched, some remote hosts failed.
    Partial,
    /// Failed (see notices).
    Failed,
}

/// Per-remote result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteOutcome {
    /// Target.
    pub target_id: TargetId,
    /// Status.
    pub status: OutcomeStatus,
    /// Error, if failed.
    #[serde(default)]
    pub error: Option<Error>,
}

/// Result of [`crate::SwitchService::execute`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchOutcome {
    /// Plan id.
    pub plan_id: String,
    /// Status.
    pub status: OutcomeStatus,
    /// Time from start of execution to done.
    pub elapsed_ms: u64,
    /// Generation after the switch.
    #[serde(default)]
    pub generation: Option<u64>,
    /// What restart actually happened locally.
    pub restart: RestartKind,
    /// Verification results.
    #[serde(default)]
    pub verification: Option<Verification>,
    /// Remote results.
    #[serde(default)]
    pub remotes: Vec<RemoteOutcome>,
    /// CLI processes still on the old config.
    #[serde(default)]
    pub stale_processes: Vec<ProcessInfo>,
    /// After the switch: every running Codex consumer of the local home, the
    /// configuration it runs and how it picks the new one up (`None` when the
    /// local home was not part of the switch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<Box<crate::runtime::RuntimeView>>,
    /// Thread the UI was navigated back to.
    #[serde(default)]
    pub returned_to: Option<ThreadRef>,
    /// Toast / summary messages.
    #[serde(default)]
    pub notices: Vec<Notice>,
}

// ---------------------------------------------------------------------------
// Read side
// ---------------------------------------------------------------------------

/// A key given to [`crate::SwitchService::check_key`].
#[derive(Clone, Debug)]
pub enum KeyInput {
    /// Typed by the user (not stored yet).
    Secret(Secret),
    /// A stored key.
    Ref(KeyRef),
    /// The key of a saved profile.
    Profile(ProfileId),
}

/// Whether the local app can be hot-switched right now (overview badge).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HotSwitchStatus {
    /// Running with CDP and the bridge: hot switching works.
    Ready,
    /// Running but a hot switch is not possible.
    Unavailable {
        /// Reason.
        reason: FullRestartReason,
    },
    /// Installed but not running.
    NotRunning,
    /// No desktop app installed (CLI-only: hide hot-switch UI).
    NotInstalled,
    /// Safe mode is on (no CDP by choice).
    SafeMode,
}

/// Local desktop app card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppOverview {
    /// Installs found.
    pub installs: Vec<AppInstall>,
    /// Running app.
    #[serde(default)]
    pub session: Option<AppSession>,
    /// Hot-switch badge.
    pub hot_switch: HotSwitchStatus,
    /// The Krill line whose origin the running app uses for PAT auth
    /// ("auth address: Overseas (follows the current line after the next full restart)").
    #[serde(default)]
    pub authapi_line: Option<LineId>,
}

/// Local CLI card.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliOverview {
    /// Binaries found.
    pub binaries: Vec<CodexBinary>,
    /// Codex processes (CLI, daemon, IDE app-servers).
    pub processes: Vec<ProcessInfo>,
}

impl CliOverview {
    /// The Codex CLI a new terminal runs ([`crate::app::terminal_cli`]); the
    /// desktop app's own copies are not a CLI (user bug 55).
    pub fn terminal_cli(&self) -> Option<&CodexBinary> {
        crate::app::terminal_cli(&self.binaries)
    }

    /// Processes still on an old config.
    pub fn stale_count(&self) -> usize {
        self.processes.iter().filter(|p| p.stale).count()
    }

    /// Processes known to lack `CODEX_AUTHAPI_BASE_URL` (PAT would send the key to
    /// the default auth API).
    pub fn missing_env_count(&self) -> usize {
        self.processes
            .iter()
            .filter(|p| !matches!(p.kind, CodexProcessKind::AppMain))
            .filter(|p| matches!(p.authapi_env, crate::env::EnvObservation::Absent))
            .count()
    }
}

/// Remote host card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteOverview {
    /// Host.
    pub host: RemoteHost,
    /// Last probe.
    #[serde(default)]
    pub state: Option<RemoteState>,
    /// Probe error.
    #[serde(default)]
    pub error: Option<Error>,
}

/// Snapshot for the overview page and `spb status`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Overview {
    /// When assembled.
    pub generated_at: Timestamp,
    /// Local Codex home.
    pub codex_home: PathBuf,
    /// Applied state.
    #[serde(default)]
    pub active: Option<ActiveState>,
    /// Observed config.
    #[serde(default)]
    pub config: Option<ConfigState>,
    /// Desktop app.
    pub app: AppOverview,
    /// CLI.
    pub cli: CliOverview,
    /// Remote hosts.
    #[serde(default)]
    pub remotes: Vec<RemoteOverview>,
    /// Account card.
    #[serde(default)]
    pub account: Option<AccountSummary>,
    /// A baseline exists (takeover happened).
    pub baseline_captured: bool,
    /// When the baseline (the user's original configuration) was captured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_at: Option<Timestamp>,
    /// The stashed ChatGPT login of the local home, if one is kept (restoring
    /// the official login puts it back).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_stash: Option<crate::config::LoginStashInfo>,
    /// Written configuration versus what each running Codex consumer uses
    /// (running turns not checked on this cheap path, see
    /// [`crate::SwitchService::runtime`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<crate::runtime::RuntimeView>,
    /// Banners.
    #[serde(default)]
    pub notices: Vec<Notice>,
    /// Changes made outside Superba to what it owns in the local Codex home
    /// (GH#5: cc-switch, AI Toolbox, Codex, hand edits), with who most likely
    /// made them; `None` when nothing changed or Superba owns nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external: Option<crate::ownership::ExternalChange>,
    /// Claude Code's `settings.json` points at the local router (the shell
    /// keeps the router watchdog running for it too).
    #[serde(default)]
    pub claude_routed: bool,
    /// Codex ingress from Superba's recorded router listener, never inferred
    /// from the observed upstream URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router_base_url: Option<url::Url>,
}

/// One diagnostics item (plan 4 #21, 11.1 diagnostics page).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorFinding {
    /// Stable id (`openai_env_conflict`, `profile_override`, `codex_home_mismatch` ...).
    pub id: String,
    /// Severity.
    pub severity: Severity,
    /// Title.
    pub title: LocalizedText,
    /// Details.
    #[serde(default)]
    pub detail: Option<LocalizedText>,
    /// A one-click fix exists.
    #[serde(default)]
    pub fixable: bool,
}
