//! # superba-switch
//!
//! The application service shared by the GUI and `spb`. [`Switcher`] implements
//! [`SwitchService`] on top of the component traits:
//!
//! - planning ([`planner`], plan 5.2): version gates (plan 9.8 table in [`rules`]),
//!   key x line checks and `LineCaps`, model catalog check, pending journal,
//!   session distribution (takeover / reverse migration), app session (CDP, bridge,
//!   `CODEX_AUTHAPI_BASE_URL` and its whoami health), auth family change, local and
//!   remote turn activity -> [`superba_core::SwitchPlan`] with the restart decision
//!   of the plan 7.1 table ([`rules::decide_local`]);
//! - execution ([`executor`], plan 7.2 states after the UI's countdown /
//!   confirmation): TurnCheck, WaitingTurn, Quitting, Applying (baseline ->
//!   journaled config -> env -> migration), Restarting (turns re-checked right
//!   before the IPC), Launching, Verifying (process, hashes, whoami/models,
//!   renderer), NavRestore, Recovery, RollingBack; remote hosts in parallel;
//! - start-up recovery, overview, doctor, profiles and sites ([`service`]);
//! - account data of a profile (identity, balance, usage; `SwitchService::account`);
//! - provider import from other tools ([`superba_import`]: cc-switch, AI Toolbox);
//! - the UI state machine ([`machine`]) and the shared wording ([`messages`]).
//!
//! Construction: [`detect_context`] resolves the real home / data dir once;
//! [`SwitcherDeps::with_defaults`] wires the production implementations; tests
//! pass fakes through [`SwitcherDeps`] directly.

mod account;
pub mod ciphers;
pub mod claude;
mod claude_router;
pub mod compaction;
pub mod core_watch;
pub mod executor;
mod files;
pub mod harness;
pub mod hosts;
mod import;
pub mod journal;
pub mod localtime;
pub mod machine;
pub mod messages;
pub mod model_keep;
pub mod models;
pub mod monitor;
pub mod notify;
pub mod ownership;
pub mod planner;
pub mod privilege;
pub mod router;
pub mod router_doctor;
pub mod rules;
pub mod runtime;
mod selfupdate;
pub mod service;
pub mod session_events;
mod settings;
mod sites;
mod transports;
pub mod usage;
pub mod usage_import;
pub mod versions;
pub mod winapp;
mod wsl;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use superba_core::{
    AccountReport, AccountRequest, AppController, AppSession, AuthMode, Clock, ConfigEngine,
    DataDir, DoctorFinding, EnvPersister, EnvSource, Error, ExecuteOptions, KeyCheck, KeyInput,
    LineCaps, LineId, LineProber, LocalContext, MigrationReceipt, Overview, ProcessEnv, Profile,
    ProfileId, ProfileSet, ProfileStore, RecoveryReport, RemoteHosts, Result, Secret, SecretStore,
    SessionMigrator, SessionReport, Settings, Site, SiteId, SwitchDecider, SwitchEvents,
    SwitchOutcome, SwitchPlan, SwitchRequest, SwitchService, SystemClock,
    codex::ENV_SUPERBA_DATA_DIR,
};

pub use machine::{Effect, Input, MachineState, PlanTicket, SwitchMachine};
pub use planner::ENV_SUPERBA_TEST_SITES;
/// Claude Code's `settings.json` helpers (pure parts shared by the GUI and
/// `spb`).
pub use superba_config::claude as claude_files;
/// The file editor's pure parts (checks, formatting, masking, diffs; GitHub
/// issue 9), so the GUI and `spb` check an edit exactly as the service does.
pub use superba_config::codex_files;
/// Build a user-defined OpenAI-compatible site from its Responses base URL (for the
/// GUI and `spb site add`; validated with the prober's key-safety rules).
pub use superba_net::new_compatible_site;
/// Stopping the router processes of an install (the Windows MSI's removal step).
pub use superba_versions::selfupdate::msi as installer;

/// Resolve the real [`LocalContext`]: home from the OS, data dir from
/// `SUPERBA_DATA_DIR` or the platform data directory
/// (`~/.local/share/superba`, `~/Library/Application Support/com.krill-code.Superba`,
/// `%LOCALAPPDATA%\krill-code\Superba\data`).
pub fn detect_context() -> Result<LocalContext> {
    let env: Arc<dyn EnvSource> = Arc::new(ProcessEnv);
    let base = directories::BaseDirs::new()
        .ok_or_else(|| Error::other("cannot determine the home directory"))?;
    let data_dir = match env.var_non_empty(ENV_SUPERBA_DATA_DIR) {
        Some(dir) => PathBuf::from(dir),
        // The local (non-roaming) data directory: it holds machine state (pids,
        // ports, the router's key file), which must not follow a Windows roaming
        // profile to other machines. Same path as `data_dir` on Linux and macOS;
        // `%LOCALAPPDATA%` on Windows.
        None => directories::ProjectDirs::from("com", "krill-code", "Superba")
            .ok_or_else(|| Error::other("cannot determine the data directory"))?
            .data_local_dir()
            .to_path_buf(),
    };
    Ok(LocalContext::new(
        base.home_dir(),
        DataDir::new(data_dir),
        env,
    ))
}

