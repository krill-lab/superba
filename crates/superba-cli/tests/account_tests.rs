//! `spb account`: request building, human and JSON output, exit codes.

use std::sync::{Arc, Mutex};

use clap::Parser;
use superba_cli::{Cli, Output, Runtime};
use superba_core::{
    AccountReport, AccountRequest, AccountSummary, AppSession, AuthMode, BillingSummary,
    CostSource, DailyUsage, DoctorFinding, Error, ExecuteOptions, KeyCheck, KeyInput, LineCaps,
    LineId, ManualClock, MapEnv, MigrationReceipt, ModelUsage, Overview, Profile, ProfileId,
    ProfileSet, RecoveryReport, Result, Secret, SessionReport, Settings, Site, SiteId,
    SwitchDecider, SwitchEvents, SwitchOutcome, SwitchPlan, SwitchRequest, SwitchService,
    Timestamp, UsageNotice, UsageReport, Usd, async_trait, catalog,
};

const DAY: i64 = 86_400_000;
const NOV1: i64 = 1_793_491_200_000;

#[derive(Default)]
struct AccountService {
    requests: Mutex<Vec<AccountRequest>>,
    report: Mutex<Option<Result<AccountReport>>>,
}

fn unsupported<T>(what: &str) -> Result<T> {
    Err(Error::not_implemented(what))
}

