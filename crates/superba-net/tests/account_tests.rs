mod support;
use support::*;

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use superba_core::{
    CostSource, Error, LineProber, ManualClock, ModelUsage, Timestamp, UsageNotice, UsageQuery, Usd,
};
use superba_net::HttpLineProber;

const BILLING_QUERY: &str = "start_date=2026-11-01&end_date=2026-11-02";
const COSTS_QUERY: &str = "start_time=1792972800&end_time=1793577600&bucket_width=1d&limit=7";
const USAGE_QUERY: &str =
    "start_time=1792972800&end_time=1793577600&bucket_width=1d&limit=7&group_by=model";
const RANGE_START: i64 = 1_792_972_800;
const DAY_SECS: i64 = 86_400;

fn subscription_body() -> Value {
    json!({
        "object": "billing_subscription",
        "hard_limit_usd": 20,
        "soft_limit_usd": "15",
        "access_until": 1_793_491_200 + 30 * DAY_SECS,
        "has_payment_method": true,
        "plan": {"title": "Pro monthly"},
    })
}

fn billing_response(request: &RecordedRequest) -> MockResponse {
    match request.path.as_str() {
        WHOAMI => MockResponse::json(200, whoami_body()),
        BILLING_SUBSCRIPTION => MockResponse::json(200, subscription_body()),
        BILLING_USAGE => MockResponse::json(200, json!({"total_usage": 525})),
        _ => MockResponse::empty(404),
    }
}

// whoami/billing and costs/usage run concurrently; only compare their request
// sets here. Pagination and fallback ordering are checked separately below.
fn assert_requests(server: &MockServer, expected: &[(&str, Option<&str>)]) {
    let requests = server.recorded();
    for request in &requests {
        assert_eq!(request.method, "GET");
        assert!(request.body.is_empty());
    }
    let mut actual: Vec<_> = requests
        .iter()
        .map(|r| (r.path.as_str(), r.query.as_deref()))
        .collect();
    let mut expected = expected.to_vec();
    actual.sort_unstable();
    expected.sort_unstable();
    assert_eq!(actual, expected);
    assert_authenticated(server);
}

fn assert_usage_requests(server: &MockServer, fetches: usize) {
    assert_eq!(server.count(COSTS), fetches);
    assert_eq!(server.count(USAGE_COMPLETIONS), fetches);
    assert_requests(
        server,
        &[
            (COSTS, Some(COSTS_QUERY)),
            (USAGE_COMPLETIONS, Some(USAGE_QUERY)),
        ]
        .repeat(fetches),
    );
}

fn cost_bucket(start: i64, dollars: f64) -> Value {
    json!({
        "object": "bucket",
        "start_time": start,
        "end_time": start + DAY_SECS,
        "results": [{"amount": {"value": dollars, "currency": "usd"}}],
    })
}

fn costs_body() -> Value {
    let buckets: Vec<_> = (0..7)
        .map(|day| cost_bucket(RANGE_START + day * DAY_SECS, (day + 1) as f64))
        .collect();
    json!({"object": "page", "data": buckets, "has_more": false, "next_page": null})
}

fn usage_body() -> Value {
    json!({
        "object": "page",
        "data": [
            {"object": "bucket", "start_time": RANGE_START,
             "end_time": RANGE_START + DAY_SECS, "results": [
                {"model": "gpt-a", "cost_usd": 0.25, "num_model_requests": 2,
                 "input_tokens": 100, "input_cached_tokens": 40, "output_tokens": 10},
                {"model": "gpt-z", "cost_usd": 2, "num_model_requests": 3,
                 "input_tokens": 300, "input_cached_tokens": 120, "output_tokens": 30}
            ]},
            {"object": "bucket", "start_time": RANGE_START + DAY_SECS,
             "end_time": RANGE_START + 2 * DAY_SECS, "results": [
                {"model": "gpt-a", "cost_usd": 0.75, "num_model_requests": 1,
                 "input_tokens": 50, "input_cached_tokens": 20, "output_tokens": 5},
                {"model": "gpt-z", "cost_usd": 1, "num_model_requests": 2,
                 "input_tokens": 200, "input_cached_tokens": 80, "output_tokens": 20}
            ]},
            {"object": "bucket", "start_time": RANGE_START + 6 * DAY_SECS,
             "end_time": RANGE_START + 7 * DAY_SECS, "results": [
                {"model": "gpt-a", "cost_usd": 0.5, "num_model_requests": 4,
                 "input_tokens": 400, "input_cached_tokens": 160, "output_tokens": 40}
            ]}
        ],
        "has_more": false,
        "next_page": null,
    })
}