/// Everything [`Switcher`] depends on, as trait objects.
#[derive(Clone)]
pub struct SwitcherDeps {
    /// Local machine context.
    pub ctx: LocalContext,
    /// GUI login registration, separate from the router service.
    pub autostart: Arc<dyn superba_config::autostart::LoginStartup>,
    /// Config engine.
    pub config: Arc<dyn ConfigEngine>,
    /// Env persister.
    pub env: Arc<dyn EnvPersister>,
    /// Secret store.
    pub secrets: Arc<dyn SecretStore>,
    /// Profile store.
    pub profiles: Arc<dyn ProfileStore>,
    /// Line prober.
    pub prober: Arc<dyn LineProber>,
    /// Session migrator.
    pub sessions: Arc<dyn SessionMigrator>,
    /// App controller.
    pub app: Arc<dyn AppController>,
    /// Remote hosts.
    pub remote: Arc<dyn RemoteHosts>,
    /// Clock.
    pub clock: Arc<dyn Clock>,
    /// The local router (router mode).
    pub router: Arc<dyn router::RouterControl>,
    /// The Codex settings catalog of this machine's OS (version filtering, settings
    /// page).
    pub catalog: Arc<dyn superba_core::CatalogSource>,
    /// The catalog for a remote host's OS (feature stages differ per OS).
    pub remote_catalog: RemoteCatalog,
    /// Human-written Chinese explanation of a setting path, if one exists (the
    /// catalog's zh-CN overlay; Superba never generates these).
    pub explain_zh: ZhLookup,
    /// Codex releases, the verified Codex store and app packages, built per use
    /// with the current network settings (Preferences can change them).
    pub versions: VersionsFactory,
    /// Superba's signed feeds (model list, prices, versions), built per use with
    /// the current network settings.
    pub feeds: FeedsFactory,
    /// Model catalog generation (`superba-models`; a fake in tests).
    pub models: Arc<dyn models::CatalogBuilder>,
    /// Paths of the curated "common" settings (`[common]` of the catalog's
    /// curation), for the Settings page.
    pub common_settings: Arc<Vec<String>>,
}

/// Builds the [`superba_core::VersionStore`] for the current network settings.
pub type VersionsFactory = Arc<
    dyn Fn(&superba_core::NetworkSettings) -> Arc<dyn superba_core::VersionStore> + Send + Sync,
>;

/// Builds the [`superba_core::FeedSource`] for the current network settings.
pub type FeedsFactory =
    Arc<dyn Fn(&superba_core::NetworkSettings) -> Arc<dyn superba_core::FeedSource> + Send + Sync>;

/// Human-written Chinese explanation of a setting path.
pub type ZhLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Catalog for a remote host's OS.
pub type RemoteCatalog =
    Arc<dyn Fn(&superba_core::RemoteOs) -> Arc<dyn superba_core::CatalogSource> + Send + Sync>;

impl std::fmt::Debug for SwitcherDeps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SwitcherDeps")
            .field("ctx", &self.ctx)
            .finish_non_exhaustive()
    }
}

impl SwitcherDeps {
    /// Production wiring for `ctx`.
    pub fn with_defaults(ctx: LocalContext) -> Self {
        let settings = settings_catalog(&ctx.data_dir);
        let config: Arc<dyn ConfigEngine> =
            Arc::new(superba_config::TomlConfigEngine::new(ctx.clone()));
        Self {
            autostart: Arc::new(superba_config::autostart::Manager::new(&ctx)),
            env: Arc::new(superba_config::ShellEnvPersister::new(ctx.clone())),
            secrets: Arc::new(superba_config::KeyringSecretStore::new(ctx.clone())),
            profiles: Arc::new(superba_config::JsonProfileStore::new(ctx.clone())),
            // Usage reports are rate-limited by the site (Krill: 10 per hour), so
            // they are cached on disk across processes.
            // Provider model lists are kept on disk too (ETag reuse across `spb`
            // runs; the files hold no key).
            prober: Arc::new(
                superba_net::HttpLineProber::default()
                    .with_cache_dir(ctx.data_dir.cache_dir().join("usage"))
                    .with_model_cache_dir(ctx.data_dir.cache_dir().join("models")),
            ),
            // `with_home_dir` lets `~` in `sqlite_home` expand and lets remote
            // discovery read `~/.ssh/config` even when the Codex home is not
            // `<home>/.codex`.
            sessions: Arc::new(
                superba_sessions::CodexSessionMigrator::new(ctx.data_dir.clone())
                    .with_home_dir(ctx.home_dir.clone()),
            ),
            app: Arc::new(superba_appctl::DesktopAppController::new(ctx.clone())),
            remote: Arc::new(
                superba_remote::SshRemoteHosts::new(config.clone(), ctx.data_dir.clone())
                    .with_home_dir(ctx.home_dir.clone()),
            ),
            clock: Arc::new(SystemClock::new()),
            router: Arc::new(router::SystemRouterControl::new(
                &ctx.data_dir,
                ctx.home_dir.clone(),
            )),
            catalog: catalog_source(settings.as_ref()),
            remote_catalog: remote_catalog(settings.as_ref()),
            explain_zh: zh_lookup(settings.as_ref()),
            // The version store reads checksums, release dates and the Codex each
            // app build bundles from the signed feeds.
            versions: {
                let data_dir = ctx.data_dir.clone();
                Arc::new(move |network: &superba_core::NetworkSettings| {
                    let store = superba_versions::HttpVersionStore::new(&data_dir, network.clone())
                        .with_feed(default_feeds(&data_dir, network));
                    // Windows: app packages for the machine's native architecture
                    // (arm64 while this x64 build runs emulated on ARM64).
                    let store = if cfg!(windows) {
                        store.with_native_arch(superba_appctl::package::native_arch())
                    } else {
                        store
                    };
                    Arc::new(store) as Arc<dyn superba_core::VersionStore>
                })
            },
            feeds: {
                let data_dir = ctx.data_dir.clone();
                Arc::new(move |network: &superba_core::NetworkSettings| {
                    default_feeds(&data_dir, network)
                })
            },
            models: Arc::new(models::ModelsCrate),
            common_settings: Arc::new(
                settings
                    .as_ref()
                    .map(|c| c.source.store().curation().common().to_vec())
                    .unwrap_or_default(),
            ),
            config,
            ctx,
        }
    }
}