#[async_trait]
impl SwitchService for AccountService {
    async fn overview(&self) -> Result<Overview> {
        unsupported("overview")
    }
    async fn sites(&self) -> Result<Vec<Site>> {
        Ok(catalog::builtin_sites())
    }
    async fn profile_set(&self) -> Result<ProfileSet> {
        unsupported("profile_set")
    }
    async fn save_profile(&self, _: Profile, _: Option<Secret>) -> Result<Profile> {
        unsupported("save_profile")
    }
    async fn delete_profile(&self, _: &ProfileId) -> Result<()> {
        unsupported("delete_profile")
    }
    async fn save_settings(&self, _: Settings) -> Result<()> {
        unsupported("save_settings")
    }
    async fn save_site(&self, _: Site) -> Result<Site> {
        unsupported("save_site")
    }
    async fn delete_site(&self, _: &SiteId) -> Result<()> {
        unsupported("delete_site")
    }
    async fn probe_lines(&self, _: &SiteId) -> Result<Vec<LineCaps>> {
        unsupported("probe_lines")
    }
    async fn check_key(
        &self,
        _: &SiteId,
        _: &LineId,
        _: AuthMode,
        _: KeyInput,
    ) -> Result<KeyCheck> {
        unsupported("check_key")
    }
    async fn sessions(&self) -> Result<(SessionReport, Vec<MigrationReceipt>)> {
        unsupported("sessions")
    }
    async fn doctor(&self) -> Result<Vec<DoctorFinding>> {
        unsupported("doctor")
    }
    async fn open_app(&self) -> Result<AppSession> {
        unsupported("open_app")
    }
    async fn recover(&self) -> Result<RecoveryReport> {
        unsupported("recover")
    }
    async fn plan(&self, _: SwitchRequest) -> Result<SwitchPlan> {
        unsupported("plan")
    }
    async fn execute(
        &self,
        _: SwitchPlan,
        _: ExecuteOptions,
        _: Arc<dyn SwitchDecider>,
        _: Arc<dyn SwitchEvents>,
    ) -> Result<SwitchOutcome> {
        unsupported("execute")
    }
    async fn account(&self, request: AccountRequest) -> Result<AccountReport> {
        self.requests.lock().unwrap().push(request);
        self.report
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| Ok(full_report()))
    }

    async fn probe_site(
        &self,
        _: superba_core::SiteProbeRequest,
    ) -> superba_core::Result<superba_core::SiteProbeReport> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::probe_site",
        ))
    }

    async fn plan_settings(&self, _: &str) -> superba_core::Result<superba_core::PlanSettings> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::plan_settings",
        ))
    }

    async fn settings_view(
        &self,
        _: superba_core::SettingsViewRequest,
    ) -> superba_core::Result<superba_core::SettingsView> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::settings_view",
        ))
    }

    async fn edit_settings(
        &self,
        _: superba_core::SettingsEdit,
    ) -> superba_core::Result<superba_core::SettingsEditReport> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::edit_settings",
        ))
    }

    async fn router_status(&self) -> superba_core::Result<superba_core::RouterView> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::router_status",
        ))
    }

    async fn router_install(
        &self,
        _: superba_core::RouterInstallRequest,
    ) -> superba_core::Result<superba_core::RouterView> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::router_install",
        ))
    }

    async fn router_set_enabled(&self, _: bool) -> superba_core::Result<superba_core::RouterView> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::router_set_enabled",
        ))
    }

    async fn router_uninstall(&self) -> superba_core::Result<superba_core::RouterView> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::router_uninstall",
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

    async fn models_view(
        &self,
        _: superba_core::ModelsViewRequest,
    ) -> superba_core::Result<superba_core::ModelsView> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::models_view",
        ))
    }

    async fn save_models(
        &self,
        _: superba_core::ModelsEdit,
    ) -> superba_core::Result<superba_core::ModelsEditReport> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::save_models",
        ))
    }

    async fn hosts(&self) -> superba_core::Result<Vec<superba_core::HostView>> {
        Err(superba_core::Error::not_implemented("SwitchService::hosts"))
    }

    async fn host_probe(
        &self,
        _: &superba_core::TargetId,
    ) -> superba_core::Result<superba_core::HostView> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::host_probe",
        ))
    }

    async fn set_host_follow(
        &self,
        _: &superba_core::TargetId,
        _: bool,
    ) -> superba_core::Result<superba_core::HostView> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::set_host_follow",
        ))
    }

    async fn host_sync_plan(
        &self,
        _: &superba_core::TargetId,
    ) -> superba_core::Result<superba_core::HostSyncPlan> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::host_sync_plan",
        ))
    }

    async fn host_sessions(
        &self,
        _: &superba_core::TargetId,
    ) -> superba_core::Result<superba_core::HostSessions> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::host_sessions",
        ))
    }

    async fn versions(&self, _: bool) -> superba_core::Result<superba_core::VersionsView> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::versions",
        ))
    }

    async fn install_codex(
        &self,
        _: superba_core::InstallRequest,
    ) -> superba_core::Result<superba_core::InstallReport> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::install_codex",
        ))
    }

    async fn rollback_codex(
        &self,
        _: superba_core::InstallTarget,
    ) -> superba_core::Result<superba_core::InstallReport> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::rollback_codex",
        ))
    }

    async fn download_app_update(&self) -> superba_core::Result<superba_core::AppDownloadReport> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::download_app_update",
        ))
    }

    async fn router_requests(
        &self,
        _: superba_core::RequestQuery,
    ) -> superba_core::Result<superba_core::RequestPage> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::router_requests",
        ))
    }

    async fn router_request(&self, _: i64) -> superba_core::Result<superba_core::RequestDetail> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::router_request",
        ))
    }

    async fn router_inflight(&self) -> superba_core::Result<Vec<superba_core::InflightRequest>> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::router_inflight",
        ))
    }

    async fn router_stats(
        &self,
        _: superba_core::StatsQuery,
    ) -> superba_core::Result<superba_core::RequestStats> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::router_stats",
        ))
    }

    async fn router_clear(
        &self,
        _: superba_core::ClearRequest,
    ) -> superba_core::Result<superba_core::ClearReport> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::router_clear",
        ))
    }

    async fn router_set_stats(
        &self,
        _: superba_core::RouterStatsSettings,
    ) -> superba_core::Result<superba_core::RouterView> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::router_set_stats",
        ))
    }

    async fn session_titles(
        &self,
        _: Vec<String>,
    ) -> superba_core::Result<std::collections::BTreeMap<String, superba_core::SessionTitle>> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::session_titles",
        ))
    }

    async fn price_table(&self) -> superba_core::Result<superba_core::PriceTable> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::price_table",
        ))
    }

    async fn feeds_status(&self) -> superba_core::Result<superba_core::FeedsStatus> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::feeds_status",
        ))
    }

    async fn refresh_feeds(&self) -> superba_core::Result<superba_core::FeedsStatus> {
        Err(superba_core::Error::not_implemented(
            "SwitchService::refresh_feeds",
        ))
    }
}

fn usd(dollars: f64) -> Option<Usd> {
    Usd::from_dollars(dollars)
}

