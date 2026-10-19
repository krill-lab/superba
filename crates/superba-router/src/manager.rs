//! Lifecycle of router mode as seen from Superba (GUI and `spb`): write the
//! configuration, install the service, wait for health, apply changes, and leave
//! router mode (fallback first, then uninstall).

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use superba_core::{DataDir, Error, Result, Secret};
use url::Url;

use crate::client::AdminClient;
use crate::config::{ROUTER_BASE_PATH, RouterConfig, RouterSecrets};
use crate::doctor::{self, DoctorInput, RouterFacts};
use crate::paths::RouterPaths;
use crate::service::{
    self, CommandRunner, ServiceHost, ServiceManager, ServiceSpec, ServiceStatus, SystemRunner,
};
use crate::stats::StatsConfig;
use crate::watchdog::{self, DirectModeFallback, FallbackReason, Liveness};

/// What Codex is configured with in router mode.
#[derive(Clone, Debug)]
pub struct RouterEndpoint {
    /// `openai_base_url`: `http://127.0.0.1:<port>/backend-api/codex`.
    pub base_url: Url,
    /// `auth.json` `OPENAI_API_KEY` (with `"auth_mode":"apikey"`).
    pub token: Secret,
    /// Loopback port.
    pub port: u16,
}

impl RouterEndpoint {
    fn new(port: u16, token: Secret) -> Result<Self> {
        let base_url = Url::parse(&format!("http://127.0.0.1:{port}{ROUTER_BASE_PATH}"))
            .map_err(|e| Error::other(format!("router base URL: {e}")))?;
        Ok(Self {
            base_url,
            token,
            port,
        })
    }
}

/// Router state for the UI.
#[derive(Clone, Debug)]
pub struct RouterOverview {
    /// `config.json` exists and is valid.
    pub configured: bool,
    /// The service as the service manager sees it.
    pub service: Result<ServiceStatus>,
    /// The router process as its admin API sees it.
    pub liveness: Liveness,
}

/// File name of the router program on this platform (`superba-router[.exe]`).
pub fn router_exe_name() -> String {
    format!("superba-router{}", std::env::consts::EXE_SUFFIX)
}

/// `superba-router[.exe]` next to `current_exe` (where Superba's binaries ship).
pub fn router_exe_next_to(current_exe: &Path) -> PathBuf {
    let name = router_exe_name();
    current_exe
        .parent()
        .map(|d| d.join(&name))
        .unwrap_or_else(|| PathBuf::from(name))
}

/// Where the router program of the Superba binary `current_exe` may be, in
/// order. Every release layout puts it next to `superba` / `spb`:
///
/// 1. next to `current_exe`: Linux and Windows archives, `Superba.app/Contents/MacOS`
///    (for `superba` inside the bundle), cargo target directories;
/// 2. next to the resolved `current_exe`, for a symlink such as
///    `/usr/local/bin/spb` pointing into an unpacked archive;
/// 3. on macOS, inside a `Superba.app` next to the executable (the release ZIP and
///    DMG put `spb` beside the bundle), then `/Applications/Superba.app` and
///    `~/Applications/Superba.app` (for `spb` copied elsewhere);
/// 4. every directory on `PATH` (`path_var`).
pub fn router_exe_candidates(
    current_exe: &Path,
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&Path>,
) -> Vec<PathBuf> {
    router_exe_candidates_for(current_exe, path_var, home, cfg!(target_os = "macos"))
}

/// [`router_exe_candidates`] with the macOS bundle locations included or not
/// (`macos`), so the macOS layouts are tested on every platform.
pub fn router_exe_candidates_for(
    current_exe: &Path,
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&Path>,
    macos: bool,
) -> Vec<PathBuf> {
    let name = router_exe_name();
    let mut out: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        if !out.contains(&p) {
            out.push(p);
        }
    };
    push(router_exe_next_to(current_exe));
    if let Ok(real) = std::fs::canonicalize(current_exe) {
        push(router_exe_next_to(&real));
    }
    if macos {
        // A macOS bundle's program never has the host's `.exe` suffix.
        let in_bundle = |app: PathBuf| app.join("Contents").join("MacOS").join("superba-router");
        if let Some(dir) = current_exe.parent() {
            push(in_bundle(dir.join("Superba.app")));
        }
        push(in_bundle(PathBuf::from("/Applications/Superba.app")));
        if let Some(home) = home {
            push(in_bundle(home.join("Applications").join("Superba.app")));
        }
    }
    if let Some(path) = path_var {
        for dir in std::env::split_paths(path).filter(|d| d.is_absolute()) {
            push(dir.join(&name));
        }
    }
    out
}

