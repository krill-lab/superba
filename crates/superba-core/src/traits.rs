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
use crate::error::{Error, Result};
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
use crate::session_progress::SessionProgressSink;
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
    AppDownloadReport, AppDownloadRequest, CodexInstallKind, DownloadProgressSink, InstallReport,
    InstallRequest, InstallTarget, InstalledPackage, PackageSigner, VersionsView,
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

    /// The keys Superba's current generation owns in the home's `config.toml`
    /// with the values it wrote (empty = it owns nothing): the file editor
    /// marks them and names the ones an edit changes (GitHub issue 9).
    async fn owned_keys(&self, home: &CodexHome) -> Result<Vec<crate::config::KeySnapshot>> {
        let _ = home;
        Ok(Vec::new())
    }
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

    /// What makes `var` = `value` reach the desktop app Superba is about to
    /// open (user bug 69): where the app gets no per-launch environment
    /// (Windows: the packaged app takes `HKCU\Environment` at activation, and
    /// with "Run Codex in WSL" its Codex sees the variable only when `WSLENV`
    /// names it), the edits of persisting it; empty where the launch passes the
    /// value itself. Applied with [`EnvPersister::apply`].
    async fn plan_launch(&self, var: &str, value: &str) -> Result<EnvPlan> {
        let _ = (var, value);
        Err(Error::not_implemented("EnvPersister::plan_launch"))
    }

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

    /// Claude Code's endpoint check: one `POST <endpoint>/v1/messages` with
    /// `max_tokens: 1` to `model`, the key sent the endpoint's way plus
    /// `anthropic-version`. The caller vouches that the endpoint may receive
    /// the key (it is the provider's own). Costs a token.
    async fn probe_messages(
        &self,
        request: MessagesProbeRequest<'_>,
    ) -> Result<crate::claude::ClaudeProbe> {
        let _ = request;
        Err(Error::NotImplemented {
            what: "the Anthropic endpoint check".into(),
        })
    }

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

    /// Codex remote compaction at `<openai_base_url>/responses` for `key` and
    /// `model` ([`crate::compaction`]): one streamed request with a two-message
    /// conversation and a `compaction_trigger` item, as Codex sends it (costs a
    /// few tokens). Exactly one `compaction` output item = native; a refusal
    /// that names the item or an answer without a compaction item =
    /// unsupported; answers that do not tell (401, 403, 429, 5xx, a refused
    /// model, no answer) = unknown. Never on a switch's path: only explicit
    /// checks call it. Sends the key only to the endpoints' own origin.
    async fn probe_compaction(
        &self,
        endpoints: &ResolvedEndpoints,
        key: &Secret,
        model: &str,
    ) -> Result<crate::CompactionCheck> {
        let _ = (endpoints, key, model);
        Err(crate::Error::not_implemented("compaction probe"))
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

    /// `migrate_stopped` reporting its progress to `progress`. When the sink
    /// asks to stop while [`crate::SessionProgress::can_stop`], everything done
    /// so far is undone and the call fails with [`crate::Error::Cancelled`].
    /// Default: `migrate_stopped` without reports.
    async fn migrate_stopped_with_progress(
        &self,
        home: &CodexHome,
        plan: &MigrationPlan,
        stopped: &ConsumersStopped,
        progress: Arc<dyn SessionProgressSink>,
    ) -> Result<MigrationReceipt> {
        let _ = progress;
        self.migrate_stopped(home, plan, stopped).await
    }

    /// `rollback_stopped` reporting its progress to `progress` (a rollback never
    /// stops half-way). Default: `rollback_stopped` without reports.
    async fn rollback_stopped_with_progress(
        &self,
        home: &CodexHome,
        migration_id: &str,
        stopped: &ConsumersStopped,
        progress: Arc<dyn SessionProgressSink>,
    ) -> Result<RollbackReport> {
        let _ = progress;
        self.rollback_stopped(home, migration_id, stopped).await
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

    /// Bring the running app to the front the way its own icon does (a running
    /// app activated again shows its window; no arguments, no debugging port), for
    /// "Open ChatGPT with Superba" while it already runs (GH#7). `NotImplemented`
    /// where that cannot be done without starting a second instance.
    async fn activate(&self, session: &AppSession) -> Result<()> {
        let _ = session;
        Err(crate::Error::NotImplemented {
            what: "bringing the desktop app to the front".into(),
        })
    }

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

    /// After a quit of `session` that timed out: what that quit did and which
    /// of the app's processes still run (user bug 53). Default: nothing
    /// recorded, the main process listed while it runs.
    async fn quit_report(&self, session: &AppSession) -> Result<crate::QuitReport> {
        Ok(crate::QuitReport {
            steps: Vec::new(),
            left: vec![crate::LingeringProcess {
                pid: session.main_pid,
                image: session.executable.clone(),
                role: "main".into(),
                started_at: session.started_at,
                ..crate::LingeringProcess::default()
            }],
        })
    }

    /// End exactly the listed processes of the app (the user's "end these
    /// processes" after [`Self::quit_report`]): each only while it is still
    /// the process listed (same pid and start time), its child processes
    /// with it. Returns the report afterwards (what still runs, with why it
    /// could not be ended). Default: not implemented.
    async fn end_app_processes(
        &self,
        session: &AppSession,
        processes: &[crate::LingeringProcess],
    ) -> Result<crate::QuitReport> {
        let _ = (session, processes);
        Err(crate::Error::NotImplemented {
            what: "ending the desktop app's processes".into(),
        })
    }

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

    /// Windows: the MSIX / APPX packages registered for this user (read from the
    /// registry), so framework dependencies already installed are not
    /// downloaded. Empty elsewhere.
    async fn installed_packages(&self) -> Result<Vec<InstalledPackage>> {
        Ok(Vec::new())
    }

    /// Windows: check the signature of a downloaded MSIX / APPX package with
    /// `WinVerifyTrust` (chain to a trusted root, timestamp honoured) and return
    /// its signer. The caller decides whether that signer is acceptable.
    async fn verify_package_signature(&self, path: &Path) -> Result<PackageSigner> {
        let _ = path;
        Err(Error::Unsupported {
            what: "checking package signatures on this platform".into(),
        })
    }

    /// Windows: open a package with its default handler (`ShellExecute` "open":
    /// App Installer), where the user installs it. Never installs silently and
    /// never elevates.
    async fn open_package_installer(&self, path: &Path) -> Result<()> {
        let _ = path;
        Err(Error::Unsupported {
            what: "opening package installers on this platform".into(),
        })
    }

    /// [`Self::running_session`] and [`Self::processes`] from one look at the
    /// process table: the overview's fast facts, read every few seconds. Spawns
    /// no process. Default: the two calls.
    async fn session_and_processes(
        &self,
        home: &CodexHome,
    ) -> (Result<Option<AppSession>>, Result<Vec<ProcessInfo>>) {
        (self.running_session().await, self.processes(home).await)
    }

    /// A cheap fingerprint of what [`Self::detect_installs`] and
    /// [`Self::codex_binaries`] found and where they look: existence, size and
    /// modification time of the install locations and the directories installs
    /// are discovered in, the PATH directories, the standalone Codex layout in
    /// the Codex home, and `installs` / `binaries` themselves (on Windows also
    /// the MSIX package registry). Reads metadata
    /// only: no process, no directory listing. The environment monitor of the
    /// switch service detects again when it changes. Default: `None` (no cheap
    /// check; the monitor relies on its events and long interval).
    fn environment_stamp(&self, installs: &[AppInstall], binaries: &[CodexBinary]) -> Option<u64> {
        let _ = (installs, binaries);
        None
    }

    /// Forget cached probe results (Codex `--version` answers, package queries,
    /// CDP listener checks) so the next detection asks again. Called on an
    /// explicit refresh, never on a timer. Default: nothing is cached.
    fn forget_cached_probes(&self) {}

    /// Stop one Codex consumer process the user asked Superba to stop (a codex
    /// CLI session, `codex exec`, an IDE extension's or another app-server):
    /// without `force` ask it to exit (SIGTERM on Unix) and wait up to
    /// `timeout_ms`; Windows cannot ask a console program or a pipe app-server
    /// to exit, so it answers [`crate::StopOutcome::ForceOnly`] without
    /// touching it. With `force` end it and its child processes (SIGKILL /
    /// `TerminateProcess`). Refuses the desktop app (use [`Self::quit`]) and a
    /// pid whose start time no longer matches `process.started_at` (reused by
    /// another program: [`crate::StopOutcome::NotRunning`]). Callers only pass
    /// processes the user saw listed and agreed to stop. A forced stop names
    /// what still runs afterwards ([`crate::StopReport::left`]). Default: not
    /// implemented.
    async fn stop_process(
        &self,
        process: &ProcessInfo,
        force: bool,
        timeout_ms: u64,
    ) -> Result<crate::runtime::StopReport> {
        let _ = (process, force, timeout_ms);
        Err(Error::NotImplemented {
            what: "stopping Codex processes".into(),
        })
    }
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

    /// Run Superba's Linux helper `helper` (a local file) inside a WSL
    /// distribution with `args`: copied into the distribution once per checksum
    /// (`~/.local/share/superba/wsl-helper/<sha256>/spb`, verified there), run as
    /// the distribution's user with the host's `CODEX_HOME`, its output lines
    /// handed to `sink` as they arrive.
    async fn run_helper(
        &self,
        host: &RemoteHost,
        helper: &Path,
        args: &[String],
        sink: crate::remote::LineSink,
    ) -> Result<crate::remote::HelperRun> {
        let _ = (host, helper, args, sink);
        Err(Error::not_implemented("RemoteHosts::run_helper"))
    }

    /// WSL on this computer: the configured networking mode and why WSL could
    /// not be listed, if it could not (user bug 45).
    async fn wsl_status(&self) -> Result<(crate::wsl::WslNetworking, Option<String>)> {
        Err(Error::not_implemented("RemoteHosts::wsl_status"))
    }

    /// Ask Windows which WSL distributions run now (`wsl.exe --list --running`,
    /// which never starts one; user bug 54). Later [`RemoteHosts::discover`]
    /// results carry the answer ([`crate::wsl::WslDistro::running`]) until it
    /// is asked again; discovery itself never starts a process. `Ok(None)` when
    /// WSL cannot tell.
    async fn wsl_running(&self) -> Result<Option<Vec<String>>> {
        Err(Error::not_implemented("RemoteHosts::wsl_running"))
    }

    /// Superba has written this host (its baseline is kept locally): it has
    /// something to restore. Read locally; never reaches the host.
    async fn written(&self, host: &RemoteHost) -> Result<bool> {
        let _ = host;
        Ok(false)
    }

    /// Forget cached discovery (WSL distributions listed, which of them run), so
    /// the next call reads it again. Called on an explicit refresh.
    fn forget_cached(&self) {}

    /// `GET http://<address>:<port><path>` from inside the host (a WSL
    /// distribution checks that Superba's router answers it there, user bug 45):
    /// the response (status line, headers, body; at most 4 KiB), `Ok(None)` when
    /// nothing accepted the connection within a few seconds.
    async fn http_get(
        &self,
        host: &RemoteHost,
        address: &str,
        port: u16,
        path: &str,
    ) -> Result<Option<String>> {
        let _ = (host, address, port, path);
        Err(Error::not_implemented("RemoteHosts::http_get"))
    }

    /// Read `config.toml` or `auth.json` of the host for the file editor
    /// (GitHub issue 9), with the host's Codex version and the keys Superba
    /// owns there.
    async fn read_codex_file(
        &self,
        host: &RemoteHost,
        kind: crate::config::CodexFileKind,
    ) -> Result<crate::remote::RemoteCodexFile> {
        let _ = (host, kind);
        Err(Error::not_implemented("RemoteHosts::read_codex_file"))
    }

    /// Replace one Codex file of the host with an edit: under the host's write
    /// lock, only when the file still holds `write.expect` (compare and swap;
    /// [`Error::Conflict`] otherwise), keeping its mode (`auth.json` 0600).
    async fn write_codex_file(
        &self,
        host: &RemoteHost,
        write: &crate::remote::RemoteFileWrite,
    ) -> Result<()> {
        let _ = (host, write);
        Err(Error::not_implemented("RemoteHosts::write_codex_file"))
    }
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
    /// Which lists to read: `None` = the one `base_url` serves (the Codex
    /// list on a Codex backend path, else `<base>/models`), as before
    /// explicit sources; `Some` reads the Codex and / or the standard list
    /// and reports each ([`ModelList::sources`], [`crate::ModelSpec::lists`]).
    pub source: Option<crate::models::ModelSource>,
    /// The standard list's base when it is not derived from the site: Claude
    /// Code's Anthropic endpoint (`<base>/v1/models`, Anthropic or OpenAI
    /// shape, asked with `x-api-key` and `anthropic-version` as well). The
    /// caller vouches that this origin may receive the key.
    pub standard_base: Option<&'a Url>,
}

/// [`LineProber::probe_messages`]: one minimal Anthropic Messages request.
#[derive(Clone, Copy, Debug)]
pub struct MessagesProbeRequest<'a> {
    /// The endpoint.
    pub endpoint: &'a crate::claude::AnthropicEndpoint,
    /// The key.
    pub key: &'a Secret,
    /// Model asked.
    pub model: &'a str,
}

