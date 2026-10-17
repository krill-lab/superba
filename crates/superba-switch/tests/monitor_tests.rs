//! The environment monitor (user request 34): views read its snapshot, detection
//! runs only on events, changes and a long interval, and the overview poll never
//! starts a process.
//!
//! Child processes are counted with `superba_core::process::commands_built`.
//! Only `the_overview_poll_starts_no_process_after_the_first_snapshot` starts any
//! (the other tests use fakes), so the counts are exact.

#![allow(missing_docs)]

mod fakes;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use fakes::*;
use superba_core::{
    AppController, AppInstall, AppSession, BridgeStatus, CodexBinary, CodexHome, CodexVersion,
    EnvironmentEvent, HotRestartOutcome, HotRestartRequest, LaunchOptions, ProcessInfo,
    QuitOptions, QuitOutcome, RemoteHost, RendererState, Result, SwitchService, ThreadRef,
    TurnActivity,
};
use superba_switch::Switcher;
use superba_switch::monitor::{EnvMonitor, MonitorTuning, RefreshKind, Step};

/// The world's fake app, with detection either counted (installs and binaries
/// from the fake, a settable change stamp) or done for real by appctl.
struct Detection {
    fake: Arc<FakeApp>,
    real: Option<superba_appctl::DesktopAppController>,
    detections: AtomicUsize,
    stamp: AtomicU64,
    forgotten: AtomicUsize,
}

impl Detection {
    fn counted(fake: Arc<FakeApp>) -> Arc<Self> {
        Arc::new(Self {
            fake,
            real: None,
            detections: AtomicUsize::new(0),
            stamp: AtomicU64::new(1),
            forgotten: AtomicUsize::new(0),
        })
    }