fn full_report() -> AccountReport {
    let mut billing = BillingSummary {
        limit: usd(20.0),
        used: usd(5.25),
        period_start: Some("2026-11-01".into()),
        period_end: Some("2026-11-15".into()),
        plan_title: Some("Pro monthly".into()),
        access_until: Some(Timestamp(NOV1 + 30 * DAY)),
        ..BillingSummary::default()
    };
    billing.derive_remaining();
    AccountReport {
        profile_id: ProfileId::new("work"),
        profile_name: "Work".into(),
        site_id: SiteId::new("krill"),
        line_id: LineId::new("overseas"),
        summary: Some(AccountSummary {
            email: Some("a@example.com".into()),
            key_last4: Some("cdef".into()),
            plan_name: Some("Pro monthly".into()),
            balance: Some("14.75".into()),
            currency: Some("USD".into()),
            billing: Some(billing),
            ..AccountSummary::default()
        }),
        summary_error: None,
        usage: Some(UsageReport {
            site_id: SiteId::new("krill"),
            start: Timestamp(NOV1),
            end: Timestamp(NOV1 + 2 * DAY),
            days: 2,
            daily: vec![
                DailyUsage {
                    start: Timestamp(NOV1),
                    cost: usd(1.5),
                    requests: Some(12),
                    input_tokens: Some(3000),
                    output_tokens: Some(400),
                },
                DailyUsage {
                    start: Timestamp(NOV1 + DAY),
                    cost: usd(0.0042),
                    requests: Some(1),
                    input_tokens: Some(10),
                    output_tokens: Some(2),
                },
            ],
            models: vec![ModelUsage {
                model: "gpt-x".into(),
                cost: usd(1.5042),
                requests: 13,
                input_tokens: 3010,
                cached_input_tokens: 1000,
                output_tokens: 402,
            }],
            total_cost: usd(1.5042),
            cost_source: Some(CostSource::Costs),
            usage_available: true,
            fetched_at: Timestamp(NOV1 + DAY),
            from_cache: false,
            notice: None,
        }),
        usage_error: None,
    }
}

async fn invoke(service: Arc<AccountService>, args: &[&str]) -> (u8, String, String) {
    let mut rt = Runtime::with_service(service);
    rt.env = Arc::new(MapEnv::new().with("SUPERBA_LOCALE", "en"));
    rt.clock = Arc::new(ManualClock::new(Timestamp::EPOCH));
    let cli = Cli::try_parse_from(std::iter::once("spb").chain(args.iter().copied())).unwrap();
    let (mut out, stdout, stderr) = Output::capture();
    let code = superba_cli::run(&cli, &rt, &mut out).await;
    (code, stdout.contents(), stderr.contents())
}

#[tokio::test]
async fn account_prints_identity_balance_and_usage() {
    let service = Arc::new(AccountService::default());
    let (code, stdout, stderr) = invoke(service.clone(), &["account", "--usage"]).await;
    assert_eq!(code, 0, "{stderr}");
    for expected in [
        "Profile: Work (work)",
        "Site: Krill (krill)",
        "Line: Overseas (overseas)",
        "Email: a@example.com",
        "Key last4: cdef",
        "Plan: Pro monthly",
        "Limit: $20.00",
        "Used (2026-11-01 to 2026-11-14): $5.25",
        "Remaining: $14.75",
        "Expires: 2026-12-01",
        "Usage 2026-11-01 to 2026-11-02 (2 days)",
        "Cost source: Costs API",
        "Total cost: $1.50",
        "2026-11-01 | $1.50 | 12 | 3,000 | 400",
        "2026-11-02 | $0.0042 | 1 | 10 | 2",
        "gpt-x | $1.50 | 13 | 3,010 | 1,000 | 402",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in\n{stdout}"
        );
    }
    assert!(!stdout.contains("Saved data"));
    let requests = service.requests.lock().unwrap().clone();
    assert_eq!(
        requests,
        [AccountRequest {
            profile_id: None,
            line_id: None,
            usage: Some(superba_core::UsageQuery {
                days: 7,
                refresh: false
            }),
        }]
    );
}

#[tokio::test]
async fn days_and_refresh_imply_usage_and_flags_reach_the_service() {
    let service = Arc::new(AccountService::default());
    invoke(
        service.clone(),
        &[
            "account",
            "--profile",
            "work",
            "--line",
            "cdn",
            "--days",
            "30",
        ],
    )
    .await;
    invoke(service.clone(), &["account", "--refresh"]).await;
    invoke(service.clone(), &["account"]).await;
    let requests = service.requests.lock().unwrap().clone();
    assert_eq!(requests[0].profile_id, Some(ProfileId::new("work")));
    assert_eq!(requests[0].line_id, Some(LineId::new("cdn")));
    assert_eq!(
        requests[0].usage.map(|u| (u.days, u.refresh)),
        Some((30, false))
    );
    assert_eq!(
        requests[1].usage.map(|u| (u.days, u.refresh)),
        Some((7, true))
    );
    assert_eq!(requests[2].usage, None);
    for bad in ["0", "32", "x"] {
        assert!(
            Cli::try_parse_from(["spb", "account", "--days", bad]).is_err(),
            "{bad}"
        );
    }
}

