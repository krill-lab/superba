//! Router mode lifecycle through `RouterManager` with an in-process "service"
//! that runs the real router from the files the manager wrote.

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use common::*;
use superba_core::{DataDir, Error, Result, Secret, async_trait};
use superba_router::RouterServer;
use superba_router::manager::RouterManager;
use superba_router::paths::RouterPaths;
use superba_router::service::{ServiceKind, ServiceManager, ServiceSpec, ServiceStatus};
use superba_router::watchdog::{self, DirectModeFallback, FallbackReason, Liveness};
use superba_router::{MemberKind, ShutdownHandle};

/// The tests of this file run one at a time (each holds this lock). A first
/// configuration's port is the stable port of its (temporary) data directory,
/// outside the OS ephemeral ranges since user bug 65, so a parallel test's
/// outgoing socket no longer takes it between the choice and the router's bind
/// (before, it came from bind port 0: "cannot listen ... os error 10048", CI run
/// 39106144381); two data directories can still meet on one port, and
/// [`InProcess`] starts the router once, where the real services restart it.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Runs `superba-router run` in a thread of this process.
#[derive(Debug, Default)]
struct InProcess {
    running: Mutex<Option<(ShutdownHandle, std::thread::JoinHandle<()>)>>,
    log: Arc<Mutex<Vec<String>>>,
    broken: bool,
}

impl InProcess {
    fn start(&self, spec: &ServiceSpec) {
        if self.broken {
            return;
        }
        let paths = RouterPaths::new(&DataDir::new(&spec.data_dir));
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let server = RouterServer::bind_from_files(paths).await.unwrap();
                tx.send(server.shutdown_handle()).unwrap();
                server.serve().await.unwrap();
            });
        });
        let handle = rx.recv().unwrap();
        *self.running.lock().unwrap() = Some((handle, thread));
    }

    fn stop(&self) {
        if let Some((handle, thread)) = self.running.lock().unwrap().take() {
            handle.shutdown();
            thread.join().unwrap();
        }
    }
}

impl ServiceManager for InProcess {
    fn kind(&self) -> ServiceKind {
        ServiceKind::SystemdUser
    }
    fn install(&self, spec: &ServiceSpec) -> Result<ServiceStatus> {
        self.log.lock().unwrap().push("install".into());
        self.stop();
        self.start(spec);
        self.status(spec)
    }
    fn uninstall(&self, _: &ServiceSpec) -> Result<()> {
        self.log.lock().unwrap().push("uninstall".into());
        self.stop();
        Ok(())
    }
    fn status(&self, _: &ServiceSpec) -> Result<ServiceStatus> {
        let running = self.running.lock().unwrap().is_some();
        Ok(ServiceStatus {
            kind: self.kind(),
            installed: running,
            running: Some(running),
            pid: None,
            detail: None,
        })
    }
    fn restart(&self, spec: &ServiceSpec) -> Result<()> {
        self.log.lock().unwrap().push("restart".into());
        self.stop();
        self.start(spec);
        Ok(())
    }
}

/// Records the call and checks the router is still serving at that moment, so
/// Codex is repointed before the router goes away.
struct Fallback {
    paths: RouterPaths,
    log: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl DirectModeFallback for Fallback {
    async fn fall_back_to_direct(&self, reason: &FallbackReason) -> Result<()> {
        let up = watchdog::check(&self.paths).await.is_up();
        self.log
            .lock()
            .unwrap()
            .push(format!("fallback {reason:?} router_up={up}"));
        Ok(())
    }
}

/// An existing file standing in for the router program (the service here runs the
/// router in-process; `enable` only checks that the program is there).
fn fake_exe(dir: &std::path::Path) -> std::path::PathBuf {
    let exe = dir.join(superba_router::manager::router_exe_name());
    std::fs::write(&exe, b"").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    exe
}

fn keys() -> BTreeMap<String, Secret> {
    [("krill".to_owned(), Secret::new(key_of("krill")))].into()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enable_update_and_disable() {
    let _serial = SERIAL.lock().await;
    let up = upstream(Mode::Json(200, r#"{"ok":true}"#)).await;
    let dir = tempfile::tempdir().unwrap();
    let data = DataDir::new(dir.path());
    let service = InProcess::default();
    let log = service.log.clone();
    let manager = RouterManager::new(&data, fake_exe(dir.path()), Box::new(service));
    let cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", up.base("/backend-api/codex"))],
    )]);
    assert_eq!(cfg.listen_port, 0);

    let endpoint = manager
        .enable(&cfg, keys(), Duration::from_secs(10))
        .await
        .unwrap();
    assert_ne!(endpoint.port, 0, "a free port was chosen and persisted");
    assert_eq!(
        endpoint.base_url.as_str(),
        format!("http://127.0.0.1:{}/backend-api/codex", endpoint.port)
    );
    assert!(endpoint.token.expose().starts_with("sbr_"));

    // Codex's view: API key = router token, base URL = the router.
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let resp = http
        .post(format!("{}/responses", endpoint.base_url))
        .bearer_auth(endpoint.token.expose())
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        up.seen()[0].headers["authorization"],
        format!("Bearer {}", key_of("krill")).as_str()
    );

