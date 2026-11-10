//! Withdrawn plain-HTTP consent against a real `RouterManager` whose service
//! runs the real router in this process (as in superba-router's manager
//! tests): the site is saved as unconfirmed only once the running router no
//! longer sends its key over plain HTTP. Covers a reload a health check would
//! have skipped while the router serves, and a reload the router refuses
//! followed by a second attempt for a provider the settings no longer list.
#![allow(missing_docs)]
mod fakes;
#[path = "../../superba-net/tests/support/mod.rs"]
mod upstream;

use std::sync::{Arc, Mutex, mpsc};

use fakes::{World, krill_profile};
use serde_json::json;
use superba_core::{
    AuthMode, DataDir, LineProber, ProfileId, ResolvedEndpoints, Result, RouterInstallRequest,
    Secret, Site, SwitchService,
};
use superba_router::RouterServer;
use superba_router::ShutdownHandle;
use superba_router::config::RouterConfig;
use superba_router::manager::RouterManager;
use superba_router::paths::RouterPaths;
use superba_router::service::{ServiceKind, ServiceManager, ServiceSpec, ServiceStatus};
use superba_switch::router::SystemRouterControl;
use superba_switch::{Switcher, Tuning};
use upstream::{MockResponse, MockServer, RecordedRequest};

/// A model no catalog lists.
const MODEL: &str = "lan-test-model";

/// One test at a time: each binds the stable port of its data directory.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Runs the router from the files the manager wrote, in a thread of this
/// process; "running" for as long as it serves.
#[derive(Debug, Default)]
struct InProcess {
    running: Mutex<Option<(ShutdownHandle, std::thread::JoinHandle<()>)>>,
}

impl InProcess {
    fn start(&self, spec: &ServiceSpec) {
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
        self.stop();
        self.start(spec);
        self.status(spec)
    }
    fn uninstall(&self, _: &ServiceSpec) -> Result<()> {
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
        self.stop();
        self.start(spec);
        Ok(())
    }
}

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

fn serve(r: &RecordedRequest) -> MockResponse {
    match r.path.as_str() {
        // Listing no model keeps the router's merged catalog out of the way:
        // an unlisted model goes to the members in their order.
        "/v1/models" => MockResponse::json(200, json!({"data": []})),
        "/v1/responses" => MockResponse::sse(upstream::COMPLETED_SSE),
        _ => MockResponse::empty(404),
    }
}

/// Requests that carried a key.
fn keyed(up: &MockServer) -> usize {
    up.recorded()
        .iter()
        .filter(|r| r.header("authorization").is_some())
        .count()
}

fn responses(up: &MockServer) -> usize {
    up.recorded()
        .iter()
        .filter(|r| r.path == "/v1/responses")
        .count()
}

struct Setup {
    _dir: tempfile::TempDir,
    w: World,
    sw: Switcher,
    paths: RouterPaths,
    lan_up: MockServer,
    local_up: MockServer,
    lan: Site,
    prober: Arc<superba_net::HttpLineProber>,
    router: ResolvedEndpoints,
    token: Secret,
}

impl Setup {
    /// A LAN site with consent and a loopback site, both router members, with
    /// the router installed and serving.
    async fn new() -> Self {
        let lan_up = MockServer::start_off_loopback(serve).await;
        let local_up = MockServer::start(serve).await;
        let lan_base = url::Url::parse(&format!("{}/v1", lan_up.origin())).unwrap();
        let local_base = url::Url::parse(&format!("{}/v1", local_up.origin())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("router")).unwrap();
        let data = DataDir::new(dir.path().join("router"));
        let manager =
            RouterManager::new(&data, fake_exe(dir.path()), Box::new(InProcess::default()));
        let paths = manager.paths().clone();
        let w = World::new();
        *w.app.session.lock().unwrap() = None;
        let prober = Arc::new(upstream::prober());
        let mut deps = w.deps();
        deps.prober = prober.clone();
        deps.router = Arc::new(SystemRouterControl::with_manager(manager));
        let sw = Switcher::with_tuning(deps, Tuning::default());
        let mut lan = superba_switch::new_compatible_site("lan".into(), "LAN", &lan_base).unwrap();
        lan.allow_plain_http = true;
        let local =
            superba_switch::new_compatible_site("local".into(), "Local", &local_base).unwrap();
        for site in [&lan, &local] {
            sw.save_site(site.clone()).await.unwrap();
            let mut profile = krill_profile(site.id.as_str(), None, AuthMode::ApiKey);
            profile.site_id = site.id.clone();
            profile.model.model = Some(MODEL.into());
            sw.save_profile(profile, Some(Secret::new(upstream::KEY)))
                .await
                .unwrap();
        }
        sw.router_install(RouterInstallRequest {
            members: vec!["lan".into(), "local".into()],
        })
        .await
        .unwrap();
        let endpoint = sw.deps().router.endpoint().unwrap();
        let router = ResolvedEndpoints::for_base_url(&endpoint.base_url).unwrap();
        Self {
            _dir: dir,
            w,
            sw,
            paths,
            lan_up,
            local_up,
            lan,
            prober,
            router,
            token: endpoint.token,
        }
    }

