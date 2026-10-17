mod support;

use std::time::{Duration, Instant};

use pretty_assertions::assert_eq;
use serde_json::json;
use superba_core::{AuthMode, CodexPath, LatencyGrade, LineProber, ProbeEndpoint};
use tokio::time::timeout;

use support::*;

#[tokio::test]
async fn healthy_relay_probes_every_endpoint_without_authorization() {
    let server = MockServer::start(|_| relay_401()).await;
    let site = site(&server);
    let prober = prober();
    let caps = prober.probe(&site, &site.lines[0]).await.unwrap();

    assert_eq!(caps.site_id, site.id);
    assert_eq!(caps.line_id, site.lines[0].id);
    assert_eq!(caps.checked_at, CHECKED_AT);
    assert!(caps.reachable);
    assert!(caps.latency_ms.is_some());
    assert_ne!(caps.latency_grade(), LatencyGrade::Unknown);
    assert!(caps.pat_ok);
    assert!(caps.apikey_ok);
    assert_eq!(caps.codex_path, Some(CodexPath::BackendApiCodex));
    assert!(!caps.blocked_by_edge);
    assert_eq!(caps.key_access, None);
    assert_detail(&caps, "probe.ok", &[]);
    assert!(caps.allows(AuthMode::Pat));
    assert!(caps.allows(AuthMode::ApiKey));
    assert_eq!(
        caps.checks.iter().map(|c| c.endpoint).collect::<Vec<_>>(),
        vec![
            ProbeEndpoint::ModelsUnauth,
            ProbeEndpoint::Whoami,
            ProbeEndpoint::WhamAccountsCheck,
            ProbeEndpoint::BackendCodexModels,
            ProbeEndpoint::CodexV1Models,
        ]
    );
    for check in &caps.checks {
        assert_eq!(check.status, Some(401));
        assert!(check.ok);
        assert!(!check.edge_blocked);
        assert!(check.latency_ms.is_some());
        assert_eq!(check.detail, None);
    }

    let requests = server.recorded();
    let samples = 1 + prober.config().latency_samples as usize;
    assert_eq!(server.count(MODELS), samples);
    assert_eq!(requests.len(), samples + 4);
    for request in &requests {
        assert_eq!(request.method, "GET");
        assert_eq!(request.header("authorization"), None);
        assert!(request.body.is_empty());
    }
    let models = requests
        .iter()
        .filter(|r| r.path == MODELS)
        .collect::<Vec<_>>();
    assert!(models.iter().all(|r| r.peer_addr == models[0].peer_addr));
    let backend = requests.iter().find(|r| r.path == BACKEND_MODELS).unwrap();
    assert_client_version(backend, &prober.config().client_version);
    for path in [WHOAMI, WHAM, BACKEND_MODELS, CODEX_V1_MODELS] {
        assert_eq!(server.count(path), 1);
    }
}

async fn assert_wham_interception(response: MockResponse, diagnostic: &str) {
    let server = MockServer::start(move |r| {
        if r.path == WHAM {
            response.clone()
        } else {
            relay_401()
        }
    })
    .await;
    let site = site(&server);
    let caps = prober().probe(&site, &site.lines[0]).await.unwrap();

    assert!(caps.reachable);
    assert!(!caps.pat_ok);
    assert!(caps.apikey_ok);
    assert!(caps.blocked_by_edge);
    assert_eq!(caps.codex_path, Some(CodexPath::BackendApiCodex));
    assert_detail(&caps, "probe.pat_edge_blocked", &[("endpoint", WHAM)]);
    assert!(!caps.allows(AuthMode::Pat));
    assert!(caps.allows(AuthMode::ApiKey));
    let check = caps
        .checks
        .iter()
        .find(|c| c.endpoint == ProbeEndpoint::WhamAccountsCheck)
        .unwrap();
    assert_eq!(check.status, Some(200));
    assert!(!check.ok);
    assert!(check.edge_blocked);
    assert_eq!(check.detail.as_deref(), Some(diagnostic));
}

#[tokio::test]
async fn wham_with_edge_header_disables_pat() {
    assert_wham_interception(
        edge_block(),
        "intercepted by the line's edge (HTTP 200 with an l-block header)",
    )
    .await;
}

#[tokio::test]
async fn empty_wham_without_edge_header_disables_pat() {
    assert_wham_interception(
        MockResponse::empty(200),
        "intercepted by the line's edge (HTTP 200 with an empty body)",
    )
    .await;
}

