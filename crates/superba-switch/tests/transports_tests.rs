//! Line transport checks in the service (`line_transports`): cached per line
//! and key, re-checked when missing, stale or asked for, and handed to the
//! local router as per-line zstd / WebSocket hints.

mod fakes;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use fakes::*;
use pretty_assertions::assert_eq;
use superba_core::{
    AccountSummary, AuthApiHealth, AuthMode, Clock, Error, KeyCheck, Line, LineCaps, LineProber,
    LineTransportsRequest, ModelInfo, ModelList, ModelListRequest, Origin, ProfileId,
    ResolvedEndpoints, ResponsesProbe, Result, RouterInstallRequest, Secret, Site, SwitchService,
    TransportCheck, TransportProbe, TransportRefresh, TransportSupport, UsageQuery, UsageReport,
};
use superba_router::config::RouterConfig;
use superba_router::watchdog::Liveness;
use superba_switch::Switcher;
use superba_switch::router::{RouterControl, RouterEndpointInfo, RouterServiceInfo};
use url::Url;

/// Answers only transport probes: per host, in call order.
#[derive(Debug, Default)]
struct TransportProber {
    /// Host -> (WebSocket, zstd).
    answers: Mutex<BTreeMap<String, (TransportSupport, TransportSupport)>>,
    /// Base URLs probed, in order.
    probed: Mutex<Vec<String>>,
}

#[async_trait]
impl LineProber for TransportProber {
    async fn probe(&self, _: &Site, _: &Line) -> Result<LineCaps> {
        Err(Error::not_implemented("probe"))
    }
    async fn probe_all(&self, _: &Site) -> Result<Vec<LineCaps>> {
        Err(Error::not_implemented("probe_all"))
    }
    async fn check_key(&self, _: &Site, _: &Line, _: AuthMode, _: &Secret) -> Result<KeyCheck> {
        Err(Error::not_implemented("check_key"))
    }
    async fn check_authapi(&self, _: &Site, _: &Origin, _: &Secret) -> Result<AuthApiHealth> {
        Err(Error::not_implemented("check_authapi"))
    }
    async fn list_models(&self, _: &Url, _: &Secret) -> Result<Vec<ModelInfo>> {
        Err(Error::not_implemented("list_models"))
    }
    async fn model_list(&self, _: ModelListRequest<'_>) -> Result<ModelList> {
        Err(Error::not_implemented("model_list"))
    }
    async fn probe_responses(
        &self,
        _: &ResolvedEndpoints,
        _: &Secret,
        _: &str,
    ) -> Result<ResponsesProbe> {
        Err(Error::not_implemented("probe_responses"))
    }
    async fn account_summary(
        &self,
        _: &Site,
        _: &Line,
        _: &Secret,
    ) -> Result<Option<AccountSummary>> {
        Err(Error::not_implemented("account_summary"))
    }
    async fn usage_report(
        &self,
        _: &Site,
        _: &Line,
        _: &Secret,
        _: &UsageQuery,
    ) -> Result<UsageReport> {
        Err(Error::not_implemented("usage_report"))
    }
    async fn probe_transports(
        &self,
        endpoints: &ResolvedEndpoints,
        key: &Secret,
    ) -> Result<TransportProbe> {
        assert_eq!(key.expose(), KEY, "the provider's own key");
        let base = endpoints.openai_base_url.clone();
        self.probed.lock().unwrap().push(base.to_string());
        let host = base.host_str().unwrap_or_default().to_owned();
        let (ws, zstd) = self
            .answers
            .lock()
            .unwrap()
            .get(&host)
            .copied()
            .unwrap_or((TransportSupport::Unknown, TransportSupport::Unknown));
        Ok(TransportProbe {
            websocket: TransportCheck::new(ws, Some(101), "ws"),
            zstd: TransportCheck::new(zstd, Some(400), "zstd"),
        })
    }
}

/// Keeps every configuration written to the router.
#[derive(Debug, Default)]
struct CaptureRouter {
    configs: Mutex<Vec<RouterConfig>>,
}

impl CaptureRouter {
    fn info(&self) -> Result<RouterEndpointInfo> {
        if self.configs.lock().unwrap().is_empty() {
            return Err(Error::NotFound {
                what: "router configuration".into(),
            });
        }
        Ok(RouterEndpointInfo {
            base_url: Url::parse("http://127.0.0.1:47000/backend-api/codex").unwrap(),
            token: Secret::new(ROUTER_TOKEN),
            port: 47000,
        })
    }
    fn keep(&self, config: RouterConfig) -> Result<RouterEndpointInfo> {
        self.configs.lock().unwrap().push(config);
        self.info()
    }
    fn last(&self) -> RouterConfig {
        self.configs
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("a config")
    }
}

#[async_trait]
impl RouterControl for CaptureRouter {
    fn endpoint(&self) -> Result<RouterEndpointInfo> {
        self.info()
    }
    async fn install(
        &self,
        config: RouterConfig,
        _: BTreeMap<String, Secret>,
    ) -> Result<RouterEndpointInfo> {
        self.keep(config)
    }
    async fn update(
        &self,
        config: RouterConfig,
        _: BTreeMap<String, Secret>,
    ) -> Result<RouterEndpointInfo> {
        self.keep(config)
    }
    async fn rewrite(
        &self,
        config: RouterConfig,
        _: BTreeMap<String, Secret>,
    ) -> Result<RouterEndpointInfo> {
        self.keep(config)
    }
    async fn uninstall(&self) -> Result<()> {
        Ok(())
    }
    async fn overview(&self) -> (RouterServiceInfo, Liveness) {
        (
            RouterServiceInfo::default(),
            Liveness::Down {
                detail: "test".into(),
            },
        )
    }
    async fn liveness(&self) -> Liveness {
        Liveness::Down {
            detail: "test".into(),
        }
    }
}