/// The program to use among `candidates` (in order): the first existing one
/// that is not on a disk image or in a translocated copy, else the first
/// existing one (so the "temporary location" error names it), else `fallback`.
///
/// A copy in `/Applications` therefore wins over the one on a still-mounted
/// DMG when `spb` is run from the DMG (or the app was once opened from it): the
/// service must keep working after the DMG is ejected.
pub fn pick_router_exe(
    candidates: &[PathBuf],
    is_file: impl Fn(&Path) -> bool,
    fallback: PathBuf,
) -> PathBuf {
    let existing: Vec<&PathBuf> = candidates.iter().filter(|p| is_file(p)).collect();
    existing
        .iter()
        .find(|p| !is_transient_location(p))
        .or(existing.first())
        .map(|p| (*p).clone())
        .unwrap_or(fallback)
}

/// The router program for the Superba binary `current_exe`: see
/// [`router_exe_candidates`] and [`pick_router_exe`]; the path next to
/// `current_exe` when none exists (so a missing program is reported where it
/// was expected). Always absolute (launchd and systemd need absolute paths).
pub fn find_router_exe(current_exe: &Path) -> PathBuf {
    let path = std::env::var_os("PATH");
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let candidates = router_exe_candidates(current_exe, path.as_deref(), home.as_deref());
    absolute(pick_router_exe(
        &candidates,
        |p| p.is_file(),
        router_exe_next_to(current_exe),
    ))
}

/// `path` made absolute against the current directory (unchanged when it is
/// already absolute or the current directory is unknown).
pub fn absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        std::path::absolute(&path).unwrap_or(path)
    }
}

/// [`find_router_exe`] for the running process.
pub fn current_router_exe() -> PathBuf {
    std::env::current_exe()
        .map(|p| find_router_exe(&p))
        .unwrap_or_else(|_| PathBuf::from(router_exe_name()))
}

/// A path the per-user service cannot keep using: a macOS disk image
/// (`/Volumes/...` on a read-only file system) or an app that Gatekeeper runs
/// from a temporary, randomized location (`.../AppTranslocation/...`) because it
/// was opened straight from the download. A writable `/Volumes/<disk>` (an
/// external drive the user installed apps on) is not transient.
pub fn is_transient_location(exe: &Path) -> bool {
    transient_location(exe, &read_only_fs)
}

/// [`is_transient_location`] with the read-only check injected (`None` = the
/// file system cannot be inspected, e.g. the path does not exist, which counts
/// as read-only under `/Volumes`).
pub fn transient_location(exe: &Path, read_only: &dyn Fn(&Path) -> Option<bool>) -> bool {
    if exe
        .components()
        .any(|c| c.as_os_str() == "AppTranslocation")
    {
        return true;
    }
    let mut components = exe.components();
    let first_two = (components.next(), components.next());
    matches!(first_two, (Some(Component::RootDir), Some(Component::Normal(v))) if v == "Volumes")
        && read_only(exe).unwrap_or(true)
}

/// A program on a network share: a Windows UNC path (`\\server\share\...`, as
/// a Parallels shared folder `\\Mac\Home\...` is) or, on Windows, a drive
/// letter mapped to one (its canonical path is a UNC path). The per-user
/// service (the Run key) starts it at login, often before the share is
/// connected, so the router does not come up (user bug 39); an installed copy
/// (the MSI) does not have that problem.
pub fn is_network_location(path: &Path) -> bool {
    if network_path_text(&path.to_string_lossy()) {
        return true;
    }
    #[cfg(windows)]
    if let Ok(real) = std::fs::canonicalize(path) {
        return network_path_text(&real.to_string_lossy());
    }
    false
}

