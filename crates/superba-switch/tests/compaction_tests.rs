//! Remote compaction in the service (GitHub issue 3): the check is cached per
//! provider and key and repeated only when asked (or stale), the profile's
//! choice and Krill's fact decide the effective value, router members carry
//! it, and switch plans warn about providers that cannot compact and about
//! conversations compacted by a provider on another site — without sending
//! anything while planning.

mod fakes;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use fakes::*;
use pretty_assertions::assert_eq;
use superba_core::{
    AccountSummary, AuthApiHealth, AuthMode, Clock, CompactionCheck, CompactionChoice,
    CompactionRequest, CompactionSource, CompactionSupport, Error, GenerationRecord, KeyCheck,
    Line, LineCaps, LineProber, ModelInfo, ModelList, ModelListRequest, Origin, ProfileId,
    ResolvedEndpoints, ResponsesProbe, Result, Secret, Site, SiteId, SwitchPlan, SwitchService,
    Timestamp, TransportRefresh, UsageQuery, UsageReport,
};
use superba_router::config::RouterConfig;
use superba_router::watchdog::Liveness;
use superba_switch::Switcher;
use superba_switch::router::{RouterControl, RouterEndpointInfo, RouterServiceInfo};
use url::Url;

/// Answers model lists and compaction checks only.
#[derive(Debug)]
struct CompactionProber {
    answer: Mutex<CompactionSupport>,
    /// (base URL, model) of every check.
    checks: Mutex<Vec<(String, String)>>,
}

impl Default for CompactionProber {
    fn default() -> Self {
        Self {
            answer: Mutex::new(CompactionSupport::Native),
            checks: Mutex::default(),
        }
    }
}

#[async_trait]
impl LineProber for CompactionProber {
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
        Ok(vec![ModelInfo {
            id: "gpt-listed".into(),
            ..ModelInfo::default()
        }])
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
    async fn probe_compaction(
        &self,
        endpoints: &ResolvedEndpoints,
        key: &Secret,
        model: &str,
    ) -> Result<CompactionCheck> {
        assert_eq!(key.expose(), KEY, "the provider's own key");
        self.checks
            .lock()
            .unwrap()
            .push((endpoints.openai_base_url.to_string(), model.to_owned()));
        let s = *self.answer.lock().unwrap();
        Ok(CompactionCheck::new(s, Some(200), "checked"))
    }
}

fn setup() -> (World, Arc<CompactionProber>, Switcher) {
    let w = World::new();
    let prober = Arc::new(CompactionProber::default());
    let mut deps = w.deps();
    deps.prober = prober.clone();
    (w, prober, Switcher::new(deps))
}

fn work() -> ProfileId {
    ProfileId::new("work")
}