    let overview = manager.overview().await;
    assert!(overview.configured);
    assert!(overview.service.unwrap().installed);
    let Liveness::Up(status) = overview.liveness else {
        panic!("router not up");
    };
    assert_eq!(status.config_generation, 1);

    // A change keeps the token (auth.json holds it) and reloads in place.
    // `listen_port` 0 in an update keeps the port Codex is pointed at.
    let mut changed = cfg.clone();
    changed.members[0].websocket = true;
    let again = manager
        .update(&changed, keys(), Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(again.token, endpoint.token);
    assert_eq!(again.port, endpoint.port);
    let Liveness::Up(status) = watchdog::check(manager.paths()).await else {
        panic!("router not up after reload");
    };
    assert_eq!(status.config_generation, 2);
    assert!(status.members[0].websocket);

    // Leaving router mode: fallback first (router still up), then uninstall.
    let fallback = Fallback {
        paths: manager.paths().clone(),
        log: log.clone(),
    };
    manager.disable(&fallback).await.unwrap();
    assert_eq!(
        *log.lock().unwrap(),
        ["install", "fallback Disabled router_up=true", "uninstall"]
    );
    assert!(!manager.paths().secrets_json().exists());
    assert!(!manager.paths().runtime_json().exists());
    assert!(manager.paths().config_json().exists());
    assert!(!watchdog::check(manager.paths()).await.is_up());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reinstalling_a_stopped_router_keeps_its_port_and_token() {
    let _serial = SERIAL.lock().await;
    let up = upstream(Mode::Json(200, r#"{"ok":true}"#)).await;
    let dir = tempfile::tempdir().unwrap();
    let data = DataDir::new(dir.path());
    let service = InProcess::default();
    let log = service.log.clone();
    let manager = RouterManager::new(&data, fake_exe(dir.path()), Box::new(service));
    let cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", up.base("/backend-api/codex"))],
    )]);
    let first = manager
        .enable(&cfg, keys(), Duration::from_secs(10))
        .await
        .unwrap();

    // The router goes away (crash, reboot): Codex still points at its port.
    manager.uninstall_service().unwrap();
    assert!(!watchdog::check(manager.paths()).await.is_up());

    // Rewriting the members while it is down never starts it.
    let mut changed = cfg.clone();
    changed.members[0].websocket = true;
    let rewritten = manager.rewrite(&changed, keys()).await.unwrap();
    assert_eq!(rewritten.port, first.port);
    assert!(!watchdog::check(manager.paths()).await.is_up());
    assert_eq!(*log.lock().unwrap(), ["install", "uninstall"]);

    // Install / Restart (listen_port 0) brings it back on the same port.
    let again = manager
        .enable(&changed, keys(), Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(again.port, first.port, "Codex's config.toml keeps working");
    assert_eq!(again.token, first.token, "Codex's auth.json keeps working");
    let Liveness::Up(status) = watchdog::check(manager.paths()).await else {
        panic!("router not up after the reinstall");
    };
    assert!(status.members[0].websocket);
    manager.uninstall_service().unwrap();
}

/// Another program listening on a port: it answers every request with a 404
/// (not a Superba router), until dropped.
struct ForeignProgram {
    port: u16,
    task: tokio::task::JoinHandle<()>,
}

impl ForeignProgram {
    async fn on(port: u16) -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .unwrap();
        let task = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = stream.read(&mut buf).await;
                    let _ = stream
                        .write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                        .await;
                });
            }
        });
        Self { port, task }
    }
}