#[tokio::test]
async fn html_wham_without_edge_header_disables_pat() {
    let response = MockResponse {
        body: b"<html>challenge</html>".to_vec(),
        ..MockResponse::empty(200)
    }
    .with_header("Content-Type", "text/html");
    assert_wham_interception(
        response,
        "intercepted by the line's edge (HTTP 200 with a non-JSON body)",
    )
    .await;
}

#[tokio::test]
async fn missing_whoami_reports_endpoint_and_status() {
    let server = MockServer::start(|r| {
        if r.path == WHOAMI {
            MockResponse::json(404, json!({"error": "not found"}))
        } else {
            relay_401()
        }
    })
    .await;
    let site = site(&server);
    let caps = prober().probe(&site, &site.lines[0]).await.unwrap();

    assert!(caps.reachable);
    assert!(!caps.pat_ok);
    assert!(caps.apikey_ok);
    assert!(!caps.blocked_by_edge);
    assert_detail(
        &caps,
        "probe.pat_unavailable",
        &[("endpoint", WHOAMI), ("status", "HTTP 404")],
    );
    let check = caps
        .checks
        .iter()
        .find(|c| c.endpoint == ProbeEndpoint::Whoami)
        .unwrap();
    assert_eq!(check.status, Some(404));
    assert!(!check.ok);
    assert_eq!(check.detail.as_deref(), Some("HTTP 404: not found"));
}

#[tokio::test]
async fn intercepted_backend_uses_codex_v1_for_api_keys() {
    let server = MockServer::start(|r| {
        if r.path == BACKEND_MODELS {
            edge_block()
        } else {
            relay_401()
        }
    })
    .await;
    let site = site(&server);
    let caps = prober().probe(&site, &site.lines[0]).await.unwrap();

    assert!(caps.reachable);
    assert!(caps.pat_ok, "the PAT endpoints themselves still answer");
    assert_eq!(caps.codex_path, Some(CodexPath::CodexV1));
    assert!(caps.apikey_ok);
    assert!(caps.blocked_by_edge);
    assert_detail(&caps, "probe.codex_v1_fallback", &[]);
    assert!(!caps.allows(AuthMode::Pat));
    assert!(caps.allows(AuthMode::ApiKey));
    assert_eq!(server.count(CODEX_V1_MODELS), 1);
}

#[tokio::test]
async fn both_codex_paths_intercepted_report_no_usable_path() {
    let server = MockServer::start(|r| {
        if matches!(r.path.as_str(), BACKEND_MODELS | CODEX_V1_MODELS) {
            edge_block()
        } else {
            relay_401()
        }
    })
    .await;
    let site = site(&server);
    let caps = prober().probe(&site, &site.lines[0]).await.unwrap();

    assert!(caps.reachable);
    assert_eq!(caps.codex_path, None);
    assert!(!caps.apikey_ok);
    assert!(caps.blocked_by_edge);
    assert!(!caps.allows(AuthMode::Pat));
    assert!(!caps.allows(AuthMode::ApiKey));
    assert_detail(
        &caps,
        "probe.no_codex_path",
        &[(
            "detail",
            "/backend-api/codex/models: intercepted by the line's edge (HTTP 200 with an l-block header)",
        )],
    );
}

#[tokio::test]
async fn closed_port_is_unreachable_without_latency() {
    let prober = prober();
    let site = site_with_origins(&[&closed_origin().await]);
    let started = Instant::now();
    let caps = timeout(Duration::from_secs(2), prober.probe(&site, &site.lines[0]))
        .await
        .expect("closed port should fail promptly")
        .unwrap();

    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(!caps.reachable);
    assert_eq!(caps.latency_ms, None);
    assert_eq!(caps.latency_grade(), LatencyGrade::Unknown);
    assert_eq!(caps.codex_path, None);
    assert!(!caps.pat_ok);
    assert!(!caps.apikey_ok);
    assert_eq!(caps.checks.len(), 1);
    assert_eq!(caps.checks[0].endpoint, ProbeEndpoint::ModelsUnauth);
    assert_eq!(caps.checks[0].status, None);
    let reason = caps.checks[0].detail.as_deref().unwrap();
    assert!(!reason.is_empty());
    // OS wording varies; the localized argument must preserve that diagnostic.
    assert_detail(&caps, "probe.unreachable", &[("detail", reason)]);
}