fn usage_response(request: &RecordedRequest) -> MockResponse {
    match request.path.as_str() {
        COSTS => MockResponse::json(200, costs_body()),
        USAGE_COMPLETIONS => MockResponse::json(200, usage_body()),
        _ => MockResponse::empty(404),
    }
}

fn assert_cache_has_no_key(dir: &Path) {
    let files: Vec<_> = fs::read_dir(dir).unwrap().map(Result::unwrap).collect();
    assert_eq!(files.len(), 1, "a report must actually be stored on disk");
    for file in files {
        assert!(file.file_type().unwrap().is_file());
        assert!(!file.file_name().to_string_lossy().contains(KEY));
        assert!(!fs::read_to_string(file.path()).unwrap().contains(KEY));
    }
}

#[tokio::test]
async fn account_summary_combines_krill_identity_and_billing() {
    let server = MockServer::start(billing_response).await;
    let site = site(&server);
    let summary = prober()
        .account_summary(&site, &site.lines[0], &key())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(summary.email.as_deref(), Some("a@example.com"));
    assert_eq!(summary.key_last4.as_deref(), Some("cdef"));
    assert_eq!(summary.plan_name.as_deref(), Some("Pro monthly"));
    assert_eq!(summary.balance.as_deref(), Some("14.75"));
    assert_eq!(summary.currency.as_deref(), Some("USD"));
    let expiry = Timestamp::from_unix_secs(1_793_491_200 + 30 * DAY_SECS);
    assert_eq!(summary.expires_at, Some(expiry));
    assert_eq!(summary.fetched_at, Some(CHECKED_AT));
    let billing = summary.billing.unwrap();
    assert_eq!(billing.limit, Some(Usd::from_micros(20_000_000)));
    assert_eq!(billing.soft_limit, Some(Usd::from_micros(15_000_000)));
    assert_eq!(billing.used, Some(Usd::from_micros(5_250_000)));
    assert_eq!(billing.remaining, Some(Usd::from_micros(14_750_000)));
    assert_eq!(billing.plan_title.as_deref(), Some("Pro monthly"));
    assert_eq!(billing.access_until, Some(expiry));
    assert_eq!(billing.has_payment_method, Some(true));
    assert_eq!(billing.period_start.as_deref(), Some("2026-11-01"));
    assert_eq!(billing.period_end.as_deref(), Some("2026-11-02"));
    assert!(!billing.unlimited);
    assert_requests(
        &server,
        &[
            (WHOAMI, None),
            (BILLING_SUBSCRIPTION, None),
            (BILLING_USAGE, Some(BILLING_QUERY)),
        ],
    );
}

#[tokio::test]
async fn account_summary_tries_the_subscription_fallback_family_for_usage_first() {
    let server = MockServer::start(|r| match r.path.as_str() {
        BILLING_SUBSCRIPTION_NO_V1 => MockResponse::json(200, subscription_body()),
        BILLING_USAGE_NO_V1 => MockResponse::json(200, json!({"total_usage": 825})),
        _ => MockResponse::empty(404),
    })
    .await;
    let site = compatible_site(&server);
    let summary = prober()
        .account_summary(&site, &site.lines[0], &key())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(summary.plan_name.as_deref(), Some("Pro monthly"));
    assert_eq!(summary.balance.as_deref(), Some("11.75"));
    assert_eq!(
        summary.billing.unwrap().used,
        Some(Usd::from_micros(8_250_000))
    );
    assert_requests(
        &server,
        &[
            (BILLING_SUBSCRIPTION, None),
            (BILLING_SUBSCRIPTION_NO_V1, None),
            (BILLING_USAGE_NO_V1, Some(BILLING_QUERY)),
        ],
    );
    assert_eq!(server.count(BILLING_USAGE), 0);
    assert_eq!(
        server
            .recorded()
            .iter()
            .map(|r| r.path.as_str())
            .collect::<Vec<_>>(),
        [
            BILLING_SUBSCRIPTION,
            BILLING_SUBSCRIPTION_NO_V1,
            BILLING_USAGE_NO_V1
        ]
    );
}

