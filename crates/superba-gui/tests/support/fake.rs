//! An in-memory fake world for GUI tests (test-only; Superba ships no demo mode).
//!
//! All paths, processes, credentials, and probes are fixtures. Only clocks and
//! Tokio timers are used; no machine configuration or external service is read.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::{
    Arc, Mutex, MutexGuard,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use superba_core::{catalog, *};
use tokio::time::{Instant, MissedTickBehavior};

// Per-area state and the bodies of the v0.3 service methods (plan §4 C2): each
// area's owner edits only its file; `impl SwitchService` below delegates.
mod claude;
mod files;
mod harness;
mod hosts;
mod import;
mod maintenance;
mod models;
mod notify;
mod providers;
mod router;
mod selfupdate;
mod sessions;
mod settings;
pub mod tools;
mod versions;

const HOME: &str = "/home/user/.codex";
const BACKUPS: &str = "/home/user/.local/share/superba/backups";
const APP: &str = "/usr/lib/chatgpt/ChatGPT";
const BUNDLED: &str = "/usr/lib/chatgpt/resources/codex";
const TIMEOUT: &str = "whoami timed out";

macro_rules! texts {
    ($($name:ident: $key:literal => $fallback:literal;)*) => {
        /// Every non-core i18n key produced by this module, all under `fake.`.
        pub const FAKE_TEXT_KEYS: &[&str] = &[$($key),*];
        $(fn $name() -> LocalizedText { LocalizedText::new($key, $fallback) })*
    };
}

texts! {
    caps_blocked: "fake.caps.cn_fast_pat_blocked" => "PAT is unavailable on this line: the edge intercepts /backend-api/wham.";
    dest: "fake.plan.dest" => "{site} · {line} · {auth}";
    dest_official: "fake.plan.dest_official" => "Official ChatGPT login";
    dest_original: "fake.plan.dest_original" => "Original configuration";
    alt_apikey: "fake.plan.alt_apikey" => "Use an API key on this line instead";
    warn_cli: "fake.plan.warn_cli" => "{count} codex processes in terminals are not affected; they use the new line after they are reopened.";
    warn_authapi: "fake.plan.warn_authapi" => "Auth still goes through the {line} line ChatGPT was started with (checked: healthy).";
    warn_remotes: "fake.plan.warn_remotes" => "Remote hosts {hosts} will switch too; their running turns and commands are interrupted as well.";
    warn_login_age: "fake.plan.warn_login_age" => "The saved ChatGPT login is {days} days old; you may need to sign in again.";
    warn_hidden: "fake.plan.warn_hidden" => "{count} sessions will be hidden in the app after the restore.";
    restart_app: "fake.plan.restart_app" => "ChatGPT will be restarted through Superba.";
    cleanup_marker: "fake.cleanup.marker_block" => "Remove the CODEX_AUTHAPI_BASE_URL block from ~/.bashrc";
    cleanup_legacy: "fake.cleanup.legacy_line" => "Uncomment the old tutorial line in ~/.bashrc";
    cleanup_launcher: "fake.cleanup.launcher" => "Remove the \"ChatGPT (via Superba)\" launcher entry";
    cleanup_backups: "fake.cleanup.backups" => "Keep backups in ~/.local/share/superba/backups";
    verify_failed: "fake.outcome.verify_failed" => "The new Codex core failed verification ({reason}); the previous configuration was restored.";
    log_applied: "fake.log.applied" => "Wrote config.toml and auth.json (generation {generation})";
    log_restart: "fake.log.restart_sent" => "Sent the app-server restart (hostId {host})";
    log_verified: "fake.log.verified" => "Verification passed in {ms} ms";
    home_ok: "fake.doctor.codex_home_ok" => "GUI and terminals use the same CODEX_HOME";
    ide_missing: "fake.doctor.ide_missing_authapi" => "1 IDE app-server lacks CODEX_AUTHAPI_BASE_URL";
    ide_detail: "fake.doctor.ide_missing_authapi_detail" => "Its whoami would go to the default auth API. Restart VS Code after switching.";
    pat_blocked: "fake.doctor.pat_line_blocked" => "China Express blocks the PAT endpoints";
    pat_detail: "fake.doctor.pat_line_blocked_detail" => "Use an API key on that line or pick another line.";
    no_openai_env: "fake.doctor.no_openai_env" => "No conflicting OPENAI_* variables";
}

/// Options of the fake backend.
#[derive(Clone, Debug, Default)]
pub struct FakeOptions {
    /// Start with legacy Krill configuration, no baseline, two built-in profiles,
    /// 128 Krill sessions, and an app opened without Superba or CDP.
    pub first_run: bool,
    /// Simulate two busy turns on the first executable switch only.
    pub busy_turns: bool,
    /// Fail the first execution's auth verification with "whoami timed out".
    pub fail_verification: bool,
    /// Skip presentation delays; busy turns still finish after 150/300 ms.
    pub fast: bool,
    /// Stretch presentation delays 4x (screenshots of in-progress states).
    pub slow: bool,
}

/// A self-contained, thread-safe implementation of the GUI's switching backend.
pub struct FakeService {
    state: Mutex<FakeState>,
    options: FakeOptions,
    next_plan: AtomicU64,
    areas: Areas,
    open_calls: AtomicU64,
}

/// Fake state per page area (`fake/<area>.rs`).
#[derive(Debug, Default)]
struct Areas {
    storage: Mutex<Option<Result<superba_core::data::DataUsage>>>,
    claude: Mutex<claude::State>,
    files: Mutex<files::State>,
    maintenance: Mutex<maintenance::State>,
    providers: Mutex<providers::State>,
    models: Mutex<models::State>,
    settings: Mutex<settings::State>,
    sessions: Mutex<sessions::State>,
    router: Mutex<router::State>,
    hosts: Mutex<hosts::State>,
    harness: Mutex<harness::State>,
    versions: Mutex<versions::State>,
    selfupdate: Mutex<selfupdate::State>,
    tools: Mutex<tools::State>,
    import: Mutex<import::State>,
    notify: Mutex<notify::State>,
}

struct FakeState {
    profiles: ProfileSet,
    config: ConfigState,
    app: AppOverview,
    cli: CliOverview,
    remotes: Vec<RemoteOverview>,
    account: Option<AccountSummary>,
    account_summary_enabled: bool,
    baseline_captured: bool,
    sessions: SessionReport,
    history: Vec<MigrationReceipt>,
    caps: BTreeMap<(SiteId, LineId), LineCaps>,
    busy_armed: bool,
    fail_armed: bool,
    router_installed: bool,
    /// The upstream the router last used per member (`router_status`).
    last_routes: BTreeMap<ProfileId, RouterLineRoute>,
    /// GH#5: what another tool changed (the overview's `external`).
    external: Option<superba_core::ExternalChange>,
}

impl FakeService {
    /// Fixture returned by the data block's asynchronous storage scan.
    pub fn set_data_usage(&self, result: Result<superba_core::data::DataUsage>) {
        *self.areas.storage.lock().unwrap() = Some(result);
    }

    /// Construct an isolated fake world without inspecting the real machine.
    pub fn new(options: FakeOptions) -> Self {
        Self {
            state: Mutex::new(FakeState::new(&options)),
            options,
            next_plan: AtomicU64::new(1),
            areas: Areas::default(),
            open_calls: AtomicU64::new(0),
        }
        .with_account_summary(true)
    }

    /// Choose whether the simulated relay serves the standard billing endpoints
    /// (account summaries).
    pub fn with_account_summary(self, enabled: bool) -> Self {
        self.state().account_summary_enabled = enabled;
        self
    }

    /// Claude Code through the local router: the router's base URL Claude Code
    /// gets (`None`: the router is not set up) and what the Router page shows.
    pub fn set_claude_router(
        &self,
        base_url: Option<url::Url>,
        view: Option<superba_core::RouterClaudeView>,
    ) {
        let mut c = self.areas.claude.lock().unwrap();
        c.router_base_url = base_url;
        c.router = view;
    }

    /// GH#5: report a change another tool made in the overview.
    pub fn set_external(&self, external: Option<superba_core::ExternalChange>) {
        self.state().external = external;
    }

    /// User bug 61: the running ChatGPT was opened outside Superba (from the
    /// Start menu, the Dock): no debugging port, so switches restart it fully
    /// until Superba opens it again.
    pub fn open_app_elsewhere(&self) {
        let mut st = self.state();
        if let Some(s) = st.app.session.as_mut() {
            s.cdp = None;
        }
        st.app.hot_switch = HotSwitchStatus::Unavailable {
            reason: FullRestartReason::NoCdp,
        };
    }

    /// The upstream the router last used for `profile` (user bug 105: what
    /// `router_status` reports as the member's `last_route`).
    pub fn set_last_route(&self, profile: ProfileId, route: RouterLineRoute) {
        self.state().last_routes.insert(profile, route);
    }

    /// GH#7: ChatGPT is installed but not running.
    pub fn quit_app(&self) {
        let mut st = self.state();
        st.app.session = None;
        st.app.hot_switch = HotSwitchStatus::NotRunning;
    }

    /// How often `open_app` was called (GH#7).
    pub fn open_app_calls(&self) -> u64 {
        self.open_calls.load(Ordering::SeqCst)
    }

    /// Store the router model list mode in the saved settings (a direct
    /// connection keeps it as a preference).
    pub fn set_router_catalog_mode(&self, mode: superba_core::CatalogMode) {
        self.state().profiles.settings.router.catalog.mode = mode;
    }

