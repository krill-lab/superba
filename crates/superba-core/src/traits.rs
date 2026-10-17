//! Trait contracts between crates.
//!
//! All traits are object safe (`#[async_trait]`, `Send + Sync`) and are used as
//! `Arc<dyn Trait>`, so `superba-switch` can be wired with real implementations in
//! production and with fakes in tests. Implementations live in:
//!
//! | Trait | Crate | Placeholder type |
//! |---|---|---|
//! | [`ConfigEngine`], [`EnvPersister`], [`SecretStore`], [`ProfileStore`] | `superba-config` | `TomlConfigEngine`, `ShellEnvPersister`, `KeyringSecretStore`, `JsonProfileStore` |
//! | [`LineProber`] | `superba-net` | `HttpLineProber` |
//! | [`SessionMigrator`] | `superba-sessions` | `CodexSessionMigrator` |
//! | [`AppController`] | `superba-appctl` | `DesktopAppController` |
//! | [`RemoteHosts`] | `superba-remote` | `SshRemoteHosts` |
//! | [`FeedSource`] | `superba-catalog` | `FeedStore` (offline), `HttpFeeds` |
//! | [`crate::VersionStore`] | `superba-versions` | `HttpVersionStore` |
//! | [`SwitchService`] | `superba-switch` | `Switcher` |
//!
//! Conventions:
//! - Planning methods never write. Writing methods take the plan they execute.
//! - Methods never prompt; interactive questions go through [`SwitchDecider`].
//! - Secrets are passed as [`Secret`] and never logged.
//! - Every method the production implementations provide is required (no
//!   default body); a test fake that does not support one returns
//!   [`crate::Error::NotImplemented`], which callers treat as "not available".
//! - Failure atomicity: a writing method that fails restores what it changed before
//!   returning. If that restoration itself fails it returns an error naming what is
//!   left, and its own journal keeps the entry for start-up recovery.
//! - Locking: `superba-switch` holds `DataDir::state_lock` for a whole execution and
//!   records every component receipt in its switch journal (`journal/switch/`);
//!   components never take `state.lock` themselves.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use url::Url;

use crate::app::{
    AppInstall, AppSession, BridgeStatus, CodexBinary, HotRestartOutcome, HotRestartRequest,
    LaunchOptions, ProcessInfo, QuitOptions, QuitOutcome, RendererState, ThreadRef, TurnActivity,
};
use crate::auth::AuthMode;
use crate::caps::{
    AccountSummary, AuthApiHealth, KeyCheck, LineCaps, ModelInfo, ResponsesProbe, SiteProbeReport,
    SiteProbeRequest,
};
use crate::codex::CodexVersion;
use crate::config::{
    ActiveCredentials, ApplyPlan, ApplyReport, Baseline, BaselineExtras, CodexFiles, ConfigState,
    GenerationRecord, LoginStashInfo, RecoveryReport, RenderInput, RenderedFiles,
    ResolvedEndpoints, ResolvedProfile, StashScope,
};
use crate::env::{EnvApplied, EnvOp, EnvPlan, EnvReport};
use crate::error::Result;
use crate::feeds::{FeedSnapshot, FeedsStatus};
use crate::ids::{LineId, ProfileId, SiteId, TargetId};
use crate::models::{ModelList, ModelsEdit, ModelsEditReport, ModelsView, ModelsViewRequest};
use crate::origin::Origin;
use crate::paths::CodexHome;
use crate::plan::{
    Decision, DecisionRequest, DoctorFinding, ExecuteOptions, KeyInput, Overview, SwitchEvent,
    SwitchOutcome, SwitchPlan, SwitchRequest,
};
use crate::profile::{Profile, ProfileSet, Settings};
use crate::remote::{
    HostSyncPlan, HostView, RemoteApplyReport, RemoteApplyScope, RemoteInstallReport, RemotePlan,
    RemoteState, RemoteVerification, RemoteVerifyRequest,
};
use crate::requests::{
    ClearReport, ClearRequest, InflightRequest, PriceTable, RequestDetail, RequestPage,
    RequestQuery, RequestStats, StatsQuery,
};
use crate::router::{RouterInstallRequest, RouterStatsSettings, RouterView, RouterWatch};
use crate::secret::{KeyRef, Secret, SecretBackend, SecretBytes};
use crate::sessions::{
    ConsumersStopped, HostSessions, MigrationPlan, MigrationReceipt, MigrationRequest,
    RollbackReport, SessionReport, SessionTitle,
};
use crate::site::{Line, Site};
use crate::target::RemoteHost;
use crate::time::Timestamp;
use crate::transports::{LineTransports, LineTransportsRequest, TransportProbe};
use crate::usage::{AccountReport, AccountRequest, UsageQuery, UsageReport};
use crate::versions::{
    AppDownloadReport, CodexInstallKind, InstallReport, InstallRequest, InstallTarget, VersionsView,
};
use crate::views::{
    PlanSettings, SettingsEdit, SettingsEditReport, SettingsView, SettingsViewRequest,
};

// ---------------------------------------------------------------------------
// superba-config
// ---------------------------------------------------------------------------