impl Drop for ForeignProgram {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn stored_port(manager: &RouterManager) -> u16 {
    superba_router::RouterConfig::load(manager.paths())
        .unwrap()
        .listen_port
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_first_configuration_gets_the_stable_port_of_its_data_directory() {
    use superba_router::manager::port_seed;
    use superba_router::port::{STABLE_PORTS, ephemeral_ranges, pick};
    let _serial = SERIAL.lock().await;
    let up = upstream(Mode::Json(200, r#"{"ok":true}"#)).await;
    let dir = tempfile::tempdir().unwrap();
    let data = DataDir::new(dir.path());
    let manager = RouterManager::new(&data, fake_exe(dir.path()), Box::new(InProcess::default()));
    let cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", up.base("/backend-api/codex"))],
    )]);
    let expected = pick(&port_seed(dir.path()), &[], &[]).unwrap();
    let endpoint = manager
        .enable(&cfg, keys(), Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(endpoint.port, expected, "the data directory's port");
    assert!(STABLE_PORTS.contains(&endpoint.port));
    assert!(
        !ephemeral_ranges()
            .iter()
            .any(|r| r.contains(&endpoint.port)),
        "outside the OS ephemeral range"
    );
    assert_eq!(stored_port(&manager), endpoint.port, "persisted");
    manager.uninstall_service().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_router_whose_port_another_program_took_moves_and_keeps_the_new_port() {
    use superba_router::port::STABLE_PORTS;
    let _serial = SERIAL.lock().await;
    let up = upstream(Mode::Json(200, r#"{"ok":true}"#)).await;
    let dir = tempfile::tempdir().unwrap();
    let data = DataDir::new(dir.path());
    let service = InProcess::default();
    let log = service.log.clone();
    let manager = RouterManager::new(&data, fake_exe(dir.path()), Box::new(service));
    let cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", up.base("/backend-api/codex"))],
    )]);
    let first = manager
        .enable(&cfg, keys(), Duration::from_secs(10))
        .await
        .unwrap();

    // Nothing to move while the router serves its port, or while the port is
    // free after it stopped (the service manager brings it back).
    assert_eq!(manager.port_to_leave().await.unwrap(), None);
    manager.uninstall_service().unwrap();
    assert_eq!(manager.port_to_leave().await.unwrap(), None);
    assert_eq!(
        manager
            .move_off_taken_port(Duration::from_secs(10))
            .await
            .unwrap()
            .map(|e| e.port),
        None
    );

    // The computer restarts and another program gets the port first.
    let foreign = ForeignProgram::on(first.port).await;
    let (old, new) = manager.port_to_leave().await.unwrap().expect("a move");
    assert_eq!(old, first.port);
    assert_ne!(new, first.port);
    assert!(STABLE_PORTS.contains(&new));
    let moved = manager
        .move_off_taken_port(Duration::from_secs(10))
        .await
        .unwrap()
        .expect("moved");
    assert_eq!(moved.port, new);
    assert_eq!(moved.token, first.token, "Codex keeps its router token");
    assert_eq!(
        moved.base_url.as_str(),
        format!("http://127.0.0.1:{new}/backend-api/codex")
    );
    assert_eq!(stored_port(&manager), new, "persisted");
    let Liveness::Up(status) = watchdog::check(manager.paths()).await else {
        panic!("router not up on its new port");
    };
    assert_eq!(status.listen, format!("127.0.0.1:{new}"));
    assert_eq!(*log.lock().unwrap(), ["install", "uninstall", "restart"]);

    // The other program goes away: the router stays on its new port across
    // restarts and reinstalls (Codex now points there).
    drop(foreign);
    manager.uninstall_service().unwrap();
    let again = manager
        .enable(&cfg, keys(), Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(again.port, new, "not back to {}", first.port);
    manager.uninstall_service().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_or_restart_moves_off_a_port_another_program_took() {
    let _serial = SERIAL.lock().await;
    let up = upstream(Mode::Json(200, r#"{"ok":true}"#)).await;
    let dir = tempfile::tempdir().unwrap();
    let data = DataDir::new(dir.path());
    let manager = RouterManager::new(&data, fake_exe(dir.path()), Box::new(InProcess::default()));
    let cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", up.base("/backend-api/codex"))],
    )]);
    let first = manager
        .enable(&cfg, keys(), Duration::from_secs(10))
        .await
        .unwrap();
    manager.uninstall_service().unwrap();
    let foreign = ForeignProgram::on(first.port).await;
    // Install / Restart (listen_port 0): a router that could not bind its
    // port would only crash-loop; it moves instead, keeping its token.
    let again = manager
        .enable(&cfg, keys(), Duration::from_secs(10))
        .await
        .unwrap();
    assert_ne!(again.port, foreign.port);
    assert_eq!(again.token, first.token);
    assert_eq!(stored_port(&manager), again.port);
    assert!(watchdog::check(manager.paths()).await.is_up());
    // A members rewrite keeps the new port.
    let rewritten = manager.rewrite(&cfg, keys()).await.unwrap();
    assert_eq!(rewritten.port, again.port);
    manager.uninstall_service().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enable_times_out_when_the_service_never_starts() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let data = DataDir::new(dir.path());
    let service = InProcess {
        broken: true,
        ..InProcess::default()
    };
    let manager = RouterManager::new(&data, fake_exe(dir.path()), Box::new(service));
    let cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", dead_base())],
    )]);
    let err = manager
        .enable(&cfg, keys(), Duration::from_millis(600))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Timeout { .. }), "{err:?}");
    assert!(err.to_string().contains("not running"), "{err}");
}