#[tokio::test]
async fn checks_are_cached_and_repeated_only_when_asked_or_stale() {
    let (w, prober, svc) = setup();
    let cached = CompactionRequest::cached(work());
    // Never checked: Krill is native by Superba's fact, nothing is sent.
    let r = svc.provider_compaction(cached.clone()).await.unwrap();
    assert_eq!(r.support, CompactionSupport::Native);
    assert_eq!(r.source, CompactionSource::Builtin);
    assert_eq!(r.checked_at, None);
    assert!(prober.checks.lock().unwrap().is_empty());

    // Missing: one check at the router's base URL of the first line, with
    // the provider's listed model.
    let r = svc
        .provider_compaction(cached.clone().with_refresh(TransportRefresh::Missing))
        .await
        .unwrap();
    assert_eq!(r.source, CompactionSource::Check);
    assert_eq!(r.checked_at, Some(w.clock.now()));
    assert_eq!(r.model.as_deref(), Some("gpt-listed"));
    let checks = prober.checks.lock().unwrap().clone();
    assert_eq!(checks.len(), 1);
    assert!(checks[0].0.ends_with("/backend-api/codex"), "{checks:?}");
    // Cached and not stale: no new request.
    assert_eq!(svc.provider_compaction(cached.clone()).await.unwrap(), r);
    svc.provider_compaction(cached.clone().with_refresh(TransportRefresh::Missing))
        .await
        .unwrap();
    assert_eq!(prober.checks.lock().unwrap().len(), 1);
    // A week later a definite result is stale.
    w.clock.advance(Duration::from_secs(8 * 24 * 60 * 60));
    svc.provider_compaction(cached.clone().with_refresh(TransportRefresh::Missing))
        .await
        .unwrap();
    assert_eq!(prober.checks.lock().unwrap().len(), 2);
    // "All" always checks; an inconclusive answer keeps Krill's fact.
    *prober.answer.lock().unwrap() = CompactionSupport::Unknown;
    let r = svc
        .provider_compaction(cached.clone().with_refresh(TransportRefresh::All))
        .await
        .unwrap();
    assert_eq!(prober.checks.lock().unwrap().len(), 3);
    assert_eq!(r.check.unwrap().support, CompactionSupport::Unknown);
    assert_eq!(r.support, CompactionSupport::Native);
    // The cache file holds no key.
    let text = std::fs::read_to_string(w.ctx.data_dir.cache_dir().join("compaction.json")).unwrap();
    assert!(!text.contains(KEY));
    // Unknown profiles are an error.
    assert!(
        svc.provider_compaction(CompactionRequest::cached(ProfileId::new("nope")))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn the_profile_s_choice_wins_and_uses_the_profile_s_model() {
    let (w, prober, svc) = setup();
    {
        let mut set = w.profiles.set.lock().unwrap();
        let p = set.profiles.iter_mut().find(|p| p.id == work()).unwrap();
        p.compaction = CompactionChoice::Unsupported;
        p.model.model = Some("gpt-mine".into());
    }
    let r = svc
        .provider_compaction(CompactionRequest::cached(work()))
        .await
        .unwrap();
    assert_eq!(r.support, CompactionSupport::Unsupported);
    assert_eq!(r.source, CompactionSource::Override);
    // A check still runs when asked, with the profile's model, but the
    // choice keeps deciding.
    let r = svc
        .provider_compaction(CompactionRequest::cached(work()).with_refresh(TransportRefresh::All))
        .await
        .unwrap();
    assert_eq!(prober.checks.lock().unwrap()[0].1, "gpt-mine");
    assert_eq!(r.check.unwrap().support, CompactionSupport::Native);
    assert_eq!(r.support, CompactionSupport::Unsupported);
}

// ---------------------------------------------------------------------------
// Plans
// ---------------------------------------------------------------------------

fn req(profile: &str) -> superba_core::SwitchRequest {
    superba_core::SwitchRequest::profile(ProfileId::new(profile))
}

fn keys(p: &SwitchPlan) -> Vec<String> {
    p.warnings.iter().map(|w| w.text.key.clone()).collect()
}

/// A compatible site and a profile `custom` on it.
fn add_custom(w: &World) {
    let base = Url::parse("https://api.example.com/v1").unwrap();
    let site =
        superba_switch::new_compatible_site(SiteId::new("example"), "Example", &base).unwrap();
    let mut set = w.profiles.set.lock().unwrap();
    set.custom_sites.push(site);
    let mut p = krill_profile("custom", None, AuthMode::ApiKey);
    p.site_id = SiteId::new("example");
    p.name = "Example relay".into();
    set.profiles.push(p);
    drop(set);
    w.secrets
        .map
        .lock()
        .unwrap()
        .insert("profile:custom".into(), KEY.into());
}

#[tokio::test]
async fn plans_name_a_provider_that_cannot_compact_and_send_nothing() {
    let (w, prober, svc) = setup();
    // Krill: native, nothing to say.
    let p = svc.plan(req("work")).await.unwrap();
    assert!(
        !keys(&p).iter().any(|k| k.contains("compaction")),
        "{:?}",
        keys(&p)
    );
    // A compatible site never checked: a hint.
    add_custom(&w);
    let p = svc.plan(req("custom")).await.unwrap();
    assert!(
        keys(&p).contains(&"switch.warn.compaction_unknown".to_owned()),
        "{:?}",
        keys(&p)
    );
    // Marked unsupported: a warning naming it.
    {
        let mut set = w.profiles.set.lock().unwrap();
        let c = set
            .profiles
            .iter_mut()
            .find(|p| p.id.as_str() == "custom")
            .unwrap();
        c.compaction = CompactionChoice::Unsupported;
    }
    let p = svc.plan(req("custom")).await.unwrap();
    let n = p
        .warnings
        .iter()
        .find(|n| n.text.key == "switch.warn.compaction_unsupported")
        .expect("warning");
    assert_eq!(n.severity, superba_core::Severity::Warning);
    assert!(
        n.text
            .args
            .iter()
            .any(|(_, v)| v.to_string().contains("Example relay"))
    );
    assert!(
        prober.checks.lock().unwrap().is_empty(),
        "planning never checks"
    );
}

/// A rollout of `thread` compacted at T0 - 1 h (a compaction response
/// followed by the `compacted` record).
fn compacted_rollout(thread: &str) -> String {
    let at = |sec: i64| Timestamp::from_millis(T0 - 3_600_000 + sec * 1000).to_rfc3339();
    let line = |sec: i64, kind: &str, payload: serde_json::Value| {
        serde_json::json!({"timestamp": at(sec), "type": kind, "payload": payload}).to_string()
            + "\n"
    };
    let usage = serde_json::json!({"input_tokens": 9000, "cached_input_tokens": 0,
        "cache_write_input_tokens": 0, "output_tokens": 300, "reasoning_output_tokens": 0,
        "total_tokens": 9300});
    let mut s = String::new();
    s += &line(
        0,
        "session_meta",
        serde_json::json!({"id": thread, "timestamp": at(0),
        "cwd": "/w", "originator": "Codex Desktop", "cli_version": "0.162.0", "model_provider": "openai"}),
    );
    s += &line(
        1,
        "event_msg",
        serde_json::json!({"type": "task_started", "turn_id": "t1"}),
    );
    s += &line(
        1,
        "turn_context",
        serde_json::json!({"turn_id": "t1", "model": "gpt-x"}),
    );
    s += &line(
        2,
        "response_item",
        serde_json::json!({"type": "message", "role": "user", "content": []}),
    );
    s += &line(
        5,
        "token_usage_record",
        serde_json::json!({"thread_id": thread, "turn_id": "t1",
        "response_id": "resp_cmp", "usage": usage}),
    );
    s += &line(
        6,
        "compacted",
        serde_json::json!({"message": "", "compaction_response_id": "resp_cmp"}),
    );
    s += &line(
        7,
        "event_msg",
        serde_json::json!({"type": "task_complete", "turn_id": "t1"}),
    );
    s
}

#[tokio::test]
async fn conversations_compacted_on_another_site_are_counted_before_the_switch() {
    let (w, _prober, svc) = setup();
    add_custom(&w);
    let thread = "019a7c00-0000-7000-8000-0000000c0a11";
    let dir = w.ctx.codex_home().sessions_dir().join("2025/10/09");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("rollout-2025-10-09T08-00-00-{thread}.jsonl")),
        compacted_rollout(thread),
    )
    .unwrap();
    // Krill ("work") was applied when the conversation was compacted.
    *w.config.generations.lock().unwrap() = vec![GenerationRecord {
        generation: 1,
        profile_id: Some(work()),
        site_kind: None,
        applied_at: Timestamp(T0 - 7_200_000),
    }];
    w.config.state.lock().unwrap().active = Some(superba_core::ActiveState {
        profile_id: Some(work()),
        site_id: Some(SiteId::new(superba_core::catalog::KRILL_SITE_ID)),
        line_id: None,
        auth_mode: Some(AuthMode::Pat),
        generation: 1,
        applied_at: Timestamp(T0 - 7_200_000),
    });
    let p = svc.plan(req("custom")).await.unwrap();
    let n = p
        .warnings
        .iter()
        .find(|n| n.text.key == "switch.warn.compacted_elsewhere")
        .unwrap_or_else(|| panic!("{:?}", keys(&p)));
    let args: std::collections::BTreeMap<String, String> = n
        .text
        .args
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    assert_eq!(args["count"], "1");
    assert_eq!(args["provider"], "work");
    // Staying on Krill: nothing to warn about.
    let p = svc.plan(req("work")).await.unwrap();
    assert!(!keys(&p).contains(&"switch.warn.compacted_elsewhere".to_owned()));
}