/// Owns `config.toml` and `auth.json` of a Codex home (plan 9).
///
/// Rules: only owned keys change (toml_edit, every other byte preserved); never
/// write `[model_providers.*]`; `model_provider` is always `"openai"`; unparseable
/// files are refused; writes are temp + fsync + rename with a sha256 re-check
/// before rename (max 3 retries); auth.json is 0600 and never empty; the first
/// write of each file keeps a pre-image; every apply is journaled
/// (Pending -> Publishing -> Applied) under `state.lock`; a ChatGPT login is
/// stashed (encrypted) before it is replaced. Order: auth.json, then config.toml.
/// `model_provider` other than `"openai"` is written only when restoring a
/// baseline with `ProviderRestore::Baseline`.
#[async_trait]
pub trait ConfigEngine: Send + Sync {
    /// Pure render (no IO): compute the new file bytes for `input.desired` from the
    /// given current bytes. Shared with `superba-remote`, which reads and writes the
    /// remote files over ssh but must produce byte-identical results. Drifted owned
    /// keys without a resolution in `desired.key_resolutions` are an error
    /// (`Error::Conflict`) so the planner can raise `PlanChoice::OwnedKeyConflict`.
    /// The caller keeps the ownership record: `owned` is the next `last_written`
    /// and `newly_owned` extends its baseline (the user's value of a key Superba
    /// takes over, restored when the key is released).
    fn render(&self, input: &RenderInput<'_>) -> Result<RenderedFiles>;

    /// Pure: describe files without IO (remote probes use this instead of parsing
    /// TOML/JSON themselves). Local-only fields (`active`, `journal_pending`) are empty.
    fn inspect_files(&self, codex_home: &Path, files: &CodexFiles) -> Result<ConfigState>;

    /// Pure: build a baseline from files (remote hosts keep their baseline under
    /// `DataDir::remote_dir`; local homes use [`ConfigEngine::snapshot_baseline`]).
    fn capture_baseline(
        &self,
        codex_home: &Path,
        files: &CodexFiles,
        extras: BaselineExtras,
        now: Timestamp,
    ) -> Result<Baseline>;

    /// Read and describe the current state of a Codex home (hashes, owned keys,
    /// provider, auth shape, drift, pending journal).
    async fn read_state(&self, home: &CodexHome) -> Result<ConfigState>;

    /// The stored baseline, if a takeover happened.
    async fn baseline(&self, home: &CodexHome) -> Result<Option<Baseline>>;

    /// Capture the baseline before the first write (idempotent: returns the existing
    /// baseline when one exists). `extras` carries session and env data gathered by
    /// the other components.
    async fn snapshot_baseline(&self, home: &CodexHome, extras: BaselineExtras)
    -> Result<Baseline>;

    /// Compute the change for `desired` (also used for restore: pass
    /// [`crate::ResolvedMode::Official`] or [`crate::ResolvedMode::Original`]).
    /// Never writes.
    async fn plan(&self, home: &CodexHome, desired: &ResolvedProfile) -> Result<ApplyPlan>;

    /// Execute a plan (journaled). Re-reads the files just before writing and
    /// re-renders if they changed since planning.
    async fn apply(&self, plan: &ApplyPlan) -> Result<ApplyReport>;

    /// Undo an apply (verification failed, user cancelled after writing). Only files
    /// still equal to the written hash are restored; others are reported as conflict.
    async fn rollback(&self, report: &ApplyReport) -> Result<()>;

    /// Finish or roll back interrupted journal entries (run at start-up, before any
    /// plan).
    async fn recover(&self, home: &CodexHome) -> Result<RecoveryReport>;

    /// Generation history of a home (for reverse migration by generation).
    async fn generations(&self, home: &CodexHome) -> Result<Vec<GenerationRecord>>;

    /// Token credentials currently in the home (`None` for ChatGPT login / no auth),
    /// so the switch can self-test restored configurations.
    async fn active_credentials(&self, home: &CodexHome) -> Result<Option<ActiveCredentials>>;

    /// Metadata of the stashed ChatGPT login for `scope`, if any (age warning).
    async fn login_stash(&self, scope: &StashScope) -> Result<Option<LoginStashInfo>>;

    /// Stash (encrypt and store) a ChatGPT login `auth.json` for `scope`. Local
    /// applies call this internally; `superba-remote` calls it for remote logins.
    async fn stash_login(
        &self,
        scope: &StashScope,
        auth_json: &SecretBytes,
    ) -> Result<LoginStashInfo>;

    /// Decrypt the stashed login for `scope` (input of [`RenderInput::login_stash`]).
    async fn load_login_stash(&self, scope: &StashScope) -> Result<Option<SecretBytes>>;
}

/// Persists environment variables for future processes (plan 6.1, 9.9):
/// shell rc marker blocks (`crate::env::render_block`), Linux `environment.d`,
/// macOS `launchctl setenv` + LaunchAgent, Windows `HKCU\Environment` + broadcast.
/// Takes over legacy Krill tutorial lines by commenting them out (restored on
/// `RestoreBaseline`). Home directory comes from the injected context.
#[async_trait]
pub trait EnvPersister: Send + Sync {
    /// Describe current persisted values, legacy lines and foreign lines.
    async fn inspect(&self, var: &str) -> Result<EnvReport>;

    /// Compute the edits for `op` (preview); never writes.
    async fn plan(&self, var: &str, op: EnvOp) -> Result<EnvPlan>;

    /// Perform exactly the planned edits (with rc backups).
    async fn apply(&self, plan: &EnvPlan) -> Result<EnvApplied>;

    /// Undo an apply.
    async fn revert(&self, applied: &EnvApplied) -> Result<()>;