    fn state(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    async fn delay(&self, ms: u64) {
        if self.options.fast {
            tokio::task::yield_now().await;
        } else {
            let ms = if self.options.slow { ms * 4 } else { ms };
            tokio::time::sleep(Duration::from_millis(ms)).await;
        }
    }

    fn elapsed(&self, start: Instant) -> u64 {
        if self.options.fast {
            1200
        } else {
            start.elapsed().as_millis() as u64
        }
    }
}

fn ago(now: Timestamp, seconds: u64) -> Timestamp {
    now.saturating_sub(Duration::from_secs(seconds))
}

fn fake_hash(n: u64) -> String {
    format!("{n:064x}")
}

fn not_found(what: impl ToString) -> Error {
    Error::NotFound {
        what: what.to_string(),
    }
}

fn full(reason: FullRestartReason) -> RestartKind {
    RestartKind::FullAppRestart { reason }
}

fn hot(host_id: &str, kill_remote: bool) -> RestartKind {
    RestartKind::AppServerHot {
        host_id: host_id.into(),
        kill_remote,
    }
}

fn key_ref(id: &ProfileId) -> KeyRef {
    KeyRef::Keyring {
        service: "superba".into(),
        account: format!("profile:{id}"),
    }
}

fn krill_profile(line: LineId, auth: AuthMode) -> Profile {
    Profile {
        model_overrides: Default::default(),
        model_picker: Default::default(),
        compaction: Default::default(),
        anthropic: None,
        model_source: None,
        id: "krill".into(),
        name: "Krill".into(),
        builtin: None,
        site_id: "krill".into(),
        line_id: Some(line),
        auth_mode: auth,
        key_ref: Some(key_ref(&"krill".into())),
        base_url_override: None,
        model: ModelDefaults::default(),
        auth: superba_core::AuthChoice::from_mode(auth),
        backup_lines: Vec::new(),
        disabled_lines: Default::default(),
        codex_settings: Default::default(),
    }
}

fn auth_shape(mode: AuthMode) -> AuthShape {
    if mode == AuthMode::Pat {
        AuthShape::Pat {
            key_last4: "9f3a".into(),
        }
    } else if mode.uses_key() {
        AuthShape::ApiKey {
            key_last4: "9f3a".into(),
        }
    } else {
        AuthShape::ChatGpt {
            email: Some("user@example.com".into()),
            account_id: None,
        }
    }
}

fn fake_account(now: Timestamp) -> AccountSummary {
    AccountSummary {
        display_name: Some("Test User".into()),
        email: Some("user@example.com".into()),
        key_last4: Some("9f3a".into()),
        plan_name: Some("Pro · Monthly".into()),
        balance: Some("128.40".into()),
        currency: Some("CNY".into()),
        expires_at: Some(now.saturating_add(Duration::from_secs(23 * 86400))),
        usage: [("5h", 42.0, 3 * 3600), ("Weekly", 18.0, 4 * 86400)]
            .into_iter()
            .map(|(label, used, seconds)| UsageWindow {
                label: label.into(),
                used_percent: Some(used),
                resets_at: Some(now.saturating_add(Duration::from_secs(seconds))),
            })
            .collect(),
        ..AccountSummary::default()
    }
}

fn app_session(now: Timestamp, launched: bool, origin: Option<String>) -> AppSession {
    AppSession {
        app_server_exe: None,
        install_id: Some("chatgpt-deb".into()),
        main_pid: 3120,
        started_at: Some(now),
        executable: Some(APP.into()),
        cdp: launched.then_some(CdpEndpoint {
            port: 9333,
            owner_verified: true,
        }),
        launched_by_us: launched,
        authapi_env: origin.map_or(EnvObservation::Absent, EnvObservation::Present),
        home_override: None,
        codex_home: Some(HOME.into()),
    }
}

fn fake_remote(now: Timestamp, devbox: bool) -> RemoteOverview {
    let (label, host_id, user) = if devbox {
        ("devbox", "devbox-7c1e", "dev")
    } else {
        ("gpu-01", "gpu-01-31aa", "ml")
    };
    let target_id = TargetId::remote(host_id);
    RemoteOverview {
        host: RemoteHost {
            target_id: target_id.clone(),
            label: label.into(),
            host_id: Some(host_id.into()),
            kind: superba_core::HostKind::Ssh,
            ssh: SshEndpoint {
                alias: devbox.then(|| label.into()),
                host: Some(format!("{label}.example.com")),
                port: Some(22),
                user: Some(user.into()),
                identity: None,
            },
            sources: vec![HostSource::GlobalState, HostSource::ProcessScan],
            connected: Some(true),
            wsl: None,
        },
        state: Some(RemoteState {
            disk_free_kib: None,
            kernel: None,
            arch: None,
            codex_install: None,
            codex_resolved_path: None,
            target_id,
            probed_at: now,
            os: RemoteOs::Linux,
            shell: if devbox {
                ShellKind::Bash
            } else {
                ShellKind::Zsh
            },
            codex_home: format!("/home/{user}/.codex"),
            codex_path: Some("/usr/local/bin/codex".into()),
            codex_version: Some(CodexVersion::new(0, if devbox { 162 } else { 158 }, 0)),
            config_sha256: Some(fake_hash(11)),
            auth_sha256: Some(fake_hash(12)),
            auth: auth_shape(if devbox {
                AuthMode::Pat
            } else {
                AuthMode::OfficialChatGpt
            }),
            model_provider: Some("openai".into()),
            openai_base_url: devbox
                .then(|| format!("{}/backend-api/codex", catalog::KRILL_ORIGIN_OVERSEAS)),
            machine_id: None,
            marker_machine_id: None,
            authapi_env: if devbox {
                EnvObservation::Present(catalog::KRILL_ORIGIN_OVERSEAS.into())
            } else {
                EnvObservation::Absent
            },
            app_server_pids: vec![if devbox { 812 } else { 2041 }],
            owned_paths: Vec::new(),
            config_toml: None,
            app_bash: None,
            login_fallback: None,
            gateway: None,
        }),
        error: None,
    }
}

impl FakeState {
    fn new(options: &FakeOptions) -> Self {
        let now = Timestamp::now();
        let first = options.first_run;
        let active = (!first).then(|| ActiveState {
            profile_id: Some("krill".into()),
            site_id: Some("krill".into()),
            line_id: Some("overseas".into()),
            auth_mode: Some(AuthMode::Pat),
            generation: 7,
            applied_at: ago(now, 7200),
        });
        let mut profiles = ProfileSet::default();
        // Existing page and accessibility fixtures exercise the complete interface.
        // Simple-page tests select their dedicated renderer explicitly.
        profiles.settings.simple_mode = false;
        if !first {
            profiles
                .profiles
                .push(krill_profile("overseas".into(), AuthMode::Pat));
            profiles.last_selected = Some("krill".into());
        }
        let bundled_version = CodexVersion {
            pre: Some("alpha.17.2".into()),
            ..CodexVersion::new(0, 162, 0)
        };
        let session = app_session(
            ago(now, 7100),
            !first,
            (!first).then(|| catalog::KRILL_ORIGIN_OVERSEAS.into()),
        );
        let processes = [
            (3120, CodexProcessKind::AppMain, 7100, false),
            (3188, CodexProcessKind::AppServer, 7099, false),
            (4211, CodexProcessKind::CliTui, 10800, true),
            (4380, CodexProcessKind::CliTui, 1200, false),
            (5022, CodexProcessKind::IdeAppServer, 600, false),
        ]
        .into_iter()
        .map(|(pid, kind, seconds, stale)| {
            let app = matches!(
                kind,
                CodexProcessKind::AppMain | CodexProcessKind::AppServer
            );
            let server = matches!(
                kind,
                CodexProcessKind::AppServer | CodexProcessKind::IdeAppServer
            );
            ProcessInfo {
                pid,
                ppid: (pid == 3188).then_some(3120),
                exe: Some(
                    if pid == 3120 {
                        APP
                    } else if app {
                        BUNDLED
                    } else {
                        "/usr/local/bin/codex"
                    }
                    .into(),
                ),
                cmdline: if server {
                    vec!["codex".into(), "app-server".into()]
                } else {
                    vec![if app { "ChatGPT" } else { "codex" }.into()]
                },
                started_at: Some(ago(now, seconds)),
                authapi_env: if pid == 5022 || (first && app) {
                    EnvObservation::Absent
                } else {
                    EnvObservation::Present(catalog::KRILL_ORIGIN_OVERSEAS.into())
                },
                codex_home: Some(HOME.into()),
                stale,
                kind,
                ide: None,
                cwd: None,
            }
        })
        .collect();
        let by_provider = if first {
            BTreeMap::from([
                (
                    "krill".into(),
                    ProviderSessions {
                        threads: 128,
                        rollouts: 131,
                        archived: 4,
                        compressed: 0,
                        distinct: 128,
                    },
                ),
                (
                    "openai".into(),
                    ProviderSessions {
                        threads: 12,
                        rollouts: 12,
                        distinct: 12,
                        ..Default::default()
                    },
                ),
            ])
        } else {
            BTreeMap::from([(
                "openai".into(),
                ProviderSessions {
                    threads: 174,
                    rollouts: 171,
                    archived: 6,
                    compressed: 2,
                    distinct: 174,
                },
            )])
        };
        let backup_dir = std::path::PathBuf::from(format!("{BACKUPS}/migrate-mig-20261012-0915"));
        Self {
            profiles,
            config: ConfigState {
                codex_home: HOME.into(),
                config_sha256: Some(fake_hash(7)),
                auth_sha256: Some(fake_hash(8)),
                parse_error: None,
                model_provider: Some(if first { "krill" } else { "openai" }.into()),
                provider_table_present: first,
                openai_base_url: (!first)
                    .then(|| format!("{}/backend-api/codex", catalog::KRILL_ORIGIN_OVERSEAS)),
                chatgpt_base_url: (!first)
                    .then(|| format!("{}/backend-api/", catalog::KRILL_ORIGIN_OVERSEAS)),
                model: Some("gpt-5.5".into()),
                model_owned: false,
                reasoning_effort: Some("high".into()),
                profile_override: None,
                cli_auth_credentials_store: None,
                auth: auth_shape(if first {
                    AuthMode::ApiKey
                } else {
                    AuthMode::Pat
                }),
                active,
                drift: vec![],
                auth_drift: None,
                handed_off: None,
                since_release: Vec::new(),
                since_release_auth: None,
                journal_pending: false,
            },
            app: AppOverview {
                installs: vec![AppInstall {
                    id: "chatgpt-deb".into(),
                    variant: AppVariant::ChatGpt,
                    path: "/usr/lib/chatgpt".into(),
                    executable: APP.into(),
                    app_version: Some("26.1013.2".into()),
                    bundled_codex: Some(BUNDLED.into()),
                    bundled_codex_version: Some(bundled_version.clone()),
                }],
                session: Some(session),
                hot_switch: if first {
                    HotSwitchStatus::Unavailable {
                        reason: FullRestartReason::NoCdp,
                    }
                } else {
                    HotSwitchStatus::Ready
                },
                authapi_line: (!first).then(|| "overseas".into()),
            },
            cli: CliOverview {
                binaries: vec![
                    CodexBinary {
                        path: "/usr/local/bin/codex".into(),
                        version: Some(CodexVersion::new(0, 162, 0)),
                        source: InstallSource::Npm,
                        path_rank: Some(0),
                    },
                    CodexBinary {
                        path: BUNDLED.into(),
                        version: Some(bundled_version),
                        source: InstallSource::AppBundled,
                        path_rank: None,
                    },
                ],
                processes,
            },
            remotes: vec![fake_remote(now, true), fake_remote(now, false)],
            account: (!first).then(|| fake_account(now)),
            account_summary_enabled: true,
            baseline_captured: !first,
            sessions: SessionReport {
                codex_home: HOME.into(),
                scanned_at: now,
                sqlite_paths: vec![format!("{HOME}/state_5.sqlite").into()],
                by_provider,
                inconsistent: 0,
                warnings: vec![],
            },
            history: if first {
                vec![]
            } else {
                vec![MigrationReceipt {
                    id: "mig-20261012-0915".into(),
                    codex_home: HOME.into(),
                    migrated_at: ago(now, 4 * 86400),
                    to_provider: "openai".into(),
                    by_source_provider: BTreeMap::from([("krill".into(), 128)]),
                    threads: 128,
                    rollouts: 131,
                    ledger_path: backup_dir.join("ledger.jsonl"),
                    backup_dir,
                    rolled_back: false,
                }]
            },
            caps: BTreeMap::new(),
            busy_armed: options.busy_turns,
            fail_armed: options.fail_verification,
            router_installed: false,
            last_routes: BTreeMap::new(),
            external: None,
        }
    }