/// The production feed source: the verified cache and the built-in feeds, online
/// refresh per `network`.
fn default_feeds(
    data_dir: &DataDir,
    network: &superba_core::NetworkSettings,
) -> Arc<dyn superba_core::FeedSource> {
    Arc::new(superba_catalog::HttpFeeds::new(
        superba_catalog::FeedStore::new(data_dir),
        network.clone(),
    ))
}

/// The Codex settings catalog every part of Superba reads: the planner's version
/// filtering, `spb settings`, and the GUI settings page (through
/// [`SettingsCatalog::source`]'s store).
#[derive(Clone, Debug)]
pub struct SettingsCatalog {
    /// Version-matched snapshots for this OS (`for_os` for remote hosts).
    pub source: Arc<superba_catalog::SnapshotSource>,
    /// Notices from loading it (for example a downloaded catalog that failed
    /// verification and is not used).
    pub notices: Vec<superba_core::Notice>,
}

/// Load the settings catalog: the built-in snapshot, or a newer catalog that was
/// downloaded and verified into `<data dir>/catalog` (remote updates are off by
/// default, so normally the built-in one). `None` when even the built-in catalog
/// cannot be loaded.
pub fn settings_catalog(data_dir: &DataDir) -> Option<SettingsCatalog> {
    let cache = superba_catalog::remote::cache_dir(data_dir);
    match superba_catalog::load(Some(&cache)) {
        Ok(loaded) => Some(SettingsCatalog {
            source: Arc::new(superba_catalog::SnapshotSource::new(
                loaded.store,
                superba_catalog::Os::current(),
            )),
            notices: loaded.notices,
        }),
        Err(e) => {
            tracing::warn!(error = %e, "the settings catalog cannot be loaded");
            None
        }
    }
}

/// [`SwitcherDeps::catalog`] from a loaded catalog; an empty catalog (nothing
/// filtered, a "no catalog" note) without one.
pub fn catalog_source(catalog: Option<&SettingsCatalog>) -> Arc<dyn superba_core::CatalogSource> {
    match catalog {
        Some(c) => c.source.clone(),
        None => Arc::new(superba_core::StaticCatalog::default()),
    }
}

/// [`SwitcherDeps::remote_catalog`]: the same catalog for a remote host's OS (this
/// OS's when the remote OS is unknown).
pub fn remote_catalog(catalog: Option<&SettingsCatalog>) -> RemoteCatalog {
    let source = catalog.map(|c| c.source.clone());
    Arc::new(
        move |os: &superba_core::RemoteOs| -> Arc<dyn superba_core::CatalogSource> {
            match (&source, superba_catalog::Os::from_remote(os)) {
                (Some(s), Some(os)) => Arc::new(s.for_os(os)),
                (Some(s), None) => s.clone(),
                (None, _) => Arc::new(superba_core::StaticCatalog::default()),
            }
        },
    )
}

/// [`SwitcherDeps::explain_zh`]: Chinese explanations from the catalog's zh-CN
/// overlay (config keys `config:<path>`, feature flags `feature:<key>` for
/// `features.<key>`).
pub fn zh_lookup(catalog: Option<&SettingsCatalog>) -> ZhLookup {
    let store = catalog.map(|c| c.source.store().clone());
    Arc::new(move |path: &str| {
        let overlay = store.as_ref()?.overlay();
        let entry = overlay
            .get(&superba_catalog::EntryId::Config(path.to_owned()))
            .filter(|e| e.has_zh())
            .or_else(|| {
                let key = path.strip_prefix("features.")?.split('.').next()?;
                overlay
                    .get(&superba_catalog::EntryId::Feature(key.to_owned()))
                    .filter(|e| e.has_zh())
            })?;
        let text = entry.zh.trim();
        match (&entry.zh_name, text.is_empty()) {
            (Some(name), true) => Some(name.trim().to_owned()),
            (Some(name), false) => Some(format!("{}: {text}", name.trim())),
            (None, _) => Some(text.to_owned()),
        }
    })
}

/// Timing knobs of execution. Defaults follow plan 7.2 / 7.4; tests shrink them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tuning {
    /// Poll interval while waiting for turns / consumers / the new app-server.
    pub poll: Duration,
    /// Wait for the new app-server process after the IPC or launch.
    pub verify_timeout: Duration,
    /// After a relaunch: how long from the launch the app may take to start
    /// its first app-server (a slow machine needs tens of seconds; the check
    /// passes as soon as one runs, so this only bounds a failure).
    pub launch_verify_timeout: Duration,
    /// The new app-server must stay alive this long ("no crash within 5 s").
    pub stable: Duration,
    /// Ask again whether to keep waiting for turns after this long (15 min).
    pub reask_after: Duration,
    /// Plans kept for execution (latest selections).
    pub plan_cache: usize,
    /// The desktop app behaves as on Windows (`cfg!(windows)`; tests set it):
    /// closing its windows leaves it running in the notification area, so one
    /// Superba did not open (no debugging port) can only be ended by force.
    pub windows_app: bool,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            poll: Duration::from_millis(1_000),
            verify_timeout: Duration::from_secs(15),
            launch_verify_timeout: Duration::from_secs(60),
            stable: Duration::from_secs(5),
            reask_after: Duration::from_secs(15 * 60),
            plan_cache: 16,
            windows_app: cfg!(windows),
        }
    }
}