#[tokio::test]
async fn account_summary_treats_html_success_as_a_missing_endpoint() {
    let server = MockServer::start(|r| match r.path.as_str() {
        BILLING_SUBSCRIPTION => MockResponse {
            body: b"<html>".to_vec(),
            ..MockResponse::empty(200)
        }
        .with_header("Content-Type", "text/html"),
        BILLING_SUBSCRIPTION_NO_V1 => MockResponse::json(200, subscription_body()),
        BILLING_USAGE_NO_V1 => MockResponse::json(200, json!({"total_usage": 525})),
        _ => MockResponse::empty(404),
    })
    .await;
    let site = compatible_site(&server);
    let summary = prober()
        .account_summary(&site, &site.lines[0], &key())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(summary.plan_name.as_deref(), Some("Pro monthly"));
    assert_eq!(summary.balance.as_deref(), Some("14.75"));
    assert_eq!(
        summary.billing.unwrap().limit,
        Some(Usd::from_micros(20_000_000))
    );
    assert_requests(
        &server,
        &[
            (BILLING_SUBSCRIPTION, None),
            (BILLING_SUBSCRIPTION_NO_V1, None),
            (BILLING_USAGE_NO_V1, Some(BILLING_QUERY)),
        ],
    );
}

#[tokio::test]
async fn account_summary_keeps_the_plan_when_billing_usage_is_missing() {
    let server = MockServer::start(|r| match r.path.as_str() {
        BILLING_USAGE | BILLING_USAGE_NO_V1 => MockResponse::empty(404),
        _ => billing_response(r),
    })
    .await;
    let site = site(&server);
    let summary = prober()
        .account_summary(&site, &site.lines[0], &key())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(summary.plan_name.as_deref(), Some("Pro monthly"));
    assert_eq!(summary.balance, None);
    assert_eq!(summary.currency, None);
    let billing = summary.billing.unwrap();
    assert_eq!(billing.limit, Some(Usd::from_micros(20_000_000)));
    assert_eq!(billing.used, None);
    assert_eq!(billing.remaining, None);
    assert_eq!(billing.period_start, None);
    assert_eq!(billing.period_end, None);
    assert_requests(
        &server,
        &[
            (WHOAMI, None),
            (BILLING_SUBSCRIPTION, None),
            (BILLING_USAGE, Some(BILLING_QUERY)),
            (BILLING_USAGE_NO_V1, Some(BILLING_QUERY)),
        ],
    );
}

#[tokio::test]
async fn account_summary_recognizes_the_unlimited_billing_sentinel() {
    let server = MockServer::start(|r| {
        if r.path == BILLING_SUBSCRIPTION {
            let mut body = subscription_body();
            body["hard_limit_usd"] = json!(100_000_000);
            MockResponse::json(200, body)
        } else {
            billing_response(r)
        }
    })
    .await;
    let site = site(&server);
    let summary = prober()
        .account_summary(&site, &site.lines[0], &key())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(summary.balance, None);
    assert_eq!(summary.currency, None);
    let billing = summary.billing.unwrap();
    assert!(billing.unlimited);
    assert_eq!(billing.used, Some(Usd::from_micros(5_250_000)));
    assert_eq!(billing.remaining, None);
    assert_requests(
        &server,
        &[
            (WHOAMI, None),
            (BILLING_SUBSCRIPTION, None),
            (BILLING_USAGE, Some(BILLING_QUERY)),
        ],
    );
}