    fn site(&self, id: &SiteId) -> Result<Site> {
        self.profiles
            .custom_sites
            .iter()
            .chain(catalog::builtin_sites().iter())
            .find(|s| &s.id == id)
            .cloned()
            .ok_or_else(|| not_found(id))
    }

    fn local_restart(&self) -> RestartKind {
        if self.app.session.as_ref().is_some_and(|s| s.cdp.is_some()) {
            hot("local", false)
        } else {
            full(FullRestartReason::NoCdp)
        }
    }
}

fn line_caps(site: &SiteId, line: &LineId) -> LineCaps {
    let blocked = site == "krill" && line == "cn-fast";
    let latency = match line.as_str() {
        "cn-fast" => 38,
        "overseas" => 162,
        _ => 74,
    };
    LineCaps {
        site_id: site.clone(),
        line_id: line.clone(),
        checked_at: Timestamp::now(),
        reachable: true,
        latency_ms: Some(latency),
        pat_ok: !blocked,
        apikey_ok: true,
        codex_path: Some(CodexPath::BackendApiCodex),
        blocked_by_edge: blocked,
        key_access: None,
        checks: [
            ProbeEndpoint::ModelsUnauth,
            ProbeEndpoint::Whoami,
            ProbeEndpoint::WhamAccountsCheck,
            ProbeEndpoint::BackendCodexModels,
            ProbeEndpoint::CodexV1Models,
        ]
        .into_iter()
        .map(|endpoint| {
            let edge = blocked && endpoint == ProbeEndpoint::WhamAccountsCheck;
            EndpointCheck {
                endpoint,
                status: Some(if edge { 200 } else { 401 }),
                latency_ms: Some(latency),
                ok: !edge,
                edge_blocked: edge,
                detail: None,
            }
        })
        .collect(),
        detail: blocked.then(caps_blocked),
    }
}

fn original_destination() -> PlanDestination {
    PlanDestination {
        profile_id: Some("original".into()),
        site_id: None,
        line_id: None,
        auth_mode: None,
        label: dest_original(),
    }
}

fn official_destination() -> PlanDestination {
    PlanDestination {
        profile_id: Some("official".into()),
        site_id: Some("official".into()),
        line_id: None,
        auth_mode: Some(AuthMode::OfficialChatGpt),
        label: dest_official(),
    }
}

fn migration_request(from: &str, to: &str) -> MigrationRequest {
    MigrationRequest {
        to_provider: to.into(),
        selectors: vec![MigrationSelector::Provider {
            provider: from.into(),
        }],
        include_archived: true,
        exclude: vec![],
    }
}

impl FakeState {
    fn destination(
        &self,
        profile: &Profile,
        line: LineId,
        auth: AuthMode,
    ) -> Result<PlanDestination> {
        let site = self.site(&profile.site_id)?;
        let entry = site.line(&line).ok_or_else(|| not_found(&line))?;
        Ok(PlanDestination {
            profile_id: Some(profile.id.clone()),
            site_id: Some(site.id.clone()),
            line_id: Some(line),
            auth_mode: Some(auth),
            label: dest()
                .arg("site", site.name.render_fallback())
                .arg("line", entry.name.render_fallback())
                .arg("auth", auth.text().render_fallback()),
        })
    }