    /// Convenience: persist `CODEX_AUTHAPI_BASE_URL=<origin>` (legacy Krill lines are
    /// taken over, foreign lines left alone).
    async fn set_authapi_base_url(&self, origin: &Origin) -> Result<EnvApplied> {
        let var = crate::codex::ENV_AUTHAPI_BASE_URL;
        let plan = self
            .plan(
                var,
                EnvOp::Set {
                    value: origin.as_str(),
                    take_over_foreign: false,
                },
            )
            .await?;
        self.apply(&plan).await
    }

    /// Convenience: remove Superba's persisted `CODEX_AUTHAPI_BASE_URL`.
    async fn clear_authapi_base_url(&self) -> Result<EnvApplied> {
        let var = crate::codex::ENV_AUTHAPI_BASE_URL;
        let plan = self.plan(var, EnvOp::Clear).await?;
        self.apply(&plan).await
    }
}

/// Stores profile keys: OS keyring first, 0600 file fallback (headless Linux).
#[async_trait]
pub trait SecretStore: Send + Sync {
    /// Read a secret ([`KeyRef::Env`] reads the variable).
    async fn get(&self, key_ref: &KeyRef) -> Result<Secret>;

    /// Store a secret for a profile; returns where it went.
    async fn put(&self, profile_id: &ProfileId, secret: &Secret) -> Result<KeyRef>;

    /// Delete a stored secret (no-op for [`KeyRef::Env`]).
    async fn delete(&self, key_ref: &KeyRef) -> Result<()>;

    /// Backend used for new secrets (the UI warns on plaintext).
    fn backend(&self) -> SecretBackend;
}

/// Persists [`ProfileSet`] (`profiles.json`, atomic writes, schema migration).
#[async_trait]
pub trait ProfileStore: Send + Sync {
    /// Load (missing file = default set with the built-in profiles).
    async fn load(&self) -> Result<ProfileSet>;

    /// Save atomically.
    async fn save(&self, set: &ProfileSet) -> Result<()>;
}

// ---------------------------------------------------------------------------
// superba-net
// ---------------------------------------------------------------------------

/// Network probes against sites and lines (plan 4 #3/#4). Never sends a key to a
/// non-https origin other than loopback; never logs keys; never follows redirects
/// on authenticated requests; sends a site's key only to that site's origins.
#[async_trait]
pub trait LineProber: Send + Sync {
    /// Unauthenticated health of one line: warm-up then timed `GET models_unauth`
    /// (expect 401 + CORS), plus whoami / wham / codex-path probes expecting the
    /// relay's 401 (detects edge interception: `l-block` header + empty 200).
    async fn probe(&self, site: &Site, line: &Line) -> Result<LineCaps>;

    /// Probe all lines of a site concurrently.
    async fn probe_all(&self, site: &Site) -> Result<Vec<LineCaps>>;

    /// Validate a key on a line: whoami (PAT) + authenticated
    /// `GET /backend-api/codex/models` (or `/codex/v1/models`), classifying access
    /// point / quota / channel errors. Costs one request of quota per call.
    async fn check_key(
        &self,
        site: &Site,
        line: &Line,
        mode: AuthMode,
        key: &Secret,
    ) -> Result<KeyCheck>;

    /// whoami with `key` against an auth API origin (plan 7.1 rule 7: the origin the
    /// running app was launched with). Must return `Error::Invalid` without sending
    /// anything when `origin` is not one of `site`'s line origins (Krill: also any
    /// Krill host), so a key never reaches a foreign origin.
    async fn check_authapi(
        &self,
        site: &Site,
        origin: &Origin,
        key: &Secret,
    ) -> Result<AuthApiHealth>;

    /// Model catalog at `openai_base_url` (`<base>/models`).
    async fn list_models(&self, openai_base_url: &Url, key: &Secret) -> Result<Vec<ModelInfo>>;