fn setup() -> (World, Arc<TransportProber>, Arc<CaptureRouter>, Switcher) {
    let env = World::new();
    let prober = Arc::new(TransportProber::default());
    let router = Arc::new(CaptureRouter::default());
    let mut deps = env.deps();
    deps.prober = prober.clone();
    deps.router = router.clone();
    (env, prober, router, Switcher::new(deps))
}

fn work() -> ProfileId {
    ProfileId::new("work")
}

fn host(url: &str) -> String {
    Url::parse(url).unwrap().host_str().unwrap().to_owned()
}

#[tokio::test]
async fn checks_are_cached_per_line_and_refreshed_when_missing_stale_or_asked() {
    let (env, prober, _router, svc) = setup();
    let cached = LineTransportsRequest::cached(work());
    // Nothing checked yet, nothing sent.
    assert_eq!(svc.line_transports(cached.clone()).await.unwrap(), vec![]);
    assert!(prober.probed.lock().unwrap().is_empty());

    let first = svc
        .line_transports(cached.clone().with_refresh(TransportRefresh::Missing))
        .await
        .unwrap();
    let probed = prober.probed.lock().unwrap().clone();
    assert!(!first.is_empty());
    assert_eq!(probed.len(), first.len(), "every line once");
    for url in &probed {
        assert!(
            url.ends_with("/backend-api/codex"),
            "{url}: the router's base"
        );
    }
    // The profile's primary line comes first.
    assert_eq!(first[0].line_id.as_str(), "overseas");
    assert!(first.iter().all(|r| r.checked_at == env.clock.now()));

    // Cached: the same results, no request.
    assert_eq!(svc.line_transports(cached.clone()).await.unwrap(), first);
    let again = svc
        .line_transports(cached.clone().with_refresh(TransportRefresh::Missing))
        .await
        .unwrap();
    assert_eq!(again, first);
    assert_eq!(prober.probed.lock().unwrap().len(), probed.len());

    // A day later they are stale and checked again; "All" always checks.
    env.clock.advance(Duration::from_secs(25 * 60 * 60));
    svc.line_transports(cached.clone().with_refresh(TransportRefresh::Missing))
        .await
        .unwrap();
    assert_eq!(prober.probed.lock().unwrap().len(), probed.len() * 2);
    let one = LineTransportsRequest {
        profile: work(),
        lines: vec![first[0].line_id.clone()],
        refresh: TransportRefresh::All,
    };
    let only = svc.line_transports(one).await.unwrap();
    assert_eq!(only.len(), 1);
    assert_eq!(prober.probed.lock().unwrap().len(), probed.len() * 2 + 1);

    // The cache file holds no key.
    let file = env.ctx.data_dir.cache_dir().join("transports.json");
    let text = std::fs::read_to_string(file).unwrap();
    assert!(!text.contains(KEY));
    // Unknown profiles are an error.
    assert!(
        svc.line_transports(LineTransportsRequest::cached(ProfileId::new("nope")))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn router_members_get_the_lines_hints() {
    let (_env, prober, router, svc) = setup();
    // Not checked: Krill keeps WebSocket, no line is compressed for.
    svc.router_install(RouterInstallRequest {
        members: vec![work()],
    })
    .await
    .ok();
    let cfg = router.last();
    let m = &cfg.members[0];
    assert!(m.websocket);
    assert!(
        m.lines
            .iter()
            .all(|l| l.zstd.is_none() && l.websocket.is_none())
    );

    // Check: the first line upgrades and decodes zstd, the others refuse
    // WebSocket and do not tell about zstd.
    let first_host = host(m.lines[0].base_url.as_str());
    {
        let mut a = prober.answers.lock().unwrap();
        for l in &m.lines {
            a.insert(
                host(l.base_url.as_str()),
                (TransportSupport::Unsupported, TransportSupport::Unknown),
            );
        }
        a.insert(
            first_host.clone(),
            (TransportSupport::Supported, TransportSupport::Supported),
        );
    }
    let before = router.configs.lock().unwrap().len();
    svc.line_transports(LineTransportsRequest::cached(work()).with_refresh(TransportRefresh::All))
        .await
        .unwrap();
    // A member's new hints are written to the router right away.
    assert_eq!(router.configs.lock().unwrap().len(), before + 1);
    let cfg = router.last();
    let m = &cfg.members[0];
    assert_eq!(m.lines[0].zstd, Some(true));
    assert_eq!(m.lines[0].websocket, Some(true));
    for l in &m.lines[1..] {
        assert_eq!(l.zstd, None, "{}", l.id);
        assert_eq!(l.websocket, Some(false), "{}", l.id);
    }
    assert!(m.websocket, "one line still upgrades");

    // Every line refuses WebSocket: the member is refused locally (426).
    prober.answers.lock().unwrap().insert(
        first_host,
        (TransportSupport::Unsupported, TransportSupport::Supported),
    );
    svc.line_transports(LineTransportsRequest::cached(work()).with_refresh(TransportRefresh::All))
        .await
        .unwrap();
    let cfg = router.last();
    assert!(!cfg.members[0].websocket);
    assert_eq!(cfg.members[0].lines[0].zstd, Some(true));
    // Unchanged results do not rewrite the router again.
    let n = router.configs.lock().unwrap().len();
    svc.line_transports(LineTransportsRequest::cached(work()).with_refresh(TransportRefresh::All))
        .await
        .unwrap();
    assert_eq!(router.configs.lock().unwrap().len(), n);
}
