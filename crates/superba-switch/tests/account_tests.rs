//! `SwitchService::account`: which profile and line are queried, and how each
//! part (summary, usage) fails on its own.

mod fakes;

use std::sync::{Arc, Mutex};

use fakes::*;
use superba_core::{
    AccountRequest, AccountSummary, ActiveState, AuthApiHealth, AuthMode, Error, KeyCheck, Line,
    LineCaps, LineId, LineProber, ModelInfo, ProfileId, ResolvedEndpoints, ResponsesProbe, Result,
    Secret, Site, SiteId, SwitchService, Timestamp, UsageQuery, UsageReport, async_trait,
};
use superba_switch::Switcher;
use url::Url;

/// Records account calls; everything else goes to the shared fake.
#[derive(Debug)]
struct AccountProber {
    inner: Arc<FakeProber>,
    summary: Mutex<Result<Option<AccountSummary>>>,
    usage: Mutex<Result<UsageReport>>,
    calls: Mutex<Vec<String>>,
}

fn report(site: &str) -> UsageReport {
    UsageReport {
        site_id: SiteId::new(site),
        start: Timestamp(T0),
        end: Timestamp(T0 + 86_400_000),
        days: 1,
        daily: Vec::new(),
        models: Vec::new(),
        total_cost: None,
        cost_source: None,
        usage_available: true,
        fetched_at: Timestamp(T0),
        from_cache: false,
        notice: None,
    }
}

#[async_trait]
impl LineProber for AccountProber {
    async fn probe(&self, site: &Site, line: &Line) -> Result<LineCaps> {
        self.inner.probe(site, line).await
    }

    async fn probe_all(&self, site: &Site) -> Result<Vec<LineCaps>> {
        self.inner.probe_all(site).await
    }

    async fn check_key(
        &self,
        site: &Site,
        line: &Line,
        mode: AuthMode,
        key: &Secret,
    ) -> Result<KeyCheck> {
        self.inner.check_key(site, line, mode, key).await
    }

    async fn check_authapi(
        &self,
        site: &Site,
        origin: &superba_core::Origin,
        key: &Secret,
    ) -> Result<AuthApiHealth> {
        self.inner.check_authapi(site, origin, key).await
    }

    async fn list_models(&self, url: &Url, key: &Secret) -> Result<Vec<ModelInfo>> {
        self.inner.list_models(url, key).await
    }

    async fn probe_responses(
        &self,
        endpoints: &ResolvedEndpoints,
        key: &Secret,
        model: &str,
    ) -> Result<ResponsesProbe> {
        self.inner.probe_responses(endpoints, key, model).await
    }

    async fn account_summary(
        &self,
        site: &Site,
        line: &Line,
        key: &Secret,
    ) -> Result<Option<AccountSummary>> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("summary {} {} {}", site.id, line.id, key.last4()));
        self.summary.lock().unwrap().clone()
    }

    async fn usage_report(
        &self,
        site: &Site,
        line: &Line,
        key: &Secret,
        query: &UsageQuery,
    ) -> Result<UsageReport> {
        self.calls.lock().unwrap().push(format!(
            "usage {} {} {} {} {}",
            site.id,
            line.id,
            key.last4(),
            query.days,
            query.refresh
        ));
        self.usage.lock().unwrap().clone()
    }

    async fn model_list(
        &self,
        _: superba_core::ModelListRequest<'_>,
    ) -> superba_core::Result<superba_core::ModelList> {
        Err(superba_core::Error::not_implemented(
            "LineProber::model_list",
        ))
    }
}

struct Fixture {
    world: World,
    prober: Arc<AccountProber>,
    switcher: Switcher,
}

fn fixture() -> Fixture {
    let world = World::new();
    let prober = Arc::new(AccountProber {
        inner: world.prober.clone(),
        summary: Mutex::new(Ok(Some(AccountSummary {
            email: Some("a@example.com".into()),
            ..AccountSummary::default()
        }))),
        usage: Mutex::new(Ok(report("krill"))),
        calls: Mutex::new(Vec::new()),
    });
    let mut deps = world.deps();
    deps.prober = prober.clone();
    let switcher = Switcher::new(deps);
    Fixture {
        world,
        prober,
        switcher,
    }
}

impl Fixture {
    fn calls(&self) -> Vec<String> {
        self.prober.calls.lock().unwrap().clone()
    }

    fn apply(&self, profile: &str, line: &str) {
        self.world.config.state.lock().unwrap().active = Some(ActiveState {
            profile_id: Some(ProfileId::new(profile)),
            site_id: Some(SiteId::new("krill")),
            line_id: Some(LineId::new(line)),
            auth_mode: Some(AuthMode::ApiKey),
            generation: 1,
            applied_at: Timestamp(T0),
        });
    }

    fn add_profile(&self, id: &str, line: Option<&str>, with_key: bool) {
        let mut p = krill_profile(id, line, AuthMode::ApiKey);
        if with_key {
            self.world
                .secrets
                .map
                .lock()
                .unwrap()
                .insert(format!("profile:{id}"), format!("sk-{id}-key-9999"));
        } else {
            p.key_ref = None;
        }
        self.world.profiles.set.lock().unwrap().profiles.push(p);
    }
}

fn last4() -> String {
    Secret::new(KEY).last4()
}