    /// The provider's model list for the Models page and catalog generation:
    /// `/backend-api/codex/models?client_version=<x.y.z>` (Codex-native entries,
    /// kept raw) on Codex backend paths, `<base>/models` (OpenAI list, metadata
    /// parsed) elsewhere. Cached per (site, base URL, key fingerprint) for 10
    /// minutes with ETag reuse; `refresh` bypasses the cache. A 401 is an invalid
    /// key; a site that does not list models is `Error::Unsupported`. Keys never
    /// reach logs or cache files.
    async fn model_list(&self, request: ModelListRequest<'_>) -> Result<ModelList>;

    /// One minimal authenticated Responses request with `model` (user-triggered
    /// health check; compatible-site validation). Costs a few tokens.
    async fn probe_responses(
        &self,
        endpoints: &ResolvedEndpoints,
        key: &Secret,
        model: &str,
    ) -> Result<ResponsesProbe>;

    /// Account card data: identity from whoami (sites with `key_check`) and balance
    /// from the standard billing endpoints (sites with `account_summary`), each
    /// feature-detected. `Ok(None)` when the site offers neither or none answered.
    /// A 401 is an error (invalid key). Implementations may cache briefly.
    async fn account_summary(
        &self,
        site: &Site,
        line: &Line,
        key: &Secret,
    ) -> Result<Option<AccountSummary>>;

    /// Daily cost and per-model usage from the organization costs / usage APIs
    /// (sites with `usage_api`), cached on disk and fetched only when the cache is
    /// stale or `query.refresh` is set. On 429 or a failed refresh the last saved
    /// report is returned with a [`crate::UsageNotice`]. `Error::Unsupported` when
    /// the site serves neither API.
    async fn usage_report(
        &self,
        site: &Site,
        line: &Line,
        key: &Secret,
        query: &UsageQuery,
    ) -> Result<UsageReport>;

    /// Responses WebSocket and zstd request-body support at
    /// `<openai_base_url>/responses` for `key`, without a billed turn: the
    /// WebSocket handshake alone (101 = supported, then closed at once; 426 or
    /// another refusal = not), and a tiny zstd-compressed invalid body compared
    /// with the same body uncompressed (the server decoded it when both get the
    /// same validation error; 415 or a parse error = not). Answers that do not
    /// tell (401, 403, 429, 5xx, no answer) are [`crate::TransportSupport::Unknown`].
    /// Sends the key only to the endpoints' own origin; never logs it.
    async fn probe_transports(
        &self,
        endpoints: &ResolvedEndpoints,
        key: &Secret,
    ) -> Result<TransportProbe> {
        let _ = (endpoints, key);
        Err(crate::Error::not_implemented("transport probe"))
    }

    /// [`LineProber::probe`] refined with one key (plan 3.2 `LineCaps` per line and
    /// key): sets `key_access` from the API-key view of the authenticated models
    /// request, and keeps `pat_ok` only if the key's PAT endpoints (whoami, accounts
    /// check) work as well. The default composes `probe` and `check_key`;
    /// implementations may need fewer requests. Costs one request of quota.
    async fn probe_with_key(&self, site: &Site, line: &Line, key: &Secret) -> Result<LineCaps> {
        let mut caps = self.probe(site, line).await?;
        if !caps.reachable {
            return Ok(caps);
        }
        if site.supports(AuthMode::ApiKey) {
            let check = self.check_key(site, line, AuthMode::ApiKey, key).await?;
            caps.key_access = Some(check.access);
        }
        if site.supports(AuthMode::Pat) && caps.pat_ok {
            let check = self.check_key(site, line, AuthMode::Pat, key).await?;
            caps.pat_ok = check.access.is_ok()
                && matches!(check.whoami, Some(crate::caps::AuthApiHealth::Healthy));
            if caps.key_access.is_none() {
                caps.key_access = Some(check.access);
            }
        }
        Ok(caps)
    }
}

// ---------------------------------------------------------------------------
// superba-sessions
// ---------------------------------------------------------------------------

/// Scans and migrates the provider of stored sessions (plan 10). Callers stop all
/// Codex processes of the home before `migrate` / `rollback` / `recover`;
/// implementations refuse when they detect a live Codex process (unless the caller
/// vouches with a [`ConsumersStopped`] token via the `*_stopped` variants), hold
/// `CodexHome::migrate_lock` and refuse if the database is busy. Backups use the sqlite online backup API + integrity_check; rollout files
/// keep their mtime; a ledger records every item.
#[async_trait]
pub trait SessionMigrator: Send + Sync {
    /// Provider distribution (sqlite threads + rollouts, archived and `.zst`).
    async fn scan(&self, home: &CodexHome) -> Result<SessionReport>;

    /// Compute a migration; never writes.
    async fn plan(&self, home: &CodexHome, request: &MigrationRequest) -> Result<MigrationPlan>;

    /// Back up, migrate, verify. On failure everything is restored before returning.
    async fn migrate(&self, home: &CodexHome, plan: &MigrationPlan) -> Result<MigrationReceipt>;

    /// Roll a migration back by its ledger (only items unchanged since).
    async fn rollback(&self, home: &CodexHome, migration_id: &str) -> Result<RollbackReport>;

    /// Past migrations of this home, newest first.
    async fn history(&self, home: &CodexHome) -> Result<Vec<MigrationReceipt>>;

    /// Roll back migrations interrupted by a crash (ledger present, not completed).
    /// Same precondition as `migrate`: all Codex processes of the home are stopped.
    async fn recover(&self, home: &CodexHome) -> Result<RollbackReport>;

    /// `migrate` after the caller stopped every Codex process of `home` itself
    /// ([`ConsumersStopped`]): the implementation skips its own process detection
    /// but keeps every other safety check. Default: same as `migrate`.
    async fn migrate_stopped(
        &self,
        home: &CodexHome,
        plan: &MigrationPlan,
        stopped: &ConsumersStopped,
    ) -> Result<MigrationReceipt> {
        let _ = stopped;
        self.migrate(home, plan).await
    }

    /// `rollback` with the caller's [`ConsumersStopped`] token. Default: same as
    /// `rollback`.
    async fn rollback_stopped(
        &self,
        home: &CodexHome,
        migration_id: &str,
        stopped: &ConsumersStopped,
    ) -> Result<RollbackReport> {
        let _ = stopped;
        self.rollback(home, migration_id).await
    }

    /// `recover` with the caller's [`ConsumersStopped`] token. Default: same as
    /// `recover`.
    async fn recover_stopped(
        &self,
        home: &CodexHome,
        stopped: &ConsumersStopped,
    ) -> Result<RollbackReport> {
        let _ = stopped;
        self.recover(home).await
    }
}

// ---------------------------------------------------------------------------
// superba-appctl
// ---------------------------------------------------------------------------

/// Detects, launches, inspects and restarts the desktop app and Codex processes
/// (plan 6, 7). CDP is used only for `Runtime.evaluate` and read-only checks, on
/// loopback, with the listener verified to be the app main process.
#[async_trait]
pub trait AppController: Send + Sync {
    /// Installed desktop apps (with bundled Codex versions).
    async fn detect_installs(&self) -> Result<Vec<AppInstall>>;

    /// Codex CLI binaries (PATH order, versions, install source).
    async fn codex_binaries(&self) -> Result<Vec<CodexBinary>>;

