//! # superba-appctl
//!
//! Desktop app and Codex process control. [`DesktopAppController`] implements
//! [`AppController`]:
//!
//! - install detection ([`install`]: macOS bundles, Linux deb/AppImage/custom dir,
//!   Windows MSIX) and Codex CLI binaries ([`clibin`]); which program a Codex
//!   path really runs (the macOS launcher and its `CodexCLI.app` target, [`codexid`]);
//! - launch with loopback CDP and injected environment ([`launch`]), launch records
//!   in `DataDir::app_sessions_json` ([`store`]);
//! - a minimal CDP client ([`cdp`]) with listener ownership checks ([`owner`]);
//! - hot restart through `superba_core::codex::hot_restart_js`, verified by a new
//!   app-server child of the app main process; graceful quit ([`quit`]);
//! - process catalog ([`procs`]), turn activity from rollouts ([`activity`]) and
//!   the renderer ([`renderer`]), CLI daemon control ([`daemon`]) and the Linux
//!   launcher entry ([`launcher`]);
//! - stopping Codex consumer processes the user asked Superba to stop
//!   ([`stop`]).
//!
//! Platform code sits behind `cfg(target_os = ...)`; Linux is the reference and the
//! only platform exercised end to end.

pub mod activity;
pub mod appx;
pub mod cdp;
pub mod clibin;
pub mod codexbin;
pub mod codexid;
pub mod daemon;
pub mod install;
pub mod launch;
pub mod launcher;
pub mod owner;
pub mod package;
pub mod procs;
pub mod quit;
pub mod renderer;
pub mod stop;
pub mod store;
pub mod watch;

#[cfg(test)]
#[path = "../tests/common/mod.rs"]
mod cdp_test_support;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use superba_core::codex::{
    APP_SERVER_FLOOR_FALLBACK, BRIDGE_PROBE_JS, ENV_CODEX_CLI_PATH, ENV_SUPERBA_APP_ARGS,
    LOCAL_HOST_ID, hot_restart_js,
};
use superba_core::{
    AppController, AppInstall, AppSession, BridgeStatus, CdpEndpoint, CodexBinary, CodexHome,
    CodexInstallKind, CodexProcessKind, CodexVersion, EnvSource, Error, HotRestartOutcome,
    HotRestartRequest, InstallSource, LaunchOptions, LocalContext, ProcessInfo, QuitOptions,
    QuitOutcome, RemoteHost, RendererState, Result, TargetId, ThreadRef, Timestamp, TurnActivity,
};

use crate::launch::CdpPlan;
use crate::owner::OwnerCheck;
use crate::procs::{ReadOpts, Snapshot};
use crate::renderer::DomHooks;

/// Timeouts and DOM hooks of the controller.
#[derive(Clone, Debug)]
pub struct Tunables {
    /// One CDP HTTP request or evaluation.
    pub cdp_timeout: Duration,
    /// How long a new app-server may take to appear after the restart IPC.
    pub restart_timeout: Duration,
    /// How long the old app-server may linger after the new one appeared.
    pub old_exit_timeout: Duration,
    /// Default launch readiness timeout (`LaunchOptions::ready_timeout_ms == 0`).
    pub ready_timeout: Duration,
    /// Extra wait after launch for the first app-server child (environment
    /// observation), bounded by the readiness deadline.
    pub app_server_wait: Duration,
    /// Daemon stop: wait for the daemon processes to exit.
    pub daemon_stop_timeout: Duration,
    /// Renderer selectors.
    pub hooks: DomHooks,
}

impl Default for Tunables {
    fn default() -> Self {
        Self {
            cdp_timeout: cdp::DEFAULT_CDP_TIMEOUT,
            restart_timeout: Duration::from_secs(10),
            old_exit_timeout: Duration::from_secs(5),
            ready_timeout: Duration::from_secs(15),
            app_server_wait: Duration::from_secs(10),
            daemon_stop_timeout: Duration::from_secs(10),
            hooks: DomHooks::default(),
        }
    }
}

/// [`AppController`] for the local machine.
#[derive(Debug, Clone)]
pub struct DesktopAppController {
    ctx: LocalContext,
    tunables: Tunables,
    caches: Arc<codexbin::Caches>,
}

/// An [`EnvSource`] view with overrides (`None` = removed), used to resolve paths
/// the launched app will see.
#[derive(Debug)]
struct OverlayEnv {
    base: Arc<dyn EnvSource>,
    overrides: BTreeMap<String, Option<String>>,
}

impl EnvSource for OverlayEnv {
    fn var_os(&self, key: &str) -> Option<OsString> {
        match self.overrides.get(key) {
            Some(v) => v.as_ref().map(OsString::from),
            None => self.base.var_os(key),
        }
    }
}

fn blocking_err(e: tokio::task::JoinError) -> Error {
    Error::other(format!("background task failed: {e}"))
}

async fn snapshot_async(opts: ReadOpts) -> Result<Snapshot> {
    tokio::task::spawn_blocking(move || procs::snapshot(opts))
        .await
        .map_err(blocking_err)
}

/// A mismatched or externally bound listener disables CDP for the whole session,
/// including its persisted launch record. Unknown ownership remains observable.
fn checked_endpoint(port: u16, check: OwnerCheck) -> Option<CdpEndpoint> {
    if matches!(check, OwnerCheck::NotLoopback | OwnerCheck::Mismatch { .. }) {
        tracing::warn!(
            port,
            ?check,
            "CDP listener is not the launched app; disabling CDP"
        );
        return None;
    }
    Some(CdpEndpoint {
        port,
        owner_verified: check == OwnerCheck::Verified,
    })
}