/// The [`SwitchService`] implementation.
#[derive(Debug, Clone)]
pub struct Switcher {
    deps: SwitcherDeps,
    tuning: Tuning,
    plans: Arc<Mutex<PlanCache>>,
    /// Router watchdog state between ticks.
    watchdog: Arc<Mutex<superba_router::watchdog::Watchdog>>,
    /// Base instant for the watchdog (plus `clock.monotonic()`).
    watch_base: std::time::Instant,
    /// Last probe of each remote host.
    hosts: Arc<Mutex<hosts::HostCache>>,
    /// Model lists seen by the Models page (validation of overrides).
    model_lists: Arc<Mutex<HashMap<ProfileId, superba_core::ModelList>>>,
    /// Plan without a model catalog (start-up repair after a refused file).
    catalog_off: Arc<std::sync::atomic::AtomicBool>,
    /// Plan as if this were the app's Codex pin (a pin change rebuilding the
    /// catalog file for its new consumer before the pin is stored).
    pin_override: Arc<Mutex<Option<superba_core::AppCodexPin>>>,
    /// Model requests read from the local Codex home's rollouts (Usage page).
    usage: usage::SharedIndex,
    /// The language the UI reported ([`SwitchService::set_ui_locale`]); the
    /// default stats line template is resolved in it.
    reported_locale: Arc<Mutex<Option<superba_core::Locale>>>,
    /// The environment monitor: installs and Codex binaries for the views
    /// ([`monitor`]).
    monitor: monitor::EnvMonitor,
    /// GH#5: the last attribution of a change to the Codex files and the last
    /// environment check ([`ownership`]).
    ownership: ownership::SharedOwnershipCache,
    /// Claude Code: model lists and the `claude` program ([`claude`]).
    claude: Arc<claude::ClaudeCache>,
}

/// Execution context of computed plans, keyed by plan id (things the serializable
/// plan does not carry: resolved key, endpoints, hosts, app session).
#[derive(Debug, Default)]
pub(crate) struct PlanCache {
    order: Vec<String>,
    map: HashMap<String, planner::PlanContext>,
}

impl PlanCache {
    fn insert(&mut self, id: String, ctx: planner::PlanContext, cap: usize) {
        self.order.retain(|x| x != &id);
        self.order.push(id.clone());
        self.map.insert(id, ctx);
        while self.order.len() > cap.max(1) {
            let old = self.order.remove(0);
            self.map.remove(&old);
        }
    }

    fn get(&self, id: &str) -> Option<&planner::PlanContext> {
        self.map.get(id)
    }

    fn take(&mut self, id: &str) -> Option<planner::PlanContext> {
        self.order.retain(|x| x != id);
        self.map.remove(id)
    }
}

impl Switcher {
    /// Create from explicit dependencies (tests inject fakes here).
    pub fn new(deps: SwitcherDeps) -> Self {
        Self::with_tuning(deps, Tuning::default())
    }

    /// Create with explicit timing knobs.
    pub fn with_tuning(deps: SwitcherDeps, tuning: Tuning) -> Self {
        let usage = Arc::new(std::sync::Mutex::new(
            superba_sessions::usage::UsageIndex::new(deps.ctx.codex_home()),
        ));
        let monitor = monitor::EnvMonitor::new(deps.app.clone(), monitor::MonitorTuning::default());
        Self {
            deps,
            tuning,
            plans: Arc::new(Mutex::new(PlanCache::default())),
            watchdog: Arc::new(router::new_watchdog()),
            watch_base: router::watch_base(),
            hosts: Arc::new(Mutex::new(hosts::HostCache::default())),
            model_lists: Arc::new(Mutex::new(HashMap::new())),
            catalog_off: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            pin_override: Arc::new(Mutex::new(None)),
            usage,
            reported_locale: Arc::new(Mutex::new(None)),
            monitor,
            ownership: Arc::default(),
            claude: Arc::default(),
        }
    }

    /// Replace the environment monitor's intervals (before it is used).
    pub fn with_monitor_tuning(mut self, tuning: monitor::MonitorTuning) -> Self {
        self.monitor = monitor::EnvMonitor::new(self.deps.app.clone(), tuning);
        self
    }

    /// The environment monitor.
    pub fn monitor(&self) -> &monitor::EnvMonitor {
        &self.monitor
    }

    /// Dependencies.
    pub fn deps(&self) -> &SwitcherDeps {
        &self.deps
    }

    /// Timing knobs.
    pub fn tuning(&self) -> &Tuning {
        &self.tuning
    }

    pub(crate) fn remember_plan(&self, id: String, ctx: planner::PlanContext) {
        let mut cache = self.plans.lock().unwrap_or_else(|p| p.into_inner());
        cache.insert(id, ctx, self.tuning.plan_cache);
    }

    /// The settings part of a remembered plan.
    pub(crate) fn plan_settings_of(&self, id: &str) -> Option<superba_core::PlanSettings> {
        let cache = self.plans.lock().unwrap_or_else(|p| p.into_inner());
        cache.get(id).map(|pc| pc.settings.clone())
    }