    fn plan(&self, request: SwitchRequest, id: String) -> Result<SwitchPlan> {
        let current = self
            .config
            .active
            .as_ref()
            .and_then(|a| {
                let profile = self.profiles.profile(a.profile_id.as_ref()?)?;
                self.destination(profile, a.line_id.clone()?, a.auth_mode?)
                    .ok()
            })
            .unwrap_or_else(|| {
                if self.config.auth.mode() == Some(AuthMode::OfficialChatGpt) {
                    official_destination()
                } else {
                    original_destination()
                }
            });
        let mut plan = SwitchPlan {
            id,
            created_at: Timestamp::now(),
            request: request.clone(),
            from: self.config.active.clone(),
            to: current,
            primary: self.local_restart(),
            restarts: vec![],
            config: None,
            env: None,
            certificate_env: None,
            migration: None,
            remotes: vec![],
            active_threads: vec![],
            model_check: None,
            choices: vec![],
            alternatives: vec![],
            warnings: vec![],
            cleanup: vec![],
        };
        match &request.intent {
            SwitchIntent::Profile {
                profile_id,
                line_id,
                auth_mode,
            } => {
                let profile = self
                    .profiles
                    .profile(profile_id)
                    .ok_or_else(|| not_found(profile_id))?;
                let auth = auth_mode.unwrap_or(profile.auth_mode);
                // Built-in fields are placeholders: Original's ChatGPT mode is not its destination.
                if profile.builtin == Some(BuiltinProfile::Original) {
                    plan.to = original_destination();
                } else if profile.builtin == Some(BuiltinProfile::Official)
                    || auth == AuthMode::OfficialChatGpt
                {
                    plan.to = official_destination();
                    plan.to.profile_id = Some(profile_id.clone());
                    plan.primary = full(FullRestartReason::CrossAuthFamily);
                } else {
                    let line = line_id
                        .clone()
                        .or_else(|| profile.line_id.clone())
                        .or_else(|| {
                            self.site(&profile.site_id)
                                .ok()
                                .and_then(|site| site.default_line().map(|line| line.id.clone()))
                        })
                        .ok_or_else(|| not_found(&profile.site_id))?;
                    plan.to = self.destination(profile, line.clone(), auth)?;
                    plan.model_check = Some(ModelCheck {
                        current: Some("gpt-5.5".into()),
                        available: true,
                        suggestions: vec![],
                    });
                    if auth == AuthMode::Pat && line == "cn-fast" {
                        plan.primary = RestartKind::Blocked {
                            reasons: vec![BlockReason::AuthModeUnavailable {
                                line_id: line.clone(),
                                auth_mode: auth,
                            }],
                        };
                        let mut alternative = request.clone();
                        if let SwitchIntent::Profile { auth_mode, .. } = &mut alternative.intent {
                            *auth_mode = Some(AuthMode::ApiKey);
                        }
                        plan.alternatives.push(PlanAlternative {
                            label: alt_apikey(),
                            request: alternative,
                        });
                    } else if self.config.model_provider.as_deref() == Some("krill")
                        && self.sessions.by_provider.contains_key("krill")
                    {
                        plan.primary = full(FullRestartReason::SessionMigration);
                        match request.legacy_sessions {
                            None => plan.choices.push(PlanChoice::LegacySessions {
                                providers: BTreeMap::from([("krill".into(), 128)]),
                            }),
                            Some(LegacySessionChoice::Migrate) => {
                                plan.migration = Some(self.migration(
                                    &plan.id,
                                    migration_request("krill", "openai"),
                                    &request.migration_exclude,
                                )?)
                            }
                            Some(LegacySessionChoice::Skip) => {
                                plan.primary = full(FullRestartReason::NoCdp)
                            }
                        }
                    }
                    let count = self
                        .cli
                        .processes
                        .iter()
                        .filter(|p| p.kind == CodexProcessKind::CliTui)
                        .count();
                    plan.warnings
                        .push(Notice::info(warn_cli().arg("count", count)));
                    if auth == AuthMode::Pat
                        && let Some(old) = &self.app.authapi_line
                        && old != &line
                    {
                        let old_name = catalog::krill_site()
                            .line(old)
                            .map(|l| l.name.render_fallback())
                            .unwrap_or_else(|| old.to_string());
                        plan.warnings
                            .push(Notice::info(warn_authapi().arg("line", old_name)));
                    }
                }
            }
            SwitchIntent::Restore { target } => {
                plan.to = original_destination();
                match target {
                    RestoreTarget::Official => {
                        plan.to = official_destination();
                        plan.primary = full(FullRestartReason::CrossAuthFamily);
                        plan.warnings
                            .push(Notice::warning(warn_login_age().arg("days", 3)));
                    }
                    RestoreTarget::Original {
                        provider: ProviderRestore::Baseline { reverse_migrate },
                    } => {
                        if *reverse_migrate {
                            plan.primary = full(FullRestartReason::SessionMigration);
                            plan.migration = Some(self.migration(
                                &plan.id,
                                migration_request("openai", "krill"),
                                &request.migration_exclude,
                            )?);
                        } else {
                            plan.warnings
                                .push(Notice::warning(warn_hidden().arg("count", 174)));
                        }
                    }
                    _ => {}
                }
            }
            SwitchIntent::MigrateSessions { request: migration } => {
                plan.primary = full(FullRestartReason::SessionMigration);
                plan.migration = Some(self.migration(
                    &plan.id,
                    migration.clone(),
                    &request.migration_exclude,
                )?);
            }
            SwitchIntent::RollbackMigration { migration_id } => {
                let receipt = self
                    .history
                    .iter()
                    .find(|r| &r.id == migration_id)
                    .ok_or_else(|| not_found(migration_id))?;
                let to = receipt
                    .by_source_provider
                    .keys()
                    .next()
                    .map_or("krill", String::as_str);
                let mut migration = migration_request(&receipt.to_provider, to);
                migration.selectors = vec![MigrationSelector::Ledger {
                    migration_id: migration_id.clone(),
                }];
                plan.primary = full(FullRestartReason::SessionMigration);
                plan.migration =
                    Some(self.migration(&plan.id, migration, &request.migration_exclude)?);
            }
            SwitchIntent::RestartApp { .. } => {
                plan.primary = full(FullRestartReason::NoCdp);
                plan.warnings.push(Notice::info(restart_app()));
            }
            SwitchIntent::Uninstall { .. } => {
                plan.to = original_destination();
                plan.primary = full(FullRestartReason::CrossAuthFamily);
                plan.cleanup = vec![
                    cleanup_marker(),
                    cleanup_legacy(),
                    cleanup_launcher(),
                    cleanup_backups(),
                ];
            }
            SwitchIntent::RollbackRemote { target_id } => {
                plan.primary = self.remote_restart(target_id)?.restart;
            }
            _ => {
                return Err(Error::Unsupported {
                    what: "fake switch intent".into(),
                });
            }
        }
        plan.restarts = vec![
            ConsumerRestart {
                consumer: ConsumerKind::DesktopApp,
                restart: plan.primary.clone(),
                pids: vec![3120, 3188],
            },
            ConsumerRestart {
                consumer: ConsumerKind::CliProcesses,
                restart: RestartKind::NextLaunch,
                pids: self
                    .cli
                    .processes
                    .iter()
                    .filter(|p| p.kind == CodexProcessKind::CliTui)
                    .map(|p| p.pid)
                    .collect(),
            },
        ];
        let mut targets = request.targets.remotes.clone();
        if let SwitchIntent::RollbackRemote { target_id } = &request.intent
            && !targets.contains(target_id)
        {
            targets.push(target_id.clone());
        }
        for target in &targets {
            plan.restarts.push(self.remote_restart(target)?);
        }
        if !targets.is_empty() {
            let hosts = targets
                .iter()
                .filter_map(|t| self.remotes.iter().find(|r| &r.host.target_id == t))
                .map(|r| r.host.label.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            plan.warnings
                .push(Notice::warning(warn_remotes().arg("hosts", hosts)));
        }
        let after = self.destination_config(&plan.to, &request)?;
        plan.config = Some(config_plan(&self.config, &after, &plan));
        Ok(plan)
    }

    fn remote_restart(&self, id: &TargetId) -> Result<ConsumerRestart> {
        let remote = self
            .remotes
            .iter()
            .find(|r| &r.host.target_id == id)
            .ok_or_else(|| not_found(id))?;
        Ok(ConsumerRestart {
            consumer: ConsumerKind::Remote {
                target_id: id.clone(),
            },
            restart: hot(remote.host.host_id.as_deref().unwrap_or(id.as_str()), true),
            pids: remote
                .state
                .as_ref()
                .map(|s| s.app_server_pids.clone())
                .unwrap_or_default(),
        })
    }

    fn destination_config(
        &self,
        to: &PlanDestination,
        request: &SwitchRequest,
    ) -> Result<ConfigState> {
        let mut config = self.config.clone();
        if matches!(
            request.intent,
            SwitchIntent::RestartApp { .. }
                | SwitchIntent::MigrateSessions { .. }
                | SwitchIntent::RollbackMigration { .. }
                | SwitchIntent::RollbackRemote { .. }
        ) {
            return Ok(config);
        }
        config.openai_base_url = None;
        config.chatgpt_base_url = None;
        config.provider_table_present = false;
        let auth = to.auth_mode.unwrap_or(AuthMode::ApiKey);
        config.auth = auth_shape(auth);
        config.model_provider = Some("openai".into());
        if to.auth_mode == Some(AuthMode::OfficialChatGpt) {
            config.model_provider = None;
        }
        if matches!(
            request.intent,
            SwitchIntent::Restore {
                target: RestoreTarget::Original {
                    provider: ProviderRestore::Baseline { .. }
                }
            } | SwitchIntent::Uninstall {
                provider: Some(ProviderRestore::Baseline { .. }),
                ..
            }
        ) {
            config.model_provider = Some("krill".into());
            config.provider_table_present = true;
        }
        if auth.uses_key()
            && let (Some(site), Some(line)) = (&to.site_id, &to.line_id)
        {
            let site = self.site(site)?;
            let line = site.line(line).ok_or_else(|| not_found(line))?;
            if site.kind == SiteKind::Compatible {
                config.openai_base_url = Some(superba_gui::pages::providers::form::base_url(&site));
            } else {
                let endpoints =
                    ResolvedEndpoints::for_line(&line.origin, CodexPath::BackendApiCodex, auth)?;
                config.openai_base_url = Some(endpoints.openai_base_url.to_string());
                config.chatgpt_base_url = endpoints.chatgpt_base_url.map(|u| u.to_string());
            }
            if let Some(profile) = to
                .profile_id
                .as_ref()
                .and_then(|id| self.profiles.profile(id))
            {
                if let Some(base) = &profile.base_url_override {
                    config.openai_base_url = Some(base.to_string());
                }
                if let Some(model) = &profile.model.model {
                    config.model = Some(model.clone());
                }
                if let Some(effort) = &profile.model.reasoning_effort {
                    config.reasoning_effort = Some(effort.clone());
                }
            }
        }
        if let Some(model) = &request.model_override {
            config.model = Some(model.clone());
        }
        Ok(config)
    }
}

fn toml_string(value: &Option<String>) -> Option<String> {
    value.as_ref().map(|s| {
        format!(
            "\"{}\"",
            s.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\t', "\\t")
        )
    })
}

fn config_plan(before: &ConfigState, after: &ConfigState, plan: &SwitchPlan) -> ApplyPlan {
    let generation = before.active.as_ref().map_or(1, |a| a.generation + 1);
    let key_changes: Vec<_> = [
        (
            "model_provider",
            &before.model_provider,
            &after.model_provider,
        ),
        (
            "openai_base_url",
            &before.openai_base_url,
            &after.openai_base_url,
        ),
        (
            "chatgpt_base_url",
            &before.chatgpt_base_url,
            &after.chatgpt_base_url,
        ),
        ("model", &before.model, &after.model),
        (
            "model_reasoning_effort",
            &before.reasoning_effort,
            &after.reasoning_effort,
        ),
    ]
    .into_iter()
    .filter(|(_, a, b)| a != b)
    .map(|(path, a, b)| KeyChange {
        path: path.into(),
        before: toml_string(a),
        after: toml_string(b),
    })
    .collect();
    let auth_change = (before.auth != after.auth).then(|| AuthChange {
        before: before.auth.clone(),
        after: after.auth.clone(),
        stash_login: matches!(before.auth, AuthShape::ChatGpt { .. }),
    });
    let files = [
        (
            CodexFileKind::ConfigToml,
            "config.toml",
            &before.config_sha256,
            !key_changes.is_empty(),
        ),
        (
            CodexFileKind::AuthJson,
            "auth.json",
            &before.auth_sha256,
            auth_change.is_some(),
        ),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (kind, name, hash, changed))| PlannedFile {
        kind,
        path: std::path::Path::new(HOME).join(name),
        before_sha256: hash.clone(),
        change: if changed {
            FileChange::Write {
                sha256: fake_hash(generation * 2 + i as u64),
                contents: SecretBytes::default(),
            }
        } else {
            FileChange::Unchanged
        },
    })
    .collect();
    ApplyPlan {
        id: plan.id.clone(),
        codex_home: HOME.into(),
        generation,
        profile_id: plan.to.profile_id.clone(),
        site_id: plan.to.site_id.clone(),
        line_id: plan.to.line_id.clone(),
        auth_mode: plan.to.auth_mode,
        files,
        key_changes,
        auth_change,
        notes: vec![],
        desired: None,
        releases_ownership: false,
    }
}

impl FakeState {
    fn migration(
        &self,
        plan_id: &str,
        mut request: MigrationRequest,
        exclude: &[String],
    ) -> Result<MigrationPlan> {
        request.exclude.extend_from_slice(exclude);
        let mut selected = BTreeMap::<String, (u32, u32)>::new();
        for selector in &request.selectors {
            let (source, threads, rollouts) = match selector {
                MigrationSelector::OtherProviders { except } => {
                    for (provider, counts) in &self.sessions.by_provider {
                        if provider != except && provider != &request.to_provider {
                            selected.insert(provider.clone(), (counts.threads, counts.rollouts));
                        }
                    }
                    continue;
                }
                MigrationSelector::Provider { provider }
                | MigrationSelector::CreatedBetween { provider, .. } => {
                    let counts = self
                        .sessions
                        .by_provider
                        .get(provider)
                        .cloned()
                        .unwrap_or_default();
                    (provider.clone(), counts.threads, counts.rollouts)
                }
                MigrationSelector::Ledger { migration_id } => {
                    let r = self
                        .history
                        .iter()
                        .find(|r| &r.id == migration_id)
                        .ok_or_else(|| not_found(migration_id))?;
                    (r.to_provider.clone(), r.threads, r.rollouts)
                }
                MigrationSelector::ThreadIds { ids } => {
                    let source = self
                        .config
                        .model_provider
                        .clone()
                        .unwrap_or_else(|| "openai".into());
                    (source, ids.len() as u32, ids.len() as u32)
                }
            };
            if source != request.to_provider && threads > 0 {
                let entry = selected.entry(source).or_default();
                *entry = (entry.0.max(threads), entry.1.max(rollouts));
            }
        }
        let titles = [
            "Refactor the login module",
            "Data migration script",
            "Build the analytics dashboard",
            "Review the release checklist",
            "Improve search performance",
        ];
        let now = Timestamp::now();
        let mut items = Vec::new();
        for (source, (count, rollouts)) in &mut selected {
            for (i, title) in titles.iter().take((*count).min(5) as usize).enumerate() {
                let thread_id = format!("{source}-fake-{}", i + 1);
                if request.exclude.contains(&thread_id) {
                    *count = count.saturating_sub(1);
                    *rollouts = rollouts.saturating_sub(1);
                } else {
                    items.push(MigrationItem {
                        thread_id,
                        title: Some((*title).into()),
                        source_provider: source.clone(),
                        created_at: Some(ago(now, (i as u64 + 2) * 86400)),
                        archived: request.include_archived && i == 4,
                    });
                }
            }
        }
        let id = format!("mig-{plan_id}");
        Ok(MigrationPlan {
            backup_dir: format!("{BACKUPS}/migrate-{id}").into(),
            id,
            codex_home: HOME.into(),
            request,
            thread_count: selected.values().map(|c| c.0).sum(),
            rollout_count: selected.values().map(|c| c.1).sum(),
            by_source_provider: selected.into_iter().map(|(p, c)| (p, c.0)).collect(),
            backup_bytes: 28_450_816,
            items,
        })
    }