fn same_path(a: &Path, b: &Path) -> bool {
    procs::norm_path(a) == procs::norm_path(b)
}

/// The `--remote-debugging-port=<n>` value on a command line.
pub fn cdp_port_from_argv(argv: &[String]) -> Option<u16> {
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        if let Some(v) = a.strip_prefix("--remote-debugging-port=") {
            return v.trim().parse().ok();
        }
        if a == "--remote-debugging-port" {
            return it.next().and_then(|v| v.trim().parse().ok());
        }
    }
    None
}

impl DesktopAppController {
    /// Create a controller (launch records in `ctx.data_dir`).
    pub fn new(ctx: LocalContext) -> Self {
        Self {
            ctx,
            tunables: Tunables::default(),
            caches: Arc::new(codexbin::Caches::default()),
        }
    }

    /// Replace the tunables.
    pub fn with_tunables(mut self, tunables: Tunables) -> Self {
        self.tunables = tunables;
        self
    }

    /// The context this controller was created with.
    pub fn context(&self) -> &LocalContext {
        &self.ctx
    }

    /// The tunables in use.
    pub fn tunables(&self) -> &Tunables {
        &self.tunables
    }

    fn sessions_path(&self) -> PathBuf {
        self.ctx.data_dir.app_sessions_json()
    }

    fn launch_log_path(&self) -> PathBuf {
        self.ctx.data_dir.logs_dir().join("app-launch.log")
    }

    async fn find_install(&self, id: Option<&str>) -> Result<AppInstall> {
        let installs = install::detect_installs(self.ctx.env.as_ref(), &self.ctx.home_dir).await;
        // An id recorded earlier may be an older form of the same install (a
        // `path:` id, also with a verbatim `\\?\` prefix, for an MSIX folder).
        let found = match id {
            Some(id) => installs
                .iter()
                .position(|i| i.id == id)
                .or_else(|| {
                    installs
                        .iter()
                        .position(|i| install::install_matches(i, id))
                })
                .map(|at| installs[at].clone()),
            None => installs.into_iter().next(),
        };
        found.ok_or_else(|| Error::NotFound {
            what: match id {
                Some(id) => format!("desktop app install {id}"),
                None => {
                    "a desktop app install (set SUPERBA_APP_PATH for a custom location)".to_owned()
                }
            },
        })
    }