    /// The running app main process, with CDP info and its `CODEX_AUTHAPI_BASE_URL`.
    async fn running_session(&self) -> Result<Option<AppSession>>;

    /// Launch the app (loopback CDP, `--remote-allow-origins`, env injected) and
    /// wait until the bridge is ready or `ready_timeout_ms` passes (then the session
    /// is returned with whatever CDP state was reached).
    async fn launch(&self, options: &LaunchOptions) -> Result<AppSession>;

    /// Check that a hot restart can be sent.
    async fn probe_bridge(&self, session: &AppSession) -> Result<BridgeStatus>;

    /// Send `codex-app-server-restart` through the renderer bridge.
    async fn hot_restart(
        &self,
        session: &AppSession,
        request: &HotRestartRequest,
    ) -> Result<HotRestartOutcome>;

    /// Quit the app gracefully; force only when `options.force`.
    async fn quit(&self, session: &AppSession, options: &QuitOptions) -> Result<QuitOutcome>;

    /// Quit then launch. The switch service normally calls `quit` and `launch`
    /// separately so it can write files in between.
    async fn full_restart(
        &self,
        session: &AppSession,
        quit: &QuitOptions,
        launch: &LaunchOptions,
    ) -> Result<AppSession> {
        match self.quit(session, quit).await? {
            QuitOutcome::TimedOut => Err(crate::Error::Timeout {
                what: "app quit".into(),
            }),
            _ => self.launch(launch).await,
        }
    }

    /// Codex-related processes using `home` (app-servers, TUI, exec, daemon, IDE).
    async fn processes(&self, home: &CodexHome) -> Result<Vec<ProcessInfo>>;

    /// Busy threads of the local target (rollout tails/mtimes, renderer DOM when a
    /// session is given).
    async fn turn_activity(
        &self,
        home: &CodexHome,
        session: Option<&AppSession>,
    ) -> Result<TurnActivity>;

    /// Read-only renderer state (login page? home? open thread?).
    async fn renderer_state(&self, session: &AppSession) -> Result<RendererState>;

    /// The thread open in the renderer.
    async fn current_thread(&self, session: &AppSession) -> Result<Option<ThreadRef>> {
        Ok(self.renderer_state(session).await?.current_thread)
    }

    /// Navigate the renderer back to a thread (plan 7.5, gated by spike S6).
    async fn return_to_thread(&self, session: &AppSession, thread: &ThreadRef) -> Result<()>;

    /// Restart the CLI daemon (`app-server-control`), when supported (spike S8).
    async fn restart_daemon(&self, home: &CodexHome) -> Result<()>;

    /// Stop the CLI daemon (session migration needs every consumer stopped).
    async fn stop_daemon(&self, home: &CodexHome) -> Result<()>;

    /// Remote hosts known to the renderer (with `hostId`), read-only via CDP
    /// (plan 8.1 source 3, gated by spike S1). Merged into `RemoteHosts::discover`.
    async fn remote_connections(&self, session: &AppSession) -> Result<Vec<RemoteHost>>;

    /// Whether the "open through Superba" launcher entry is installed (Linux
    /// `.desktop` override, Windows Start-menu shortcut, macOS login item).
    async fn launcher_entry(&self) -> Result<bool>;

    /// Install or remove the launcher entry (removed again on uninstall).
    async fn set_launcher_entry(&self, enabled: bool) -> Result<()>;

    /// Process image of the app's local app-server (`/proc/<pid>/exe`,
    /// `proc_pidpath`, `QueryFullProcessImageName`); `None` when none runs.
    async fn app_server_exe(&self, session: &AppSession) -> Result<Option<PathBuf>>;

    /// The minimum app-server version the install accepts, read from its
    /// `app.asar` (cached by size and mtime); callers fall back to
    /// [`crate::codex::APP_SERVER_FLOOR_FALLBACK`] on error.
    async fn app_server_floor(&self, install: &AppInstall) -> Result<CodexVersion>;

    /// `codex debug models --bundled` (no `catalog`) or `codex debug models -c
    /// model_catalog_json="<catalog>"` with `binary`, in a private empty
    /// `CODEX_HOME` (15 s timeout, output capped): the stdout JSON. A rejected
    /// catalog is `Error::Parse` with Codex's message. Cached per binary sha256
    /// and catalog content.
    async fn codex_debug_models(&self, binary: &Path, catalog: Option<&Path>) -> Result<String>;

    /// How a Codex binary was installed (npm, Homebrew, standalone, app-bundled,
    /// Superba's store, other), from its resolved path.
    async fn cli_install_kind(&self, binary: &Path) -> Result<CodexInstallKind>;
}

// ---------------------------------------------------------------------------
// superba-remote
// ---------------------------------------------------------------------------

/// Remote SSH hosts the app is connected to (plan 8). Uses the system `ssh` with
/// `BatchMode=yes`, `ConnectTimeout=8`, never changes host key policy, sends keys
/// only over stdin, runs POSIX scripts via `sh -s`. Config bytes are produced by
/// the injected [`ConfigEngine::render`]. The remote hot restart itself is sent by
/// [`AppController::hot_restart`] with the host's `hostId`.
#[async_trait]
pub trait RemoteHosts: Send + Sync {
    /// Discover hosts: `.codex-global-state.json` of `home`, merged with hosts the
    /// renderer reported (`from_app`, via [`AppController::remote_connections`]) and
    /// live ssh links (`live`, from [`AppController::processes`]); ssh config as hints.
    async fn discover(
        &self,
        home: &CodexHome,
        from_app: &[RemoteHost],
        live: &[ProcessInfo],
    ) -> Result<Vec<RemoteHost>>;