    fn migrate(&mut self, plan: &MigrationPlan) {
        let mut remaining_rollouts = plan.rollout_count;
        let mut moved = ProviderSessions::default();
        for (provider, count) in &plan.by_source_provider {
            if let Some(source) = self.sessions.by_provider.get_mut(provider) {
                let threads = (*count).min(source.threads);
                let rollouts = remaining_rollouts.min(source.rollouts);
                let archived = if plan.request.include_archived {
                    source.archived.min(threads)
                } else {
                    0
                };
                let compressed = source.compressed.min(rollouts);
                moved.threads += threads;
                moved.rollouts += rollouts;
                moved.distinct += threads;
                moved.archived += archived;
                moved.compressed += compressed;
                source.threads -= threads;
                source.rollouts -= rollouts;
                source.distinct = source.distinct.saturating_sub(threads);
                source.archived -= archived;
                source.compressed -= compressed;
                remaining_rollouts -= rollouts;
            }
        }
        let destination = self
            .sessions
            .by_provider
            .entry(plan.request.to_provider.clone())
            .or_default();
        destination.threads += moved.threads;
        destination.rollouts += moved.rollouts;
        destination.distinct += moved.distinct;
        destination.archived += moved.archived;
        destination.compressed += moved.compressed;
        self.sessions
            .by_provider
            .retain(|_, c| c.distinct + c.rollouts + c.archived > 0);
        self.sessions.scanned_at = Timestamp::now();
        for selector in &plan.request.selectors {
            if let MigrationSelector::Ledger { migration_id } = selector
                && let Some(r) = self.history.iter_mut().find(|r| &r.id == migration_id)
            {
                r.rolled_back = true;
            }
        }
        self.history.insert(
            0,
            MigrationReceipt {
                id: plan.id.clone(),
                codex_home: HOME.into(),
                migrated_at: Timestamp::now(),
                to_provider: plan.request.to_provider.clone(),
                by_source_provider: plan.by_source_provider.clone(),
                threads: moved.threads,
                rollouts: moved.rollouts,
                backup_dir: plan.backup_dir.clone(),
                ledger_path: plan.backup_dir.join("ledger.jsonl"),
                rolled_back: false,
            },
        );
    }

    fn launch(&mut self) -> AppSession {
        let origin = self
            .config
            .chatgpt_base_url
            .as_ref()
            .and_then(|u| url::Url::parse(u).ok())
            .map(|u| u.origin().ascii_serialization());
        let session = app_session(Timestamp::now(), true, origin);
        self.app.hot_switch = HotSwitchStatus::Ready;
        self.app.authapi_line = if self.config.auth.mode() == Some(AuthMode::Pat) {
            self.config.active.as_ref().and_then(|a| a.line_id.clone())
        } else {
            None
        };
        self.app.session = Some(session.clone());
        session
    }

    fn commit(
        &mut self,
        plan: &SwitchPlan,
        mut config: ConfigState,
        launched: bool,
        migrated: bool,
    ) -> u64 {
        let now = Timestamp::now();
        let generation = self.config.active.as_ref().map_or(1, |a| a.generation + 1);
        config.active = Some(ActiveState {
            profile_id: plan.to.profile_id.clone(),
            site_id: plan.to.site_id.clone(),
            line_id: plan.to.line_id.clone(),
            auth_mode: plan.to.auth_mode,
            generation,
            applied_at: now,
        });
        if let Some(apply) = &plan.config {
            for file in &apply.files {
                if let FileChange::Write { sha256, .. } = &file.change {
                    match file.kind {
                        CodexFileKind::ConfigToml => config.config_sha256 = Some(sha256.clone()),
                        CodexFileKind::AuthJson => config.auth_sha256 = Some(sha256.clone()),
                        CodexFileKind::ClaudeSettings => {}
                    }
                }
            }
        }
        self.config = config;
        self.baseline_captured = true;
        self.account = Some(fake_account(now));
        self.profiles.last_selected = plan.to.profile_id.clone();
        if plan.to.site_id.as_ref().is_some_and(|s| s == "krill")
            && self.profiles.profile(&"krill".into()).is_none()
        {
            self.profiles.profiles.push(krill_profile(
                plan.to.line_id.clone().unwrap_or_else(|| "overseas".into()),
                plan.to.auth_mode.unwrap_or(AuthMode::Pat),
            ));
        }
        if let Some(p) = self
            .profiles
            .profiles
            .iter_mut()
            .find(|p| Some(&p.id) == plan.to.profile_id.as_ref())
            && !p.is_builtin()
        {
            p.line_id = plan.to.line_id.clone();
            if let Some(auth) = plan.to.auth_mode {
                p.auth_mode = auth;
            }
        }
        if migrated && let Some(migration) = &plan.migration {
            self.migrate(migration);
        }
        if launched {
            self.launch();
        }
        let app_env = self
            .app
            .session
            .as_ref()
            .map(|s| s.authapi_env.clone())
            .unwrap_or_default();
        for p in &mut self.cli.processes {
            if p.kind == CodexProcessKind::CliTui {
                p.stale = true;
            }
            if migrated && p.kind == CodexProcessKind::AppServer {
                p.pid += 1;
                p.started_at = Some(now);
                p.authapi_env = app_env.clone();
                p.stale = false;
            }
            if launched && p.kind == CodexProcessKind::AppMain {
                p.started_at = Some(now);
                p.authapi_env = app_env.clone();
            }
        }
        if migrated {
            for target in remote_ids(plan) {
                if let Some(remote) = self.remotes.iter_mut().find(|r| r.host.target_id == target)
                    && let Some(state) = &mut remote.state
                {
                    if matches!(plan.request.intent, SwitchIntent::RollbackRemote { .. }) {
                        *remote = fake_remote(now, remote.host.label == "devbox");
                    } else {
                        state.auth = self.config.auth.clone();
                        state.model_provider = self.config.model_provider.clone();
                        state.openai_base_url = self.config.openai_base_url.clone();
                        state.config_sha256 = self.config.config_sha256.clone();
                        state.auth_sha256 = self.config.auth_sha256.clone();
                        for pid in &mut state.app_server_pids {
                            *pid += 1;
                        }
                    }
                }
            }
        }
        generation
    }
}

fn phase(events: &dyn SwitchEvents, phase: SwitchPhase) {
    events.emit(SwitchEvent::Phase {
        phase,
        at: Timestamp::now(),
    });
}

fn step_status(events: &dyn SwitchEvents, step: SwitchStep, status: StepStatus) {
    events.emit(SwitchEvent::Step {
        step,
        status,
        at: Timestamp::now(),
    });
}

fn log(events: &dyn SwitchEvents, text: LocalizedText) {
    events.emit(SwitchEvent::Log {
        level: Severity::Info,
        text,
    });
}

fn remote_ids(plan: &SwitchPlan) -> impl Iterator<Item = TargetId> + '_ {
    plan.restarts.iter().filter_map(|r| match &r.consumer {
        ConsumerKind::Remote { target_id } => Some(target_id.clone()),
        _ => None,
    })
}

fn busy_threads() -> Vec<ThreadActivity> {
    [
        (
            "local",
            "t-31",
            "Refactor the login module",
            ActivitySignal::RolloutTail,
        ),
        (
            "ssh:devbox-7c1e",
            "t-84",
            "Data migration script",
            ActivitySignal::RolloutMtime,
        ),
    ]
    .into_iter()
    .map(|(target, id, title, signal)| ThreadActivity {
        target_id: target.into(),
        thread_id: Some(id.into()),
        title: Some(title.into()),
        rollout_path: None,
        last_event_at: Some(Timestamp::now()),
        signal,
        originator: None,
        cwd: None,
    })
    .collect()
}

impl FakeService {
    async fn wait_turns(
        &self,
        quiet_ms: u64,
        decider: &dyn SwitchDecider,
        events: &dyn SwitchEvents,
    ) -> bool {
        phase(events, SwitchPhase::WaitingTurn);
        let busy = busy_threads();
        let start = Instant::now();
        let mut question = Some(decider.decide(DecisionRequest::WaitForTurns {
            busy: busy.clone(),
            waited_ms: 0,
        }));
        let mut tick = tokio::time::interval(Duration::from_millis(if self.options.fast {
            50
        } else {
            500
        }));
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                answer = async { match &mut question { Some(f) => f.await, None => std::future::pending().await } } => {
                    question = None;
                    match answer { Decision::Force => return true, Decision::Wait => {}, _ => return false }
                }
                _ = tick.tick() => {
                    let waited_ms = start.elapsed().as_millis() as u64;
                    let local_done = waited_ms >= if self.options.fast { 150 } else { 5000 };
                    let remote_done = waited_ms >= if self.options.fast { 300 } else { 8000 };
                    let activity = busy.iter().enumerate().map(|(i, thread)| TurnActivity {
                        target_id: thread.target_id.clone(), checked_at: Timestamp::now(), complete: true,
                        busy: if (i == 0 && local_done) || (i == 1 && remote_done) { vec![] } else { vec![thread.clone()] },
                    }).collect();
                    events.emit(SwitchEvent::Turns { activity, waited_ms });
                    if remote_done { break; }
                }
            }
        }
        // Closing the GUI's pending question must happen before the quiet period.
        drop(question);
        self.delay(quiet_ms).await;
        true
    }

    async fn step(&self, events: &dyn SwitchEvents, step: SwitchStep, ms: u64) {
        step_status(events, step.clone(), StepStatus::Running);
        self.delay(ms).await;
        step_status(events, step, StepStatus::Done);
    }

    async fn apply_steps(&self, plan: &SwitchPlan, migrate: bool, events: &dyn SwitchEvents) {
        phase(events, SwitchPhase::Applying);
        self.step(events, SwitchStep::WriteConfig, 150).await;
        if plan.to.auth_mode == Some(AuthMode::Pat) {
            self.step(events, SwitchStep::WriteEnv, 120).await;
        } else {
            step_status(events, SwitchStep::WriteEnv, StepStatus::Skipped);
        }
        if migrate && plan.migration.is_some() {
            self.step(events, SwitchStep::MigrateSessions, 900).await;
        }
        let generation = plan.config.as_ref().map_or(1, |c| c.generation);
        log(events, log_applied().arg("generation", generation));
    }

    async fn restart_steps(
        &self,
        plan: &SwitchPlan,
        full_restart: bool,
        events: &dyn SwitchEvents,
    ) {
        if full_restart {
            phase(events, SwitchPhase::Quitting);
            self.step(events, SwitchStep::QuitApp, 600).await;
        }
        self.apply_steps(plan, true, events).await;
        if full_restart {
            phase(events, SwitchPhase::Launching);
            self.step(events, SwitchStep::LaunchApp, 1500).await;
        } else {
            phase(events, SwitchPhase::Restarting);
            self.step(events, SwitchStep::RestartCore, 200).await;
            let host = match &plan.primary {
                RestartKind::AppServerHot { host_id, .. } => host_id.as_str(),
                _ => "local",
            };
            log(events, log_restart().arg("host", host));
        }
        for target_id in remote_ids(plan) {
            self.step(events, SwitchStep::Remote { target_id }, 180)
                .await;
        }
    }

    async fn verify(&self, fail: bool, events: &dyn SwitchEvents) -> Verification {
        phase(events, SwitchPhase::Verifying);
        let start = Instant::now();
        let mut result = Verification::default();
        for (step, check, ms) in [
            (SwitchStep::VerifyProcess, &mut result.process, 120),
            (SwitchStep::VerifyFiles, &mut result.files, 140),
            (SwitchStep::VerifyAuth, &mut result.auth, 160),
            (SwitchStep::VerifyLine, &mut result.line, 120),
            (SwitchStep::VerifyRenderer, &mut result.renderer, 130),
        ] {
            step_status(events, step.clone(), StepStatus::Running);
            self.delay(ms).await;
            if fail && step == SwitchStep::VerifyAuth {
                *check = CheckResult::Failed {
                    detail: TIMEOUT.into(),
                };
                step_status(
                    events,
                    step,
                    StepStatus::Failed {
                        message: TIMEOUT.into(),
                    },
                );
                break;
            }
            *check = CheckResult::Passed;
            step_status(events, step, StepStatus::Done);
        }
        if result.passed() {
            log(
                events,
                log_verified().arg(
                    "ms",
                    if self.options.fast {
                        670
                    } else {
                        start.elapsed().as_millis()
                    },
                ),
            );
        }
        result
    }
}

