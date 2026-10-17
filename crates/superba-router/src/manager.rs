//! Lifecycle of router mode as seen from Superba (GUI and `spb`): write the
//! configuration, install the service, wait for health, apply changes, and leave
//! router mode (fallback first, then uninstall).

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use superba_core::{DataDir, Error, Result, Secret};
use url::Url;

use crate::client::AdminClient;
use crate::config::{ROUTER_BASE_PATH, RouterConfig, RouterSecrets};
use crate::paths::RouterPaths;
use crate::service::{self, ServiceHost, ServiceManager, ServiceSpec, ServiceStatus};
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
    if cfg!(target_os = "macos") {
        let in_bundle = |app: PathBuf| app.join("Contents").join("MacOS").join(&name);
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

/// The router program for the Superba binary `current_exe`: the first of
/// [`router_exe_candidates`] that exists, else the path next to `current_exe`
/// (so a missing program is reported where it was expected).
pub fn find_router_exe(current_exe: &Path) -> PathBuf {
    let path = std::env::var_os("PATH");
    let home = std::env::var_os("HOME").map(PathBuf::from);
    router_exe_candidates(current_exe, path.as_deref(), home.as_deref())
        .into_iter()
        .find(|p| p.is_file())
        .unwrap_or_else(|| router_exe_next_to(current_exe))
}

/// [`find_router_exe`] for the running process.
pub fn current_router_exe() -> PathBuf {
    std::env::current_exe()
        .map(|p| find_router_exe(&p))
        .unwrap_or_else(|_| PathBuf::from(router_exe_name()))
}

/// A path the per-user service cannot keep using: a macOS disk image
/// (`/Volumes/...`) or an app that Gatekeeper runs from a temporary, randomized
/// location (`.../AppTranslocation/...`) because it was opened straight from the
/// download.
pub fn is_transient_location(exe: &Path) -> bool {
    let mut components = exe.components().map(|c| c.as_os_str());
    let first_two = (components.next(), components.next());
    matches!(first_two, (Some(root), Some(v)) if root == "/" && v == "Volumes")
        || exe
            .components()
            .any(|c| c.as_os_str() == "AppTranslocation")
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
    if cfg!(target_os = "macos") && is_transient_location(exe) {
        return Err(Error::invalid(format!(
            "Superba runs from a temporary location ({}); move Superba.app to the \
             Applications folder and open it from there before turning on the local router",
            exe.display()
        )));
    }
    Ok(())
}

/// A free port on `127.0.0.1`, for a new configuration's `listen_port`.
pub fn free_loopback_port() -> Result<u16> {
    let l = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .map_err(|e| Error::other(format!("no free loopback port: {e}")))?;
    l.local_addr()
        .map(|a| a.port())
        .map_err(|e| Error::other(format!("no free loopback port: {e}")))
}

/// The service definition for `data_dir` running `exe`.
pub fn service_spec(data_dir: &DataDir, exe: PathBuf) -> ServiceSpec {
    let paths = RouterPaths::new(data_dir);
    ServiceSpec {
        exe,
        data_dir: data_dir.root().to_path_buf(),
        log_file: paths.log_file(),
        pid_file: paths.supervisor_pid(),
        env: vec![("RUST_LOG".into(), "info".into())],
    }
}

/// Router mode lifecycle.
#[derive(Debug)]
pub struct RouterManager {
    paths: RouterPaths,
    spec: ServiceSpec,
    service: Box<dyn ServiceManager>,
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
        }
    }

    /// The real platform service manager for the user whose home is `home`.
    pub fn system(data_dir: &DataDir, home: PathBuf, exe: PathBuf) -> Self {
        Self::new(
            data_dir,
            exe,
            service::for_platform(ServiceHost::system(home)),
        )
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
    /// is down); only a first configuration gets a free port.
    ///
    /// A `config` without `stats` keeps the stored `stats` section (member
    /// rewrites never change recording or the stats line), or gets the defaults
    /// (recording on, stats line off, no prices) when none is stored; only
    /// [`RouterManager::configure`] or a `config` with `stats` changes it.
    pub fn write_config(
        &self,
        config: &RouterConfig,
        member_keys: BTreeMap<String, Secret>,
    ) -> Result<RouterEndpoint> {
        let mut config = config.clone();
        let stored = RouterConfig::load(&self.paths).ok();
        if config.listen_port == 0 {
            config.listen_port = match &stored {
                Some(stored) if stored.listen_port != 0 => stored.listen_port,
                _ => free_loopback_port()?,
            };
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
    /// reinstall (`listen_port` 0) keeps the stored port and token.
    pub async fn enable(
        &self,
        config: &RouterConfig,
        member_keys: BTreeMap<String, Secret>,
        wait: Duration,
    ) -> Result<RouterEndpoint> {
        check_router_exe(&self.spec.exe)?;
        let endpoint = self.write_config(config, member_keys)?;
        self.service.install(&self.spec)?;
        self.wait_until_up(wait).await?;
        Ok(endpoint)
    }

    /// Restart the installed service (after its binary was replaced by an
    /// update) and wait until the router answers, at most `wait`. Keeps the
    /// configuration, port and token.
    pub async fn restart(&self, wait: Duration) -> Result<()> {
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
            self.service.restart(&self.spec)?;
            self.wait_until_up(wait).await?;
            return Ok(endpoint);
        }
        match AdminClient::from_runtime(&self.paths) {
            Ok(client) => {
                client.reload().await?;
            }
            Err(_) => {
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
        if watchdog::check(&self.paths).await.is_up()
            && let Ok(client) = AdminClient::from_runtime(&self.paths)
            && let Err(e) = client.reload().await
        {
            tracing::warn!(error = %e, "the local router did not reload its new configuration");
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

    /// Leave router mode: `fallback` writes the direct configuration first; then
    /// the service is stopped and removed, and the secrets and runtime files are
    /// deleted (`config.json` stays so the members can be re-enabled later).
    pub async fn disable(&self, fallback: &dyn DirectModeFallback) -> Result<()> {
        fallback
            .fall_back_to_direct(&FallbackReason::Disabled)
            .await?;
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
        self.service.uninstall(&self.spec)
    }

    /// Router state for the UI.
    pub async fn overview(&self) -> RouterOverview {
        RouterOverview {
            configured: RouterConfig::load(&self.paths).is_ok(),
            service: self.service.status(&self.spec),
            liveness: watchdog::check(&self.paths).await,
        }
    }

    async fn wait_until_up(&self, wait: Duration) -> Result<()> {
        let deadline = Instant::now() + wait;
        loop {
            let last = match watchdog::check(&self.paths).await {
                Liveness::Up(_) => return Ok(()),
                Liveness::Down { detail } => detail,
            };
            if Instant::now() >= deadline {
                return Err(Error::Timeout {
                    what: format!("the local router did not start: {last}"),
                });
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}