    /// Probe a host (OS, shell, CODEX_HOME, codex version, hashes, machine id, env).
    async fn probe(&self, host: &RemoteHost) -> Result<RemoteState>;

    /// Compute the remote change for `desired` (files + PAT env); never writes.
    async fn plan_apply(&self, host: &RemoteHost, desired: &ResolvedProfile) -> Result<RemotePlan>;

    /// Write files (hash-checked `mv`, umask 077) and env, then the remote marker.
    async fn apply(&self, host: &RemoteHost, plan: &RemotePlan) -> Result<RemoteApplyReport>;

    /// Restore what `apply` wrote.
    async fn rollback(&self, host: &RemoteHost, report: &RemoteApplyReport) -> Result<()>;

    /// Busy threads on the host (3 s timeout; `complete=false` on timeout).
    async fn turn_activity(&self, host: &RemoteHost) -> Result<TurnActivity>;

    /// After the restart IPC: a new app-server (pid not in the previous list), its
    /// env, file hashes equal to what was written.
    async fn verify(
        &self,
        host: &RemoteHost,
        request: &RemoteVerifyRequest,
    ) -> Result<RemoteVerification>;

    /// [`RemoteHosts::plan_apply`] limited to one key family: `Connection` writes
    /// routing and auth keys and keeps the host's own settings; `Settings` writes
    /// the setting keys of `desired` (already filtered for the host's Codex),
    /// releases the owned setting keys `desired.key_resolutions` names without
    /// listing them (back to the host's own value) and keeps routing and auth;
    /// `Both` is `plan_apply`.
    async fn plan_apply_scoped(
        &self,
        host: &RemoteHost,
        desired: &ResolvedProfile,
        scope: RemoteApplyScope,
    ) -> Result<RemotePlan>;

    /// Install a verified Codex package on the host without sudo: streamed over
    /// the SSH session's stdin, checksum and `--version` checked remotely, release
    /// dir + `current` + `${CODEX_INSTALL_DIR:-~/.local/bin}/codex` symlink
    /// swapped, the previous release kept; free disk checked first.
    async fn install_codex(
        &self,
        host: &RemoteHost,
        package: &Path,
        version: &CodexVersion,
    ) -> Result<RemoteInstallReport>;

    /// Switch the host back to the release kept by the last install.
    async fn rollback_codex(&self, host: &RemoteHost) -> Result<RemoteInstallReport>;

    /// Read-only provider counts of the host's sessions (`session_meta` of each
    /// rollout under `sessions/` and `archived_sessions/`; bounded output).
    async fn scan_sessions(&self, host: &RemoteHost) -> Result<SessionReport>;
}

// ---------------------------------------------------------------------------
// superba-catalog (feeds)
// ---------------------------------------------------------------------------

/// Superba's signed feeds: built-in model list, prices, Codex releases, app
/// builds. Never fails for lack of network: the cache, then the compiled-in
/// snapshot, is always available.
#[async_trait]
pub trait FeedSource: Send + Sync {
    /// The feed data in use. `refresh` = check the mirrors now (when online
    /// checks are allowed), else at most daily.
    async fn snapshot(&self, refresh: bool) -> Result<FeedSnapshot>;

    /// `codex-rs/models-manager/prompt.md` of a Codex version (base instructions
    /// of generic catalog entries), when the feed has it.
    async fn prompt(&self, version: &CodexVersion) -> Result<Option<String>>;

    /// Feed state without fetching.
    async fn status(&self) -> Result<FeedsStatus>;
}

/// Input of [`LineProber::model_list`].
#[derive(Clone, Copy, Debug)]
pub struct ModelListRequest<'a> {
    /// Site (kind, endpoints, model map).
    pub site: &'a Site,
    /// `openai_base_url` of the line (`<line>/backend-api/codex`, a compatible
    /// base).
    pub base_url: &'a Url,
    /// Key.
    pub key: &'a Secret,
    /// Consumer Codex version for `client_version` (Codex backend paths).
    pub client_version: Option<&'a CodexVersion>,
    /// Bypass the cache.
    pub refresh: bool,
}

// ---------------------------------------------------------------------------
// superba-switch (consumed by the GUI and spb)
// ---------------------------------------------------------------------------

/// Receives progress events during [`SwitchService::execute`]. Must not block
/// (GUI: push to a channel and request a repaint; CLI: print).
pub trait SwitchEvents: Send + Sync {
    /// Handle one event.
    fn emit(&self, event: SwitchEvent);
}

/// An [`SwitchEvents`] sink that drops everything.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoEvents;

impl SwitchEvents for NoEvents {
    fn emit(&self, _event: SwitchEvent) {}
}

/// Answers questions that arise mid-execution (WaitingTurn, ForceQuitConfirm,
/// FullRestartOffer, Recovery, CloseConsumers). GUI: shows the dialog and awaits the
/// click. CLI: answers from flags or [`DecisionRequest::safe_default`]. Returning a
/// decision not in [`DecisionRequest::allowed`] is treated as `Cancel`.
///
/// Cancel safety: `execute` keeps a `WaitForTurns` question pending while it keeps
/// polling, and drops the future when the turns finish on their own. So the user
/// can force or cancel at any moment, and implementations must tolerate being
/// dropped (e.g. a closed oneshot). Answering `Wait` means "keep waiting quietly";
/// the question is asked again after 15 minutes.
#[async_trait]
pub trait SwitchDecider: Send + Sync {
    /// Decide.
    async fn decide(&self, request: DecisionRequest) -> Decision;
}

/// A decider that always picks the safe default.
#[derive(Debug, Default, Clone, Copy)]
pub struct SafeDefaults;

#[async_trait]
impl SwitchDecider for SafeDefaults {
    async fn decide(&self, request: DecisionRequest) -> Decision {
        request.safe_default()
    }
}

/// Application service shared by the GUI and `spb` (plan 5.2). Owns the whole
/// switch flow: plan -> (UI countdown / confirmation) -> execute -> verify.
#[async_trait]
pub trait SwitchService: Send + Sync {
    /// Overview snapshot (cheap enough to poll every few seconds).
    async fn overview(&self) -> Result<Overview>;