#[tokio::test]
async fn write_config_refuses_missing_keys_and_the_official_login() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let data = DataDir::new(dir.path());
    let manager = RouterManager::new(
        &data,
        "/opt/superba-router".into(),
        Box::new(InProcess::default()),
    );
    let cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", dead_base())],
    )]);
    assert!(matches!(
        manager.write_config(&cfg, BTreeMap::new()).unwrap_err(),
        Error::Secret { .. }
    ));
    let mut official = cfg.clone();
    official.members[0].lines[0].base_url =
        "https://chatgpt.com/backend-api/codex".parse().unwrap();
    assert!(manager.write_config(&official, keys()).is_err());
    assert!(!manager.paths().config_json().exists());
}

#[tokio::test]
async fn enable_refuses_a_missing_router_program_before_writing_anything() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let data = DataDir::new(dir.path());
    let service = InProcess::default();
    let log = service.log.clone();
    let manager = RouterManager::new(
        &data,
        dir.path().join("missing").join("superba-router"),
        Box::new(service),
    );
    let cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", dead_base())],
    )]);
    let err = manager
        .enable(&cfg, keys(), Duration::from_millis(600))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotFound { .. }), "{err:?}");
    assert!(log.lock().unwrap().is_empty(), "no service installed");
    assert!(!manager.paths().secrets_json().exists(), "no keys written");
}