#[tokio::test]
async fn account_summary_stops_on_subscription_401_and_redacts_the_key() {
    let server = MockServer::start(|_| {
        MockResponse::json(
            401,
            json!({"error": {"message": format!("rejected {KEY}")}}),
        )
    })
    .await;
    let site = compatible_site(&server);
    let error = prober()
        .account_summary(&site, &site.lines[0], &key())
        .await
        .unwrap_err();

    assert!(matches!(error, Error::Http { status: 401, .. }));
    assert!(!error.to_string().contains(KEY));
    assert!(!format!("{error:?}").contains(KEY));
    assert!(!serde_json::to_string(&error).unwrap().contains(KEY));
    assert_requests(&server, &[(BILLING_SUBSCRIPTION, None)]);
}

#[tokio::test]
async fn account_summary_memory_cache_expires_after_a_minute() {
    let server = MockServer::start(billing_response).await;
    let site = site(&server);
    let clock = Arc::new(ManualClock::new(CHECKED_AT));
    let prober = HttpLineProber::new(probe_config()).with_clock(clock.clone());
    let first = prober
        .account_summary(&site, &site.lines[0], &key())
        .await
        .unwrap()
        .unwrap();
    let requests = [
        (WHOAMI, None),
        (BILLING_SUBSCRIPTION, None),
        (BILLING_USAGE, Some(BILLING_QUERY)),
    ];
    assert_requests(&server, &requests);

    let cached = prober
        .account_summary(&site, &site.lines[0], &key())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cached, first);
    assert_requests(&server, &requests);

    clock.advance(Duration::from_secs(61));
    let refreshed = prober
        .account_summary(&site, &site.lines[0], &key())
        .await
        .unwrap()
        .unwrap();
    let mut expected = first;
    expected.fetched_at = Some(CHECKED_AT.saturating_add(Duration::from_secs(61)));
    assert_eq!(refreshed, expected);
    assert_requests(&server, &requests.repeat(2));
}

#[tokio::test]
async fn account_summary_remembers_missing_billing_for_ten_minutes() {
    let server = MockServer::start(|r| match r.path.as_str() {
        WHOAMI => MockResponse::json(200, whoami_body()),
        _ => MockResponse::empty(404),
    })
    .await;
    let site = site(&server);
    let clock = Arc::new(ManualClock::new(CHECKED_AT));
    let prober = HttpLineProber::new(probe_config()).with_clock(clock.clone());
    let first = prober
        .account_summary(&site, &site.lines[0], &key())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.email.as_deref(), Some("a@example.com"));
    assert_eq!(first.billing, None);
    let mut requests = vec![
        (WHOAMI, None),
        (BILLING_SUBSCRIPTION, None),
        (BILLING_SUBSCRIPTION_NO_V1, None),
    ];
    assert_requests(&server, &requests);

    clock.advance(Duration::from_secs(61));
    let mut expected = first;
    expected.fetched_at = Some(CHECKED_AT.saturating_add(Duration::from_secs(61)));
    let summary = prober
        .account_summary(&site, &site.lines[0], &key())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(summary, expected);
    requests.push((WHOAMI, None));
    assert_requests(&server, &requests);

    clock.advance(Duration::from_secs(601));
    let summary = prober
        .account_summary(&site, &site.lines[0], &key())
        .await
        .unwrap()
        .unwrap();
    expected.fetched_at = Some(CHECKED_AT.saturating_add(Duration::from_secs(662)));
    assert_eq!(summary, expected);
    requests.extend([
        (WHOAMI, None),
        (BILLING_SUBSCRIPTION, None),
        (BILLING_SUBSCRIPTION_NO_V1, None),
    ]);
    assert_requests(&server, &requests);
}

#[tokio::test]
async fn account_summary_is_none_for_a_compatible_site_without_billing() {
    let server = MockServer::start(|_| MockResponse::empty(404)).await;
    let site = compatible_site(&server);
    let summary = prober()
        .account_summary(&site, &site.lines[0], &key())
        .await
        .unwrap();

    assert_eq!(summary, None);
    assert_requests(
        &server,
        &[
            (BILLING_SUBSCRIPTION, None),
            (BILLING_SUBSCRIPTION_NO_V1, None),
        ],
    );
}