    pub(crate) fn take_plan(&self, id: &str) -> Option<planner::PlanContext> {
        let mut cache = self.plans.lock().unwrap_or_else(|p| p.into_inner());
        cache.take(id)
    }
}

/// Production service for `ctx` as a trait object (what the GUI and `spb` hold).
pub fn default_service(ctx: LocalContext) -> Arc<dyn SwitchService> {
    Arc::new(Switcher::new(SwitcherDeps::with_defaults(ctx)))
}

#[async_trait]
impl SwitchService for Switcher {
    async fn overview(&self) -> Result<Overview> {
        self.overview_impl().await
    }

    async fn sites(&self) -> Result<Vec<Site>> {
        self.sites_impl().await
    }

    async fn profile_set(&self) -> Result<ProfileSet> {
        let mut set = self.deps.profiles.load().await?;
        set.settings.autostart = match self.deps.autostart.status() {
            Ok(status) => status.enabled,
            Err(error) => {
                tracing::warn!(error = %error, "login startup status cannot be read; treating it as disabled");
                false
            }
        };
        Ok(set)
    }

    async fn autostart_status(&self) -> Result<superba_core::autostart::AutostartStatus> {
        self.deps.autostart.status()
    }

    async fn set_autostart(
        &self,
        enabled: bool,
    ) -> Result<superba_core::autostart::AutostartStatus> {
        self.refuse_elevated()?;
        let _lock = journal::StateLock::acquire(&self.deps.ctx.data_dir)?;
        self.deps.autostart.set_enabled(enabled)
    }

    async fn save_profile(&self, profile: Profile, key: Option<Secret>) -> Result<Profile> {
        self.refuse_elevated()?;
        self.save_profile_impl(profile, key).await
    }

    async fn delete_profile(&self, id: &ProfileId) -> Result<()> {
        self.refuse_elevated()?;
        self.delete_profile_impl(id).await
    }

    async fn save_settings(&self, settings: Settings) -> Result<()> {
        self.refuse_elevated()?;
        self.save_settings_impl(settings).await
    }

    async fn save_site(&self, site: Site) -> Result<Site> {
        self.refuse_elevated()?;
        self.save_site_impl(site).await
    }

    async fn delete_site(&self, id: &SiteId) -> Result<()> {
        self.refuse_elevated()?;
        self.delete_site_impl(id).await
    }

    async fn probe_lines(&self, site_id: &SiteId) -> Result<Vec<LineCaps>> {
        let site = self.site(site_id).await?;
        self.deps.prober.probe_all(&site).await
    }

    async fn check_key(
        &self,
        site_id: &SiteId,
        line_id: &LineId,
        mode: AuthMode,
        key: KeyInput,
    ) -> Result<KeyCheck> {
        self.check_key_impl(site_id, line_id, mode, key).await
    }

    async fn probe_site(
        &self,
        request: superba_core::SiteProbeRequest,
    ) -> Result<superba_core::SiteProbeReport> {
        self.probe_site_impl(request).await
    }

    async fn line_transports(
        &self,
        request: superba_core::LineTransportsRequest,
    ) -> Result<Vec<superba_core::LineTransports>> {
        self.line_transports_impl(request).await
    }

    async fn provider_compaction(
        &self,
        request: superba_core::CompactionRequest,
    ) -> Result<superba_core::ProviderCompaction> {
        self.provider_compaction_impl(request).await
    }

    async fn import_usage(
        &self,
        request: superba_core::UsageImportRequest,
    ) -> Result<superba_core::UsageImportReport> {
        self.import_usage_impl(request).await
    }

    async fn plan_settings(&self, plan_id: &str) -> Result<superba_core::PlanSettings> {
        self.plan_settings_of(plan_id)
            .ok_or_else(|| Error::NotFound {
                what: format!("plan {plan_id} (plans are kept until they are executed)"),
            })
    }

    async fn settings_view(
        &self,
        request: superba_core::SettingsViewRequest,
    ) -> Result<superba_core::SettingsView> {
        self.settings_view_impl(request).await
    }

    async fn edit_settings(
        &self,
        edit: superba_core::SettingsEdit,
    ) -> Result<superba_core::SettingsEditReport> {
        self.refuse_elevated()?;
        self.edit_settings_impl(edit).await
    }

    async fn router_status(&self) -> Result<superba_core::RouterView> {
        self.router_status_impl().await
    }

    async fn router_install(
        &self,
        request: superba_core::RouterInstallRequest,
    ) -> Result<superba_core::RouterView> {
        self.refuse_elevated()?;
        self.router_install_impl(request).await
    }

    async fn router_set_enabled(&self, enabled: bool) -> Result<superba_core::RouterView> {
        self.refuse_elevated()?;
        self.router_set_enabled_impl(enabled).await
    }

    async fn router_uninstall(&self) -> Result<superba_core::RouterView> {
        self.refuse_elevated()?;
        self.router_uninstall_impl().await
    }

    async fn router_doctor(&self) -> Result<superba_core::RouterDoctorReport> {
        self.router_doctor_impl().await
    }

    async fn router_fall_back(&self, reason: String) -> Result<Option<SwitchOutcome>> {
        self.router_fall_back_impl(reason).await
    }

    async fn router_watch(&self) -> Result<superba_core::RouterWatch> {
        self.router_watch_impl().await
    }

    async fn self_update_view(&self) -> Result<superba_core::SelfUpdateView> {
        self.self_update_view_impl().await
    }