#[test]
fn the_router_program_is_found_in_every_release_layout() {
    use superba_router::manager::{
        find_router_exe, is_transient_location, router_exe_candidates, router_exe_name,
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let name = router_exe_name();

    // Archives and cargo target dirs: next to superba / spb.
    let archive = root.join("superba-0.2.0-linux-x86_64");
    std::fs::create_dir_all(&archive).unwrap();
    std::fs::write(archive.join(&name), b"").unwrap();
    assert_eq!(find_router_exe(&archive.join("spb")), archive.join(&name));

    // A symlink to spb elsewhere resolves to the archive.
    #[cfg(unix)]
    {
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(archive.join("spb"), b"").unwrap();
        std::os::unix::fs::symlink(archive.join("spb"), bin.join("spb")).unwrap();
        assert_eq!(find_router_exe(&bin.join("spb")), archive.join(&name));
    }

    // PATH directories come last.
    let other = root.join("elsewhere");
    let on_path = root.join("path-dir");
    let candidates = router_exe_candidates(
        &other.join("spb"),
        Some(std::ffi::OsStr::new(on_path.to_str().unwrap())),
        Some(&root),
    );
    assert_eq!(candidates.first(), Some(&other.join(&name)));
    assert_eq!(candidates.last(), Some(&on_path.join(&name)));
    if cfg!(target_os = "macos") {
        // The release ZIP / DMG: spb next to Superba.app.
        assert!(candidates.contains(&other.join("Superba.app/Contents/MacOS").join(&name)));
        assert!(candidates.contains(
            &std::path::PathBuf::from("/Applications/Superba.app/Contents/MacOS").join(&name)
        ));
    }

    // Nothing found: the expected path next to the binary, for the error (unless
    // the machine running the tests has a router on its PATH).
    let on_real_path = std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(&name).is_file()));
    if !on_real_path && !cfg!(target_os = "macos") {
        assert_eq!(find_router_exe(&other.join("spb")), other.join(&name));
    }

    assert!(is_transient_location(std::path::Path::new(
        "/Volumes/Superba 0.2.0/Superba.app/Contents/MacOS/superba-router"
    )));
    assert!(is_transient_location(std::path::Path::new(
        "/private/var/folders/x/AppTranslocation/ABC/d/Superba.app/Contents/MacOS/superba-router"
    )));
    assert!(!is_transient_location(std::path::Path::new(
        "/Applications/Superba.app/Contents/MacOS/superba-router"
    )));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configure_sets_the_stats_section_and_member_rewrites_keep_it() {
    use superba_router::{RouterConfig, StatsConfig, StatsLineConfig};
    let _serial = SERIAL.lock().await;

    let up = upstream(Mode::Json(200, r#"{"ok":true}"#)).await;
    let dir = tempfile::tempdir().unwrap();
    let data = DataDir::new(dir.path());
    let service = InProcess::default();
    let manager = RouterManager::new(&data, fake_exe(dir.path()), Box::new(service));

    // No configuration yet.
    let err = manager.configure(StatsConfig::default()).await.unwrap_err();
    assert!(matches!(err, Error::NotFound { .. }), "{err:?}");

    let cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", up.base("/backend-api/codex"))],
    )]);
    assert!(cfg.stats.is_none());
    manager
        .enable(&cfg, keys(), Duration::from_secs(10))
        .await
        .unwrap();
    // v0.3 writes the defaults: recording on, no stats line.
    let stored = RouterConfig::load(manager.paths()).unwrap();
    assert_eq!(stored.stats, Some(StatsConfig::default()));
    let Liveness::Up(status) = watchdog::check(manager.paths()).await else {
        panic!("router not up");
    };
    assert!(status.stats.unwrap().recording);

    let stats = StatsConfig {
        line: Some(StatsLineConfig {
            template: "⚡ {tps} tok/s".into(),
            style: Default::default(),
            originators: vec!["Codex Desktop".into()],
        }),
        ..StatsConfig::default()
    };
    assert!(manager.configure(stats.clone()).await.unwrap(), "reloaded");
    let Liveness::Up(status) = watchdog::check(manager.paths()).await else {
        panic!("router not up");
    };
    assert_eq!(status.config_generation, 2);
    assert!(
        !manager.configure(stats.clone()).await.unwrap(),
        "unchanged"
    );

    // A member rewrite without stats keeps the stored section.
    let mut changed = cfg.clone();
    changed.members[0].websocket = true;
    manager
        .update(&changed, keys(), Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(
        RouterConfig::load(manager.paths()).unwrap().stats,
        Some(stats.clone())
    );

    // Out-of-range values are refused before anything is written.
    let mut bad = stats.clone();
    bad.max_rows = 0;
    assert!(manager.configure(bad).await.is_err());
    assert_eq!(
        RouterConfig::load(manager.paths()).unwrap().stats,
        Some(stats)
    );
    manager.uninstall_service().unwrap();
}