#[tokio::test]
async fn latency_warmup_timeout_stops_the_probe_promptly() {
    let server = MockServer::start(|r| {
        if r.path == MODELS {
            relay_401().with_delay(Duration::from_secs(3))
        } else {
            relay_401()
        }
    })
    .await;
    let site = site(&server);
    let prober = prober();
    let started = Instant::now();
    let caps = timeout(
        Duration::from_millis(2500),
        prober.probe(&site, &site.lines[0]),
    )
    .await
    .expect("probe exceeded its request timeout")
    .unwrap();

    assert!(started.elapsed() < Duration::from_millis(2500));
    assert!(!caps.reachable);
    assert_eq!(caps.latency_ms, None);
    assert_eq!(caps.latency_grade(), LatencyGrade::Unknown);
    assert_eq!(caps.checks.len(), 1);
    assert_eq!(caps.checks[0].endpoint, ProbeEndpoint::ModelsUnauth);
    assert_eq!(caps.checks[0].status, None);
    assert!(!caps.checks[0].ok);
    assert_eq!(
        caps.checks[0].detail.as_deref(),
        Some("no answer before the timeout")
    );
    assert_detail(
        &caps,
        "probe.unreachable",
        &[("detail", "no answer before the timeout")],
    );
    assert_eq!(
        server.count(MODELS),
        1,
        "a full timeout must not be retried"
    );
    assert_eq!(server.recorded().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_all_runs_lines_concurrently_and_preserves_site_order() {
    let first = MockServer::start(|r| match r.path.as_str() {
        MODELS => relay_401().with_delay(Duration::from_millis(300)),
        // Make the first line finish last, so completion order is not site order.
        WHOAMI => relay_401().with_delay(Duration::from_millis(100)),
        _ => relay_401(),
    })
    .await;
    let second = MockServer::start(|r| {
        if r.path == MODELS {
            relay_401().with_delay(Duration::from_millis(300))
        } else {
            relay_401()
        }
    })
    .await;
    let site = site_with_origins(&[&first.origin(), &second.origin()]);
    let prober = prober();
    // Generous watchdog only; concurrency is proven by the arrival times below.
    let caps = timeout(Duration::from_secs(5), prober.probe_all(&site))
        .await
        .expect("probe_all finishes")
        .unwrap();

    assert_eq!(caps.len(), 2);
    for (caps, line) in caps.iter().zip(&site.lines) {
        assert_eq!(caps.line_id, line.id);
        assert_eq!(caps.site_id, site.id);
        assert!(caps.reachable);
        assert!(caps.latency_ms.is_some());
        assert!(caps.pat_ok && caps.apikey_ok);
        assert_detail(caps, "probe.ok", &[]);
    }
    assert_eq!(first.count(MODELS), 3);
    assert_eq!(second.count(MODELS), 3);
    // Each latency probe is sequential (warm-up, then samples, 300 ms apart). The
    // lines ran concurrently iff each line's first latency request arrived before
    // the other line's last one.
    let arrivals = |server: &MockServer| {
        let times: Vec<_> = server
            .recorded()
            .into_iter()
            .filter(|r| r.path == MODELS)
            .map(|r| r.received_at)
            .collect();
        (*times.iter().min().unwrap(), *times.iter().max().unwrap())
    };
    let (first_start, first_end) = arrivals(&first);
    let (second_start, second_end) = arrivals(&second);
    assert!(
        first_start < second_end && second_start < first_end,
        "the two lines' latency probes did not overlap"
    );
}

#[tokio::test]
async fn compatible_site_accepts_html_404_and_only_probes_its_models_path() {
    let server = MockServer::start(|_| {
        MockResponse {
            body: b"<html>not found</html>".to_vec(),
            ..MockResponse::empty(404)
        }
        .with_header("Content-Type", "text/html")
    })
    .await;
    let site = compatible_site(&server);
    let caps = prober().probe(&site, &site.lines[0]).await.unwrap();

    assert!(caps.reachable);
    assert!(caps.latency_ms.is_some());
    assert_eq!(caps.codex_path, Some(CodexPath::AsIs));
    assert!(caps.apikey_ok);
    assert!(!caps.pat_ok);
    assert!(!caps.blocked_by_edge);
    assert_detail(&caps, "probe.ok", &[]);
    assert_eq!(caps.checks.len(), 1);
    assert_eq!(caps.checks[0].endpoint, ProbeEndpoint::ModelsUnauth);
    assert_eq!(caps.checks[0].status, Some(404));
    assert!(caps.checks[0].ok);
    assert_eq!(server.count(MODELS), 3);
    for request in server.recorded() {
        assert_eq!(request.path, MODELS);
        assert_eq!(request.query, None);
        assert_eq!(request.header("authorization"), None);
    }
}