#[tokio::test]
async fn the_applied_profile_and_its_applied_line_are_the_default() {
    let f = fixture();
    f.add_profile("other", Some("cdn"), true);
    f.apply("work", "cn-fast");
    let r = f.switcher.account(AccountRequest::default()).await.unwrap();
    assert_eq!(r.profile_id.as_str(), "work");
    assert_eq!(r.line_id.as_str(), "cn-fast");
    assert_eq!(r.site_id.as_str(), "krill");
    assert_eq!(r.summary.unwrap().email.as_deref(), Some("a@example.com"));
    assert!(r.usage.is_none() && r.usage_error.is_none());
    assert_eq!(f.calls(), [format!("summary krill cn-fast {}", last4())]);
}

#[tokio::test]
async fn without_an_applied_profile_the_last_selected_then_the_first_keyed_one_is_used() {
    let f = fixture();
    f.add_profile("nokey", None, false);
    f.add_profile("other", Some("cdn"), true);
    let r = f.switcher.account(AccountRequest::default()).await.unwrap();
    assert_eq!(r.profile_id.as_str(), "work", "first profile with a key");
    assert_eq!(r.line_id.as_str(), "overseas", "the profile's own line");

    f.world.profiles.set.lock().unwrap().last_selected = Some(ProfileId::new("other"));
    let r = f.switcher.account(AccountRequest::default()).await.unwrap();
    assert_eq!(r.profile_id.as_str(), "other");
    assert_eq!(r.line_id.as_str(), "cdn");

    // An applied profile without a key (e.g. the official login) is skipped.
    f.apply("official", "overseas");
    let r = f.switcher.account(AccountRequest::default()).await.unwrap();
    assert_eq!(r.profile_id.as_str(), "other");
}

#[tokio::test]
async fn explicit_profile_and_line_are_checked() {
    let f = fixture();
    f.add_profile("nokey", None, false);
    let r = f
        .switcher
        .account(AccountRequest {
            profile_id: Some(ProfileId::new("work")),
            line_id: Some(LineId::new("cdn")),
            usage: None,
        })
        .await
        .unwrap();
    assert_eq!(r.line_id.as_str(), "cdn");
    for (profile, line) in [("nokey", None), ("missing", None), ("work", Some("nope"))] {
        let e = f
            .switcher
            .account(AccountRequest {
                profile_id: Some(ProfileId::new(profile)),
                line_id: line.map(LineId::new),
                usage: None,
            })
            .await
            .unwrap_err();
        assert!(
            matches!(e, Error::Invalid { .. } | Error::NotFound { .. }),
            "{profile}: {e}"
        );
    }
    assert_eq!(f.calls().len(), 1, "failed lookups send nothing");
}

#[tokio::test]
async fn usage_is_fetched_only_on_request_and_parts_fail_independently() {
    let f = fixture();
    *f.prober.summary.lock().unwrap() = Err(Error::Network {
        url: "https://api.kbsfdxc.net/v1/dashboard/billing/subscription".into(),
        message: "connection refused".into(),
    });
    let r = f
        .switcher
        .account(AccountRequest {
            usage: Some(UsageQuery {
                days: 30,
                refresh: true,
            }),
            ..AccountRequest::default()
        })
        .await
        .unwrap();
    assert!(r.summary.is_none());
    assert!(matches!(r.summary_error, Some(Error::Network { .. })));
    assert_eq!(r.usage.unwrap().site_id.as_str(), "krill");
    assert_eq!(
        f.calls(),
        [
            format!("summary krill overseas {}", last4()),
            format!("usage krill overseas {} 30 true", last4()),
        ]
    );

    *f.prober.usage.lock().unwrap() = Err(Error::Http {
        url: "https://api.kbsfdxc.net/v1/organization/costs".into(),
        status: 429,
        message: "rate limit exceeded".into(),
    });
    let r = f
        .switcher
        .account(AccountRequest {
            usage: Some(UsageQuery::default()),
            ..AccountRequest::default()
        })
        .await
        .unwrap();
    assert!(matches!(
        r.usage_error,
        Some(Error::Http { status: 429, .. })
    ));
}

#[tokio::test]
async fn compatible_sites_have_no_usage_api_and_bad_ranges_are_refused() {
    let f = fixture();
    let base = Url::parse("https://api.example.com/v1").unwrap();
    let site =
        superba_switch::new_compatible_site(SiteId::new("example"), "Example", &base).unwrap();
    f.world.profiles.set.lock().unwrap().custom_sites.push(site);
    let mut p = krill_profile("compat", None, AuthMode::ApiKey);
    p.site_id = SiteId::new("example");
    f.world.profiles.set.lock().unwrap().profiles.push(p);
    f.world
        .secrets
        .map
        .lock()
        .unwrap()
        .insert("profile:compat".into(), "sk-compat-key-1234".into());
    let r = f
        .switcher
        .account(AccountRequest {
            profile_id: Some(ProfileId::new("compat")),
            line_id: None,
            usage: Some(UsageQuery::default()),
        })
        .await
        .unwrap();
    assert_eq!(r.line_id.as_str(), "default");
    assert!(matches!(r.usage_error, Some(Error::Unsupported { .. })));
    assert_eq!(f.calls(), ["summary example default 1234"]);

    for days in [0, 32] {
        let e = f
            .switcher
            .account(AccountRequest {
                usage: Some(UsageQuery {
                    days,
                    refresh: false,
                }),
                ..AccountRequest::default()
            })
            .await
            .unwrap_err();
        assert!(matches!(e, Error::Invalid { .. }), "{days}: {e}");
    }
    assert_eq!(f.calls().len(), 1);
}