#[tokio::test]
async fn json_output_is_the_report() {
    let service = Arc::new(AccountService::default());
    let (code, stdout, _) = invoke(service, &["--json", "account", "--usage"]).await;
    assert_eq!(code, 0);
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["profile_id"], "work");
    assert_eq!(value["summary"]["billing"]["remaining"], 14.75);
    assert_eq!(value["usage"]["cost_source"], "costs");
    assert_eq!(value["usage"]["models"][0]["model"], "gpt-x");
}

#[tokio::test]
async fn failed_parts_are_reported_and_set_the_exit_code() {
    let service = Arc::new(AccountService::default());
    let mut report = full_report();
    report.summary = None;
    report.summary_error = Some(Error::Http {
        url: "https://api.krill-code.net/v1/dashboard/billing/subscription".into(),
        status: 401,
        message: "invalid key".into(),
    });
    report.usage = None;
    report.usage_error = Some(Error::Unsupported {
        what: "usage data on site krill".into(),
    });
    *service.report.lock().unwrap() = Some(Ok(report));
    let (code, stdout, _) = invoke(service.clone(), &["account", "--usage"]).await;
    assert_eq!(code, 1);
    assert!(stdout.contains("Account data unavailable:"), "{stdout}");
    assert!(stdout.contains("Usage data is not available on this site."));

    // Unsupported usage alone is not a failure.
    let mut report = full_report();
    report.usage = None;
    report.usage_error = Some(Error::Unsupported { what: "x".into() });
    *service.report.lock().unwrap() = Some(Ok(report));
    let (code, _, _) = invoke(service.clone(), &["account", "--usage"]).await;
    assert_eq!(code, 0);

    *service.report.lock().unwrap() = Some(Err(Error::NotFound {
        what: "a profile with a key".into(),
    }));
    let (code, _, stderr) = invoke(service, &["account"]).await;
    assert_eq!(code, 1);
    assert!(stderr.contains("a profile with a key"), "{stderr}");
}

#[tokio::test]
async fn unlimited_missing_billing_and_saved_usage_are_explained() {
    let service = Arc::new(AccountService::default());
    let mut report = full_report();
    if let Some(b) = report.summary.as_mut().and_then(|s| s.billing.as_mut()) {
        b.limit = usd(100_000_000.0);
        b.derive_remaining();
    }
    if let Some(u) = report.usage.as_mut() {
        u.from_cache = true;
        u.notice = Some(UsageNotice::RateLimited {
            retry_after_secs: Some(1200),
        });
        u.usage_available = false;
        u.models.clear();
    }
    *service.report.lock().unwrap() = Some(Ok(report));
    let (code, stdout, _) = invoke(service.clone(), &["account", "--usage"]).await;
    assert_eq!(code, 0);
    assert!(stdout.contains("Limit: Unlimited"), "{stdout}");
    assert!(!stdout.contains("Remaining:"));
    assert!(stdout.contains("Try again in 20 min."));
    assert!(stdout.contains("Saved data from 2026-11-02T00:00:00.000Z"));
    assert!(stdout.contains("Per-model usage: not available from this site"));

    let mut report = full_report();
    if let Some(s) = report.summary.as_mut() {
        s.billing = None;
    }
    report.usage = None;
    *service.report.lock().unwrap() = Some(Ok(report));
    let (_, stdout, _) = invoke(service.clone(), &["account"]).await;
    assert!(stdout.contains("Balance: not available from this site"));
    assert!(!stdout.contains("Usage"));
}

#[tokio::test]
async fn account_output_is_localized() {
    let service = Arc::new(AccountService::default());
    let (code, stdout, _) = invoke(service, &["--locale", "zh-CN", "account", "--usage"]).await;
    assert_eq!(code, 0);
    for expected in ["剩余", "套餐", "按模型", "费用来源"] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in\n{stdout}"
        );
    }
}