// ---------------------------------------------------------------------------
// superba-switch (consumed by the GUI and spb)
// ---------------------------------------------------------------------------

/// Receives progress events during [`SwitchService::execute`]. Must not block
/// (GUI: push to a channel and request a repaint; CLI: print).
pub trait SwitchEvents: Send + Sync {
    /// Handle one event.
    fn emit(&self, event: SwitchEvent);

    /// The user asked to stop the session migration in progress. Polled while
    /// sessions move; honoured only while the latest
    /// [`SwitchEvent::Sessions`] report says
    /// [`can_stop`](crate::SessionProgress::can_stop) (everything is then
    /// undone and the switch ends as cancelled). Default: never.
    fn stop_requested(&self) -> bool {
        false
    }
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

    /// Save settings. The notification channels are kept as stored (they
    /// change through [`SwitchService::notify_save_channel`] /
    /// [`SwitchService::notify_delete_channel`] only).
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

    /// Remote compaction of a provider ([`crate::ProviderCompaction`]): the
    /// last check (cached in `<data dir>/cache/compaction.json` per site, base
    /// URL and key fingerprint), the profile's choice and the effective value.
    /// [`crate::TransportRefresh::Cached`] sends nothing; a check costs a few
    /// tokens ([`LineProber::probe_compaction`]). A changed effective value of
    /// a router member updates the local router (a reload).
    async fn provider_compaction(
        &self,
        request: crate::CompactionRequest,
    ) -> Result<crate::ProviderCompaction> {
        let _ = request;
        Err(crate::Error::not_implemented("provider compaction"))
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

    /// Everything about the local router for a support request: the program
    /// and its macOS state (quarantine, translocation, signature), the service
    /// definition and the service manager's view (`launchctl print` on macOS),
    /// configuration and keys (present or missing only), port and health, the
    /// end of the log, which providers can be members and why, and the
    /// problems found. Read-only; never a key or token.
    async fn router_doctor(&self) -> Result<crate::RouterDoctorReport> {
        Err(crate::Error::not_implemented("router doctor"))
    }

    /// Identity, balance and (on request) usage of a profile's key from the
    /// standard account endpoints. Each part is optional and fails independently;
    /// never blocks or changes anything.
    async fn account(&self, request: AccountRequest) -> Result<AccountReport>;

    // --- Superba's own update ----------------------------------------------

    /// The cached self-update state (never touches the network).
    async fn self_update_view(&self) -> Result<crate::SelfUpdateView> {
        Err(crate::Error::not_implemented("self update"))
    }

    /// Check the release page (ETag cached, short timeout). Automatic checks
    /// (`manual` false) do nothing while the preference is off or the cached
    /// answer is under 24 h old. A private or missing release page ends in
    /// [`crate::SelfUpdateStatus::PageUnavailable`], never in an error.
    async fn self_update_check(
        &self,
        request: crate::SelfUpdateCheckRequest,
    ) -> Result<crate::SelfUpdateView> {
        let _ = request;
        Err(crate::Error::not_implemented("self update check"))
    }

    /// Skip a version (`Some`) or clear the skip (`None`); persisted.
    async fn self_update_skip(&self, version: Option<String>) -> Result<crate::SelfUpdateView> {
        let _ = version;
        Err(crate::Error::not_implemented("self update skip"))
    }

    /// Download the offered release for this OS and architecture, verify it
    /// against the `SHA256SUMS.txt` of the same release and replace Superba in
    /// place (macOS bundle, Windows hand-off, Linux binaries). Only ever called
    /// after the user chose to install.
    async fn self_update_install(
        &self,
        request: crate::SelfUpdateInstallRequest,
    ) -> Result<crate::SelfUpdateReport> {
        let _ = request;
        Err(crate::Error::not_implemented("self update install"))
    }

    /// Mark the last attempt's problem ([`crate::SelfUpdateView::problem`]) as
    /// shown, so the next start does not announce it again; it stays on the
    /// Updates page until the next attempt.
    async fn self_update_acknowledge(&self) -> Result<crate::SelfUpdateView> {
        Err(crate::Error::not_implemented("self update acknowledge"))
    }

    /// Restart the local router service when it is installed and running, so it
    /// runs the binary on disk (the last step of an update; `spb self-update
    /// restart-router` runs it for the Windows hand-off). `false` = nothing to
    /// restart.
    async fn self_update_restart_router(&self) -> Result<bool> {
        Err(crate::Error::not_implemented("self update router restart"))
    }

    // --- provider import from other tools ----------------------------------------

    /// Look for the configurations of other tools (cc-switch, AI Toolbox) and
    /// preview their providers as Superba would import them: target (Krill
    /// preset or a custom site), key present or not, conflicts with existing
    /// providers. Reads the other tools' files only; writes nothing.
    async fn import_scan(&self, request: crate::ImportScanRequest) -> Result<crate::ImportScan> {
        let _ = request;
        Err(crate::Error::not_implemented("provider import scan"))
    }

    /// Import the selected providers from a fresh scan of their sources: new
    /// custom sites, profiles and keys (into Superba's key storage), or updates
    /// of existing providers where chosen. Nothing is written to Codex and
    /// nothing restarts; with `dry_run` nothing is written at all.
    async fn import_apply(
        &self,
        request: crate::ImportApplyRequest,
    ) -> Result<crate::ImportReport> {
        let _ = request;
        Err(crate::Error::not_implemented("provider import"))
    }

    /// Import (or with `dry_run` preview, or with `remove` take out again) the
    /// usage history another tool (cc-switch, AI Toolbox) recorded for Codex
    /// into Superba's own store (`<data dir>/imported-usage.sqlite3`): its
    /// requests by their own ids (a second import adds only what is new),
    /// recorded costs and provider names, and the daily totals of days it
    /// keeps only as totals. Rows the tool read from Codex's session files are
    /// skipped where Superba reads those files itself. The tool's files are
    /// read only; Codex is not touched.
    async fn import_usage(
        &self,
        request: crate::UsageImportRequest,
    ) -> Result<crate::UsageImportReport> {
        let _ = request;
        Err(crate::Error::not_implemented("usage import"))
    }

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

    /// [`SwitchService::hosts`] after looking again at what changes without
    /// Superba (user bug 68): Windows' list of WSL distributions and which of
    /// them run now (`wsl.exe --list --running`, which starts none). For an
    /// explicit refresh, a visit of the page and after a switch; `hosts` itself
    /// starts no process (the GUI lists hosts on a timer).
    async fn hosts_now(&self) -> Result<Vec<HostView>> {
        self.hosts().await
    }

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

    /// WSL on this computer: networking mode, distributions (as hosts, with the
    /// last probe) and the router's WSL listeners (user bug 45). Empty outside
    /// Windows.
    async fn wsl_overview(&self) -> Result<crate::wsl::WslOverview> {
        Err(Error::not_implemented("SwitchService::wsl_overview"))
    }

    /// Migrate the sessions of a WSL distribution (`request` like
    /// [`crate::SwitchIntent::MigrateSessions`]) with Superba's Linux helper
    /// inside the distribution, which takes the same backups, locks and process
    /// checks as on this computer; `events` gets its progress lines as
    /// [`crate::SwitchEvent::Log`], in `locale` like its errors.
    async fn host_migrate_sessions(
        &self,
        target_id: &TargetId,
        request: &crate::sessions::MigrationRequest,
        locale: crate::Locale,
        events: Arc<dyn SwitchEvents>,
    ) -> Result<crate::remote::HostMigrationReport> {
        let _ = (target_id, request, locale, events);
        Err(Error::not_implemented(
            "SwitchService::host_migrate_sessions",
        ))
    }

    /// Let a WSL distribution reach the local router: on WSL 1 / mirrored
    /// networking a check of `127.0.0.1`; behind WSL 2 NAT the router also
    /// listens on the Windows host's WSL adapter address and, when Windows
    /// Firewall blocks it, Superba adds its firewall rule (asks for
    /// administrator approval once). Applies at the next switch.
    async fn wsl_router_access(&self, target_id: &TargetId) -> Result<crate::wsl::WslAccessReport> {
        let _ = target_id;
        Err(Error::not_implemented("SwitchService::wsl_router_access"))
    }

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

    /// Download and verify the newest app package for this OS, reporting to
    /// `progress` (a cancel there keeps the partial file for resuming). Windows:
    /// the MSIX from the source of `request` (else the network setting), with
    /// its manifest identity, the framework packages it still needs and every
    /// signature checked. Elsewhere [`SwitchService::download_app_update`].
    async fn download_app_package(
        &self,
        request: AppDownloadRequest,
        progress: Arc<dyn DownloadProgressSink>,
    ) -> Result<AppDownloadReport> {
        let _ = (request, progress);
        self.download_app_update().await
    }

    /// Windows: open a package downloaded by
    /// [`SwitchService::download_app_package`] with App Installer (its
    /// signature is checked again first); the user installs it. Never installs
    /// silently and never elevates.
    async fn open_app_package(&self, path: PathBuf) -> Result<()> {
        let _ = path;
        Err(Error::not_implemented("SwitchService::open_app_package"))
    }

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

    /// Tell the environment monitor about the window or an explicit refresh
    /// ([`crate::EnvironmentEvent`]). Cheap and non-blocking: the monitor acts
    /// on its own thread. Default: nothing (no monitor).
    fn environment_event(&self, event: crate::app::EnvironmentEvent) {
        let _ = event;
    }

    /// Bumped each time the environment monitor's facts (installs, Codex
    /// binaries, versions) change; a UI reloads the views built from them when
    /// it sees a new value. Default: 0.
    fn environment_generation(&self) -> u64 {
        0
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

    /// The configuration written last versus what every running Codex
    /// consumer of the local home uses, with running turns checked (slower
    /// than [`Self::overview`], which carries the same view without them).
    async fn runtime(&self) -> Result<crate::runtime::RuntimeView> {
        Err(Error::NotImplemented {
            what: "runtime state".into(),
        })
    }

    /// Stop Codex consumer processes the user picked (the runtime view's
    /// "Stop" offer): only processes that are still listed for the local home
    /// with the same pid, kind and start time are touched; without `force`
    /// each is asked to exit ([`AppController::stop_process`]), with `force`
    /// ended. The desktop app is never stopped here.
    async fn stop_processes(
        &self,
        processes: Vec<ProcessInfo>,
        force: bool,
    ) -> Result<Vec<crate::runtime::StopReport>> {
        let _ = (processes, force);
        Err(Error::NotImplemented {
            what: "stopping Codex processes".into(),
        })
    }

    /// Open `config.toml` or `auth.json` of this computer (`TargetId::local()`),
    /// a remote host or a WSL distribution for editing (GitHub issue 9): the
    /// bytes, their hash, what the catalog check needs and the keys Superba
    /// manages there.
    async fn codex_file(
        &self,
        target: TargetId,
        kind: crate::config::CodexFileKind,
    ) -> Result<crate::codex_files::CodexFileDoc> {
        let _ = (target, kind);
        Err(Error::NotImplemented {
            what: "editing Codex files".into(),
        })
    }

    /// Save an edited Codex file: refused while a switch runs or did not
    /// finish, when the file changed since it was opened, when it does not
    /// parse, or (without `force`) when the catalog check finds errors. The
    /// file it replaces is backed up first (encrypted). The edit is the user's
    /// own change: Superba never reverts it by itself (GH#5).
    async fn save_codex_file(
        &self,
        save: crate::codex_files::CodexFileSave,
    ) -> Result<crate::codex_files::CodexFileSaved> {
        let _ = save;
        Err(Error::NotImplemented {
            what: "editing Codex files".into(),
        })
    }

    /// Backups of the Codex files, newest first: the editor's own, the
    /// pre-images of Superba's switches and the originals (local home).
    async fn codex_file_backups(
        &self,
        query: crate::codex_files::CodexFileBackupQuery,
    ) -> Result<Vec<crate::codex_files::CodexFileBackup>> {
        let _ = query;
        Err(Error::NotImplemented {
            what: "editing Codex files".into(),
        })
    }

    /// One backup, decrypted, for a restore (saved like an edit with
    /// [`crate::codex_files::SaveReason::Restore`]).
    async fn codex_file_backup(
        &self,
        id: &str,
    ) -> Result<crate::codex_files::CodexFileBackupContents> {
        let _ = id;
        Err(Error::NotImplemented {
            what: "editing Codex files".into(),
        })
    }

    /// Claude Code (user bug 79): the state of its two pages (where its
    /// `settings.json` is, the provider and endpoint it uses, what Superba
    /// applied, what is not applied yet).
    async fn claude_view(&self) -> Result<crate::claude::ClaudeView> {
        Err(Error::not_implemented("Claude Code"))
    }

    /// Save Superba's Claude Code configuration (provider, route, models per
    /// provider). Writes no Claude Code file: [`Self::claude_plan`] /
    /// [`Self::claude_apply`] do.
    async fn save_claude_config(
        &self,
        config: crate::claude::ClaudeConfig,
    ) -> Result<crate::claude::ClaudeView> {
        let _ = config;
        Err(Error::not_implemented("Claude Code"))
    }

    /// A provider's models for Claude Code: its Codex list and its standard
    /// list at the Anthropic endpoint, merged, plus the ids added by hand.
    async fn claude_models(
        &self,
        request: crate::claude::ClaudeModelsRequest,
    ) -> Result<crate::claude::ClaudeModelsView> {
        let _ = request;
        Err(Error::not_implemented("Claude Code"))
    }

    /// Plan writing the keys Superba owns in `settings.json` for the current
    /// configuration, or restoring what they held before Superba. Never
    /// writes.
    async fn claude_plan(
        &self,
        kind: crate::claude::ClaudePlanKind,
    ) -> Result<crate::claude::ClaudePlan> {
        let _ = kind;
        Err(Error::not_implemented("Claude Code"))
    }

    /// Execute a plan of [`Self::claude_plan`]: refused when the file changed
    /// since it was planned; keys someone changed after Superba wrote them
    /// are written only with `take_over` (else kept and no longer owned);
    /// every other key of the file is kept; the replaced file is backed up
    /// first (the Claude Code editor's backups).
    async fn claude_apply(
        &self,
        plan: crate::claude::ClaudePlan,
        take_over: bool,
    ) -> Result<crate::claude::ClaudeApplyReport> {
        let _ = (plan, take_over);
        Err(Error::not_implemented("Claude Code"))
    }

    /// Diagnostics of Claude Code: the program, `settings.json`, managed
    /// settings that override Superba's keys, conflicting environment
    /// variables, a claude.ai login, `apiKeyHelper`.
    async fn claude_diagnostics(&self) -> Result<crate::claude::ClaudeDiagnostics> {
        Err(Error::not_implemented("Claude Code"))
    }

    /// Check the endpoint with one 1-token request to `model` (default: the
    /// configured main model, else the first slot).
    async fn claude_probe(&self, model: Option<String>) -> Result<crate::claude::ClaudeProbe> {
        let _ = model;
        Err(Error::not_implemented("Claude Code"))
    }

    /// Notifications (GH#12): save a channel and its secret fields. Secret
    /// fields not in `secrets` keep their stored value (an edit that leaves a
    /// token empty keeps it); an empty channel id gets a new one. Returns the
    /// saved channel (with its key reference and masked hints).
    async fn notify_save_channel(
        &self,
        channel: crate::NotifyChannel,
        secrets: crate::NotifySecrets,
    ) -> Result<crate::NotifyChannel> {
        let _ = (channel, secrets);
        Err(Error::NotImplemented {
            what: "notifications".into(),
        })
    }

    /// Delete a channel and its stored secret.
    async fn notify_delete_channel(&self, id: &str) -> Result<()> {
        let _ = id;
        Err(Error::NotImplemented {
            what: "notifications".into(),
        })
    }

    /// Send `event` to the channels (all, or [`crate::SendOptions::channel`])
    /// under the preferences' triggers, quiet hours, rate limit and
    /// deduplication unless [`crate::SendOptions::force`]. Never fails because
    /// a channel failed: the report says which did.
    async fn notify_send(
        &self,
        event: crate::NotifyEvent,
        options: crate::SendOptions,
    ) -> Result<crate::NotifyReport> {
        let _ = (event, options);
        Err(Error::NotImplemented {
            what: "notifications".into(),
        })
    }

    /// A harness hook fired: `payload` is what the harness passed (Codex
    /// `notify`: the JSON argument; Claude Code / Codex hooks: stdin). The
    /// service parses it, adds what the local files know (duration, tokens,
    /// title) and sends it like [`SwitchService::notify_send`].
    async fn notify_hook(&self, harness: &str, payload: &str) -> Result<crate::NotifyReport> {
        let _ = (harness, payload);
        Err(Error::NotImplemented {
            what: "notifications".into(),
        })
    }

    /// The harness hooks that report events and their state.
    async fn notify_hooks(&self) -> Result<crate::NotifyHooks> {
        Err(Error::NotImplemented {
            what: "notifications".into(),
        })
    }

    /// Install or remove a harness hook (Codex `notify` in `config.toml`, with
    /// a backup; a program the user had there keeps running through Superba's
    /// hook and comes back on removal).
    async fn notify_set_hook(&self, request: crate::HookRequest) -> Result<crate::NotifyHooks> {
        let _ = request;
        Err(Error::NotImplemented {
            what: "notifications".into(),
        })
    }

    /// Look for events no hook reports (a Codex turn that ended with an
    /// error is only in its rollout file) and send them; the GUI calls it on
    /// a timer while it runs.
    async fn notify_watch(&self) -> Result<crate::NotifyReport> {
        Ok(crate::NotifyReport::default())
    }

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

    // -- other coding-agent harnesses (dsh, opencode, pi, ZCode, WorkBuddy) --

    /// Every harness: found or not, what Superba applied there.
    async fn harness_list(&self) -> Result<Vec<crate::harness::HarnessSummary>> {
        Err(Error::not_implemented("harnesses"))
    }

    /// One harness: its files, Superba's settings, the endpoint an apply
    /// would write, what Superba applied and what is not applied yet.
    async fn harness_view(
        &self,
        harness: crate::harness::HarnessId,
    ) -> Result<crate::harness::HarnessView> {
        let _ = harness;
        Err(Error::not_implemented("harnesses"))
    }

    /// Save Superba's settings for a harness (provider, protocol, models,
    /// route). Writes no harness file: [`Self::harness_plan`] /
    /// [`Self::harness_apply`] do.
    async fn save_harness_settings(
        &self,
        harness: crate::harness::HarnessId,
        settings: crate::harness::HarnessSettings,
    ) -> Result<crate::harness::HarnessView> {
        let _ = (harness, settings);
        Err(Error::not_implemented("harnesses"))
    }

    /// Declare the Chat Completions / Anthropic endpoints of a provider that
    /// does not have them built in (empty = forget them).
    async fn save_provider_protocols(
        &self,
        provider: ProfileId,
        protocols: crate::harness::ProviderProtocols,
    ) -> Result<()> {
        let _ = (provider, protocols);
        Err(Error::not_implemented("harnesses"))
    }

    /// Store (or with `None` delete) a harness's router token in the key
    /// store; routed mode writes it as the harness's key.
    async fn set_harness_router_token(
        &self,
        harness: crate::harness::HarnessId,
        token: Option<Secret>,
    ) -> Result<crate::harness::HarnessView> {
        let _ = (harness, token);
        Err(Error::not_implemented("harnesses"))
    }

    /// Plan writing Superba's provider into a harness for its settings, or
    /// restoring what the owned entries held before Superba. Never writes.
    async fn harness_plan(
        &self,
        harness: crate::harness::HarnessId,
        kind: crate::harness::HarnessPlanKind,
    ) -> Result<crate::harness::HarnessPlan> {
        let _ = (harness, kind);
        Err(Error::not_implemented("harnesses"))
    }

    /// Execute a plan of [`Self::harness_plan`]: refused when a file changed
    /// since it was planned; entries someone changed after Superba wrote them
    /// are written only with `take_over` (else kept); every other entry and
    /// comment of the files is kept; each replaced file is backed up first.
    async fn harness_apply(
        &self,
        plan: crate::harness::HarnessPlan,
        take_over: bool,
    ) -> Result<crate::harness::HarnessApplyReport> {
        let _ = (plan, take_over);
        Err(Error::not_implemented("harnesses"))
    }

    /// The encrypted copies of a harness's files Superba took before writing
    /// them, newest first.
    async fn harness_backups(
        &self,
        harness: crate::harness::HarnessId,
    ) -> Result<Vec<crate::harness::HarnessBackup>> {
        let _ = harness;
        Err(Error::not_implemented("harnesses"))
    }

    /// Put one backed-up file back as a whole (after backing up what is
    /// there). Superba then owns nothing in that file any more.
    async fn harness_restore_backup(
        &self,
        harness: crate::harness::HarnessId,
        id: String,
    ) -> Result<crate::harness::HarnessApplyReport> {
        let _ = (harness, id);
        Err(Error::not_implemented("harnesses"))
    }
}