    /// Built-in and custom sites.
    async fn sites(&self) -> Result<Vec<Site>>;

    /// Profiles and settings.
    async fn profile_set(&self) -> Result<ProfileSet>;

    /// Create or update a profile; `key` (if given) is stored in the secret store and
    /// referenced from the profile. Returns the saved profile.
    async fn save_profile(&self, profile: Profile, key: Option<Secret>) -> Result<Profile>;

    /// Delete a non-built-in profile and its stored key.
    async fn delete_profile(&self, id: &ProfileId) -> Result<()>;

    /// Save settings.
    async fn save_settings(&self, settings: Settings) -> Result<()>;

    /// Create or update a custom (compatible) site; validates line origins.
    async fn save_site(&self, site: Site) -> Result<Site>;

    /// Delete a custom site (refused while profiles reference it).
    async fn delete_site(&self, id: &SiteId) -> Result<()>;

    /// Probe all lines of a site (cached results are refreshed).
    async fn probe_lines(&self, site_id: &SiteId) -> Result<Vec<LineCaps>>;

    /// Validate a key on a line.
    async fn check_key(
        &self,
        site_id: &SiteId,
        line_id: &LineId,
        mode: AuthMode,
        key: KeyInput,
    ) -> Result<KeyCheck>;

    /// WebSocket and zstd support of a provider's lines for its key
    /// ([`LineProber::probe_transports`]), cached per line and key with the
    /// time of the check (`<data dir>/cache/transports.json`).
    /// [`crate::TransportRefresh::Cached`] sends nothing; a fresh check of a
    /// router member also updates the local router's per-line hints. Lines the
    /// provider cannot be checked on (no key, official login) are an error.
    async fn line_transports(&self, request: LineTransportsRequest) -> Result<Vec<LineTransports>> {
        let _ = request;
        Err(crate::Error::not_implemented("line transports"))
    }

    /// Probe one line of a site: latency and reachability, and with a key the
    /// key access, model catalog (with the site's model map checked against it)
    /// and optionally one minimal Responses request. Never writes.
    async fn probe_site(&self, request: SiteProbeRequest) -> Result<SiteProbeReport>;

    /// The settings part of a plan computed by [`SwitchService::plan`]: the
    /// settings plan of every Codex home it writes and the diff preview. Available
    /// until the plan is executed.
    async fn plan_settings(&self, plan_id: &str) -> Result<PlanSettings>;

    /// Settings page rows for a profile: catalog keys of the local Codex with the
    /// current, default and managed values and availability per consumer.
    async fn settings_view(&self, request: SettingsViewRequest) -> Result<SettingsView>;

    /// Change the Codex settings a profile manages (validated against the catalog
    /// of the local Codex). Takes effect at the next switch to the profile.
    async fn edit_settings(&self, edit: SettingsEdit) -> Result<SettingsEditReport>;

    /// Router mode status (service, liveness, members, lines, whether Codex goes
    /// through the router).
    async fn router_status(&self) -> Result<RouterView>;

    /// Configure the router members (each with its own key), install and start
    /// the router service and wait until it answers. Does not change Codex's
    /// configuration.
    async fn router_install(&self, request: RouterInstallRequest) -> Result<RouterView>;

    /// Persist router mode on or off. The caller then plans a switch to the
    /// applied profile, which points Codex at the router or back to direct mode.
    async fn router_set_enabled(&self, enabled: bool) -> Result<RouterView>;

    /// Stop and remove the router service and its secrets. Refused while Codex
    /// still goes through the router (disable router mode first).
    async fn router_uninstall(&self) -> Result<RouterView>;

    /// Watchdog fallback: the router died or vanished. Turns router mode off and
    /// switches the applied profile back to direct mode (hot restart when
    /// possible, never waiting for questions). Idempotent.
    async fn router_fall_back(&self, reason: String) -> Result<Option<SwitchOutcome>>;

    /// One watchdog tick: check the router while Codex goes through it and fall
    /// back to direct mode once it stayed down past the grace period (20 s). The
    /// GUI calls this every few seconds; `spb router watch` in a loop.
    async fn router_watch(&self) -> Result<RouterWatch>;

    /// Identity, balance and (on request) usage of a profile's key from the
    /// standard account endpoints. Each part is optional and fails independently;
    /// never blocks or changes anything.
    async fn account(&self, request: AccountRequest) -> Result<AccountReport>;

    // --- v0.3: models and speed ------------------------------------------------

    /// The Models page of a profile: model list (provider, feed or snapshot, with
    /// its source), overrides, defaults, speed support and catalog file status.
    async fn models_view(&self, request: ModelsViewRequest) -> Result<ModelsView>;