#[superba_core::async_trait]
impl SwitchService for FakeService {
    async fn switch_turn_activity(&self, targets: Vec<TargetId>) -> Result<Vec<TurnActivity>> {
        Ok(targets
            .into_iter()
            .map(|target_id| TurnActivity {
                target_id,
                checked_at: Timestamp::now(),
                busy: vec![],
                complete: true,
            })
            .collect())
    }
    async fn data_usage(&self) -> Result<superba_core::data::DataUsage> {
        self.areas
            .storage
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| Err(Error::not_implemented("data usage")))
    }

    async fn data_where(&self) -> Result<superba_core::data::DataLocation> {
        self.data_usage().await.map(|u| u.location)
    }

    async fn overview(&self) -> Result<Overview> {
        self.delay(200).await;
        let s = self.state();
        Ok(Overview {
            generated_at: Timestamp::now(),
            codex_home: HOME.into(),
            active: s.config.active.clone(),
            config: Some(s.config.clone()),
            app: s.app.clone(),
            cli: s.cli.clone(),
            remotes: s.remotes.clone(),
            account: s.account.clone().filter(|_| s.account_summary_enabled),
            baseline_captured: s.baseline_captured,
            claude_routed: false,
            router_base_url: s
                .router_installed
                .then(|| url::Url::parse("http://127.0.0.1:4321/backend-api/codex").unwrap()),
            baseline_at: s
                .baseline_captured
                .then(|| Timestamp::from_millis(1_760_572_800_000)),
            login_stash: Some(LoginStashInfo {
                stashed_at: Timestamp::from_millis(1_760_572_800_000),
                last_refresh: None,
                id_token_exp: None,
                email: None,
                account_id: None,
            }),
            notices: vec![],
            external: s.external.clone(),
            runtime: None,
        })
    }

    async fn sites(&self) -> Result<Vec<Site>> {
        let mut sites = catalog::builtin_sites();
        for site in &mut sites {
            if site.kind == SiteKind::Krill {
                site.capabilities.account_summary = self.state().account_summary_enabled;
            }
        }
        sites.extend(self.state().profiles.custom_sites.clone());
        Ok(sites)
    }

    async fn profile_set(&self) -> Result<ProfileSet> {
        Ok(self.state().profiles.clone())
    }

    async fn set_autostart(
        &self,
        enabled: bool,
    ) -> Result<superba_core::autostart::AutostartStatus> {
        self.state().profiles.settings.autostart = enabled;
        Ok(superba_core::autostart::AutostartStatus {
            enabled,
            ..Default::default()
        })
    }

    async fn router_status(&self) -> Result<RouterView> {
        let s = self.state();
        let members = &s.profiles.settings.router.members;
        Ok(RouterView {
            settings: s.profiles.settings.router.clone(),
            translation: self.router_translation(),
            failover: (!members.is_empty()).then(|| self.router_failover()),
            configured: !members.is_empty(),
            installed: s.router_installed,
            running: s.router_installed,
            port: s.router_installed.then_some(4321),
            stats: s.router_installed.then_some(RouterStatsStatus {
                recording: true,
                rows_written: 1_234,
                dropped: 0,
                db_bytes: Some(2_500_000),
            }),
            members: members
                .iter()
                .map(|id| RouterMemberView {
                    protocols: Vec::new(),
                    last_route: s.last_routes.get(id).cloned(),
                    profile_id: id.clone(),
                    name: s
                        .profiles
                        .profile(id)
                        .map_or_else(|| id.to_string(), |p| p.name.clone()),
                    lines: Vec::new(),
                })
                .collect(),
            claude: self.areas.claude.lock().unwrap().router.clone(),
            ..RouterView::default()
        })
    }

    async fn router_set_conversion(&self, conversion: RouterConversion) -> Result<RouterView> {
        self.edit_router_conversion(conversion).await
    }

    async fn router_set_failover(
        &self,
        failover: superba_core::RouterFailover,
    ) -> Result<RouterView> {
        self.edit_router_failover(failover).await
    }

    async fn router_install(&self, request: RouterInstallRequest) -> Result<RouterView> {
        {
            let mut s = self.state();
            for id in &request.members {
                if s.profiles.profile(id).is_none_or(|p| p.key_ref.is_none()) {
                    return Err(not_found(id));
                }
            }
            if !request.members.is_empty() {
                s.profiles.settings.router.members = request.members;
            }
            s.router_installed = true;
        }
        self.router_status().await
    }

    async fn router_uninstall(&self) -> Result<RouterView> {
        {
            let mut s = self.state();
            s.profiles.settings.router.members.clear();
            s.router_installed = false;
        }
        self.router_status().await
    }

    async fn save_profile(&self, mut profile: Profile, key: Option<Secret>) -> Result<Profile> {
        let mut s = self.state();
        if profile.builtin.is_some()
            || s.profiles
                .profile(&profile.id)
                .is_some_and(Profile::is_builtin)
        {
            return Err(Error::invalid("built-in profiles cannot be changed"));
        }
        let site = s.site(&profile.site_id)?;
        if !site.supports(profile.auth_mode) {
            return Err(Error::invalid("the site does not support this auth mode"));
        }
        if key.as_ref().is_some_and(Secret::is_empty) {
            return Err(Error::invalid("empty key"));
        }
        if key.is_some() && profile.key_ref.is_none() {
            profile.key_ref = Some(key_ref(&profile.id));
        }
        if let Some(old) = s.profiles.profiles.iter_mut().find(|p| p.id == profile.id) {
            *old = profile.clone();
        } else {
            s.profiles.profiles.push(profile.clone());
        }
        Ok(profile)
    }

    async fn delete_profile(&self, id: &ProfileId) -> Result<()> {
        let mut s = self.state();
        let profile = s.profiles.profile(id).ok_or_else(|| not_found(id))?;
        if profile.is_builtin() {
            return Err(Error::invalid("cannot delete a built-in profile"));
        }
        s.profiles.profiles.retain(|p| &p.id != id);
        if s.profiles.last_selected.as_ref() == Some(id) {
            s.profiles.last_selected = None;
        }
        Ok(())
    }

    async fn save_settings(&self, settings: Settings) -> Result<()> {
        self.state().profiles.settings = settings;
        Ok(())
    }

    async fn save_site(&self, site: Site) -> Result<Site> {
        if site.name.render_fallback().trim().is_empty() {
            return Err(Error::invalid("the site name must not be empty"));
        }
        if site.lines.is_empty() {
            return Err(Error::invalid("a site must have at least one line"));
        }
        let mut s = self.state();
        if let Some(old) = s.profiles.custom_sites.iter_mut().find(|s| s.id == site.id) {
            *old = site.clone();
        } else {
            s.profiles.custom_sites.push(site.clone());
        }
        Ok(site)
    }

    async fn delete_site(&self, id: &SiteId) -> Result<()> {
        let mut s = self.state();
        if s.profiles.profiles.iter().any(|p| &p.site_id == id) {
            return Err(Error::Conflict {
                message: format!("site {id} is referenced by a profile"),
            });
        }
        if !s.profiles.custom_sites.iter().any(|s| &s.id == id) {
            return Err(not_found(id));
        }
        s.profiles.custom_sites.retain(|s| &s.id != id);
        s.caps.retain(|(site, _), _| site != id);
        Ok(())
    }

    async fn probe_lines(&self, site_id: &SiteId) -> Result<Vec<LineCaps>> {
        self.delay(700).await;
        let mut s = self.state();
        s.site(site_id)?
            .lines
            .iter()
            .map(|line| {
                let key = (site_id.clone(), line.id.clone());
                let mut caps = line_caps(site_id, &line.id);
                caps.key_access = s.caps.get(&key).and_then(|c| c.key_access.clone());
                s.caps.insert(key, caps.clone());
                Ok(caps)
            })
            .collect()
    }

    async fn check_key(
        &self,
        site: &SiteId,
        line: &LineId,
        mode: AuthMode,
        key: KeyInput,
    ) -> Result<KeyCheck> {
        self.delay(500).await;
        let mut s = self.state();
        if s.site(site)?.line(line).is_none() {
            return Err(not_found(line));
        }
        let (last4, bad) = match key {
            KeyInput::Secret(key) => {
                if key.is_empty() {
                    return Err(Error::invalid("key is empty"));
                }
                (key.last4(), key.expose().starts_with("bad"))
            }
            KeyInput::Ref(_) | KeyInput::Profile(_) => ("9f3a".into(), false),
        };
        let access = if bad {
            KeyAccess::InvalidKey
        } else if line == "cn-fast" && mode == AuthMode::Pat {
            KeyAccess::EdgeBlocked
        } else {
            KeyAccess::Ok
        };
        let whoami = (mode == AuthMode::Pat).then(|| match access {
            KeyAccess::InvalidKey => AuthApiHealth::Unhealthy {
                reason: "invalid key".into(),
            },
            KeyAccess::EdgeBlocked => AuthApiHealth::Unhealthy {
                reason: "edge intercepted whoami".into(),
            },
            _ => AuthApiHealth::Healthy,
        });
        s.caps
            .entry((site.clone(), line.clone()))
            .or_insert_with(|| line_caps(site, line))
            .key_access = Some(access.clone());
        Ok(KeyCheck {
            site_id: site.clone(),
            line_id: line.clone(),
            checked_at: Timestamp::now(),
            key_last4: last4,
            whoami,
            identity: access.is_ok().then(|| AccountIdentity {
                email: Some("user@example.com".into()),
                name: Some("Test User".into()),
                user_id: None,
            }),
            access,
            codex_path: Some(CodexPath::BackendApiCodex),
        })
    }

    // --- delegated to the area files (fake/<area>.rs) ---------------------------

    async fn probe_site(&self, request: SiteProbeRequest) -> Result<SiteProbeReport> {
        providers::probe_site(self, request).await
    }

    async fn line_transports(
        &self,
        request: superba_core::LineTransportsRequest,
    ) -> Result<Vec<superba_core::LineTransports>> {
        providers::line_transports(self, request).await
    }

    async fn provider_compaction(
        &self,
        request: superba_core::CompactionRequest,
    ) -> Result<superba_core::ProviderCompaction> {
        providers::provider_compaction(self, request).await
    }

    async fn import_usage(
        &self,
        request: superba_core::UsageImportRequest,
    ) -> Result<superba_core::UsageImportReport> {
        import::import_usage(self, request).await
    }

    async fn account(&self, request: AccountRequest) -> Result<AccountReport> {
        providers::account(self, request).await
    }

    async fn settings_view(&self, request: SettingsViewRequest) -> Result<SettingsView> {
        settings::settings_view(self, request).await
    }

    async fn codex_file(&self, target: TargetId, kind: CodexFileKind) -> Result<CodexFileDoc> {
        files::codex_file(self, target, kind).await
    }

    async fn harness_list(&self) -> Result<Vec<superba_core::harness::HarnessSummary>> {
        let fixtures = self.areas.maintenance.lock().unwrap().harnesses.clone();
        Ok(if fixtures.is_empty() {
            harness::list(self)
        } else {
            fixtures
        })
    }
    async fn harness_view(
        &self,
        h: superba_core::harness::HarnessId,
    ) -> Result<superba_core::harness::HarnessView> {
        self.areas
            .maintenance
            .lock()
            .unwrap()
            .views
            .get(&h)
            .cloned()
            .unwrap_or_else(|| Ok(self.tool_fixture(h)))
    }
    async fn harness_backups(
        &self,
        h: superba_core::harness::HarnessId,
    ) -> Result<Vec<superba_core::harness::HarnessBackup>> {
        self.areas
            .maintenance
            .lock()
            .unwrap()
            .backups
            .get(&h)
            .cloned()
            .unwrap_or_else(|| Ok(vec![]))
    }
    async fn harness_restore_backup(
        &self,
        h: superba_core::harness::HarnessId,
        id: String,
    ) -> Result<superba_core::harness::HarnessApplyReport> {
        self.areas
            .maintenance
            .lock()
            .unwrap()
            .restores
            .push((h, id));
        Ok(superba_core::harness::HarnessApplyReport {
            kind: superba_core::harness::HarnessPlanKind::Restore,
            harness: h,
            files: vec![],
            changed: vec![],
            kept: vec![],
            notes: vec![],
        })
    }
    async fn claude_diagnostics(&self) -> Result<superba_core::ClaudeDiagnostics> {
        let view = claude::view(self);
        Ok(superba_core::ClaudeDiagnostics {
            install: view.install,
            settings_path: view.settings_path,
            file: view.file,
            managed: vec![],
            env_conflicts: vec![],
            official_login: false,
            api_key_helper: false,
            findings: vec![],
        })
    }
    async fn claude_probe(&self, _model: Option<String>) -> Result<superba_core::ClaudeProbe> {
        Ok(superba_core::ClaudeProbe {
            model: "test".into(),
            url: "https://example.test/v1/messages".into(),
            status: Some(200),
            latency_ms: 214,
            ok: true,
            message: None,
        })
    }
    async fn harness_preview(
        &self,
        h: superba_core::harness::HarnessId,
        settings: superba_core::harness::HarnessSettings,
    ) -> Result<superba_core::harness::HarnessView> {
        Ok(harness::preview(self, h, settings))
    }
    async fn save_harness_settings(
        &self,
        h: superba_core::harness::HarnessId,
        s: superba_core::harness::HarnessSettings,
    ) -> Result<superba_core::harness::HarnessView> {
        Ok(harness::save(self, h, s))
    }
    async fn harness_models(&self, _: ProfileId, _: bool) -> Result<ModelList> {
        harness::models(self)
    }
    async fn harness_plan_preview(
        &self,
        h: superba_core::harness::HarnessId,
        _: superba_core::harness::HarnessSettings,
    ) -> Result<superba_core::harness::HarnessPlan> {
        Ok(harness::plan(
            h,
            superba_core::harness::HarnessPlanKind::Apply,
        ))
    }

    async fn harness_plan(
        &self,
        h: superba_core::harness::HarnessId,
        k: superba_core::harness::HarnessPlanKind,
    ) -> Result<superba_core::harness::HarnessPlan> {
        Ok(harness::plan(h, k))
    }
    async fn harness_apply(
        &self,
        p: superba_core::harness::HarnessPlan,
        take: bool,
    ) -> Result<superba_core::harness::HarnessApplyReport> {
        Ok(harness::apply(self, p, take))
    }
    async fn harness_file(
        &self,
        h: superba_core::harness::HarnessId,
        f: String,
    ) -> Result<superba_core::harness::HarnessFileDoc> {
        harness::file(self, h, f)
    }

    async fn claude_view(&self) -> Result<superba_core::ClaudeView> {
        Ok(claude::view(self))
    }

    async fn save_claude_config(
        &self,
        config: superba_core::ClaudeConfig,
    ) -> Result<superba_core::ClaudeView> {
        claude::save(self, config)
    }

    async fn claude_models(
        &self,
        request: superba_core::ClaudeModelsRequest,
    ) -> Result<superba_core::ClaudeModelsView> {
        Ok(claude::models(self, request))
    }

    async fn claude_plan(
        &self,
        kind: superba_core::ClaudePlanKind,
    ) -> Result<superba_core::ClaudePlan> {
        Ok(claude::plan(kind))
    }

    async fn claude_apply(
        &self,
        plan: superba_core::ClaudePlan,
        _take_over: bool,
    ) -> Result<superba_core::ClaudeApplyReport> {
        Ok(claude::apply(plan))
    }

    async fn save_codex_file(&self, save: CodexFileSave) -> Result<CodexFileSaved> {
        files::save_codex_file(self, save).await
    }

    async fn codex_file_backups(
        &self,
        query: CodexFileBackupQuery,
    ) -> Result<Vec<CodexFileBackup>> {
        files::codex_file_backups(self, query).await
    }

    async fn codex_file_backup(&self, id: &str) -> Result<CodexFileBackupContents> {
        files::codex_file_backup(self, id).await
    }

    async fn edit_settings(&self, edit: SettingsEdit) -> Result<SettingsEditReport> {
        settings::edit_settings(self, edit).await
    }

    async fn models_view(&self, request: ModelsViewRequest) -> Result<ModelsView> {
        models::models_view(self, request).await
    }

    async fn save_models(&self, edit: ModelsEdit) -> Result<ModelsEditReport> {
        models::save_models(self, edit).await
    }

    async fn save_router_catalog(&self, settings: RouterCatalogSettings) -> Result<()> {
        models::save_router_catalog(self, settings).await
    }

    async fn test_router_model(&self, model: String) -> Result<RouterModelTest> {
        models::test_router_model(self, model).await
    }

    async fn host_sessions(&self, target_id: &TargetId) -> Result<HostSessions> {
        sessions::host_sessions(self, target_id).await
    }

    async fn hosts(&self) -> Result<Vec<HostView>> {
        hosts::hosts(self).await
    }

    async fn hosts_now(&self) -> Result<Vec<HostView>> {
        hosts::hosts_now(self).await
    }

    async fn host_probe(&self, target_id: &TargetId) -> Result<HostView> {
        hosts::host_probe(self, target_id).await
    }

    async fn set_host_follow(&self, target_id: &TargetId, follow: bool) -> Result<HostView> {
        hosts::set_host_follow(self, target_id, follow).await
    }

    async fn host_sync_plan(&self, target_id: &TargetId) -> Result<HostSyncPlan> {
        hosts::host_sync_plan(self, target_id).await
    }

    async fn versions(&self, refresh: bool) -> Result<VersionsView> {
        versions::versions(self, refresh).await
    }

    async fn self_update_view(&self) -> Result<SelfUpdateView> {
        selfupdate::view(self).await
    }

    async fn notify_save_channel(
        &self,
        channel: NotifyChannel,
        secrets: NotifySecrets,
    ) -> Result<NotifyChannel> {
        notify::save_channel(self, channel, secrets).await
    }

    async fn notify_send(&self, event: NotifyEvent, options: SendOptions) -> Result<NotifyReport> {
        notify::send(self, event, options).await
    }

    async fn notify_hooks(&self) -> Result<NotifyHooks> {
        notify::hooks(self).await
    }

    async fn notify_set_hook(&self, request: HookRequest) -> Result<NotifyHooks> {
        notify::set_hook(self, request).await
    }

    async fn tools(&self, refresh: bool) -> Result<superba_core::ToolsView> {
        tools::tools(self, refresh).await
    }

    async fn tool_run(
        &self,
        request: superba_core::ToolRequest,
        progress: Arc<dyn superba_core::ToolProgressSink>,
    ) -> Result<superba_core::ToolReport> {
        tools::tool_run(self, request, progress).await
    }

    async fn tool_settings(
        &self,
        tool: superba_core::ToolId,
        change: superba_core::ToolSettingsChange,
    ) -> Result<superba_core::ToolRow> {
        tools::tool_settings(self, tool, change).await
    }

    async fn self_update_check(&self, request: SelfUpdateCheckRequest) -> Result<SelfUpdateView> {
        selfupdate::check(self, request).await
    }

    async fn self_update_skip(&self, version: Option<String>) -> Result<SelfUpdateView> {
        selfupdate::skip(self, version).await
    }

    async fn self_update_install(
        &self,
        request: SelfUpdateInstallRequest,
    ) -> Result<SelfUpdateReport> {
        selfupdate::install(self, request).await
    }

    async fn import_scan(&self, request: ImportScanRequest) -> Result<ImportScan> {
        import::scan(self, request).await
    }

    async fn import_apply(&self, request: ImportApplyRequest) -> Result<ImportReport> {
        import::apply(self, request).await
    }

    async fn install_codex(&self, request: InstallRequest) -> Result<InstallReport> {
        versions::install_codex(self, request).await
    }

    async fn rollback_codex(&self, target: InstallTarget) -> Result<InstallReport> {
        versions::rollback_codex(self, target).await
    }

    async fn download_app_package(
        &self,
        request: AppDownloadRequest,
        progress: Arc<dyn DownloadProgressSink>,
    ) -> Result<AppDownloadReport> {
        versions::download_app_package(self, request, progress).await
    }

    async fn open_app_package(&self, path: std::path::PathBuf) -> Result<()> {
        versions::open_app_package(self, path).await
    }

    async fn download_app_update(&self) -> Result<AppDownloadReport> {
        versions::download_app_update(self).await
    }

    async fn router_requests(&self, query: RequestQuery) -> Result<RequestPage> {
        router::router_requests(self, query).await
    }

    async fn router_request(&self, id: i64) -> Result<RequestDetail> {
        router::router_request(self, id).await
    }

    async fn router_inflight(&self) -> Result<Vec<InflightRequest>> {
        router::router_inflight(self).await
    }

    async fn router_stats(&self, query: StatsQuery) -> Result<RequestStats> {
        router::router_stats(self, query).await
    }

    async fn router_clear(&self, request: ClearRequest) -> Result<ClearReport> {
        router::router_clear(self, request).await
    }

    async fn router_set_stats(&self, settings: RouterStatsSettings) -> Result<RouterView> {
        router::router_set_stats(self, settings).await
    }

    async fn set_ui_locale(&self, locale: Locale) -> Result<()> {
        router::set_ui_locale(self, locale).await
    }

    async fn session_titles(&self, keys: Vec<String>) -> Result<BTreeMap<String, SessionTitle>> {
        router::session_titles(self, keys).await
    }

    async fn price_table(&self) -> Result<PriceTable> {
        router::price_table(self).await
    }

    // --- shell (this file) ----------------------------------------------------------

    async fn feeds_status(&self) -> Result<FeedsStatus> {
        Err(Error::not_implemented("fake feeds_status"))
    }

    async fn refresh_feeds(&self) -> Result<FeedsStatus> {
        Err(Error::not_implemented("fake refresh_feeds"))
    }

    async fn sessions(&self) -> Result<(SessionReport, Vec<MigrationReceipt>)> {
        sessions::sessions(self).await
    }

    async fn session_list(&self) -> Result<SessionList> {
        sessions::session_list(self).await
    }

    async fn session_export(
        &self,
        thread_id: String,
        format: SessionExportFormat,
    ) -> Result<SessionExport> {
        sessions::session_export(self, thread_id, format).await
    }

    async fn doctor(&self) -> Result<Vec<DoctorFinding>> {
        self.delay(400).await;
        Ok([
            ("codex_home_consistent", Severity::Info, home_ok(), None),
            (
                "ide_missing_authapi",
                Severity::Warning,
                ide_missing(),
                Some(ide_detail()),
            ),
            (
                "pat_line_blocked",
                Severity::Warning,
                pat_blocked(),
                Some(pat_detail()),
            ),
            ("openai_env", Severity::Info, no_openai_env(), None),
        ]
        .into_iter()
        .map(|(id, severity, title, detail)| DoctorFinding {
            id: id.into(),
            severity,
            title,
            detail,
            fixable: false,
        })
        .collect())
    }

    async fn open_app(&self) -> Result<AppSession> {
        self.open_calls.fetch_add(1, Ordering::SeqCst);
        let mut s = self.state();
        // Running already: the session as it is (the real service brings it
        // forward when Superba opened it, and refuses otherwise).
        if let Some(session) = s.app.session.clone() {
            return if session.cdp.is_some() {
                Ok(session)
            } else {
                Err(Error::Conflict {
                    message: "ChatGPT is already running without Superba".into(),
                })
            };
        }
        Ok(s.launch())
    }

    async fn recover(&self) -> Result<RecoveryReport> {
        Ok(RecoveryReport::default())
    }

    async fn plan(&self, request: SwitchRequest) -> Result<SwitchPlan> {
        self.delay(250).await;
        let id = format!("plan-{}", self.next_plan.fetch_add(1, Ordering::Relaxed));
        let mut plan = self.state().plan(request, id)?;
        if matches!(plan.request.intent, SwitchIntent::RollbackMigration { .. }) {
            // Like the real service: the rollback ledger is private executor
            // state, not a new migration in the public preview.
            plan.migration = None;
        }
        Ok(plan)
    }

    async fn execute(
        &self,
        plan: SwitchPlan,
        options: ExecuteOptions,
        decider: Arc<dyn SwitchDecider>,
        events: Arc<dyn SwitchEvents>,
    ) -> Result<SwitchOutcome> {
        let plan = if matches!(plan.request.intent, SwitchIntent::RollbackMigration { .. }) {
            self.state().plan(plan.request.clone(), plan.id.clone())?
        } else {
            plan
        };
        if let RestartKind::Blocked { reasons } = &plan.primary {
            return Err(Error::Blocked {
                reasons: reasons.clone(),
            });
        }
        if !plan.choices.is_empty() {
            return Err(Error::invalid("plan has unanswered choices"));
        }
        if plan.needs_confirmation() && !options.allow_full_restart && !options.write_only {
            return Err(Error::Blocked { reasons: vec![] });
        }
        let start = Instant::now();
        let (config, busy, fail) = {
            let mut s = self.state();
            let config = s.destination_config(&plan.to, &plan.request)?;
            (
                config,
                std::mem::take(&mut s.busy_armed),
                std::mem::take(&mut s.fail_armed),
            )
        };
        let mut outcome = SwitchOutcome {
            plan_id: plan.id.clone(),
            status: OutcomeStatus::Cancelled,
            elapsed_ms: 0,
            generation: None,
            restart: RestartKind::None,
            verification: None,
            remotes: vec![],
            stale_processes: vec![],
            runtime: None,
            returned_to: None,
            notices: vec![],
        };
        phase(events.as_ref(), SwitchPhase::TurnCheck);
        self.delay(150).await;
        if busy
            && !options.force_turns
            && !self
                .wait_turns(options.turn_quiet_ms, decider.as_ref(), events.as_ref())
                .await
        {
            outcome.elapsed_ms = self.elapsed(start);
            return Ok(outcome);
        }
        let mut full_restart = matches!(plan.primary, RestartKind::FullAppRestart { .. });
        let mut written_only = options.write_only;
        if written_only {
            self.apply_steps(&plan, false, events.as_ref()).await;
        } else {
            self.restart_steps(&plan, full_restart, events.as_ref())
                .await;
            let verification = self.verify(fail, events.as_ref()).await;
            if !verification.passed() {
                phase(events.as_ref(), SwitchPhase::Recovery);
                match decider
                    .decide(DecisionRequest::VerificationFailed {
                        verification: verification.clone(),
                    })
                    .await
                {
                    Decision::FullRestart => {
                        full_restart = true;
                        self.restart_steps(&plan, true, events.as_ref()).await;
                        outcome.verification = Some(self.verify(false, events.as_ref()).await);
                    }
                    Decision::KeepWritten => {
                        written_only = true;
                        outcome.verification = Some(verification);
                    }
                    _ => {
                        phase(events.as_ref(), SwitchPhase::RollingBack);
                        self.step(events.as_ref(), SwitchStep::WriteConfig, 150)
                            .await;
                        phase(events.as_ref(), SwitchPhase::RollbackRestart);
                        phase(events.as_ref(), SwitchPhase::Done);
                        outcome.status = OutcomeStatus::RolledBack;
                        outcome.verification = Some(verification);
                        outcome
                            .notices
                            .push(Notice::danger(verify_failed().arg("reason", TIMEOUT)));
                        outcome.elapsed_ms = self.elapsed(start);
                        return Ok(outcome);
                    }
                }
            } else {
                outcome.verification = Some(verification);
            }
        }
        {
            let mut s = self.state();
            outcome.generation = Some(s.commit(
                &plan,
                config,
                full_restart && !written_only,
                !options.write_only,
            ));
            outcome.stale_processes = s
                .cli
                .processes
                .iter()
                .filter(|p| p.stale && p.kind == CodexProcessKind::CliTui)
                .cloned()
                .collect();
        }
        outcome.status = if written_only {
            OutcomeStatus::WrittenOnly
        } else {
            OutcomeStatus::Switched
        };
        if !written_only {
            outcome.restart = if full_restart && plan.primary.is_hot() {
                full(FullRestartReason::NoCdp)
            } else {
                plan.primary.clone()
            };
            outcome.remotes = remote_ids(&plan)
                .map(|target_id| RemoteOutcome {
                    target_id,
                    status: OutcomeStatus::Switched,
                    error: None,
                })
                .collect();
        }
        outcome.elapsed_ms = self.elapsed(start);
        phase(events.as_ref(), SwitchPhase::Done);
        Ok(outcome)
    }

    async fn plan_settings(&self, _: &str) -> superba_core::Result<superba_core::PlanSettings> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::plan_settings",
        ))
    }

    async fn router_set_enabled(&self, _: bool) -> superba_core::Result<superba_core::RouterView> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::router_set_enabled",
        ))
    }

    async fn router_fall_back(
        &self,
        _: String,
    ) -> superba_core::Result<Option<superba_core::SwitchOutcome>> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::router_fall_back",
        ))
    }

    async fn router_watch(&self) -> superba_core::Result<superba_core::RouterWatch> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::router_watch",
        ))
    }
}