#[tokio::test]
async fn usage_report_combines_daily_costs_and_models_sorted_by_cost() {
    let server = MockServer::start(usage_response).await;
    let site = site(&server);
    let tempdir = tempfile::tempdir().unwrap();
    let prober = prober().with_cache_dir(tempdir.path().join("usage"));
    let report = prober
        .usage_report(&site, &site.lines[0], &key(), &UsageQuery::last_days(7))
        .await
        .unwrap();

    assert_eq!(report.site_id, site.id);
    assert_eq!(report.cost_source, Some(CostSource::Costs));
    assert_eq!(report.days, 7);
    assert_eq!(report.daily.len(), 7);
    assert_eq!(report.start, Timestamp::from_unix_secs(1_792_972_800));
    assert_eq!(report.end, Timestamp::from_unix_secs(1_793_577_600));
    assert_eq!(report.total_cost, Some(Usd::from_micros(28_000_000)));
    assert_eq!(
        report
            .daily
            .iter()
            .map(|d| d.start.utc_date_string())
            .collect::<Vec<_>>(),
        [
            "2026-10-26",
            "2026-10-27",
            "2026-10-28",
            "2026-10-29",
            "2026-10-30",
            "2026-10-31",
            "2026-11-01"
        ]
    );
    assert_eq!(
        report.daily.iter().map(|d| d.cost).collect::<Vec<_>>(),
        [
            1_000_000, 2_000_000, 3_000_000, 4_000_000, 5_000_000, 6_000_000, 7_000_000
        ]
        .map(|micros| Some(Usd::from_micros(micros)))
    );
    assert_eq!(
        report
            .daily
            .iter()
            .map(|d| (d.requests, d.input_tokens, d.output_tokens))
            .collect::<Vec<_>>(),
        [
            (5, 400, 40),
            (3, 250, 25),
            (0, 0, 0),
            (0, 0, 0),
            (0, 0, 0),
            (0, 0, 0),
            (4, 400, 40)
        ]
        .map(|(r, i, o)| (Some(r), Some(i), Some(o)))
    );
    assert_eq!(
        report.models,
        [
            ModelUsage {
                model: "gpt-z".into(),
                cost: Some(Usd::from_micros(3_000_000)),
                requests: 5,
                input_tokens: 500,
                cached_input_tokens: 200,
                output_tokens: 50,
            },
            ModelUsage {
                model: "gpt-a".into(),
                cost: Some(Usd::from_micros(1_500_000)),
                requests: 7,
                input_tokens: 550,
                cached_input_tokens: 220,
                output_tokens: 55,
            },
        ]
    );
    assert!(report.usage_available);
    assert!(!report.from_cache);
    assert_eq!(report.fetched_at, CHECKED_AT);
    assert_eq!(report.notice, None);
    assert_usage_requests(&server, 1);
}

#[tokio::test]
async fn usage_report_sums_usage_costs_when_costs_are_missing() {
    let server = MockServer::start(|r| {
        if r.path == COSTS {
            MockResponse::empty(404)
        } else {
            usage_response(r)
        }
    })
    .await;
    let site = site(&server);
    let tempdir = tempfile::tempdir().unwrap();
    let prober = prober().with_cache_dir(tempdir.path().join("usage"));
    let report = prober
        .usage_report(&site, &site.lines[0], &key(), &UsageQuery::last_days(7))
        .await
        .unwrap();

    assert_eq!(report.cost_source, Some(CostSource::UsageApi));
    assert_eq!(report.total_cost, Some(Usd::from_micros(4_500_000)));
    assert_eq!(
        report.daily.iter().map(|d| d.cost).collect::<Vec<_>>(),
        [2_250_000, 1_750_000, 0, 0, 0, 0, 500_000].map(|micros| Some(Usd::from_micros(micros)))
    );
    assert!(report.usage_available);
    assert_eq!(report.models.len(), 2);
    assert_eq!(report.notice, None);
    assert_usage_requests(&server, 1);
}

