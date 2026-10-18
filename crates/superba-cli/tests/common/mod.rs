#![allow(dead_code)]

pub mod v3;

use async_trait::async_trait;
use clap::Parser;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use superba_cli::{Cli, Output, Runtime};
use superba_core::*;
use superba_switch::messages;

pub const FAKE_KEY: &str = "sk-test-0000aaaa1111";

#[derive(Clone, Debug)]
pub struct SavedProfile {
    pub profile: Profile,
    pub secret_present: bool,
    pub last4: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordedKey {
    Secret { last4: String },
    Ref(KeyRef),
    Profile(ProfileId),
}

#[derive(Clone, Debug, Default)]
pub struct Calls {
    pub names: Vec<&'static str>,
    pub plans: Vec<SwitchRequest>,
    pub executions: Vec<(String, ExecuteOptions)>,
    pub saves: Vec<SavedProfile>,
    pub deletes: Vec<ProfileId>,
    pub probes: Vec<SiteId>,
    pub checks: Vec<(SiteId, LineId, AuthMode, RecordedKey)>,
    pub answers: Vec<Decision>,
    pub site_probes: Vec<SiteProbeCall>,
    pub settings_views: Vec<SettingsViewRequest>,
    pub settings_edits: Vec<SettingsEdit>,
    pub router_installs: Vec<Vec<ProfileId>>,
    pub router_enabled: Vec<bool>,
}

/// Router and settings state of the fake service.
pub struct FakeV2 {
    pub router: RouterView,
    pub view: SettingsView,
    pub plan_settings: PlanSettings,
}

pub fn fake_v2() -> FakeV2 {
    let mut fast = CatalogKey::new("features.fast_mode", SettingKind::Boolean);
    fast.default = Some(SettingValue::Bool(false));
    fast.description = Some("Use the fast speed tier.".into());
    FakeV2 {
        router: RouterView::default(),
        view: SettingsView {
            scope: superba_core::SettingsScope::Computer,
            profile_id: Some(ProfileId::new("work")),
            builtin: None,
            config_path: Some(std::path::PathBuf::from("/home/u/.codex/config.toml")),
            consumers: Vec::new(),
            catalog_version: Some(CodexVersion::new(0, 162, 0)),
            catalog_match: Some(CatalogMatch::Exact),
            rows: vec![SettingRow {
                common: false,
                host_support: Vec::new(),
                path: "features.fast_mode".into(),
                key: Some(fast),
                feature: Some(CatalogFeature {
                    key: "fast_mode".into(),
                    config_path: "features.fast_mode".into(),
                    stage: FeatureStage::Stable,
                    default_enabled: false,
                    name: None,
                    menu_description: None,
                }),
                zh: None,
                current: Some("false".into()),
                managed: Some(SettingEntry::Set(SettingValue::Bool(true))),
                support: Vec::new(),
                advanced: false,
            }],
            notes: Vec::new(),
        },
        plan_settings: PlanSettings::default(),
    }
}

/// A settings diff row for `features.fast_mode` (false -> true, local).
pub fn fast_mode_change() -> SettingChange {
    SettingChange {
        path: "features.fast_mode".into(),
        from: Some("false".into()),
        to: Some("true".into()),
        target: TargetId::local(),
        target_label: "This computer".into(),
        reason: ChangeReason::Setting,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteProbeCall {
    pub site: SiteId,
    pub line: Option<LineId>,
    pub key: Option<RecordedKey>,
    pub model: Option<String>,
    pub responses: bool,
}

fn record_key(key: KeyInput) -> RecordedKey {
    match key {
        KeyInput::Secret(secret) => RecordedKey::Secret {
            last4: secret.last4(),
        },
        KeyInput::Ref(reference) => RecordedKey::Ref(reference),
        KeyInput::Profile(id) => RecordedKey::Profile(id),
    }
}

type Planner = dyn Fn(SwitchRequest) -> Result<SwitchPlan> + Send + Sync;

#[derive(Default)]
pub struct FakeSelfUpdate {
    pub view: superba_core::SelfUpdateView,
    pub report: Option<superba_core::SelfUpdateReport>,
    pub checks: Vec<superba_core::SelfUpdateCheckRequest>,
    pub installs: Vec<superba_core::SelfUpdateInstallRequest>,
    pub router_restarts: usize,
}

pub struct FakeService {
    pub profiles: Mutex<ProfileSet>,
    pub overview: Mutex<Overview>,
    pub sites: Mutex<Vec<Site>>,
    pub plan_script: Mutex<Arc<Planner>>,
    pub outcome: Mutex<Result<SwitchOutcome>>,
    pub calls: Mutex<Calls>,
    pub error: Mutex<Option<Error>>,
    pub events: Mutex<Vec<SwitchEvent>>,
    pub decisions: Mutex<Vec<DecisionRequest>>,
    pub key_check: Mutex<KeyCheck>,
    pub line_caps: Mutex<Vec<LineCaps>>,
    pub sessions: Mutex<(SessionReport, Vec<MigrationReceipt>)>,
    pub findings: Mutex<Vec<DoctorFinding>>,
    pub recovery: Mutex<RecoveryReport>,
    pub v2: Mutex<FakeV2>,
    pub v3: Mutex<v3::FakeV3>,
    /// `spb self-update`: the view every check returns, the install report and the calls.
    pub self_update: Mutex<FakeSelfUpdate>,
}

impl FakeService {
    pub fn new() -> Arc<Self> {
        let mut profiles = ProfileSet::default();
        profiles.profiles.push(profile("work", "krill"));
        Arc::new(Self {
            profiles: Mutex::new(profiles),
            overview: Mutex::new(overview()),
            sites: Mutex::new(catalog::builtin_sites()),
            plan_script: Mutex::new(Arc::new(|request| Ok(plan(request)))),
            outcome: Mutex::new(Ok(outcome(OutcomeStatus::Switched))),
            calls: Mutex::new(Calls::default()),
            error: Mutex::new(None),
            events: Mutex::new(Vec::new()),
            decisions: Mutex::new(Vec::new()),
            key_check: Mutex::new(KeyCheck {
                site_id: SiteId::new("krill"),
                line_id: LineId::new("overseas"),
                checked_at: Timestamp::EPOCH,
                key_last4: "1111".into(),
                whoami: Some(AuthApiHealth::Healthy),
                identity: Some(AccountIdentity {
                    email: Some("tester@example.invalid".into()),
                    name: Some("Test user".into()),
                    user_id: None,
                }),
                access: KeyAccess::Ok,
                codex_path: Some(CodexPath::BackendApiCodex),
            }),
            line_caps: Mutex::new(vec![LineCaps {
                site_id: SiteId::new("krill"),
                line_id: LineId::new("overseas"),
                checked_at: Timestamp::EPOCH,
                reachable: true,
                latency_ms: Some(123),
                pat_ok: true,
                apikey_ok: true,
                codex_path: Some(CodexPath::BackendApiCodex),
                blocked_by_edge: false,
                key_access: None,
                checks: Vec::new(),
                detail: Some(messages::text("switch.doctor.ok")),
            }]),
            sessions: Mutex::new((
                SessionReport {
                    codex_home: "/fake/codex".into(),
                    scanned_at: Timestamp::EPOCH,
                    sqlite_paths: Vec::new(),
                    by_provider: [(
                        "legacy".into(),
                        ProviderSessions {
                            distinct: 6,
                            threads: 5,
                            rollouts: 4,
                            archived: 3,
                            compressed: 2,
                        },
                    )]
                    .into(),
                    inconsistent: 1,
                    warnings: Vec::new(),
                },
                vec![MigrationReceipt {
                    id: "migration-1".into(),
                    codex_home: "/fake/codex".into(),
                    migrated_at: Timestamp::EPOCH,
                    to_provider: "openai".into(),
                    by_source_provider: [("legacy".into(), 6)].into(),
                    threads: 5,
                    rollouts: 4,
                    backup_dir: "/fake/backups".into(),
                    ledger_path: "/fake/ledger".into(),
                    rolled_back: false,
                }],
            )),
            findings: Mutex::new(Vec::new()),
            v2: Mutex::new(fake_v2()),
            self_update: Mutex::new(FakeSelfUpdate::default()),
            v3: Mutex::new(v3::fake_v3()),
            recovery: Mutex::new(RecoveryReport {
                entries: vec![RecoveredJournal {
                    journal_id: "journal-1".into(),
                    action: JournalAction::Completed,
                    files: vec!["/fake/config.toml".into()],
                }],
                notices: Vec::new(),
            }),
        })
    }
    pub fn script(
        &self,
        planner: impl Fn(SwitchRequest) -> Result<SwitchPlan> + Send + Sync + 'static,
    ) {
        *self.plan_script.lock().unwrap() = Arc::new(planner);
    }
    pub fn calls(&self) -> Calls {
        self.calls.lock().unwrap().clone()
    }
    fn record(&self, name: &'static str) -> Result<()> {
        self.calls.lock().unwrap().names.push(name);
        if let Some(error) = self.error.lock().unwrap().clone() {
            return Err(error);
        }
        Ok(())
    }
}

pub fn profile(id: &str, site: &str) -> Profile {
    Profile {
        model_overrides: Default::default(),
        model_picker: Default::default(),
        compaction: Default::default(),
        id: ProfileId::new(id),
        name: format!("Profile {id}"),
        builtin: None,
        site_id: SiteId::new(site),
        line_id: None,
        auth_mode: AuthMode::Pat,
        key_ref: None,
        base_url_override: None,
        model: ModelDefaults::default(),
        auth: superba_core::AuthChoice::from_mode(AuthMode::Pat),
        backup_lines: Vec::new(),
        codex_settings: Default::default(),
    }
}

pub fn app_session() -> AppSession {
    AppSession {
        app_server_exe: None,
        install_id: Some("fake-app".into()),
        main_pid: 424_242,
        started_at: Some(Timestamp::EPOCH),
        executable: Some("/fake/app".into()),
        cdp: Some(CdpEndpoint {
            port: 9234,
            owner_verified: true,
        }),
        launched_by_us: true,
        authapi_env: EnvObservation::Present(catalog::KRILL_ORIGIN_OVERSEAS.into()),
        home_override: None,
        codex_home: Some("/fake/codex".into()),
    }
}

pub fn overview() -> Overview {
    Overview {
        generated_at: Timestamp::EPOCH,
        codex_home: "/fake/codex".into(),
        active: Some(ActiveState {
            profile_id: Some(ProfileId::new("work")),
            site_id: Some(SiteId::new("krill")),
            line_id: Some(LineId::new("overseas")),
            auth_mode: Some(AuthMode::Pat),
            generation: 7,
            applied_at: Timestamp::EPOCH,
        }),
        config: None,
        app: AppOverview {
            installs: vec![AppInstall {
                id: "fake-app".into(),
                variant: AppVariant::Codex,
                path: "/fake/app".into(),
                executable: "/fake/app/codex".into(),
                app_version: Some("26.1013".into()),
                bundled_codex: None,
                bundled_codex_version: Some(CodexVersion::new(0, 162, 0)),
            }],
            session: Some(app_session()),
            hot_switch: HotSwitchStatus::Ready,
            authapi_line: Some(LineId::new("overseas")),
        },
        cli: CliOverview {
            binaries: vec![CodexBinary {
                path: "/fake/bin/codex".into(),
                version: Some(CodexVersion::new(0, 162, 0)),
                source: InstallSource::Npm,
                path_rank: Some(0),
            }],
            processes: vec![process()],
        },
        remotes: vec![RemoteOverview {
            host: RemoteHost {
                target_id: TargetId::new("ssh:a"),
                label: "Test host".into(),
                host_id: Some("app-host-a".into()),
                kind: superba_core::HostKind::Ssh,
                ssh: SshEndpoint {
                    alias: Some("a".into()),
                    ..Default::default()
                },
                sources: vec![
                    HostSource::GlobalState,
                    HostSource::Cdp,
                    HostSource::ProcessScan,
                    HostSource::SshConfig,
                    HostSource::Manual,
                ],
                connected: Some(true),
                wsl: None,
            },
            state: None,
            error: None,
        }],
        account: None,
        baseline_captured: true,
        baseline_at: None,
        login_stash: None,
        runtime: None,
        notices: vec![Notice::warning(messages::text(
            "switch.warn.daemon_running",
        ))],
        external: None,
    }
}

pub fn process() -> ProcessInfo {
    ProcessInfo {
        pid: 424_243,
        ppid: None,
        kind: CodexProcessKind::CliTui,
        exe: Some("/fake/bin/codex".into()),
        cmdline: vec!["codex".into()],
        started_at: Some(Timestamp::EPOCH),
        authapi_env: EnvObservation::Absent,
        codex_home: Some("/fake/codex".into()),
        stale: true,
        ide: None,
        cwd: None,
    }
}

pub fn thread() -> ThreadActivity {
    ThreadActivity {
        target_id: TargetId::local(),
        thread_id: Some("thread-1".into()),
        title: Some("Example thread".into()),
        rollout_path: None,
        last_event_at: Some(Timestamp::EPOCH),
        signal: ActivitySignal::RolloutTail,
        originator: None,
        cwd: None,
    }
}

pub fn plan(request: SwitchRequest) -> SwitchPlan {
    SwitchPlan {
        id: "plan-1".into(),
        created_at: Timestamp::EPOCH,
        request,
        from: None,
        to: PlanDestination {
            profile_id: Some(ProfileId::new("work")),
            site_id: Some(SiteId::new("krill")),
            line_id: Some(LineId::new("overseas")),
            auth_mode: Some(AuthMode::Pat),
            label: messages::text("switch.destination.official"),
        },
        primary: RestartKind::None,
        restarts: Vec::new(),
        config: None,
        env: None,
        migration: None,
        remotes: Vec::new(),
        active_threads: Vec::new(),
        model_check: None,
        choices: Vec::new(),
        alternatives: Vec::new(),
        warnings: Vec::new(),
        cleanup: Vec::new(),
    }
}

pub fn outcome(status: OutcomeStatus) -> SwitchOutcome {
    SwitchOutcome {
        plan_id: "plan-1".into(),
        status,
        elapsed_ms: 1234,
        generation: Some(8),
        restart: RestartKind::None,
        verification: None,
        remotes: Vec::new(),
        stale_processes: Vec::new(),
        runtime: None,
        returned_to: None,
        notices: Vec::new(),
    }
}

pub fn runtime(service: Arc<FakeService>) -> Runtime {
    let mut rt = Runtime::with_service(service);
    rt.env = Arc::new(MapEnv::new().with("SUPERBA_LOCALE", "en"));
    rt.clock = Arc::new(ManualClock::new(Timestamp::EPOCH));
    rt.read_stdin_line = Arc::new(|| Ok(FAKE_KEY.into()));
    rt
}
pub async fn invoke(service: Arc<FakeService>, args: &[&str]) -> (u8, String, String) {
    invoke_runtime(&runtime(service), args).await
}
pub async fn invoke_runtime(rt: &Runtime, args: &[&str]) -> (u8, String, String) {
    let cli = Cli::try_parse_from(std::iter::once("spb").chain(args.iter().copied())).unwrap();
    let (mut out, stdout, stderr) = Output::capture();
    let code = superba_cli::run(&cli, rt, &mut out).await;
    (code, stdout.contents(), stderr.contents())
}
pub fn one_json(stdout: &str) -> Value {
    let mut values = serde_json::Deserializer::from_str(stdout).into_iter::<Value>();
    let value = values.next().unwrap().unwrap();
    assert!(values.next().is_none(), "expected exactly one JSON value");
    value
}

#[async_trait]
impl SwitchService for FakeService {
    async fn self_update_view(&self) -> Result<superba_core::SelfUpdateView> {
        Ok(self.self_update.lock().unwrap().view.clone())
    }
    async fn self_update_check(
        &self,
        request: superba_core::SelfUpdateCheckRequest,
    ) -> Result<superba_core::SelfUpdateView> {
        self.record("self_update_check")?;
        let mut s = self.self_update.lock().unwrap();
        s.checks.push(request);
        Ok(s.view.clone())
    }
    async fn self_update_install(
        &self,
        request: superba_core::SelfUpdateInstallRequest,
    ) -> Result<superba_core::SelfUpdateReport> {
        self.record("self_update_install")?;
        let mut s = self.self_update.lock().unwrap();
        s.installs.push(request);
        s.report
            .clone()
            .ok_or_else(|| Error::not_implemented("self update install"))
    }
    async fn self_update_restart_router(&self) -> Result<bool> {
        self.self_update.lock().unwrap().router_restarts += 1;
        Ok(true)
    }

    async fn overview(&self) -> Result<Overview> {
        self.record("overview")?;
        Ok(self.overview.lock().unwrap().clone())
    }
    async fn models_view(&self, request: ModelsViewRequest) -> Result<ModelsView> {
        self.record("models_view")?;
        let mut view = self.v3.lock().unwrap().models.clone();
        if let Some(id) = request.profile_id {
            view.profile_id = id;
        }
        Ok(view)
    }
    async fn save_models(&self, edit: ModelsEdit) -> Result<ModelsEditReport> {
        self.record("save_models")?;
        let mut v3 = self.v3.lock().unwrap();
        v3.model_edits.push(edit.clone());
        let id = edit
            .profile_id
            .clone()
            .unwrap_or_else(|| ProfileId::new("work"));
        let mut profile = self
            .profiles
            .lock()
            .unwrap()
            .profile(&id)
            .cloned()
            .unwrap_or_else(|| profile(id.as_str(), "krill"));
        if let Some(d) = edit.defaults {
            profile.model = d.clone();
            v3.models.defaults = d;
        }
        for (slug, o) in edit.set {
            profile.model_overrides.insert(slug, o);
        }
        Ok(ModelsEditReport {
            profile,
            notes: Vec::new(),
        })
    }
    async fn hosts(&self) -> Result<Vec<HostView>> {
        self.record("hosts")?;
        Ok(self.v3.lock().unwrap().hosts.clone())
    }
    async fn host_probe(&self, target_id: &TargetId) -> Result<HostView> {
        self.record("host_probe")?;
        self.v3
            .lock()
            .unwrap()
            .hosts
            .iter()
            .find(|h| &h.host.target_id == target_id)
            .cloned()
            .ok_or_else(|| Error::NotFound {
                what: format!("remote host {target_id}"),
            })
    }
    async fn set_host_follow(&self, target_id: &TargetId, follow: bool) -> Result<HostView> {
        self.record("set_host_follow")?;
        let mut v3 = self.v3.lock().unwrap();
        v3.follows.push((target_id.clone(), follow));
        let mut view = v3.hosts[0].clone();
        view.settings.follow_switches = follow;
        Ok(view)
    }
    async fn host_sync_plan(&self, _target_id: &TargetId) -> Result<HostSyncPlan> {
        self.record("host_sync_plan")?;
        Ok(self.v3.lock().unwrap().sync.clone())
    }
    async fn host_sessions(&self, _target_id: &TargetId) -> Result<HostSessions> {
        self.record("host_sessions")?;
        Ok(self.v3.lock().unwrap().host_sessions.clone())
    }
    async fn versions(&self, _refresh: bool) -> Result<VersionsView> {
        self.record("versions")?;
        Ok(self.v3.lock().unwrap().versions.clone())
    }
    async fn install_codex(&self, request: InstallRequest) -> Result<InstallReport> {
        self.record("install_codex")?;
        self.v3.lock().unwrap().installs.push(request.clone());
        Ok(InstallReport {
            version: request.version,
            target: request.target,
            executable: Some("/fake/store/bin/codex".into()),
            previous: Some(CodexVersion::new(0, 150, 1)),
            notes: Vec::new(),
        })
    }
    async fn rollback_codex(&self, target: InstallTarget) -> Result<InstallReport> {
        self.record("rollback_codex")?;
        self.v3.lock().unwrap().rollbacks.push(target.clone());
        Ok(InstallReport {
            version: CodexVersion::new(0, 150, 1),
            target,
            executable: Some("/fake/bin/codex".into()),
            previous: Some(CodexVersion::new(0, 162, 0)),
            notes: Vec::new(),
        })
    }
    async fn router_requests(&self, query: RequestQuery) -> Result<RequestPage> {
        self.record("router_requests")?;
        let mut v3 = self.v3.lock().unwrap();
        v3.queries.push(query);
        Ok(v3.page.clone())
    }
    async fn router_stats(&self, query: StatsQuery) -> Result<RequestStats> {
        self.record("router_stats")?;
        let mut v3 = self.v3.lock().unwrap();
        v3.stats_queries.push(query);
        Ok(v3.stats.clone())
    }
    async fn router_clear(&self, request: ClearRequest) -> Result<ClearReport> {
        self.record("router_clear")?;
        self.v3.lock().unwrap().clears.push(request);
        Ok(ClearReport { rows: 3 })
    }
    async fn router_set_stats(&self, settings: RouterStatsSettings) -> Result<RouterView> {
        self.record("router_set_stats")?;
        self.v3
            .lock()
            .unwrap()
            .stats_settings
            .push(settings.clone());
        let mut v2 = self.v2.lock().unwrap();
        v2.router.settings.stats = settings;
        Ok(v2.router.clone())
    }
    async fn session_titles(
        &self,
        keys: Vec<String>,
    ) -> Result<std::collections::BTreeMap<String, SessionTitle>> {
        self.record("session_titles")?;
        let titles = self.v3.lock().unwrap().titles.clone();
        Ok(keys
            .into_iter()
            .filter_map(|k| titles.get(&k).cloned().map(|t| (k, t)))
            .collect())
    }
    async fn download_app_package(
        &self,
        request: AppDownloadRequest,
        progress: std::sync::Arc<dyn DownloadProgressSink>,
    ) -> Result<AppDownloadReport> {
        self.record("download_app_package")?;
        let windows = {
            let mut v3 = self.v3.lock().unwrap();
            v3.app_downloads.push(request);
            v3.windows_download.clone()
        };
        let Some(report) = windows else {
            return self.download_app_update().await;
        };
        progress.report(DownloadProgress {
            file: "ChatGPT-26.1013.4894.0-x64.msix".into(),
            done: 474_000_000,
            total: Some(948_993_815),
            index: 1,
            count: 1,
            ..DownloadProgress::stage(DownloadStage::Downloading, Some(AppPackageSource::Official))
        });
        Ok(report)
    }
    async fn open_app_package(&self, path: std::path::PathBuf) -> Result<()> {
        self.record("open_app_package")?;
        self.v3.lock().unwrap().opened_packages.push(path);
        Ok(())
    }
    async fn download_app_update(&self) -> Result<AppDownloadReport> {
        self.record("download_app_update")?;
        Ok(AppDownloadReport {
            package: AppPackage {
                os: AppOs::Linux,
                kind: AppPackageKind::Deb,
                arch: "amd64".into(),
                url: "https://example.invalid/chatgpt_26.1013_amd64.deb".into(),
                sha256: Some("ab".repeat(32)),
                size: None,
                signature: None,
            },
            path: "/fake/downloads/chatgpt_26.1013_amd64.deb".into(),
            sha256: "ab".repeat(32),
            command: Some("sudo apt install /fake/downloads/chatgpt_26.1013_amd64.deb".into()),
            windows: None,
        })
    }
    async fn feeds_status(&self) -> Result<FeedsStatus> {
        self.record("feeds_status")?;
        Ok(self.v3.lock().unwrap().feeds.clone())
    }
    async fn refresh_feeds(&self) -> Result<FeedsStatus> {
        self.record("refresh_feeds")?;
        let mut v3 = self.v3.lock().unwrap();
        v3.feed_refreshes += 1;
        Ok(v3.feeds.clone())
    }
    async fn sites(&self) -> Result<Vec<Site>> {
        self.record("sites")?;
        Ok(self.sites.lock().unwrap().clone())
    }
    async fn profile_set(&self) -> Result<ProfileSet> {
        self.record("profile_set")?;
        Ok(self.profiles.lock().unwrap().clone())
    }
    async fn save_profile(&self, mut profile: Profile, key: Option<Secret>) -> Result<Profile> {
        self.record("save_profile")?;
        self.calls.lock().unwrap().saves.push(SavedProfile {
            profile: profile.clone(),
            secret_present: key.is_some(),
            last4: key.as_ref().map(Secret::last4),
        });
        if key.is_some() {
            profile.key_ref = Some(KeyRef::Keyring {
                service: "test-only".into(),
                account: profile.id.to_string(),
            });
        }
        let mut set = self.profiles.lock().unwrap();
        set.profiles.retain(|p| p.id != profile.id);
        set.profiles.push(profile.clone());
        Ok(profile)
    }
    async fn delete_profile(&self, id: &ProfileId) -> Result<()> {
        self.record("delete_profile")?;
        self.calls.lock().unwrap().deletes.push(id.clone());
        self.profiles
            .lock()
            .unwrap()
            .profiles
            .retain(|p| &p.id != id);
        Ok(())
    }
    async fn save_settings(&self, settings: Settings) -> Result<()> {
        self.record("save_settings")?;
        self.profiles.lock().unwrap().settings = settings;
        Ok(())
    }
    async fn save_site(&self, site: Site) -> Result<Site> {
        self.record("save_site")?;
        let mut sites = self.sites.lock().unwrap();
        match sites.iter_mut().find(|s| s.id == site.id) {
            Some(slot) => *slot = site.clone(),
            None => sites.push(site.clone()),
        }
        Ok(site)
    }
    async fn plan_settings(&self, _plan_id: &str) -> Result<PlanSettings> {
        self.record("plan_settings")?;
        Ok(self.v2.lock().unwrap().plan_settings.clone())
    }
    async fn settings_view(&self, request: SettingsViewRequest) -> Result<SettingsView> {
        self.record("settings_view")?;
        self.calls.lock().unwrap().settings_views.push(request);
        Ok(self.v2.lock().unwrap().view.clone())
    }
    async fn edit_settings(&self, edit: SettingsEdit) -> Result<SettingsEditReport> {
        self.record("edit_settings")?;
        self.calls.lock().unwrap().settings_edits.push(edit);
        let profile = self
            .profiles
            .lock()
            .unwrap()
            .profiles
            .last()
            .unwrap()
            .clone();
        Ok(SettingsEditReport {
            profile,
            notes: Vec::new(),
        })
    }
    async fn router_status(&self) -> Result<RouterView> {
        self.record("router_status")?;
        Ok(self.v2.lock().unwrap().router.clone())
    }
    async fn router_install(&self, request: RouterInstallRequest) -> Result<RouterView> {
        self.record("router_install")?;
        self.calls
            .lock()
            .unwrap()
            .router_installs
            .push(request.members.clone());
        let mut v2 = self.v2.lock().unwrap();
        v2.router.installed = true;
        v2.router.running = true;
        v2.router.port = Some(18431);
        if !request.members.is_empty() {
            v2.router.settings.members = request.members;
        }
        Ok(v2.router.clone())
    }
    async fn router_set_enabled(&self, enabled: bool) -> Result<RouterView> {
        self.record("router_set_enabled")?;
        self.calls.lock().unwrap().router_enabled.push(enabled);
        let mut v2 = self.v2.lock().unwrap();
        v2.router.settings.enabled = enabled;
        Ok(v2.router.clone())
    }
    async fn router_uninstall(&self) -> Result<RouterView> {
        self.record("router_uninstall")?;
        let mut v2 = self.v2.lock().unwrap();
        v2.router.installed = false;
        v2.router.running = false;
        Ok(v2.router.clone())
    }
    async fn router_watch(&self) -> Result<RouterWatch> {
        self.record("router_watch")?;
        Ok(RouterWatch {
            state: RouterWatchState::Healthy,
            outcome: None,
            error: None,
        })
    }
    async fn probe_site(&self, request: SiteProbeRequest) -> Result<SiteProbeReport> {
        self.record("probe_site")?;
        let site = self
            .sites
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.id == request.site_id)
            .cloned()
            .ok_or_else(|| Error::NotFound {
                what: format!("site {}", request.site_id),
            })?;
        let has_key = request.key.is_some();
        self.calls.lock().unwrap().site_probes.push(SiteProbeCall {
            site: request.site_id.clone(),
            line: request.line_id.clone(),
            key: request.key.map(record_key),
            model: request.model.clone(),
            responses: request.responses,
        });
        let line = request
            .line_id
            .clone()
            .or_else(|| site.default_line().map(|l| l.id.clone()))
            .unwrap_or_else(|| LineId::new("default"));
        let mut caps = self.line_caps.lock().unwrap()[0].clone();
        caps.site_id = site.id.clone();
        caps.line_id = line.clone();
        let models = vec![ModelInfo {
            service_tiers: Vec::new(),
            context_window: None,
            max_context_window: None,
            raw: None,
            id: "vendor/gpt-x".into(),
            display_name: None,
        }];
        let model = request
            .model
            .as_deref()
            .map(|m| site.site_model(m).to_owned());
        Ok(SiteProbeReport {
            site_id: site.id.clone(),
            line_id: line,
            caps,
            key_check: has_key.then(|| self.key_check.lock().unwrap().clone()),
            mapped: site
                .model_map
                .iter()
                .map(|(codex, mapped)| MappedModel {
                    codex: codex.clone(),
                    site: mapped.clone(),
                    available: has_key.then(|| models.iter().any(|m| &m.id == mapped)),
                })
                .collect(),
            models: has_key.then_some(models),
            models_error: None,
            responses: (has_key && request.responses).then_some(ResponsesProbe {
                ok: true,
                status: Some(200),
                latency_ms: Some(321),
                compact_ok: None,
                detail: None,
            }),
            model: (has_key && request.responses).then(|| model.unwrap_or("vendor/gpt-x".into())),
            responses_error: None,
            account: None,
            account_error: None,
        })
    }
    async fn delete_site(&self, id: &SiteId) -> Result<()> {
        self.record("delete_site")?;
        self.sites.lock().unwrap().retain(|site| &site.id != id);
        Ok(())
    }
    async fn probe_lines(&self, site: &SiteId) -> Result<Vec<LineCaps>> {
        self.record("probe_lines")?;
        self.calls.lock().unwrap().probes.push(site.clone());
        Ok(self.line_caps.lock().unwrap().clone())
    }
    async fn check_key(
        &self,
        site: &SiteId,
        line: &LineId,
        auth: AuthMode,
        key: KeyInput,
    ) -> Result<KeyCheck> {
        self.record("check_key")?;
        let key = record_key(key);
        self.calls
            .lock()
            .unwrap()
            .checks
            .push((site.clone(), line.clone(), auth, key));
        Ok(self.key_check.lock().unwrap().clone())
    }
    async fn sessions(&self) -> Result<(SessionReport, Vec<MigrationReceipt>)> {
        self.record("sessions")?;
        Ok(self.sessions.lock().unwrap().clone())
    }
    async fn doctor(&self) -> Result<Vec<DoctorFinding>> {
        self.record("doctor")?;
        Ok(self.findings.lock().unwrap().clone())
    }
    async fn open_app(&self) -> Result<AppSession> {
        self.record("open_app")?;
        Ok(app_session())
    }
    async fn recover(&self) -> Result<RecoveryReport> {
        self.record("recover")?;
        Ok(self.recovery.lock().unwrap().clone())
    }
    async fn plan(&self, request: SwitchRequest) -> Result<SwitchPlan> {
        self.record("plan")?;
        self.calls.lock().unwrap().plans.push(request.clone());
        let planner = self.plan_script.lock().unwrap().clone();
        planner(request)
    }
    async fn execute(
        &self,
        plan: SwitchPlan,
        options: ExecuteOptions,
        decider: Arc<dyn SwitchDecider>,
        events: Arc<dyn SwitchEvents>,
    ) -> Result<SwitchOutcome> {
        self.record("execute")?;
        self.calls
            .lock()
            .unwrap()
            .executions
            .push((plan.id, options));
        let scripted_events = self.events.lock().unwrap().clone();
        for event in scripted_events {
            events.emit(event);
        }
        let questions = self.decisions.lock().unwrap().clone();
        for question in questions {
            let decision = decider.decide(question).await;
            self.calls.lock().unwrap().answers.push(decision);
        }
        tokio::task::yield_now().await;
        self.outcome.lock().unwrap().clone()
    }

    async fn router_fall_back(
        &self,
        _: String,
    ) -> superba_core::Result<Option<superba_core::SwitchOutcome>> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::router_fall_back",
        ))
    }

    async fn account(
        &self,
        _: superba_core::AccountRequest,
    ) -> superba_core::Result<superba_core::AccountReport> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::account",
        ))
    }

    async fn router_request(&self, _: i64) -> superba_core::Result<superba_core::RequestDetail> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::router_request",
        ))
    }

    async fn router_inflight(&self) -> superba_core::Result<Vec<superba_core::InflightRequest>> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::router_inflight",
        ))
    }

    async fn price_table(&self) -> superba_core::Result<superba_core::PriceTable> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::price_table",
        ))
    }
}