// ---------------------------------------------------------------------------
// Router members
// ---------------------------------------------------------------------------

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
        _: std::collections::BTreeMap<String, Secret>,
    ) -> Result<RouterEndpointInfo> {
        self.keep(config)
    }
    async fn update(
        &self,
        config: RouterConfig,
        _: std::collections::BTreeMap<String, Secret>,
    ) -> Result<RouterEndpointInfo> {
        self.keep(config)
    }
    async fn rewrite(
        &self,
        config: RouterConfig,
        _: std::collections::BTreeMap<String, Secret>,
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

#[tokio::test]
async fn router_members_carry_the_effective_support_and_follow_a_check() {
    let w = World::new();
    add_custom(&w);
    let prober = Arc::new(CompactionProber::default());
    let router = Arc::new(CaptureRouter::default());
    let mut deps = w.deps();
    deps.prober = prober.clone();
    deps.router = router.clone();
    let svc = Switcher::new(deps);
    svc.router_install(superba_core::RouterInstallRequest {
        members: vec![work(), ProfileId::new("custom")],
    })
    .await
    .ok();
    let cfg = router.last();
    assert_eq!(
        cfg.members[0].compaction,
        CompactionSupport::Native,
        "Krill's fact"
    );
    assert_eq!(cfg.members[1].compaction, CompactionSupport::Unknown);
    // The custom site turns out not to compact: the router learns it at once.
    *prober.answer.lock().unwrap() = CompactionSupport::Unsupported;
    let before = router.configs.lock().unwrap().len();
    svc.provider_compaction(
        CompactionRequest::cached(ProfileId::new("custom")).with_refresh(TransportRefresh::All),
    )
    .await
    .unwrap();
    assert_eq!(router.configs.lock().unwrap().len(), before + 1);
    assert_eq!(
        router.last().members[1].compaction,
        CompactionSupport::Unsupported
    );
    // The same answer again rewrites nothing.
    svc.provider_compaction(
        CompactionRequest::cached(ProfileId::new("custom")).with_refresh(TransportRefresh::All),
    )
    .await
    .unwrap();
    assert_eq!(router.configs.lock().unwrap().len(), before + 1);
}