    fn detections(&self) -> usize {
        self.detections.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl AppController for Detection {
    async fn detect_installs(&self) -> Result<Vec<AppInstall>> {
        self.detections.fetch_add(1, Ordering::SeqCst);
        match &self.real {
            Some(real) => real.detect_installs().await,
            None => self.fake.detect_installs().await,
        }
    }
    async fn codex_binaries(&self) -> Result<Vec<CodexBinary>> {
        match &self.real {
            Some(real) => real.codex_binaries().await,
            None => self.fake.codex_binaries().await,
        }
    }
    fn environment_stamp(&self, installs: &[AppInstall], binaries: &[CodexBinary]) -> Option<u64> {
        match &self.real {
            Some(real) => real.environment_stamp(installs, binaries),
            None => Some(self.stamp.load(Ordering::SeqCst)),
        }
    }
    fn forget_cached_probes(&self) {
        self.forgotten.fetch_add(1, Ordering::SeqCst);
        if let Some(real) = &self.real {
            real.forget_cached_probes();
        }
    }
    // Processes, sessions and the renderer stay fake: the test never looks at
    // (or talks to) the processes of the machine it runs on.
    async fn running_session(&self) -> Result<Option<AppSession>> {
        self.fake.running_session().await
    }
    async fn launch(&self, options: &LaunchOptions) -> Result<AppSession> {
        self.fake.launch(options).await
    }
    async fn probe_bridge(&self, session: &AppSession) -> Result<BridgeStatus> {
        self.fake.probe_bridge(session).await
    }
    async fn hot_restart(
        &self,
        session: &AppSession,
        request: &HotRestartRequest,
    ) -> Result<HotRestartOutcome> {
        self.fake.hot_restart(session, request).await
    }
    async fn quit(&self, session: &AppSession, options: &QuitOptions) -> Result<QuitOutcome> {
        self.fake.quit(session, options).await
    }
    async fn processes(&self, home: &CodexHome) -> Result<Vec<ProcessInfo>> {
        self.fake.processes(home).await
    }
    async fn turn_activity(
        &self,
        home: &CodexHome,
        session: Option<&AppSession>,
    ) -> Result<TurnActivity> {
        self.fake.turn_activity(home, session).await
    }
    async fn renderer_state(&self, session: &AppSession) -> Result<RendererState> {
        self.fake.renderer_state(session).await
    }
    async fn return_to_thread(&self, session: &AppSession, thread: &ThreadRef) -> Result<()> {
        self.fake.return_to_thread(session, thread).await
    }
    async fn restart_daemon(&self, home: &CodexHome) -> Result<()> {
        self.fake.restart_daemon(home).await
    }
    async fn stop_daemon(&self, home: &CodexHome) -> Result<()> {
        self.fake.stop_daemon(home).await
    }
    async fn remote_connections(&self, session: &AppSession) -> Result<Vec<RemoteHost>> {
        self.fake.remote_connections(session).await
    }
    async fn launcher_entry(&self) -> Result<bool> {
        self.fake.launcher_entry().await
    }
    async fn set_launcher_entry(&self, enabled: bool) -> Result<()> {
        self.fake.set_launcher_entry(enabled).await
    }
    async fn app_server_exe(&self, session: &AppSession) -> Result<Option<PathBuf>> {
        self.fake.app_server_exe(session).await
    }
    async fn app_server_floor(&self, install: &AppInstall) -> Result<CodexVersion> {
        self.fake.app_server_floor(install).await
    }
    async fn codex_debug_models(&self, binary: &Path, catalog: Option<&Path>) -> Result<String> {
        self.fake.codex_debug_models(binary, catalog).await
    }
    async fn cli_install_kind(&self, binary: &Path) -> Result<superba_core::CodexInstallKind> {
        self.fake.cli_install_kind(binary).await
    }
}

fn tuning() -> MonitorTuning {
    MonitorTuning {
        check_every: Duration::from_secs(30),
        refresh_every: Duration::from_secs(30 * 60),
        refresh_on_focus_after: Duration::from_secs(10 * 60),
    }
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

#[tokio::test]
async fn views_read_the_snapshot_and_detect_only_once() {
    let w = World::new();
    let app = Detection::counted(w.app.clone());
    let mut deps = w.deps();
    deps.app = app.clone();
    let sw = Switcher::new(deps);
    for _ in 0..50 {
        sw.overview().await.unwrap();
        sw.settings_view(Default::default()).await.ok();
        sw.models_view(Default::default()).await.ok();
        sw.versions(false).await.ok();
    }
    assert_eq!(
        app.detections(),
        1,
        "the first read detects, the others reuse it"
    );
    assert_eq!(sw.environment_generation(), 1);

    // "Check now" on the Updates page and an explicit refresh look again.
    sw.versions(true).await.ok();
    assert_eq!(app.detections(), 2);
    sw.environment_event(EnvironmentEvent::Refresh);
    sw.overview().await.unwrap();
    assert_eq!(app.detections(), 3);
    assert_eq!(
        app.forgotten.load(Ordering::SeqCst),
        1,
        "only the refresh forgets"
    );
    // Nothing changed on disk: the generation stays.
    assert_eq!(sw.environment_generation(), 1);
}

#[tokio::test]
async fn hidden_means_nothing_runs_and_shown_means_cheap_checks() {
    let w = World::new();
    let app = Detection::counted(w.app.clone());
    let monitor = EnvMonitor::new(app.clone(), tuning());
    let t0 = Instant::now();

    // Hidden (the default before any event): steps do nothing, even when due.
    for i in 0..100 {
        assert_eq!(monitor.step(t0 + secs(60 * i)).await, Step::Idle);
    }
    assert_eq!(app.detections(), 0);
    assert_eq!(monitor.stats().checks, 0);

    // Shown: the first step detects, then the stamp is checked every 30 s.
    monitor.record(EnvironmentEvent::Shown);
    assert_eq!(
        monitor.step(t0).await,
        Step::Refreshed(RefreshKind::Initial)
    );
    assert_eq!(monitor.step(t0 + secs(10)).await, Step::Idle);
    assert_eq!(
        monitor.step(t0 + secs(30)).await,
        Step::Checked { changed: false }
    );
    assert_eq!(app.detections(), 1);

    // An install changes the stamp: the next check detects.
    app.stamp.store(2, Ordering::SeqCst);
    w.app.installs.lock().unwrap().clear();
    assert_eq!(
        monitor.step(t0 + secs(60)).await,
        Step::Checked { changed: true }
    );
    assert_eq!(app.detections(), 2);
    assert!(monitor.facts().await.installs.is_empty());
    assert_eq!(monitor.generation(), 2);

    // Without changes the long interval detects every 30 min (no change, no
    // new generation).
    let mut detections = app.detections();
    let mut t = t0 + secs(60);
    for _ in 0..(2 * 3600 / 30) {
        t += secs(30);
        monitor.step(t).await;
    }
    detections = app.detections() - detections;
    assert_eq!(detections, 4, "two hours, one detection per half hour");
    assert_eq!(monitor.generation(), 2);

    // Hidden again: nothing, however long.
    let checks = monitor.stats().checks;
    monitor.record(EnvironmentEvent::Hidden);
    for i in 1..=200 {
        assert_eq!(monitor.step(t + secs(60 * i)).await, Step::Idle);
    }
    assert_eq!(monitor.stats().checks, checks);

    // Focus back after an hour away: detection (the last one is old).
    t += secs(3600);
    monitor.record(EnvironmentEvent::FocusGained);
    assert_eq!(
        monitor.step(t).await,
        Step::Refreshed(RefreshKind::Periodic)
    );
    // Focus back a minute later: only a check.
    monitor.record(EnvironmentEvent::FocusGained);
    assert_eq!(
        monitor.step(t + secs(60)).await,
        Step::Checked { changed: false }
    );
}

#[tokio::test]
async fn events_while_hidden_wait_for_the_next_read() {
    let w = World::new();
    let app = Detection::counted(w.app.clone());
    let monitor = EnvMonitor::new(app.clone(), tuning());
    let t0 = Instant::now();
    monitor.facts().await;
    assert_eq!(app.detections(), 1);
    // A switch while hidden (the watchdog's fallback): no background detection ...
    monitor.invalidate();
    assert_eq!(monitor.step(t0 + secs(1)).await, Step::Idle);
    assert_eq!(app.detections(), 1);
    assert!(monitor.current().is_none(), "the snapshot is stale");
    // ... the next read detects (once, however many readers).
    let (a, b) = tokio::join!(monitor.facts(), monitor.facts());
    assert_eq!(a, b);
    assert_eq!(app.detections(), 2);
    // An explicit refresh while shown runs in the background with probes forgotten.
    monitor.record(EnvironmentEvent::Shown);
    monitor.record(EnvironmentEvent::Refresh);
    assert_eq!(
        monitor.step(t0 + secs(2)).await,
        Step::Refreshed(RefreshKind::Forced)
    );
    assert_eq!(app.forgotten.load(Ordering::SeqCst), 1);
    assert_eq!(monitor.stats().forced, 1);
}

#[tokio::test]
async fn the_thread_detects_in_the_background_once_shown() {
    let w = World::new();
    let app = Detection::counted(w.app.clone());
    let monitor = EnvMonitor::new(app.clone(), tuning());
    assert!(!monitor.thread_started());
    monitor.event(EnvironmentEvent::Shown);
    assert!(monitor.thread_started());
    let deadline = Instant::now() + secs(10);
    while monitor.current().is_none() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        monitor.current().is_some(),
        "the thread made the first snapshot"
    );
    // Reads after it detect nothing.
    for _ in 0..20 {
        monitor.facts().await;
    }
    assert_eq!(app.detections(), 1);
    // An explicit refresh is picked up by the thread at once.
    monitor.event(EnvironmentEvent::Refresh);
    let deadline = Instant::now() + secs(10);
    while monitor.stats().forced == 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(monitor.stats().forced, 1);
    drop(monitor);
}

#[cfg(unix)]
fn script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The guarantee of user request 34, with appctl's real detection: after the
/// first snapshot, two simulated hours of the GUI's idle loop (overview every
/// 5 s, monitor steps, host list, versions, focus changes) start no process; an
/// explicit refresh starts at most one per binary; a changed binary one.
#[cfg(unix)]
#[tokio::test]
async fn the_overview_poll_starts_no_process_after_the_first_snapshot() {
    use superba_core::process::commands_built;
    use superba_core::{DataDir, LocalContext, MapEnv};

    let w = World::new();
    let root = w.tmp.path().join("machine");
    let home = root.join("home");
    // A desktop app with a bundled Codex, and a Codex CLI on PATH.
    let app_dir = root.join("ChatGPT");
    std::fs::create_dir_all(app_dir.join("resources")).unwrap();
    std::fs::write(app_dir.join("ChatGPT"), "app").unwrap();
    std::fs::write(app_dir.join("resources/app.asar"), "asar").unwrap();
    script(&app_dir.join("resources/codex"), "echo 'codex-cli 0.162.0'");
    let bin = root.join("bin");
    script(&bin.join("codex"), "echo 'codex-cli 0.162.1'");
    let env = MapEnv::new()
        .with("PATH", bin.as_os_str())
        .with("HOME", &home)
        .with(superba_core::codex::ENV_SUPERBA_APP_PATH, &app_dir);
    let ctx = LocalContext::new(&home, DataDir::new(root.join("data")), Arc::new(env));
    let app = Arc::new(Detection {
        fake: w.app.clone(),
        real: Some(superba_appctl::DesktopAppController::new(ctx)),
        detections: AtomicUsize::new(0),
        stamp: AtomicU64::new(0),
        forgotten: AtomicUsize::new(0),
    });
    let mut deps = w.deps();
    deps.app = app.clone();
    let sw = Switcher::new(deps).with_monitor_tuning(tuning());
    let monitor = sw.monitor().clone();
    // Shown, with the steps driven here instead of the thread.
    monitor.record(EnvironmentEvent::Shown);

    let start = commands_built();
    let first = sw.overview().await.unwrap();
    let initial = commands_built() - start;
    let canonical = std::fs::canonicalize(&app_dir).unwrap();
    assert!(
        first.app.installs.iter().any(|i| i.path == canonical),
        "{:?}",
        first.app.installs
    );
    let ours =
        |o: &superba_core::Overview, p: &Path| o.cli.binaries.iter().find(|b| b.path == p).cloned();
    let cli = ours(&first, &bin.join("codex")).expect("the CLI");
    assert_eq!(cli.version, Some(CodexVersion::new(0, 162, 1)));
    let bound = (first.cli.binaries.len() + first.app.installs.len()) as u64;
    assert!(
        initial >= 2 && initial <= bound,
        "first snapshot: {initial} processes for {bound} binaries"
    );

    // Two hours of an idle, shown window.
    let t0 = Instant::now();
    let start = commands_built();
    let detections = app.detections();
    for tick in 1..=(2 * 3600 / 5) {
        let now = t0 + secs(5 * tick);
        sw.overview().await.unwrap();
        sw.router_watch().await.ok();
        monitor.step(now).await;
        if tick % 60 == 0 {
            sw.hosts().await.ok();
            sw.versions(false).await.ok();
        }
        if tick % 120 == 0 {
            monitor.record(EnvironmentEvent::FocusGained);
            monitor.step(now).await;
        }
    }
    assert_eq!(
        commands_built() - start,
        0,
        "no process after the first snapshot"
    );
    let stats = monitor.stats();
    assert!(stats.checks >= 200, "{stats:?}");
    assert!(
        app.detections() - detections >= 4,
        "the long interval detected"
    );

    // Hidden: nothing runs, not even the checks.
    monitor.record(EnvironmentEvent::Hidden);
    let checks = monitor.stats().checks;
    for tick in 1..=720 {
        monitor.step(t0 + secs(7200 + 5 * tick)).await;
    }
    assert_eq!(monitor.stats().checks, checks);
    assert_eq!(commands_built() - start, 0);

    // An explicit refresh asks each binary again: bounded by their number.
    monitor.record(EnvironmentEvent::Refresh);
    let start = commands_built();
    let refreshed = sw.overview().await.unwrap();
    let forced = commands_built() - start;
    let bound = (refreshed.cli.binaries.len() + refreshed.app.installs.len()) as u64;
    assert!(
        forced >= 2 && forced <= bound,
        "refresh: {forced} processes for {bound} binaries"
    );

    // An updated CLI: the next check sees it and asks only that binary.
    script(&bin.join("codex"), "echo 'codex-cli 0.163.0-alpha.1'");
    monitor.record(EnvironmentEvent::Shown);
    let start = commands_built();
    let later = Instant::now() + secs(31);
    assert_eq!(monitor.step(later).await, Step::Checked { changed: true });
    assert_eq!(commands_built() - start, 1);
    let after = sw.overview().await.unwrap();
    assert_eq!(
        ours(&after, &bin.join("codex")).unwrap().version,
        Some(CodexVersion::parse("0.163.0-alpha.1").unwrap())
    );
    assert_eq!(commands_built() - start, 1);
}