#[tokio::test]
async fn usage_report_is_unsupported_when_both_endpoints_are_missing() {
    let server = MockServer::start(|_| MockResponse::empty(404)).await;
    let site = site(&server);
    let tempdir = tempfile::tempdir().unwrap();
    let prober = prober().with_cache_dir(tempdir.path().join("usage"));
    let error = prober
        .usage_report(&site, &site.lines[0], &key(), &UsageQuery::last_days(7))
        .await
        .unwrap_err();

    assert!(matches!(error, Error::Unsupported { .. }));
    assert_usage_requests(&server, 1);
}

#[tokio::test]
async fn usage_report_disk_cache_debounces_refreshes_and_expires() {
    let server = MockServer::start(usage_response).await;
    let site = site(&server);
    let tempdir = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new(CHECKED_AT));
    let prober = HttpLineProber::new(probe_config())
        .with_clock(clock.clone())
        .with_cache_dir(tempdir.path().join("usage"));
    let query = UsageQuery::last_days(7);
    let first = prober
        .usage_report(&site, &site.lines[0], &key(), &query)
        .await
        .unwrap();
    assert!(!first.from_cache);
    assert_eq!(first.fetched_at, CHECKED_AT);
    assert_usage_requests(&server, 1);
    assert_cache_has_no_key(&tempdir.path().join("usage"));

    // Recreate the client to prove the cache survives beyond one prober.
    drop(prober);
    let prober = HttpLineProber::new(probe_config())
        .with_clock(clock.clone())
        .with_cache_dir(tempdir.path().join("usage"));
    clock.advance(Duration::from_secs(30));
    let cached = prober
        .usage_report(&site, &site.lines[0], &key(), &query)
        .await
        .unwrap();
    let mut expected = first;
    expected.from_cache = true;
    assert_eq!(cached, expected);
    assert_usage_requests(&server, 1);
    assert_cache_has_no_key(&tempdir.path().join("usage"));

    let refresh = UsageQuery {
        refresh: true,
        ..query
    };
    let debounced = prober
        .usage_report(&site, &site.lines[0], &key(), &refresh)
        .await
        .unwrap();
    assert_eq!(debounced, expected);
    assert_usage_requests(&server, 1);
    assert_cache_has_no_key(&tempdir.path().join("usage"));

    clock.advance(Duration::from_secs(61));
    let refreshed = prober
        .usage_report(&site, &site.lines[0], &key(), &refresh)
        .await
        .unwrap();
    expected.from_cache = false;
    expected.fetched_at = CHECKED_AT.saturating_add(Duration::from_secs(91));
    assert_eq!(refreshed, expected);
    assert_usage_requests(&server, 2);
    assert_cache_has_no_key(&tempdir.path().join("usage"));

    clock.advance(Duration::from_secs(11 * 60));
    let expired = prober
        .usage_report(&site, &site.lines[0], &key(), &query)
        .await
        .unwrap();
    expected.fetched_at = CHECKED_AT.saturating_add(Duration::from_secs(751));
    assert_eq!(expired, expected);
    assert_usage_requests(&server, 3);
    assert_cache_has_no_key(&tempdir.path().join("usage"));
}

#[tokio::test]
async fn usage_report_returns_expired_cache_with_a_rate_limit_notice() {
    let rate_limited = Arc::new(AtomicBool::new(false));
    let server_rate_limited = rate_limited.clone();
    let server = MockServer::start(move |r| {
        if server_rate_limited.load(Ordering::SeqCst) {
            MockResponse::empty(429).with_header("Retry-After", "1200")
        } else {
            usage_response(r)
        }
    })
    .await;
    let site = site(&server);
    let tempdir = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new(CHECKED_AT));
    let prober = HttpLineProber::new(probe_config())
        .with_clock(clock.clone())
        .with_cache_dir(tempdir.path().join("usage"));
    let query = UsageQuery::last_days(7);
    let first = prober
        .usage_report(&site, &site.lines[0], &key(), &query)
        .await
        .unwrap();
    assert_usage_requests(&server, 1);

    rate_limited.store(true, Ordering::SeqCst);
    clock.advance(Duration::from_secs(11 * 60));
    let cached = prober
        .usage_report(&site, &site.lines[0], &key(), &query)
        .await
        .unwrap();
    let mut expected = first;
    expected.from_cache = true;
    expected.notice = Some(UsageNotice::RateLimited {
        retry_after_secs: Some(1200),
    });
    assert_eq!(cached, expected);
    assert_usage_requests(&server, 2);
}