    async fn self_update_check(
        &self,
        request: superba_core::SelfUpdateCheckRequest,
    ) -> Result<superba_core::SelfUpdateView> {
        let view = self.self_update_check_impl(request).await?;
        if view.status == superba_core::SelfUpdateStatus::Available
            && let Some(latest) = &view.latest
            && view.skipped.as_deref() != Some(latest.version.as_str())
        {
            // Once per version (the dedupe key).
            self.notify_superba(
                superba_core::EventKind::UpdateAvailable,
                vec![superba_core::LocalizedText::new("", latest.version.clone())],
                Some(format!("update:{}", latest.version)),
            )
            .await;
        }
        Ok(view)
    }

    async fn self_update_skip(
        &self,
        version: Option<String>,
    ) -> Result<superba_core::SelfUpdateView> {
        self.self_update_skip_impl(version).await
    }

    async fn self_update_acknowledge(&self) -> Result<superba_core::SelfUpdateView> {
        self.self_update_acknowledge_impl().await
    }

    async fn self_update_install(
        &self,
        request: superba_core::SelfUpdateInstallRequest,
    ) -> Result<superba_core::SelfUpdateReport> {
        self.refuse_elevated()?;
        self.self_update_install_impl(request).await
    }

    async fn self_update_restart_router(&self) -> Result<bool> {
        self.restart_router_if_running().await
    }

    async fn import_scan(
        &self,
        request: superba_core::ImportScanRequest,
    ) -> Result<superba_core::ImportScan> {
        self.import_scan_impl(request).await
    }

    async fn import_apply(
        &self,
        request: superba_core::ImportApplyRequest,
    ) -> Result<superba_core::ImportReport> {
        self.import_apply_impl(request).await
    }

    async fn account(&self, request: AccountRequest) -> Result<AccountReport> {
        self.account_impl(request).await
    }

    async fn models_view(
        &self,
        request: superba_core::ModelsViewRequest,
    ) -> Result<superba_core::ModelsView> {
        self.models_view_impl(request).await
    }

    async fn save_models(
        &self,
        edit: superba_core::ModelsEdit,
    ) -> Result<superba_core::ModelsEditReport> {
        self.refuse_elevated()?;
        self.save_models_impl(edit).await
    }

    async fn hosts(&self) -> Result<Vec<superba_core::HostView>> {
        self.hosts_impl().await
    }

    async fn hosts_now(&self) -> Result<Vec<superba_core::HostView>> {
        self.hosts_now_impl().await
    }

    async fn host_probe(
        &self,
        target_id: &superba_core::TargetId,
    ) -> Result<superba_core::HostView> {
        self.host_probe_impl(target_id).await
    }

    async fn set_host_follow(
        &self,
        target_id: &superba_core::TargetId,
        follow: bool,
    ) -> Result<superba_core::HostView> {
        self.refuse_elevated()?;
        self.set_host_follow_impl(target_id, follow).await
    }

    async fn host_sync_plan(
        &self,
        target_id: &superba_core::TargetId,
    ) -> Result<superba_core::HostSyncPlan> {
        self.host_sync_plan_impl(target_id).await
    }

    async fn host_sessions(
        &self,
        target_id: &superba_core::TargetId,
    ) -> Result<superba_core::HostSessions> {
        self.host_sessions_impl(target_id).await
    }

    async fn host_migrate_sessions(
        &self,
        target_id: &superba_core::TargetId,
        request: &superba_core::MigrationRequest,
        locale: superba_core::Locale,
        events: Arc<dyn superba_core::SwitchEvents>,
    ) -> Result<superba_core::HostMigrationReport> {
        self.refuse_elevated()?;
        self.host_migrate_sessions_impl(target_id, request, locale, events)
            .await
    }

    async fn wsl_overview(&self) -> Result<superba_core::wsl::WslOverview> {
        self.wsl_overview_impl().await
    }

    async fn wsl_router_access(
        &self,
        target_id: &superba_core::TargetId,
    ) -> Result<superba_core::wsl::WslAccessReport> {
        self.refuse_elevated()?;
        self.wsl_router_access_impl(target_id).await
    }

    async fn versions(&self, refresh: bool) -> Result<superba_core::VersionsView> {
        self.versions_impl(refresh).await
    }

    async fn install_codex(
        &self,
        request: superba_core::InstallRequest,
    ) -> Result<superba_core::InstallReport> {
        self.refuse_elevated()?;
        let result = self.install_codex_impl(request).await;
        // A new or replaced Codex binary: the views read it after this.
        self.monitor.invalidate();
        result
    }

    async fn rollback_codex(
        &self,
        target: superba_core::InstallTarget,
    ) -> Result<superba_core::InstallReport> {
        self.refuse_elevated()?;
        let result = self.rollback_codex_impl(target).await;
        self.monitor.invalidate();
        result
    }

    async fn download_app_update(&self) -> Result<superba_core::AppDownloadReport> {
        self.refuse_elevated()?;
        self.download_app_update_impl().await
    }

    async fn download_app_package(
        &self,
        request: superba_core::AppDownloadRequest,
        progress: Arc<dyn superba_core::DownloadProgressSink>,
    ) -> Result<superba_core::AppDownloadReport> {
        self.refuse_elevated()?;
        self.download_app_package_impl(request, progress).await
    }

    async fn open_app_package(&self, path: std::path::PathBuf) -> Result<()> {
        self.refuse_elevated()?;
        self.open_app_package_impl(path).await
    }