    async fn send(&self) {
        let probe = self
            .prober
            .probe_responses(&self.router, &self.token, MODEL)
            .await
            .unwrap();
        assert!(probe.ok, "{probe:?} via {}", self.router.openai_base_url);
    }

    fn revoked(&self) -> Site {
        let mut site = self.lan.clone();
        site.allow_plain_http = false;
        site
    }

    async fn stored_consent(&self) -> bool {
        self.sw
            .sites()
            .await
            .unwrap()
            .iter()
            .find(|s| s.id == self.lan.id)
            .unwrap()
            .allow_plain_http
    }

    fn stored_members(&self) -> Vec<String> {
        RouterConfig::load(&self.paths)
            .unwrap()
            .members
            .iter()
            .map(|m| m.id.clone())
            .collect()
    }

    /// After a successful revocation: the persisted configuration keeps only
    /// the loopback member and no further key reaches the LAN upstream.
    async fn assert_lan_is_out(&self) {
        assert!(!self.stored_consent().await);
        assert_eq!(self.stored_members(), ["local"]);
        let lan_keyed = keyed(&self.lan_up);
        let local_before = responses(&self.local_up);
        for _ in 0..3 {
            self.send().await;
        }
        assert_eq!(
            keyed(&self.lan_up),
            lan_keyed,
            "{:?}",
            self.lan_up.recorded()
        );
        assert_eq!(responses(&self.local_up), local_before + 3);
    }

    /// Stop the in-process router before the data directory goes away.
    async fn shutdown(self) {
        self.sw.deps().router.uninstall().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reload_a_health_check_would_skip_does_not_count_as_done() {
    let _serial = SERIAL.lock().await;
    let s = Setup::new().await;
    s.send().await;
    assert_eq!(responses(&s.lan_up), 1, "the LAN member is first");
    // The router serves, but its runtime file is gone: the health check says
    // down, so a plain rewrite skipped the reload and reported success.
    let runtime = s.paths.runtime_json();
    let saved_runtime = std::fs::read(&runtime).unwrap();
    std::fs::remove_file(&runtime).unwrap();
    assert!(s.sw.save_site(s.revoked()).await.is_err());
    assert!(
        s.stored_consent().await,
        "nothing saved while the router may forward"
    );
    assert_eq!(
        s.stored_members(),
        ["lan", "local"],
        "the files are put back"
    );
    // The router still serves the consented site as before.
    s.send().await;
    assert_eq!(responses(&s.lan_up), 2);
    // Reachable again: the revocation goes through and takes effect.
    std::fs::write(&runtime, saved_runtime).unwrap();
    s.sw.save_site(s.revoked()).await.unwrap();
    s.assert_lan_is_out().await;
    s.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_reload_then_a_second_attempt_for_an_unlisted_member_saves_nothing() {
    let _serial = SERIAL.lock().await;
    let s = Setup::new().await;
    s.send().await;
    assert_eq!(responses(&s.lan_up), 1);
    // The stored port no longer matches the running router: its reload refuses
    // the new configuration.
    let config_file = s.paths.config_json();
    let original = std::fs::read(&config_file).unwrap();
    let mut tampered = RouterConfig::load(&s.paths).unwrap();
    tampered.listen_port += 1;
    tampered.save(&s.paths).unwrap();
    assert!(s.sw.save_site(s.revoked()).await.is_err());
    assert!(s.stored_consent().await);
    assert_eq!(
        s.stored_members(),
        ["lan", "local"],
        "the files are put back"
    );
    // Second attempt for a provider the settings do not list (as one only
    // Claude Code or a tool routes): the running router still has it, so
    // nothing is saved either.
    {
        let mut set = s.w.profiles.set.lock().unwrap();
        set.settings
            .router
            .members
            .retain(|id| id != &ProfileId::new("lan"));
    }
    // And the file no longer has it either (as after a crash between the
    // write and the reload): only the running router still routes it.
    tampered.members.retain(|m| m.id != "lan");
    tampered.save(&s.paths).unwrap();
    assert!(s.sw.save_site(s.revoked()).await.is_err());
    assert!(s.stored_consent().await);
    s.send().await;
    assert_eq!(
        responses(&s.lan_up),
        2,
        "consent still stands, so does the route"
    );
    // The port is right again: the revocation reaches the running router.
    std::fs::write(&config_file, original).unwrap();
    s.sw.save_site(s.revoked()).await.unwrap();
    s.assert_lan_is_out().await;
    s.shutdown().await;
}