#[tokio::test]
async fn usage_report_returns_429_without_a_cached_report() {
    let server =
        MockServer::start(|_| MockResponse::empty(429).with_header("Retry-After", "1200")).await;
    let site = site(&server);
    let tempdir = tempfile::tempdir().unwrap();
    let prober = prober().with_cache_dir(tempdir.path().join("usage"));
    let error = prober
        .usage_report(&site, &site.lines[0], &key(), &UsageQuery::last_days(7))
        .await
        .unwrap_err();

    assert!(matches!(error, Error::Http { status: 429, .. }));
    assert_usage_requests(&server, 1);
}

#[tokio::test]
async fn usage_report_does_not_cache_partial_data_after_usage_503() {
    let server = MockServer::start(|r| {
        if r.path == USAGE_COMPLETIONS {
            MockResponse::empty(503)
        } else {
            usage_response(r)
        }
    })
    .await;
    let site = site(&server);
    let tempdir = tempfile::tempdir().unwrap();
    let cache_dir = tempdir.path().join("usage");
    fs::create_dir(&cache_dir).unwrap();
    let prober = prober().with_cache_dir(tempdir.path().join("usage"));
    let query = UsageQuery::last_days(7);
    let report = prober
        .usage_report(&site, &site.lines[0], &key(), &query)
        .await
        .unwrap();

    assert_eq!(report.cost_source, Some(CostSource::Costs));
    assert_eq!(report.total_cost, Some(Usd::from_micros(28_000_000)));
    assert!(report.models.is_empty());
    assert!(!report.usage_available);
    assert!(!report.from_cache);
    assert_eq!(
        report.notice,
        Some(UsageNotice::RefreshFailed {
            reason: "HTTP 503".into()
        })
    );
    assert_usage_requests(&server, 1);
    assert!(fs::read_dir(&cache_dir).unwrap().next().is_none());

    let retried = prober
        .usage_report(&site, &site.lines[0], &key(), &query)
        .await
        .unwrap();
    assert_eq!(retried, report);
    assert_usage_requests(&server, 2);
    assert!(fs::read_dir(&cache_dir).unwrap().next().is_none());
}

#[tokio::test]
async fn usage_report_follows_costs_pagination_and_sums_both_pages() {
    let server = MockServer::start(|r| {
        if r.path == COSTS {
            if r.query.as_deref() == Some(COSTS_QUERY) {
                MockResponse::json(200, json!({
                    "object": "page", "data": [cost_bucket(RANGE_START, 1.25)],
                    "has_more": true, "next_page": "page_2",
                }))
            } else {
                MockResponse::json(200, json!({
                    "object": "page",
                    "data": [cost_bucket(RANGE_START, 2.5), cost_bucket(RANGE_START + DAY_SECS, 4.0)],
                    "has_more": false, "next_page": null,
                }))
            }
        } else {
            usage_response(r)
        }
    }).await;
    let site = site(&server);
    let tempdir = tempfile::tempdir().unwrap();
    let prober = prober().with_cache_dir(tempdir.path().join("usage"));
    let report = prober
        .usage_report(&site, &site.lines[0], &key(), &UsageQuery::last_days(7))
        .await
        .unwrap();

    assert_eq!(report.cost_source, Some(CostSource::Costs));
    assert_eq!(report.total_cost, Some(Usd::from_micros(7_750_000)));
    assert_eq!(
        report.daily.iter().map(|d| d.cost).collect::<Vec<_>>(),
        [3_750_000, 4_000_000, 0, 0, 0, 0, 0].map(|micros| Some(Usd::from_micros(micros)))
    );
    let page_2 = format!("{COSTS_QUERY}&page=page_2");
    assert_requests(
        &server,
        &[
            (COSTS, Some(COSTS_QUERY)),
            (COSTS, Some(&page_2)),
            (USAGE_COMPLETIONS, Some(USAGE_QUERY)),
        ],
    );
    assert_eq!(
        server
            .recorded()
            .iter()
            .filter(|r| r.path == COSTS)
            .map(|r| r.query.as_deref())
            .collect::<Vec<_>>(),
        [Some(COSTS_QUERY), Some(page_2.as_str())]
    );
}