    async fn router_requests(
        &self,
        query: superba_core::RequestQuery,
    ) -> Result<superba_core::RequestPage> {
        self.router_requests_impl(query).await
    }

    async fn router_request(&self, id: i64) -> Result<superba_core::RequestDetail> {
        self.router_request_impl(id).await
    }

    async fn router_inflight(&self) -> Result<Vec<superba_core::InflightRequest>> {
        self.router_inflight_impl().await
    }

    async fn router_stats(
        &self,
        query: superba_core::StatsQuery,
    ) -> Result<superba_core::RequestStats> {
        self.router_stats_impl(query).await
    }

    async fn router_clear(
        &self,
        request: superba_core::ClearRequest,
    ) -> Result<superba_core::ClearReport> {
        self.refuse_elevated()?;
        self.router_clear_impl(request).await
    }

    async fn router_set_stats(
        &self,
        settings: superba_core::RouterStatsSettings,
    ) -> Result<superba_core::RouterView> {
        self.refuse_elevated()?;
        self.router_set_stats_impl(settings).await
    }

    async fn set_ui_locale(&self, locale: superba_core::Locale) -> Result<()> {
        self.set_ui_locale_impl(locale).await
    }

    fn environment_event(&self, event: superba_core::EnvironmentEvent) {
        if event == superba_core::EnvironmentEvent::Refresh {
            // The WSL distributions are listed again too (user bug 54).
            self.deps.remote.forget_cached();
        }
        self.monitor.event(event);
    }

    fn environment_generation(&self) -> u64 {
        self.monitor.generation()
    }

    async fn price_table(&self) -> Result<superba_core::PriceTable> {
        self.price_table_impl().await
    }

    async fn session_titles(
        &self,
        keys: Vec<String>,
    ) -> Result<std::collections::BTreeMap<String, superba_core::SessionTitle>> {
        let home = self.deps.ctx.codex_home();
        let home_dir = self.deps.ctx.home_dir.clone();
        tokio::task::spawn_blocking(move || {
            superba_sessions::titles::lookup_in(&home, Some(&home_dir), &keys)
        })
        .await
        .map_err(|e| Error::other(format!("session names: {e}")))?
    }

    async fn feeds_status(&self) -> Result<superba_core::FeedsStatus> {
        self.feeds_status_impl(false).await
    }

    async fn refresh_feeds(&self) -> Result<superba_core::FeedsStatus> {
        self.feeds_status_impl(true).await
    }

    async fn sessions(&self) -> Result<(SessionReport, Vec<MigrationReceipt>)> {
        let home = self.deps.ctx.codex_home();
        let report = self.deps.sessions.scan(&home).await?;
        let history = self.deps.sessions.history(&home).await?;
        Ok((report, history))
    }

    async fn doctor(&self) -> Result<Vec<DoctorFinding>> {
        self.doctor_impl().await
    }

    async fn runtime(&self) -> Result<superba_core::RuntimeView> {
        self.runtime_impl().await
    }

    async fn codex_file(
        &self,
        target: superba_core::TargetId,
        kind: superba_core::CodexFileKind,
    ) -> Result<superba_core::CodexFileDoc> {
        self.codex_file_impl(target, kind).await
    }

    async fn save_codex_file(
        &self,
        save: superba_core::CodexFileSave,
    ) -> Result<superba_core::CodexFileSaved> {
        self.save_codex_file_impl(save).await
    }

    async fn codex_file_backups(
        &self,
        query: superba_core::CodexFileBackupQuery,
    ) -> Result<Vec<superba_core::CodexFileBackup>> {
        self.codex_file_backups_impl(query).await
    }

    async fn codex_file_backup(&self, id: &str) -> Result<superba_core::CodexFileBackupContents> {
        self.codex_file_backup_impl(id).await
    }

    async fn claude_view(&self) -> Result<superba_core::ClaudeView> {
        self.claude_view_impl().await
    }

    async fn save_claude_config(
        &self,
        config: superba_core::ClaudeConfig,
    ) -> Result<superba_core::ClaudeView> {
        self.save_claude_config_impl(config).await
    }

    async fn claude_models(
        &self,
        request: superba_core::ClaudeModelsRequest,
    ) -> Result<superba_core::ClaudeModelsView> {
        self.claude_models_impl(request).await
    }

    async fn claude_plan(
        &self,
        kind: superba_core::ClaudePlanKind,
    ) -> Result<superba_core::ClaudePlan> {
        self.claude_plan_impl(kind).await
    }

    async fn claude_apply(
        &self,
        plan: superba_core::ClaudePlan,
        take_over: bool,
    ) -> Result<superba_core::ClaudeApplyReport> {
        self.claude_apply_impl(plan, take_over).await
    }

    async fn claude_diagnostics(&self) -> Result<superba_core::ClaudeDiagnostics> {
        self.claude_diagnostics_impl().await
    }

    async fn claude_probe(&self, model: Option<String>) -> Result<superba_core::ClaudeProbe> {
        self.claude_probe_impl(model).await
    }

    async fn stop_processes(
        &self,
        processes: Vec<superba_core::ProcessInfo>,
        force: bool,
    ) -> Result<Vec<superba_core::StopReport>> {
        self.stop_processes_impl(processes, force).await
    }

    async fn open_app(&self) -> Result<AppSession> {
        self.open_app_impl().await
    }

    async fn recover(&self) -> Result<RecoveryReport> {
        self.refuse_elevated()?;
        self.recover_impl().await
    }