/// [`is_network_location`] on the text of a path, without the file system:
/// `\\server\share\...` and `\\?\UNC\server\share\...` (`//server/share` too
/// on Windows, where it means the same). Local device and verbatim paths
/// (`\\?\C:\...`, `\\.\...`) are not network paths.
pub fn network_path_text(path: &str) -> bool {
    let unified = if cfg!(windows) {
        path.replace('/', "\\")
    } else {
        path.to_owned()
    };
    let Some(rest) = unified.strip_prefix(r"\\") else {
        return false;
    };
    if let Some(verbatim) = rest
        .strip_prefix(r"?\")
        .or_else(|| rest.strip_prefix(r".\"))
    {
        return verbatim
            .get(..4)
            .is_some_and(|p| p.eq_ignore_ascii_case(r"UNC\"));
    }
    let mut parts = rest.split('\\');
    let server = parts.next().unwrap_or_default();
    let share = parts.next().unwrap_or_default();
    !server.is_empty() && !share.is_empty()
}

/// Whether the file system holding `path` is mounted read-only (a disk image,
/// a translocated app); `None` when it cannot be inspected.
#[cfg(unix)]
pub fn read_only_fs(path: &Path) -> Option<bool> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: `c` is a NUL-terminated path and `st` points to writable memory
    // of the right type; statvfs only writes into it.
    let rc = unsafe { libc::statvfs(c.as_ptr(), st.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: statvfs succeeded and filled the struct.
    let st = unsafe { st.assume_init() };
    // ST_RDONLY is 1 on Linux and macOS (sys/statvfs.h).
    Some(st.f_flag & 1 != 0)
}

/// Whether the file system holding `path` is mounted read-only (never on
/// Windows, where the check is not needed).
#[cfg(not(unix))]
pub fn read_only_fs(_path: &Path) -> Option<bool> {
    None
}

/// Check that the service can run `exe`: it exists and, on macOS, does not live
/// on a disk image or in a translocated copy.
pub fn check_router_exe(exe: &Path) -> Result<()> {
    if !exe.is_file() {
        return Err(Error::NotFound {
            what: format!(
                "the local router program {} (it ships next to superba and spb; reinstall Superba)",
                exe.display()
            ),
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(exe)
            .map_err(|e| Error::io(exe, &e))?
            .permissions()
            .mode();
        if mode & 0o111 == 0 {
            return Err(Error::invalid(format!(
                "the local router program {} is not executable (reinstall Superba)",
                exe.display()
            )));
        }
    }
    if cfg!(target_os = "macos") && is_transient_location(exe) {
        return Err(Error::invalid(format!(
            "Superba runs from a temporary location ({}); move Superba.app to the \
             Applications folder and open it from there before turning on the local router",
            exe.display()
        )));
    }
    Ok(())
}

/// A stable port for the router of `data_dir` that is free now on
/// `127.0.0.1` and on the WSL addresses `wsl`, not one of `avoid`
/// ([`crate::port`]: per user and data directory, outside the OS ephemeral
/// ranges).
pub fn stable_port(data_dir: &Path, avoid: &[u16], wsl: &[Ipv4Addr]) -> Result<u16> {
    crate::port::pick(&port_seed(data_dir), avoid, wsl).ok_or_else(|| {
        Error::other(format!(
            "no free port for the local router in {}-{}",
            crate::port::STABLE_PORTS.start(),
            crate::port::STABLE_PORTS.end()
        ))
    })
}

/// What a data directory's first port choice is derived from: its absolute
/// path (per user, and per Superba data directory of that user).
pub fn port_seed(data_dir: &Path) -> String {
    absolute(data_dir.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// The service definition for `data_dir` running `exe`. Relative paths are
/// made absolute: a service manager starts the router from `/` (launchd) or the
/// home directory, never from the caller's directory.
pub fn service_spec(data_dir: &DataDir, exe: PathBuf) -> ServiceSpec {
    let data_dir = DataDir::new(absolute(data_dir.root().to_path_buf()));
    let paths = RouterPaths::new(&data_dir);
    ServiceSpec {
        exe: absolute(exe),
        data_dir: data_dir.root().to_path_buf(),
        log_file: paths.log_file(),
        pid_file: paths.supervisor_pid(),
        env: vec![("RUST_LOG".into(), "info".into())],
    }
}

/// How long a new router may need before it counts as crash-looping: a
/// healthy router binds in well under a second, so two exits in this time
/// (launchd restarts it after 5 s) mean it will not come up by itself.
const CRASH_LOOP_AFTER: Duration = Duration::from_secs(6);

/// Router mode lifecycle.
#[derive(Debug)]
pub struct RouterManager {
    paths: RouterPaths,
    spec: ServiceSpec,
    service: Box<dyn ServiceManager>,
    /// Runs the diagnostic commands of [`RouterManager::doctor`].
    runner: Arc<dyn CommandRunner>,
    /// Check the macOS specifics in diagnostics.
    macos: bool,
    /// The user's home (diagnostics).
    home: Option<PathBuf>,
    /// Where the router program was looked for (diagnostics).
    candidates: Vec<PathBuf>,
}

impl RouterManager {
    /// A manager for `data_dir` running `exe` through `service`.
    pub fn new(data_dir: &DataDir, exe: PathBuf, service: Box<dyn ServiceManager>) -> Self {
        let paths = RouterPaths::new(data_dir);
        let spec = service_spec(data_dir, exe);
        Self {
            paths,
            spec,
            service,
            runner: Arc::new(SystemRunner),
            macos: cfg!(target_os = "macos"),
            home: None,
            candidates: Vec::new(),
        }
    }

    /// Diagnostics run their commands through `runner`, check macOS specifics
    /// when `macos`, and look for protected folders below `home` (tests
    /// simulate a Mac with these).
    pub fn with_diagnostics(
        mut self,
        runner: Arc<dyn CommandRunner>,
        macos: bool,
        home: Option<PathBuf>,
    ) -> Self {
        self.runner = runner;
        self.macos = macos;
        self.home = home;
        self
    }

    /// The real platform service manager for the user whose home is `home`.
    pub fn system(data_dir: &DataDir, home: PathBuf, exe: PathBuf) -> Self {
        let host = ServiceHost::system(home.clone());
        let runner = host.runner.clone();
        let candidates = std::env::current_exe()
            .map(|current| {
                router_exe_candidates(&current, std::env::var_os("PATH").as_deref(), Some(&home))
            })
            .unwrap_or_default();
        let mut manager = Self::new(data_dir, exe, service::for_platform(host)).with_diagnostics(
            runner,
            cfg!(target_os = "macos"),
            Some(home),
        );
        manager.candidates = candidates;
        manager
    }

    /// Everything about the router for a support request (`spb router doctor`,
    /// the Router page's Diagnostics); see [`crate::doctor`].
    pub async fn doctor(&self) -> RouterFacts {
        doctor::collect(DoctorInput {
            paths: &self.paths,
            spec: &self.spec,
            service: self.service.as_ref(),
            runner: self.runner.as_ref(),
            macos: self.macos,
            home: self.home.as_deref(),
            candidates: self.candidates.clone(),
        })
        .await
    }

    /// Router files.
    pub fn paths(&self) -> &RouterPaths {
        &self.paths
    }

    /// Service definition.
    pub fn spec(&self) -> &ServiceSpec {
        &self.spec
    }

    /// The endpoint of the stored configuration (port and router token), if any.
    pub fn endpoint(&self) -> Result<RouterEndpoint> {
        let config = RouterConfig::load(&self.paths)?;
        let secrets = RouterSecrets::load(&self.paths)?;
        RouterEndpoint::new(config.listen_port, secrets.router_token)
    }

    /// Validate and write `config.json` + `secrets.json`. The existing router
    /// token is kept (Codex's `auth.json` holds it); a new one is generated only
    /// when none exists. A `listen_port` of 0 keeps the port of the stored
    /// configuration (Codex's `config.toml` points at it, also while the router
    /// is down); only a first configuration gets a new one, the stable port of
    /// this data directory ([`stable_port`]).
    ///
    /// A `config` without `stats` keeps the stored `stats` section (member
    /// rewrites never change recording or the stats line), or gets the defaults
    /// (recording on, stats line off, no prices) when none is stored; only
    /// [`RouterManager::configure`] or a `config` with `stats` changes it. A
    /// `config` without `claude` keeps the stored Claude Code section for the
    /// members it still has; `Some` (also an empty section) replaces it. The
    /// OpenAI ingress's `openai` section is kept the same way.
    pub fn write_config(
        &self,
        config: &RouterConfig,
        member_keys: BTreeMap<String, Secret>,
    ) -> Result<RouterEndpoint> {
        let mut config = config.clone();
        let stored = RouterConfig::load(&self.paths).ok();
        // The WSL listeners are set on their own (`set_wsl_listen`); a members
        // rewrite keeps them.
        if config.wsl_listen.is_empty()
            && let Some(stored) = &stored
        {
            config.wsl_listen = stored.wsl_listen.clone();
        }
        if config.listen_port == 0 {
            config.listen_port = match &stored {
                Some(stored) if stored.listen_port != 0 => stored.listen_port,
                _ => stable_port(&self.spec.data_dir, &[], &config.wsl_listen)?,
            };
        }
        // The Claude Code section is written by whoever owns it; a members
        // rewrite without one keeps the stored section for the members that
        // are still there.
        if config.claude.is_none()
            && let Some(mut claude) = stored.as_ref().and_then(|s| s.claude.clone())
        {
            let ids: Vec<&str> = config.members.iter().map(|m| m.id.as_str()).collect();
            claude.retain_members(&ids);
            config.claude = Some(claude);
        }
        // The same for the OpenAI ingress's section.
        if config.openai.is_none()
            && let Some(mut openai) = stored.as_ref().and_then(|s| s.openai.clone())
        {
            let ids: Vec<&str> = config.members.iter().map(|m| m.id.as_str()).collect();
            openai.retain_members(&ids);
            config.openai = Some(openai);
        }
        if config.gemini.is_none()
            && let Some(mut gemini) = stored.as_ref().and_then(|s| s.gemini.clone())
        {
            let ids: Vec<&str> = config.members.iter().map(|m| m.id.as_str()).collect();
            gemini.members.retain(|id, _| ids.contains(&id.as_str()));
            gemini.order.retain(|id| ids.contains(&id.as_str()));
            config.gemini = Some(gemini);
        }
        if config.stats.is_none() {
            config.stats = Some(stored.and_then(|s| s.stats).unwrap_or_default());
        }
        config.validate()?;
        let secrets = match RouterSecrets::load(&self.paths) {
            Ok(old) if !old.router_token.is_empty() => RouterSecrets {
                router_token: old.router_token,
                member_keys,
            },
            _ => RouterSecrets::generate(member_keys),
        };
        secrets.validate_for(&config)?;
        secrets.save(&self.paths)?;
        config.save(&self.paths)?;
        RouterEndpoint::new(config.listen_port, secrets.router_token)
    }

    /// Enter router mode: write the configuration, install and start the
    /// service, and wait until the router answers (at most `wait`). Superba
    /// writes Codex's router-mode configuration only after this succeeds. A
    /// reinstall (`listen_port` 0) keeps the stored port and token, unless
    /// another program holds that port while the router is down: then the
    /// router moves to a free one ([`RouterManager::port_to_leave`]) and Codex
    /// has to follow (the switch service points it at the new endpoint).
    pub async fn enable(
        &self,
        config: &RouterConfig,
        member_keys: BTreeMap<String, Secret>,
        wait: Duration,
    ) -> Result<RouterEndpoint> {
        check_router_exe(&self.spec.exe)?;
        let mut config = config.clone();
        if config.listen_port == 0
            && let Some((old, new)) = self.port_to_leave().await?
        {
            tracing::warn!(
                old,
                new,
                "another program holds the local router's port; the router moves"
            );
            config.listen_port = new;
        }
        let endpoint = self.write_config(&config, member_keys)?;
        lifecycle_changed();
        self.service.install(&self.spec)?;
        self.wait_until_up(wait).await?;
        Ok(endpoint)
    }

    /// Whether the stored port has to be left, and for which one: the router
    /// is down and another program (not a Superba router) listens on its port.
    /// `None` when there is no stored port, the router answers, or the port is
    /// free or held by a router (perhaps this one, still starting). The new port
    /// is free now on `127.0.0.1` and the WSL addresses of the configuration.
    pub async fn port_to_leave(&self) -> Result<Option<(u16, u16)>> {
        let Ok(stored) = RouterConfig::load(&self.paths) else {
            return Ok(None);
        };
        let old = stored.listen_port;
        if old == 0 || watchdog::check(&self.paths).await.is_up() {
            return Ok(None);
        }
        if !matches!(
            doctor::port_state(old).await,
            doctor::PortState::Busy { .. }
        ) {
            return Ok(None);
        }
        let new = stable_port(&self.spec.data_dir, &[old], &stored.wsl_listen)?;
        Ok(Some((old, new)))
    }

    /// Move a router that is down because another program holds its port
    /// (user bug 65): store a free stable port, restart the service and wait
    /// until it answers (at most `wait`). Codex's configuration is not touched:
    /// the caller points it at the returned endpoint through a normal switch.
    /// `Ok(None)` when nothing had to move (see [`RouterManager::port_to_leave`]).
    pub async fn move_off_taken_port(&self, wait: Duration) -> Result<Option<RouterEndpoint>> {
        let Some((old, new)) = self.port_to_leave().await? else {
            return Ok(None);
        };
        tracing::warn!(
            old,
            new,
            "another program holds the local router's port; the router moves"
        );
        let mut config = RouterConfig::load(&self.paths)?;
        config.listen_port = new;
        config.validate()?;
        config.save(&self.paths)?;
        lifecycle_changed();
        self.service.restart(&self.spec)?;
        self.wait_until_up(wait).await?;
        self.endpoint().map(Some)
    }

    /// Restart the installed service (after its binary was replaced by an
    /// update) and wait until the router answers, at most `wait`. Keeps the
    /// configuration, port and token.
    pub async fn restart(&self, wait: Duration) -> Result<()> {
        lifecycle_changed();
        self.service.restart(&self.spec)?;
        self.wait_until_up(wait).await
    }

    /// Apply a changed configuration: write it and reload a running router.
    /// `listen_port` 0 keeps the current port; a different port restarts the
    /// service (Codex's config must follow).
    pub async fn update(
        &self,
        config: &RouterConfig,
        member_keys: BTreeMap<String, Secret>,
        wait: Duration,
    ) -> Result<RouterEndpoint> {
        let old_port = RouterConfig::load(&self.paths).ok().map(|c| c.listen_port);
        // `write_config` keeps the port Codex is pointed at when it is 0.
        let endpoint = self.write_config(config, member_keys)?;
        if old_port.is_some_and(|p| p != endpoint.port) {
            lifecycle_changed();
            self.service.restart(&self.spec)?;
            self.wait_until_up(wait).await?;
            return Ok(endpoint);
        }
        match AdminClient::from_runtime(&self.paths) {
            Ok(client) => {
                client.reload().await?;
            }
            Err(_) => {
                lifecycle_changed();
                self.service.restart(&self.spec)?;
                self.wait_until_up(wait).await?;
            }
        }
        Ok(endpoint)
    }

    /// Rewrite the configuration and member keys (a member profile was deleted or
    /// got a new key) and reload the router when it is running. Never installs or
    /// starts the service: a stopped router reads the files when it starts. The
    /// port and token are kept.
    pub async fn rewrite(
        &self,
        config: &RouterConfig,
        member_keys: BTreeMap<String, Secret>,
    ) -> Result<RouterEndpoint> {
        let endpoint = self.write_config(config, member_keys)?;
        if watchdog::check(&self.paths).await.is_up() {
            AdminClient::from_runtime(&self.paths)?.reload().await?;
        }
        Ok(endpoint)
    }

    /// Set the `stats` section of the stored configuration (request recording,
    /// retention, the stats line and prices; build it with
    /// [`StatsConfig::from_settings`]) and reload a running router. Never
    /// installs or starts the service and never needs a Codex restart: a stopped
    /// router reads it when it starts. Members, port and keys stay as they are.
    ///
    /// Returns whether a running router reloaded it. Fails with
    /// [`Error::NotFound`] when no router configuration exists yet (write the
    /// members first with [`RouterManager::enable`] / [`RouterManager::write_config`]),
    /// and with the validation error for out-of-range values.
    pub async fn configure(&self, stats: StatsConfig) -> Result<bool> {
        stats.validate()?;
        let mut config = RouterConfig::load(&self.paths).map_err(|e| match e {
            Error::Io { .. } => Error::NotFound {
                what: "the local router configuration (enable the router first)".into(),
            },
            other => other,
        })?;
        if config.stats.as_ref() == Some(&stats) {
            return Ok(false);
        }
        config.stats = Some(stats);
        config.save(&self.paths)?;
        if !watchdog::check(&self.paths).await.is_up() {
            return Ok(false);
        }
        let client = AdminClient::from_runtime(&self.paths)?;
        client.reload().await?;
        Ok(true)
    }

    /// Listen also on these WSL adapter addresses (user bug 45; the same port,
    /// source tokens only). A running router reloads the configuration; it binds
    /// an address once it exists. Returns whether a running router reloaded.
    pub async fn set_wsl_listen(&self, addresses: Vec<std::net::Ipv4Addr>) -> Result<bool> {
        let mut config = RouterConfig::load(&self.paths).map_err(|e| match e {
            Error::Io { .. } => Error::NotFound {
                what: "the local router configuration (enable the router first)".into(),
            },
            other => other,
        })?;
        let mut addresses = addresses;
        addresses.sort();
        addresses.dedup();
        if config.wsl_listen == addresses {
            return Ok(false);
        }
        config.wsl_listen = addresses;
        config.validate()?;
        config.save(&self.paths)?;
        if !watchdog::check(&self.paths).await.is_up() {
            return Ok(false);
        }
        let client = AdminClient::from_runtime(&self.paths)?;
        client.reload().await?;
        Ok(true)
    }

    /// Leave router mode: `fallback` writes the direct configuration first; then
    /// the service is stopped and removed, and the secrets and runtime files are
    /// deleted (`config.json` stays so the members can be re-enabled later).
    pub async fn disable(&self, fallback: &dyn DirectModeFallback) -> Result<()> {
        fallback
            .fall_back_to_direct(&FallbackReason::Disabled)
            .await?;
        lifecycle_changed();
        self.service.uninstall(&self.spec)?;
        for path in [self.paths.secrets_json(), self.paths.runtime_json()] {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(Error::io(&path, &e)),
            }
        }
        Ok(())
    }

    /// Stop and remove the service only, without touching Codex's configuration
    /// (manual clean-up; [`RouterManager::disable`] is the normal path).
    pub fn uninstall_service(&self) -> Result<()> {
        lifecycle_changed();
        self.service.uninstall(&self.spec)
    }

    /// Stop the service and wait for its writer to exit before relocating files.
    /// Keep configuration, keys, token and port. A failed stop is an error: a
    /// SQLite file is never copied from a router that may still be writing it.
    pub async fn stop_for_data_move(&self, wait: Duration) -> Result<()> {
        let runtime = crate::runtime::RuntimeInfo::load(&self.paths).ok();
        let supervisor = std::fs::read_to_string(self.paths.supervisor_pid())
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());
        self.uninstall_service()?;
        if let Ok(client) = AdminClient::from_runtime(&self.paths) {
            let _ = client.shutdown().await;
        }
        let deadline = Instant::now() + wait;
        loop {
            let process_alive = runtime
                .as_ref()
                .is_some_and(|r| self.runner.is_router_process(r.pid));
            let supervisor_alive = supervisor.is_some_and(|pid| self.runner.is_router_process(pid));
            if !process_alive && !supervisor_alive && !watchdog::check(&self.paths).await.is_up() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout {
                    what: "data.router.stop".into(),
                });
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Rebind a service to this data directory and wait for health, using its
    /// copied configuration unchanged (in particular the token and port).
    pub async fn install_existing(&self, wait: Duration) -> Result<()> {
        check_router_exe(&self.spec.exe)?;
        RouterConfig::load(&self.paths)?;
        lifecycle_changed();
        self.service.install(&self.spec)?;
        self.wait_until_up(wait).await
    }

    /// Router state for the UI (polled every few seconds by the Router page).
    ///
    /// The liveness is asked every time (HTTP on loopback, no process). Asking
    /// the service manager starts a process (`systemctl`, `launchctl`,
    /// `tasklist`), so its answer is reused while nothing can have changed it:
    /// no lifecycle call (enable, restart, update, disable, uninstall) of any
    /// manager of this data directory in this process since, and the same
    /// liveness (down, or up with the same router instance). A router that
    /// stops, starts or is replaced asks again; diagnostics
    /// ([`RouterManager::doctor`]) always ask.
    pub async fn overview(&self) -> RouterOverview {
        let liveness = watchdog::check(&self.paths).await;
        RouterOverview {
            configured: RouterConfig::load(&self.paths).is_ok(),
            service: self.polled_status(&liveness),
            liveness,
        }
    }

    fn polled_status(&self, liveness: &Liveness) -> Result<ServiceStatus> {
        let key = self.paths.dir().to_path_buf();
        let live = live_key(liveness);
        let lifecycle = LIFECYCLE.load(Ordering::SeqCst);
        if let Some(memo) = status_memos()
            .iter()
            .find(|m| m.dir == key && m.lifecycle == lifecycle && m.live == live)
        {
            return memo.status.clone();
        }
        let status = self.service.status(&self.spec);
        self.remember_status(live, &status);
        status
    }

    fn remember_status(&self, live: LiveKey, status: &Result<ServiceStatus>) {
        let dir = self.paths.dir().to_path_buf();
        let mut memos = status_memos();
        memos.retain(|m| m.dir != dir);
        if memos.len() >= 8 {
            memos.clear();
        }
        memos.push(StatusMemo {
            dir,
            lifecycle: LIFECYCLE.load(Ordering::SeqCst),
            live,
            status: status.clone(),
        });
    }

    async fn wait_until_up(&self, wait: Duration) -> Result<()> {
        let started = Instant::now();
        let deadline = started + wait;
        let mut next_service_check = started + CRASH_LOOP_AFTER;
        loop {
            let last = match watchdog::check(&self.paths).await {
                Liveness::Up(_) => return Ok(()),
                Liveness::Down { detail } => detail,
            };
            let now = Instant::now();
            // A crash loop will not end by waiting: say why now instead of at the
            // deadline.
            if now >= next_service_check {
                next_service_check = now + Duration::from_secs(1);
                if self.service.diagnose(&self.spec).crash_looping {
                    return Err(Error::Process {
                        message: format!(
                            "the local router keeps stopping right after it starts: {last}\n{}",
                            doctor::start_failure_detail(
                                &self.paths,
                                &self.spec,
                                self.service.as_ref(),
                                8
                            )
                        ),
                    });
                }
            }
            if now >= deadline {
                return Err(Error::Timeout {
                    what: format!(
                        "the local router did not start within {} s: {last}\n{}",
                        wait.as_secs().max(1),
                        doctor::start_failure_detail(
                            &self.paths,
                            &self.spec,
                            self.service.as_ref(),
                            8
                        )
                    ),
                });
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

/// Bumped by every lifecycle call of a [`RouterManager`] in this process: the
/// service-manager answers remembered before are not reused.
static LIFECYCLE: AtomicU64 = AtomicU64::new(0);

fn lifecycle_changed() {
    LIFECYCLE.fetch_add(1, Ordering::SeqCst);
}

/// The router as the liveness check saw it: down, or up as one instance.
#[derive(Clone, Debug, PartialEq, Eq)]
enum LiveKey {
    Down,
    Up(String),
}

fn live_key(liveness: &Liveness) -> LiveKey {
    match liveness {
        Liveness::Up(status) => LiveKey::Up(status.instance_id.clone()),
        Liveness::Down { .. } => LiveKey::Down,
    }
}

/// A remembered service-manager answer for one data directory.
struct StatusMemo {
    dir: PathBuf,
    lifecycle: u64,
    /// The liveness it was asked with.
    live: LiveKey,
    status: Result<ServiceStatus>,
}

fn status_memos() -> std::sync::MutexGuard<'static, Vec<StatusMemo>> {
    static MEMOS: Mutex<Vec<StatusMemo>> = Mutex::new(Vec::new());
    MEMOS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