#[tokio::test]
async fn usage_report_rejects_usage_401_even_when_costs_succeed() {
    let server = MockServer::start(|r| {
        if r.path == USAGE_COMPLETIONS {
            relay_401()
        } else {
            usage_response(r)
        }
    })
    .await;
    let site = site(&server);
    let tempdir = tempfile::tempdir().unwrap();
    let prober = prober().with_cache_dir(tempdir.path().join("usage"));
    let error = prober
        .usage_report(&site, &site.lines[0], &key(), &UsageQuery::last_days(7))
        .await
        .unwrap_err();

    assert!(matches!(error, Error::Http { status: 401, .. }));
    assert_usage_requests(&server, 1);
}

#[tokio::test]
async fn usage_report_rejects_401_on_later_usage_page() {
    let server = MockServer::start(|r| {
        if r.path == USAGE_COMPLETIONS {
            if r.query.as_deref() == Some(USAGE_QUERY) {
                let mut body = usage_body();
                body["has_more"] = json!(true);
                body["next_page"] = json!("page_2");
                MockResponse::json(200, body)
            } else {
                relay_401()
            }
        } else {
            usage_response(r)
        }
    })
    .await;
    let site = site(&server);
    let tempdir = tempfile::tempdir().unwrap();
    let prober = prober().with_cache_dir(tempdir.path().join("usage"));
    let result = prober
        .usage_report(&site, &site.lines[0], &key(), &UsageQuery::last_days(7))
        .await;

    let page_2 = format!("{USAGE_QUERY}&page=page_2");
    assert_requests(
        &server,
        &[
            (COSTS, Some(COSTS_QUERY)),
            (USAGE_COMPLETIONS, Some(USAGE_QUERY)),
            (USAGE_COMPLETIONS, Some(&page_2)),
        ],
    );
    let error = result.expect_err("a 401 on any page must invalidate the key");
    assert!(matches!(error, Error::Http { status: 401, .. }));
}

#[tokio::test]
async fn usage_report_rejects_a_compatible_site_without_requests() {
    let server = MockServer::start(usage_response).await;
    let site = compatible_site(&server);
    let tempdir = tempfile::tempdir().unwrap();
    let prober = prober().with_cache_dir(tempdir.path().join("usage"));
    let error = prober
        .usage_report(&site, &site.lines[0], &key(), &UsageQuery::last_days(7))
        .await
        .unwrap_err();

    assert!(matches!(error, Error::Unsupported { .. }));
    assert!(server.recorded().is_empty());
}

async fn assert_invalid_usage_range(days: u32) {
    let server = MockServer::start(usage_response).await;
    let site = site(&server);
    let tempdir = tempfile::tempdir().unwrap();
    let prober = prober().with_cache_dir(tempdir.path().join("usage"));
    let error = prober
        .usage_report(&site, &site.lines[0], &key(), &UsageQuery::last_days(days))
        .await
        .unwrap_err();

    assert!(matches!(error, Error::Invalid { .. }));
    assert!(server.recorded().is_empty());
}

#[tokio::test]
async fn usage_report_rejects_zero_days_without_requests() {
    assert_invalid_usage_range(0).await;
}

#[tokio::test]
async fn usage_report_rejects_32_days_without_requests() {
    assert_invalid_usage_range(32).await;
}