    async fn plan(&self, request: SwitchRequest) -> Result<SwitchPlan> {
        self.plan_with_events(request, Arc::new(superba_core::NoEvents))
            .await
    }

    async fn plan_with_events(
        &self,
        request: SwitchRequest,
        events: Arc<dyn SwitchEvents>,
    ) -> Result<SwitchPlan> {
        superba_remote::operation::with_events(events, self.plan_impl(request)).await
    }

    async fn execute(
        &self,
        plan: SwitchPlan,
        options: ExecuteOptions,
        decider: Arc<dyn SwitchDecider>,
        events: Arc<dyn SwitchEvents>,
    ) -> Result<SwitchOutcome> {
        let label = plan.to.label.clone();
        let plan_id = plan.id.clone();
        let result = superba_remote::operation::with_events(
            events.clone(),
            self.execute_impl(plan, options, decider, events),
        )
        .await;
        // A switch can pin, launch or install: the views read the machine again.
        self.monitor.invalidate();
        // GH#12: tell the phone (when the user asked for switch events).
        let report = match &result {
            Ok(o) => match o.status {
                superba_core::OutcomeStatus::Switched
                | superba_core::OutcomeStatus::WrittenOnly
                | superba_core::OutcomeStatus::Partial => {
                    Some((superba_core::EventKind::SwitchDone, vec![label]))
                }
                superba_core::OutcomeStatus::Failed | superba_core::OutcomeStatus::RolledBack => {
                    let mut detail = vec![label];
                    detail.extend(
                        o.notices
                            .iter()
                            .filter(|n| n.severity == superba_core::Severity::Danger)
                            .map(|n| n.text.clone())
                            .take(1),
                    );
                    Some((superba_core::EventKind::SwitchFailed, detail))
                }
                _ => None,
            },
            Err(Error::Cancelled) => None,
            Err(e) => Some((
                superba_core::EventKind::SwitchFailed,
                vec![label, superba_core::LocalizedText::new("", e.to_string())],
            )),
        };
        if let Some((kind, detail)) = report {
            self.notify_superba(kind, detail, Some(format!("switch:{plan_id}")))
                .await;
        }
        result
    }

    async fn notify_save_channel(
        &self,
        channel: superba_core::NotifyChannel,
        secrets: superba_core::NotifySecrets,
    ) -> Result<superba_core::NotifyChannel> {
        self.notify_save_channel_impl(channel, secrets).await
    }

    async fn notify_delete_channel(&self, id: &str) -> Result<()> {
        self.notify_delete_channel_impl(id).await
    }

    async fn notify_send(
        &self,
        event: superba_core::NotifyEvent,
        options: superba_core::SendOptions,
    ) -> Result<superba_core::NotifyReport> {
        self.notify_send_impl(event, options).await
    }

    async fn notify_hook(
        &self,
        harness: &str,
        payload: &str,
    ) -> Result<superba_core::NotifyReport> {
        self.notify_hook_impl(harness, payload).await
    }

    async fn notify_hooks(&self) -> Result<superba_core::NotifyHooks> {
        self.notify_hooks_impl().await
    }

    async fn notify_set_hook(
        &self,
        request: superba_core::HookRequest,
    ) -> Result<superba_core::NotifyHooks> {
        self.notify_set_hook_impl(request).await
    }

    async fn notify_watch(&self) -> Result<superba_core::NotifyReport> {
        self.notify_watch_impl().await
    }

    // -- other coding-agent harnesses ([`harness`]) --

    async fn harness_list(&self) -> Result<Vec<superba_core::harness::HarnessSummary>> {
        self.harness_list_impl().await
    }

    async fn harness_view(
        &self,
        harness: superba_core::harness::HarnessId,
    ) -> Result<superba_core::harness::HarnessView> {
        self.harness_view_impl(harness).await
    }

    async fn save_harness_settings(
        &self,
        harness: superba_core::harness::HarnessId,
        settings: superba_core::harness::HarnessSettings,
    ) -> Result<superba_core::harness::HarnessView> {
        self.save_harness_settings_impl(harness, settings).await
    }

    async fn save_provider_protocols(
        &self,
        provider: ProfileId,
        protocols: superba_core::harness::ProviderProtocols,
    ) -> Result<()> {
        self.save_provider_protocols_impl(provider, protocols).await
    }

    async fn set_harness_router_token(
        &self,
        harness: superba_core::harness::HarnessId,
        token: Option<Secret>,
    ) -> Result<superba_core::harness::HarnessView> {
        self.set_harness_router_token_impl(harness, token).await
    }

    async fn harness_plan(
        &self,
        harness: superba_core::harness::HarnessId,
        kind: superba_core::harness::HarnessPlanKind,
    ) -> Result<superba_core::harness::HarnessPlan> {
        self.harness_plan_impl(harness, kind).await
    }

    async fn harness_apply(
        &self,
        plan: superba_core::harness::HarnessPlan,
        take_over: bool,
    ) -> Result<superba_core::harness::HarnessApplyReport> {
        self.harness_apply_impl(plan, take_over).await
    }

    async fn harness_backups(
        &self,
        harness: superba_core::harness::HarnessId,
    ) -> Result<Vec<superba_core::harness::HarnessBackup>> {
        self.harness_backups_impl(harness).await
    }

    async fn harness_restore_backup(
        &self,
        harness: superba_core::harness::HarnessId,
        id: String,
    ) -> Result<superba_core::harness::HarnessApplyReport> {
        self.harness_restore_backup_impl(harness, id).await
    }
}