    /// Effective home directory of a process launched with `home_override`.
    fn app_home(&self, home_override: Option<&Path>) -> PathBuf {
        home_override
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.ctx.home_dir.clone())
    }

    fn launch_env_view(&self, options: &LaunchOptions) -> OverlayEnv {
        let mut overrides: BTreeMap<String, Option<String>> = BTreeMap::new();
        if let Some(home) = &options.home_override {
            let (set, remove) = launch::home_override_env(home);
            for k in remove {
                overrides.insert(k, None);
            }
            for (k, v) in set {
                overrides.insert(k, Some(v));
            }
        }
        for k in &options.env_remove {
            overrides.insert(k.clone(), None);
        }
        for (k, v) in &options.env {
            overrides.insert(k.clone(), Some(v.clone()));
        }
        OverlayEnv {
            base: self.ctx.env.clone(),
            overrides,
        }
    }

    /// Build sessions for every running app main process in `snap`.
    fn sessions_in(&self, snap: &Snapshot) -> Vec<AppSession> {
        let records = store::load(&self.sessions_path());
        let mut out = Vec::new();
        for pid in snap.app_mains() {
            let Some(entry) = snap.all.get(&pid) else {
                continue;
            };
            let record = records.find(pid, entry.started_at);
            let executable = entry
                .exe
                .clone()
                .or_else(|| record.and_then(|r| r.executable.clone()));
            let install_id = record.and_then(|r| r.install_id.clone()).or_else(|| {
                executable
                    .as_deref()
                    .and_then(install::inspect_install)
                    .map(|i| i.id)
            });
            let codex_home = snap.codex_home_of(pid);
            let home_override = record.and_then(|r| r.home_override.clone());
            let port = match record {
                Some(r) => r.cdp_port,
                None => match cdp_port_from_argv(&entry.argv) {
                    Some(0) => self.active_port_for(executable.as_deref(), codex_home.as_deref()),
                    other => other,
                },
            };
            let cdp = port.filter(|p| *p != 0).map(|port| CdpEndpoint {
                port,
                owner_verified: self.caches.owner_check(port, pid, entry.started_at, || {
                    owner::check_owner(port, pid)
                }) == OwnerCheck::Verified,
            });
            out.push(AppSession {
                app_server_exe: codexbin::app_server_exe(snap, pid),
                install_id,
                main_pid: pid,
                started_at: entry.started_at,
                executable,
                cdp,
                launched_by_us: record.is_some(),
                authapi_env: snap.app_authapi_env(pid),
                home_override,
                codex_home,
            });
        }
        out
    }

    /// The CDP port of an app started with `--remote-debugging-port=0` by someone
    /// else: read `DevToolsActivePort` from its user-data dir (Linux/macOS).
    fn active_port_for(&self, exe: Option<&Path>, codex_home: Option<&Path>) -> Option<u16> {
        let inst = install::inspect_install(exe?)?;
        // The app's HOME is the parent of its default Codex home when known.
        let home = codex_home
            .filter(|h| h.file_name().is_some_and(|n| n == ".codex"))
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.ctx.home_dir.clone());
        let dir = install::user_data_dir(&inst, self.ctx.env.as_ref(), &home)?;
        let text = std::fs::read_to_string(dir.join("DevToolsActivePort")).ok()?;
        cdp::parse_devtools_active_port(&text)
    }

    fn pick_session(&self, sessions: Vec<AppSession>) -> Option<AppSession> {
        let ours = self.ctx.codex_home();
        let matches_home = |s: &AppSession| {
            s.codex_home
                .as_deref()
                .is_some_and(|h| same_path(h, ours.root()))
        };
        let rank = |s: &AppSession| (matches_home(s), s.launched_by_us, s.started_at);
        sessions.into_iter().max_by_key(rank)
    }

    /// Resolve the main renderer WebSocket URL of a session, or the bridge status
    /// explaining why there is none.
    async fn main_target(
        &self,
        session: &AppSession,
    ) -> std::result::Result<(u16, cdp::CdpTarget), BridgeStatus> {
        let Some(ep) = &session.cdp else {
            return Err(BridgeStatus::NoCdp);
        };
        let port = ep.port;
        let pid = session.main_pid;
        let started_at = session.started_at;
        let caches = self.caches.clone();
        let check = tokio::task::spawn_blocking(move || {
            caches.owner_check(port, pid, started_at, || owner::check_owner(port, pid))
        })
        .await
        .unwrap_or(OwnerCheck::Unknown);
        match check {
            OwnerCheck::Verified | OwnerCheck::Unknown => {}
            OwnerCheck::NotListening => {
                return Err(BridgeStatus::CdpUnreachable {
                    detail: format!("nothing listens on 127.0.0.1:{port}"),
                });
            }
            OwnerCheck::NotLoopback | OwnerCheck::Mismatch { .. } => {
                return Err(BridgeStatus::OwnerMismatch);
            }
        }
        let targets = cdp::list_targets(port, self.tunables.cdp_timeout)
            .await
            .map_err(|e| BridgeStatus::CdpUnreachable {
                detail: e.to_string(),
            })?;
        match cdp::pick_main_target(&targets, port) {
            Some(t) => Ok((port, t.clone())),
            None => Err(BridgeStatus::NoRendererTarget),
        }
    }

    async fn eval_main(&self, session: &AppSession, expression: &str) -> Result<(String, Value)> {
        let (_, target) = self
            .main_target(session)
            .await
            .map_err(|status| Error::Cdp {
                message: format!("renderer not reachable: {status:?}"),
            })?;
        let ws = target.ws_url.clone().unwrap_or_default();
        let v = cdp::evaluate(&ws, expression, self.tunables.cdp_timeout).await?;
        Ok((target.url, v))
    }

    async fn dom_probe(&self, session: &AppSession) -> Result<renderer::DomProbe> {
        let js = renderer::probe_js(&self.tunables.hooks);
        let (_, v) = self.eval_main(session, &js).await?;
        renderer::parse_probe(&v).ok_or_else(|| Error::Cdp {
            message: "unexpected renderer probe result".into(),
        })
    }

    /// Wait until a new app-server child (pid not in `before`) of `main_pid` is
    /// running and the old ones exited (the latter bounded separately).
    async fn wait_new_app_server(&self, main_pid: u32, before: &[u32]) -> Result<u32> {
        let deadline = tokio::time::Instant::now() + self.tunables.restart_timeout;
        let new_pid = loop {
            let snap = snapshot_async(ReadOpts::default()).await?;
            if !snap.all.contains_key(&main_pid) {
                return Err(Error::Process {
                    message: format!("the app (pid {main_pid}) exited during the restart"),
                });
            }
            let now: Vec<u32> = snap
                .app_servers_of(main_pid)
                .iter()
                .map(|e| e.pid)
                .collect();
            if let Some(p) = now.iter().find(|p| !before.contains(p)) {
                break *p;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::Timeout {
                    what: "a new app-server after codex-app-server-restart".into(),
                });
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        };
        let exit_deadline = tokio::time::Instant::now() + self.tunables.old_exit_timeout;
        loop {
            let snap = snapshot_async(ReadOpts::default()).await?;
            let lingering: Vec<u32> = snap
                .app_servers_of(main_pid)
                .iter()
                .map(|e| e.pid)
                .filter(|p| before.contains(p))
                .collect();
            if lingering.is_empty() {
                break;
            }
            if tokio::time::Instant::now() >= exit_deadline {
                tracing::warn!(?lingering, "old app-server still running after restart");
                break;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        Ok(new_pid)
    }

    /// Wait for the CDP port and the bridge after a launch.
    async fn wait_ready(
        &self,
        pid: u32,
        plan: &CdpPlan,
        launched_at: Timestamp,
        deadline: tokio::time::Instant,
    ) -> Result<Option<u16>> {
        let mut port: Option<u16> = match plan {
            CdpPlan::Disabled => return Ok(None),
            CdpPlan::Fixed(p) => Some(*p),
            CdpPlan::ActivePortFile(_) => None,
        };
        loop {
            if procs::liveness(pid, None) == procs::Liveness::Gone {
                return Err(Error::Process {
                    message: format!(
                        "the app (pid {pid}) exited right after start; another instance with \
                         the same profile may be running"
                    ),
                });
            }
            if port.is_none()
                && let CdpPlan::ActivePortFile(file) = plan
            {
                port = read_fresh_active_port(file, launched_at);
            }
            if let Some(p) = port {
                // Check ownership before even querying targets, especially when a
                // fixed port may have been claimed between reservation and launch.
                let check = tokio::task::spawn_blocking(move || owner::check_owner(p, pid))
                    .await
                    .unwrap_or(OwnerCheck::Unknown);
                if checked_endpoint(p, check.clone()).is_none() {
                    return Ok(None);
                }
                if check != OwnerCheck::NotListening
                    && let Ok(targets) = cdp::list_targets(p, self.tunables.cdp_timeout).await
                    && let Some(t) = cdp::pick_main_target(&targets, p)
                    && let Some(ws) = t.ws_url.as_deref()
                    && matches!(
                        cdp::evaluate(ws, BRIDGE_PROBE_JS, self.tunables.cdp_timeout).await,
                        Ok(Value::Bool(true))
                    )
                {
                    return Ok(Some(p));
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(port);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

/// Port from a `DevToolsActivePort` written at or after `since` (1 s slack).
fn read_fresh_active_port(file: &Path, since: Timestamp) -> Option<u16> {
    let meta = std::fs::metadata(file).ok()?;
    let mtime = Timestamp::from_system_time(meta.modified().ok()?);
    if mtime.as_millis() + 1_000 < since.as_millis() {
        return None;
    }
    cdp::parse_devtools_active_port(&std::fs::read_to_string(file).ok()?)
}

/// Kinds that consume a Codex home (turns can run in them).
fn is_consumer(kind: &CodexProcessKind) -> bool {
    matches!(
        kind,
        CodexProcessKind::AppServer
            | CodexProcessKind::CliTui
            | CodexProcessKind::CliExec
            | CodexProcessKind::CliDaemon
            | CodexProcessKind::IdeAppServer
    )
}

fn processes_for_home(snap: &Snapshot, home: &CodexHome) -> Vec<ProcessInfo> {
    snap.classified
        .iter()
        .filter_map(|(pid, _)| snap.process_info(*pid))
        .filter(|p| {
            p.codex_home
                .as_deref()
                .is_none_or(|h| same_path(h, home.root()))
        })
        .collect()
}

#[async_trait]
impl AppController for DesktopAppController {
    async fn detect_installs(&self) -> Result<Vec<AppInstall>> {
        Ok(install::detect_installs(self.ctx.env.as_ref(), &self.ctx.home_dir).await)
    }

    async fn codex_binaries(&self) -> Result<Vec<CodexBinary>> {
        let installs = install::detect_installs(self.ctx.env.as_ref(), &self.ctx.home_dir).await;
        Ok(clibin::codex_binaries(self.ctx.env.as_ref(), &self.ctx.home_dir, &installs).await)
    }

    async fn running_session(&self) -> Result<Option<AppSession>> {
        let snap = snapshot_async(ReadOpts { environ: true }).await?;
        let this = self.clone();
        let sessions = tokio::task::spawn_blocking(move || this.sessions_in(&snap))
            .await
            .map_err(blocking_err)?;
        Ok(self.pick_session(sessions))
    }

    async fn session_and_processes(
        &self,
        home: &CodexHome,
    ) -> (Result<Option<AppSession>>, Result<Vec<ProcessInfo>>) {
        let snap = match snapshot_async(ReadOpts { environ: true }).await {
            Ok(snap) => snap,
            Err(e) => return (Err(e.clone()), Err(e)),
        };
        let this = self.clone();
        let home = home.clone();
        match tokio::task::spawn_blocking(move || {
            (this.sessions_in(&snap), processes_for_home(&snap, &home))
        })
        .await
        {
            Ok((sessions, processes)) => (Ok(self.pick_session(sessions)), Ok(processes)),
            Err(e) => {
                let e = blocking_err(e);
                (Err(e.clone()), Err(e))
            }
        }
    }

    fn environment_stamp(&self, installs: &[AppInstall], binaries: &[CodexBinary]) -> Option<u64> {
        let key = watch::inputs_key(installs, binaries);
        let mut cached = codexbin::lock(&self.caches.watch);
        let paths = match cached.as_ref() {
            Some((k, paths)) if *k == key => paths,
            _ => {
                let paths = watch::watch_paths(
                    self.ctx.env.as_ref(),
                    &self.ctx.home_dir,
                    self.ctx.codex_home().root(),
                    installs,
                    binaries,
                );
                &cached.insert((key, paths)).1
            }
        };
        Some(watch::metadata_stamp(paths))
    }

    fn forget_cached_probes(&self) {
        install::forget_probe_caches();
        self.caches.forget_owners();
    }

    async fn launch(&self, options: &LaunchOptions) -> Result<AppSession> {
        if cfg!(windows) && options.env.contains_key(ENV_CODEX_CLI_PATH) {
            // Windows: packaged apps get no per-launch environment and a pinned
            // Codex runs outside the app's sandbox identity (plan §9 Q6).
            return Err(Error::Unsupported {
                what: "pinning the desktop app's Codex on Windows".into(),
            });
        }
        let install = self.find_install(options.install_id.as_deref()).await?;
        let app_home = self.app_home(options.home_override.as_deref());
        let app_codex_home = CodexHome::resolve(&self.launch_env_view(options), &app_home);

        // A second instance with the same profile would hand its arguments to the
        // running one and exit; reuse or refuse instead.
        if let Some(existing) = self.running_session().await?.filter(|s| {
            s.executable
                .as_deref()
                .is_some_and(|e| same_path(e, &install.executable))
                && s.codex_home
                    .as_deref()
                    .is_none_or(|h| same_path(h, app_codex_home.root()))
        }) {
            if existing.cdp.is_some() || options.cdp == superba_core::CdpMode::Disabled {
                return Ok(existing);
            }
            return Err(Error::Conflict {
                message: format!(
                    "the app is already running (pid {}) without a debugging port; quit it first",
                    existing.main_pid
                ),
            });
        }

        let env_view = self.launch_env_view(options);
        let user_data = install::user_data_dir(&install, &env_view, &app_home);
        let plan = launch::cdp_plan(options.cdp, user_data.as_deref())?;
        let app_args = self
            .ctx
            .env
            .var_non_empty(ENV_SUPERBA_APP_ARGS)
            .map(|s| launch::split_app_args(&s))
            .unwrap_or_default();
        let program = launch::program_for(&install);
        let spec = launch::build_spec(&program, options, &app_args, plan.clone());
        let launched_at = Timestamp::now();
        tracing::info!(
            program = %program.display(),
            args = ?superba_core::redact_argv(&spec.args),
            env = ?spec.env_set.keys().collect::<Vec<_>>(),
            "launching the desktop app"
        );
        let spawn_spec = spec.clone();
        let spawn_install = install.clone();
        let log = self.launch_log_path();
        // LaunchServices may take several seconds to expose the new main pid.
        let spawned = tokio::task::spawn_blocking(move || {
            launch::spawn(&spawn_spec, &spawn_install, Some(&log))
        })
        .await
        .map_err(blocking_err)??;
        let ready_timeout = if options.ready_timeout_ms == 0 {
            self.tunables.ready_timeout
        } else {
            Duration::from_millis(options.ready_timeout_ms)
        };
        let deadline = tokio::time::Instant::now() + ready_timeout;
        let pid = match spawned.pid {
            Some(p) => p,
            None => {
                return Err(Error::Process {
                    message: "the app started but its pid is unknown".into(),
                });
            }
        };
        let port = self.wait_ready(pid, &plan, launched_at, deadline).await?;

        // Observe the first app-server child (its environment is what a hot
        // restart inherits), bounded by the readiness deadline.
        let extra = tokio::time::Instant::now() + self.tunables.app_server_wait;
        let wait_until = deadline.max(tokio::time::Instant::now()).min(extra);
        let snap = loop {
            let snap = snapshot_async(ReadOpts { environ: true }).await?;
            if !snap.app_servers_of(pid).is_empty() || tokio::time::Instant::now() >= wait_until {
                break snap;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        };
        let started_at = snap.all.get(&pid).and_then(|e| e.started_at);
        let cdp = match port {
            Some(port) => {
                let check = tokio::task::spawn_blocking(move || owner::check_owner(port, pid))
                    .await
                    .unwrap_or(OwnerCheck::Unknown);
                self.caches
                    .record_owner(port, pid, started_at, check.clone());
                checked_endpoint(port, check)
            }
            None => None,
        };
        let record = store::LaunchRecord {
            install_id: Some(install.id.clone()),
            executable: Some(install.executable.clone()),
            main_pid: pid,
            process_started_at: started_at,
            launched_at,
            cdp_port: cdp.as_ref().map(|c| c.port),
            home_override: options.home_override.clone(),
            injected_env: spec.env_set.keys().cloned().collect(),
        };
        let path = self.sessions_path();
        tokio::task::spawn_blocking(move || {
            store::record_launch(&path, record, &|r| {
                procs::liveness(r.main_pid, r.process_started_at) == procs::Liveness::Alive
            })
        })
        .await
        .map_err(blocking_err)??;
        let codex_home = snap
            .app_servers_of(pid)
            .iter()
            .find_map(|c| c.codex_home())
            .or_else(|| Some(app_codex_home.root().to_path_buf()));
        Ok(AppSession {
            app_server_exe: codexbin::app_server_exe(&snap, pid),
            install_id: Some(install.id.clone()),
            main_pid: pid,
            started_at,
            executable: Some(install.executable.clone()),
            cdp,
            launched_by_us: true,
            authapi_env: snap.app_authapi_env(pid),
            home_override: options.home_override.clone(),
            codex_home,
        })
    }

    async fn probe_bridge(&self, session: &AppSession) -> Result<BridgeStatus> {
        let (_, target) = match self.main_target(session).await {
            Ok(t) => t,
            Err(status) => return Ok(status),
        };
        let Some(ws) = target.ws_url.as_deref() else {
            return Ok(BridgeStatus::NoRendererTarget);
        };
        Ok(
            match cdp::evaluate(ws, BRIDGE_PROBE_JS, self.tunables.cdp_timeout).await {
                Ok(Value::Bool(true)) => BridgeStatus::Ready {
                    target_url: target.url.clone(),
                },
                Ok(_) => BridgeStatus::BridgeMissing,
                Err(e) => BridgeStatus::CdpUnreachable {
                    detail: e.to_string(),
                },
            },
        )
    }

    async fn hot_restart(
        &self,
        session: &AppSession,
        request: &HotRestartRequest,
    ) -> Result<HotRestartOutcome> {
        let (_, target) = self
            .main_target(session)
            .await
            .map_err(|status| Error::Cdp {
                message: format!("restart bridge not available: {status:?}"),
            })?;
        let ws = target.ws_url.clone().unwrap_or_default();
        let local = request.host_id == LOCAL_HOST_ID;
        let before: Vec<u32> = if local {
            snapshot_async(ReadOpts::default())
                .await?
                .app_servers_of(session.main_pid)
                .iter()
                .map(|e| e.pid)
                .collect()
        } else {
            Vec::new()
        };
        let js = hot_restart_js(&request.host_id, request.kill_remote);
        let sent_at = Timestamp::now();
        match cdp::evaluate(&ws, &js, self.tunables.cdp_timeout).await? {
            Value::Bool(true) => {}
            other => {
                return Err(Error::Cdp {
                    message: format!("the restart bridge refused the call ({other})"),
                });
            }
        }
        tracing::info!(host = %request.host_id, kill = request.kill_remote, "restart IPC sent");
        if local && !before.is_empty() {
            let new_pid = self.wait_new_app_server(session.main_pid, &before).await?;
            tracing::info!(new_pid, ?before, "new app-server running");
        }
        Ok(HotRestartOutcome { sent_at })
    }

    async fn quit(&self, session: &AppSession, options: &QuitOptions) -> Result<QuitOutcome> {
        // Windows: closing the windows leaves the app running in the notification
        // area; with CDP the app is asked to quit the way its own menu does.
        let requested = if cfg!(windows) && session.cdp.is_some() {
            match self.eval_main(session, quit::QUIT_APP_JS).await {
                Ok((_, Value::Bool(true))) => Some(quit::QuitRequest::App),
                other => {
                    tracing::info!(?other, "cannot ask the app to quit over CDP");
                    None
                }
            }
        } else {
            None
        };
        let outcome = quit::quit_after(session, options, requested).await;
        if matches!(
            outcome,
            QuitOutcome::Exited | QuitOutcome::Killed | QuitOutcome::NotRunning
        ) {
            let path = self.sessions_path();
            let pid = session.main_pid;
            tokio::task::spawn_blocking(move || {
                store::forget(&path, pid, &|r| {
                    procs::liveness(r.main_pid, r.process_started_at) == procs::Liveness::Alive
                })
            })
            .await
            .map_err(blocking_err)??;
        }
        Ok(outcome)
    }

    async fn stop_process(
        &self,
        process: &ProcessInfo,
        force: bool,
        timeout_ms: u64,
    ) -> Result<superba_core::StopOutcome> {
        stop::stop(process, force, Duration::from_millis(timeout_ms)).await
    }

    async fn processes(&self, home: &CodexHome) -> Result<Vec<ProcessInfo>> {
        let snap = snapshot_async(ReadOpts { environ: true }).await?;
        let home = home.clone();
        tokio::task::spawn_blocking(move || processes_for_home(&snap, &home))
            .await
            .map_err(blocking_err)
    }

    async fn turn_activity(
        &self,
        home: &CodexHome,
        session: Option<&AppSession>,
    ) -> Result<TurnActivity> {
        let now = Timestamp::now();
        let opts = activity::ScanOpts::at(now);
        let sessions_dir = home.sessions_dir();
        let home_c = home.clone();
        let (findings, live_starts) = tokio::task::spawn_blocking(move || {
            let findings = activity::scan_rollouts(&sessions_dir, &opts);
            let snap = procs::snapshot(ReadOpts { environ: true });
            let live: Vec<Option<Timestamp>> = processes_for_home(&snap, &home_c)
                .into_iter()
                .filter(|p| is_consumer(&p.kind))
                .map(|p| p.started_at)
                .collect();
            (findings, live)
        })
        .await
        .map_err(blocking_err)?;
        let mut complete = findings.is_ok();
        let mut busy = match &findings {
            Ok(f) => activity::busy_threads(f, &live_starts, &opts),
            Err(e) => {
                tracing::warn!(error = %e, "rollout scan failed");
                Vec::new()
            }
        };
        if let Some(s) = session.filter(|s| s.cdp.is_some()) {
            match self.dom_probe(s).await {
                Ok(p) => {
                    renderer::fill_titles(&mut busy, &p);
                    for t in renderer::busy_threads(&p) {
                        let dup = t.thread_id.is_some()
                            && busy
                                .iter()
                                .any(|b| b.target_id == t.target_id && b.thread_id == t.thread_id);
                        if !dup {
                            busy.push(t);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "renderer activity probe failed");
                    complete = false;
                }
            }
        }
        Ok(TurnActivity {
            target_id: TargetId::local(),
            checked_at: now,
            busy,
            complete,
        })
    }

    async fn renderer_state(&self, session: &AppSession) -> Result<RendererState> {
        let p = self.dom_probe(session).await?;
        Ok(renderer::renderer_state(&p))
    }

    async fn return_to_thread(&self, session: &AppSession, thread: &ThreadRef) -> Result<()> {
        if !renderer::valid_thread_id(&thread.thread_id) {
            return Err(Error::invalid(format!(
                "not a thread id: {:?}",
                thread.thread_id
            )));
        }
        let js = renderer::navigate_js(&self.tunables.hooks, thread);
        let (_, v) = self.eval_main(session, &js).await?;
        match v.as_str() {
            Some("already" | "route" | "click") => Ok(()),
            _ => Err(Error::Cdp {
                message: format!("could not open thread {}", thread.thread_id),
            }),
        }
    }

    async fn restart_daemon(&self, home: &CodexHome) -> Result<()> {
        let running = self
            .processes(home)
            .await?
            .into_iter()
            .any(|p| p.kind == CodexProcessKind::CliDaemon);
        if !running {
            return Ok(());
        }
        let bin = self.daemon_binary().await?;
        daemon::daemon_command(&bin, "restart", home).await
    }

    async fn stop_daemon(&self, home: &CodexHome) -> Result<()> {
        let daemons = |ps: Vec<ProcessInfo>| {
            ps.into_iter()
                .filter(|p| p.kind == CodexProcessKind::CliDaemon)
                .count()
        };
        if daemons(self.processes(home).await?) == 0 {
            return Ok(());
        }
        let bin = self.daemon_binary().await?;
        daemon::daemon_command(&bin, "stop", home).await?;
        let deadline = tokio::time::Instant::now() + self.tunables.daemon_stop_timeout;
        loop {
            if daemons(self.processes(home).await?) == 0 {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::Timeout {
                    what: "the Codex daemon to stop".into(),
                });
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn remote_connections(&self, session: &AppSession) -> Result<Vec<RemoteHost>> {
        let p = self.dom_probe(session).await?;
        Ok(renderer::remote_hosts(&p))
    }

    async fn launcher_entry(&self) -> Result<bool> {
        if !cfg!(target_os = "linux") {
            return Err(Error::Unsupported {
                what: "the launcher entry on this platform".into(),
            });
        }
        let dir = launcher::user_applications_dir(self.ctx.env.as_ref(), &self.ctx.home_dir);
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return Ok(false);
        };
        Ok(rd.flatten().any(|e| {
            e.path().extension().is_some_and(|x| x == "desktop") && launcher::is_managed(&e.path())
        }))
    }

    async fn set_launcher_entry(&self, enabled: bool) -> Result<()> {
        if !cfg!(target_os = "linux") {
            return Err(Error::Unsupported {
                what: "the launcher entry on this platform".into(),
            });
        }
        let dir = launcher::user_applications_dir(self.ctx.env.as_ref(), &self.ctx.home_dir);
        if !enabled {
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().is_some_and(|x| x == "desktop") && launcher::is_managed(&p) {
                        std::fs::remove_file(&p).map_err(|err| Error::io(&p, &err))?;
                    }
                }
            }
            return Ok(());
        }
        let exe = install::explicit_install(self.ctx.env.as_ref())
            .or(self.find_install(None).await.ok())
            .map(|i| i.executable);
        let (name, base) =
            launcher::find_system_entry(&launcher::system_entry_dirs(), exe.as_deref())
                .map(|(n, t)| (n, Some(t)))
                .unwrap_or_else(|| (launcher::DEFAULT_ENTRY_NAME.to_owned(), None));
        let path = dir.join(&name);
        if path.exists() && !launcher::is_managed(&path) {
            return Err(Error::Conflict {
                message: format!("{} exists and is not managed by Superba", path.display()),
            });
        }
        let text = launcher::render_entry(base.as_deref(), &launcher::superba_exec());
        std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, &e))?;
        std::fs::write(&path, text).map_err(|e| Error::io(&path, &e))
    }

    async fn app_server_exe(&self, session: &AppSession) -> Result<Option<PathBuf>> {
        let snap = snapshot_async(ReadOpts::default()).await?;
        Ok(codexbin::app_server_exe(&snap, session.main_pid))
    }

    async fn app_server_floor(&self, install: &AppInstall) -> Result<CodexVersion> {
        let asar = codexbin::asar_path(install).ok_or_else(|| Error::NotFound {
            what: format!("app.asar of {}", install.path.display()),
        })?;
        let caches = self.caches.clone();
        let floor_cache = self.ctx.data_dir.cache_dir().join(FLOOR_CACHE_FILE);
        tokio::task::spawn_blocking(move || floor_cached(&caches, &asar, &floor_cache))
            .await
            .map_err(blocking_err)?
    }

    async fn codex_debug_models(&self, binary: &Path, catalog: Option<&Path>) -> Result<String> {
        let caches = self.caches.clone();
        let (bin, cat) = (binary.to_path_buf(), catalog.map(Path::to_path_buf));
        let key = tokio::task::spawn_blocking(move || -> Result<(String, String)> {
            let bin_sha = caches.binary_sha(&bin)?;
            let cat_sha = match &cat {
                Some(c) => codexbin::sha256_file(c)?,
                None => "bundled".to_owned(),
            };
            Ok((bin_sha, cat_sha))
        })
        .await
        .map_err(blocking_err)??;
        if let Some(out) = self.caches.debug_models(&key) {
            return Ok(out);
        }
        let out = codexbin::debug_models(binary, catalog, codexbin::DEBUG_MODELS_TIMEOUT).await?;
        self.caches.put_debug_models(key, out.clone());
        Ok(out)
    }

    async fn cli_install_kind(&self, binary: &Path) -> Result<CodexInstallKind> {
        let installs = install::detect_installs(self.ctx.env.as_ref(), &self.ctx.home_dir).await;
        let store = self.ctx.data_dir.codex_store_dir();
        let binary = binary.to_path_buf();
        tokio::task::spawn_blocking(move || codexbin::install_kind(&binary, &store, &installs))
            .await
            .map_err(blocking_err)
    }

    async fn installed_packages(&self) -> Result<Vec<superba_core::InstalledPackage>> {
        tokio::task::spawn_blocking(package::installed_packages)
            .await
            .map_err(blocking_err)
    }

    async fn verify_package_signature(&self, path: &Path) -> Result<superba_core::PackageSigner> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || package::verify_signature(&path))
            .await
            .map_err(blocking_err)?
    }

    async fn open_package_installer(&self, path: &Path) -> Result<()> {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || package::open_with_default_handler(&path))
            .await
            .map_err(blocking_err)?
    }
}

/// File under `DataDir::cache_dir` remembering asar floors across runs (a scan of
/// a ~560 MB `app.asar` takes about a second).
const FLOOR_CACHE_FILE: &str = "app-server-floor.json";

/// [`codexbin::scan_floor`] cached in memory and in `cache_file`, both keyed by
/// the asar's path, size and mtime (an app update changes them).
fn floor_cached(caches: &codexbin::Caches, asar: &Path, cache_file: &Path) -> Result<CodexVersion> {
    #[derive(serde::Serialize, serde::Deserialize, Default)]
    struct Disk {
        entries: Vec<(codexbin::FileStamp, String)>,
    }
    let stamp = codexbin::FileStamp::of(asar).ok_or_else(|| Error::NotFound {
        what: format!("{}", asar.display()),
    })?;
    if let Some(v) = caches.floor(&stamp) {
        return Ok(v);
    }
    let mut disk: Disk = std::fs::read(cache_file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    if let Some(v) = disk
        .entries
        .iter()
        .find(|(s, _)| s == &stamp)
        .and_then(|(_, v)| CodexVersion::parse(v).ok())
    {
        caches.put_floor(stamp, v.clone());
        return Ok(v);
    }
    let floor = codexbin::scan_floor(&stamp.path)?;
    caches.put_floor(stamp.clone(), floor.clone());
    disk.entries.retain(|(s, _)| s.path != stamp.path);
    disk.entries.push((stamp, floor.to_string()));
    disk.entries.truncate(8);
    if let Some(dir) = cache_file.parent()
        && std::fs::create_dir_all(dir).is_ok()
        && let Ok(bytes) = serde_json::to_vec_pretty(&disk)
    {
        let tmp = cache_file.with_extension("json.tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, cache_file);
        }
    }
    Ok(floor)
}

/// The floor to use when [`AppController::app_server_floor`] fails.
pub fn floor_or_fallback(result: Result<CodexVersion>) -> CodexVersion {
    result.unwrap_or(APP_SERVER_FLOOR_FALLBACK)
}

impl DesktopAppController {
    /// The standalone `codex` that manages the daemon: first on PATH, not bundled.
    async fn daemon_binary(&self) -> Result<PathBuf> {
        let installs = install::detect_installs(self.ctx.env.as_ref(), &self.ctx.home_dir).await;
        let bins =
            clibin::codex_binaries(self.ctx.env.as_ref(), &self.ctx.home_dir, &installs).await;
        bins.iter()
            .filter(|b| b.source != InstallSource::AppBundled)
            .min_by_key(|b| b.path_rank.unwrap_or(usize::MAX))
            .map(|b| b.path.clone())
            .ok_or_else(|| Error::Unsupported {
                what: "managing the Codex daemon (no standalone codex binary found)".into(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mismatched_listener_is_never_recorded_as_cdp() {
        for check in [
            OwnerCheck::NotLoopback,
            OwnerCheck::Mismatch { owners: vec![42] },
            OwnerCheck::Mismatch { owners: vec![] },
        ] {
            assert!(checked_endpoint(9222, check).is_none());
        }
        assert!(
            checked_endpoint(9222, OwnerCheck::Verified)
                .unwrap()
                .owner_verified
        );
        assert!(
            !checked_endpoint(9222, OwnerCheck::Unknown)
                .unwrap()
                .owner_verified
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn launch_readiness_rejects_foreign_listener_before_evaluating() {
        let tmp = tempfile::tempdir().unwrap();
        let controller = DesktopAppController::new(LocalContext::for_tests(tmp.path()));
        let fake =
            cdp_test_support::serve(vec!["app://-/index.html"], Arc::new(|_| Value::Bool(true)))
                .await;
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        let result = controller
            .wait_ready(
                pid,
                &CdpPlan::Fixed(fake.port),
                Timestamp::now(),
                tokio::time::Instant::now() + Duration::from_secs(2),
            )
            .await;
        let alive = child.try_wait().unwrap().is_none();
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(result.unwrap(), None);
        assert!(alive, "rejecting CDP must leave the app running");
        assert!(fake.evaluated.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn launch_readiness_evaluates_verified_listener() {
        let tmp = tempfile::tempdir().unwrap();
        let controller = DesktopAppController::new(LocalContext::for_tests(tmp.path()));
        let fake =
            cdp_test_support::serve(vec!["app://-/index.html"], Arc::new(|_| Value::Bool(true)))
                .await;
        let port = controller
            .wait_ready(
                std::process::id(),
                &CdpPlan::Fixed(fake.port),
                Timestamp::now(),
                tokio::time::Instant::now() + Duration::from_secs(2),
            )
            .await
            .unwrap();
        assert_eq!(port, Some(fake.port));
        assert_eq!(*fake.evaluated.lock().unwrap(), vec![BRIDGE_PROBE_JS]);
    }
}