    /// Save a profile's model defaults, overrides and picker mode (validated).
    /// Takes effect at the next switch to the profile; the caller plans one.
    async fn save_models(&self, edit: ModelsEdit) -> Result<ModelsEditReport>;

    // --- v0.3: remote hosts --------------------------------------------------

    /// Remote hosts the desktop app knows (never ssh-config-only suggestions),
    /// with the last probe and the per-host choices.
    async fn hosts(&self) -> Result<Vec<HostView>>;

    /// Probe one host now.
    async fn host_probe(&self, target_id: &TargetId) -> Result<HostView>;

    /// Turn "follow switches" on or off for a host (stored only; nothing is
    /// written to the host).
    async fn set_host_follow(&self, target_id: &TargetId, follow: bool) -> Result<HostView>;

    /// Preview syncing the local settings to a host, filtered by the host's Codex
    /// version (write / skip / remove / conflict rows). Executed with
    /// [`crate::SwitchIntent::HostSync`].
    async fn host_sync_plan(&self, target_id: &TargetId) -> Result<HostSyncPlan>;

    /// Sessions of one host: this computer (`local`) as [`SwitchService::sessions`],
    /// a remote host as a read-only scan.
    async fn host_sessions(&self, target_id: &TargetId) -> Result<HostSessions>;

    // --- v0.3: versions and updates -----------------------------------------

    /// The Updates page: app installed/latest, the app's Codex pin, local CLI
    /// rows, remote rows. `refresh` checks online now.
    async fn versions(&self, refresh: bool) -> Result<VersionsView>;

    /// Install a Codex version locally (standalone layout), into the store, or on
    /// a remote host (then hot restart and re-probe of that host).
    async fn install_codex(&self, request: InstallRequest) -> Result<InstallReport>;

    /// Switch a Codex install back to the release its last install replaced:
    /// the local standalone CLI ([`crate::InstallTarget::Local`]) or a remote
    /// host (then hot restart and re-probe of that host). The store has nothing
    /// to roll back.
    async fn rollback_codex(&self, target: InstallTarget) -> Result<InstallReport>;

    /// Download and verify the newest app package for this OS (Linux); the
    /// report names the file and the install command to show.
    async fn download_app_update(&self) -> Result<AppDownloadReport>;

    // --- v0.3: request log and stats ----------------------------------------

    /// Request log rows, totals and source-list groups (read-only from the
    /// router's database; works while the router is stopped).
    async fn router_requests(&self, query: RequestQuery) -> Result<RequestPage>;

    /// One request with timing and attempts (a negative id: a row read from
    /// Codex's rollout files).
    async fn router_request(&self, id: i64) -> Result<RequestDetail>;

    /// Requests in flight (router admin API; empty when it is not running).
    async fn router_inflight(&self) -> Result<Vec<InflightRequest>>;

    /// Grouped aggregates.
    async fn router_stats(&self, query: StatsQuery) -> Result<RequestStats>;

    /// Delete rows of the router's request log (Codex's session files are
    /// never changed).
    async fn router_clear(&self, request: ClearRequest) -> Result<ClearReport>;

    /// Save recording, stats line and price settings and reload the router (no
    /// Codex restart).
    async fn router_set_stats(&self, settings: RouterStatsSettings) -> Result<RouterView>;

    /// The language the UI shows (the GUI reports it at start and on every
    /// change). The default stats line template is written in it; when it
    /// changes while the line is on with the default template, a configured
    /// router gets the newly resolved template (a reload, no Codex restart).
    async fn set_ui_locale(&self, locale: crate::i18n::Locale) -> Result<()> {
        let _ = locale;
        Ok(())
    }

    /// Session names for request log keys (`prompt_cache_key` or thread id), from
    /// the local `state_5.sqlite` (title, else first user message, else the short
    /// key). Read-only.
    async fn session_titles(&self, keys: Vec<String>) -> Result<BTreeMap<String, SessionTitle>>;

    // --- v0.3: feeds ----------------------------------------------------------

    /// The public list-price table cost estimates use (the feed's; per-provider
    /// ratios and per-model overrides are in the router's stats settings).
    async fn price_table(&self) -> Result<PriceTable>;

    /// Feed state (serial, origin, last check).
    async fn feeds_status(&self) -> Result<FeedsStatus>;

    /// Check the feed mirrors now.
    async fn refresh_feeds(&self) -> Result<FeedsStatus>;

    /// Session provider distribution and migration history of the local home.
    async fn sessions(&self) -> Result<(SessionReport, Vec<MigrationReceipt>)>;

    /// Diagnostics.
    async fn doctor(&self) -> Result<Vec<DoctorFinding>>;

    /// Launch the desktop app through Superba (CDP + auth env), plan 4 #9.
    async fn open_app(&self) -> Result<AppSession>;

    /// Recover interrupted switches (call once at start-up).
    async fn recover(&self) -> Result<RecoveryReport>;

    /// Compute a plan; never writes.
    async fn plan(&self, request: SwitchRequest) -> Result<SwitchPlan>;

    /// Execute a plan after the UI's countdown / confirmation. Re-checks turns before
    /// writing and again right before the IPC; asks `decider` when needed; reports
    /// progress to `events`.
    async fn execute(
        &self,
        plan: SwitchPlan,
        options: ExecuteOptions,
        decider: Arc<dyn SwitchDecider>,
        events: Arc<dyn SwitchEvents>,
    ) -> Result<SwitchOutcome>;
}
