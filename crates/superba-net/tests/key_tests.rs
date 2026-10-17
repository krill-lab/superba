mod support;

use std::time::Duration;

use pretty_assertions::assert_eq;
use serde_json::json;
use superba_core::{AuthApiHealth, AuthMode, CodexPath, Error, KeyAccess, LineProber, Secret};

use superba_net::{HttpLineProber, ProbeConfig};
use support::*;

fn denial(status: u16, message: &str) -> MockResponse {
    MockResponse::json(
        status,
        json!({
            "type": "error",
            "error": {"type": "forbidden_error", "message": message},
        }),
    )
}

fn access_point_denied() -> MockResponse {
    denial(
        403,
        "当前套餐不包含线路「三网优化cdn」。... | Your plan does not include the entry route \"cdn\"; ...",
    )
}

#[tokio::test]
async fn pat_key_check_returns_identity_and_uses_only_the_backend_path() {
    let server = MockServer::start(healthy_authenticated).await;
    let site = site(&server);
    let prober = prober();
    let check = prober
        .check_key(&site, &site.lines[0], AuthMode::Pat, &key())
        .await
        .unwrap();

    assert_eq!(check.site_id, site.id);
    assert_eq!(check.line_id, site.lines[0].id);
    assert_eq!(check.checked_at, CHECKED_AT);
    assert_eq!(check.access, KeyAccess::Ok);
    assert_eq!(check.whoami, Some(AuthApiHealth::Healthy));
    let identity = check.identity.unwrap();
    assert_eq!(identity.email.as_deref(), Some("a@example.com"));
    assert_eq!(identity.user_id.as_deref(), Some("user-1"));
    assert_eq!(check.key_last4, "cdef");
    assert_eq!(check.codex_path, Some(CodexPath::BackendApiCodex));
    assert_authenticated(&server);
    assert_eq!(server.recorded().len(), 3);
    for path in [WHOAMI, WHAM, BACKEND_MODELS] {
        assert_eq!(server.count(path), 1);
    }
    assert_eq!(server.count(CODEX_V1_MODELS), 0);
    let requests = server.recorded();
    for request in &requests {
        assert_eq!(request.method, "GET");
        assert!(request.body.is_empty());
    }
    assert_client_version(
        requests.iter().find(|r| r.path == BACKEND_MODELS).unwrap(),
        &prober.config().client_version,
    );
}

async fn assert_api_access(response: MockResponse, expected: KeyAccess) {
    let server = MockServer::start(move |r| {
        if r.path == BACKEND_MODELS {
            response.clone()
        } else {
            healthy_authenticated(r)
        }
    })
    .await;
    let site = site(&server);
    let check = prober()
        .check_key(&site, &site.lines[0], AuthMode::ApiKey, &key())
        .await
        .unwrap();

    assert_eq!(check.access, expected);
    assert_eq!(check.key_last4, "cdef");
    assert_eq!(check.codex_path, Some(CodexPath::BackendApiCodex));
    assert_eq!(check.whoami, None);
    assert_eq!(server.count(BACKEND_MODELS), 1);
    assert_eq!(
        server.count(CODEX_V1_MODELS),
        0,
        "denials must not trigger a fallback"
    );
    assert_eq!(server.count(WHAM), 0);
    assert_authenticated(&server);
}

#[tokio::test]
async fn api_key_401_is_invalid_key() {
    assert_api_access(relay_401(), KeyAccess::InvalidKey).await;
}

#[tokio::test]
async fn api_key_plan_denial_is_access_point_denied() {
    assert_api_access(access_point_denied(), KeyAccess::AccessPointDenied).await;
}

#[tokio::test]
async fn api_key_codex_channel_denial_is_channel_denied() {
    assert_api_access(
        denial(403, "your API key is not authorized for the Codex channel"),
        KeyAccess::ChannelDenied,
    )
    .await;
}

#[tokio::test]
async fn api_key_insufficient_balance_is_quota_exhausted() {
    assert_api_access(
        denial(
            403,
            "insufficient balance, please redeem a plan code or recharge credits",
        ),
        KeyAccess::QuotaExhausted,
    )
    .await;
}

#[tokio::test]
async fn api_key_daily_limit_is_quota_exhausted() {
    assert_api_access(
        denial(429, "daily quota exceeded, please try again tomorrow"),
        KeyAccess::QuotaExhausted,
    )
    .await;
}

#[tokio::test]
async fn api_key_server_error_preserves_unknown_status() {
    assert_api_access(
        MockResponse::empty(500),
        KeyAccess::Unknown {
            status: Some(500),
            message: "HTTP 500".into(),
        },
    )
    .await;
}

#[tokio::test]
async fn api_key_closed_port_is_unreachable() {
    let prober = prober();
    let site = site_with_origins(&[&closed_origin().await]);
    let check = tokio::time::timeout(
        Duration::from_secs(2),
        prober.check_key(&site, &site.lines[0], AuthMode::ApiKey, &key()),
    )
    .await
    .expect("closed port should fail promptly")
    .unwrap();
    assert_eq!(check.access, KeyAccess::Unreachable);
    assert_eq!(check.key_last4, "cdef");
}

#[tokio::test]
async fn pat_wham_interception_is_edge_blocked() {
    let server = MockServer::start(|r| {
        if r.path == WHAM {
            edge_block()
        } else {
            healthy_authenticated(r)
        }
    })
    .await;
    let site = site(&server);
    let check = prober()
        .check_key(&site, &site.lines[0], AuthMode::Pat, &key())
        .await
        .unwrap();
    assert_eq!(check.access, KeyAccess::EdgeBlocked);
    assert_eq!(check.whoami, Some(AuthApiHealth::Healthy));
    assert_eq!(check.codex_path, Some(CodexPath::BackendApiCodex));
    assert_authenticated(&server);
    assert_eq!(server.count(CODEX_V1_MODELS), 0);
}

#[tokio::test]
async fn pat_models_denial_takes_priority_over_wham_interception() {
    let server = MockServer::start(|r| match r.path.as_str() {
        WHAM => edge_block(),
        BACKEND_MODELS => access_point_denied(),
        _ => healthy_authenticated(r),
    })
    .await;
    let site = site(&server);
    let check = prober()
        .check_key(&site, &site.lines[0], AuthMode::Pat, &key())
        .await
        .unwrap();
    assert_eq!(check.access, KeyAccess::AccessPointDenied);
    assert_eq!(check.whoami, Some(AuthApiHealth::Healthy));
    assert_eq!(server.count(WHAM), 1);
    assert_eq!(server.count(BACKEND_MODELS), 1);
    assert_eq!(server.count(CODEX_V1_MODELS), 0);
    assert_authenticated(&server);
}

#[tokio::test]
async fn api_key_falls_back_to_codex_v1_when_backend_is_intercepted() {
    let server = MockServer::start(|r| {
        if r.path == BACKEND_MODELS {
            edge_block()
        } else {
            healthy_authenticated(r)
        }
    })
    .await;
    let site = site(&server);
    let check = prober()
        .check_key(&site, &site.lines[0], AuthMode::ApiKey, &key())
        .await
        .unwrap();
    assert_eq!(check.access, KeyAccess::Ok);
    assert_eq!(check.codex_path, Some(CodexPath::CodexV1));
    assert_eq!(server.count(BACKEND_MODELS), 1);
    assert_eq!(server.count(CODEX_V1_MODELS), 1);
    let requests = server.recorded();
    let backend = requests
        .iter()
        .position(|r| r.path == BACKEND_MODELS)
        .unwrap();
    let fallback = requests
        .iter()
        .position(|r| r.path == CODEX_V1_MODELS)
        .unwrap();
    assert!(backend < fallback);
    assert_eq!(requests[fallback].query, None);
    assert_authenticated(&server);
}

#[tokio::test]
async fn check_key_rejects_a_foreign_line_before_sending_any_request() {
    let owned = MockServer::start(healthy_authenticated).await;
    let foreign = MockServer::start(healthy_authenticated).await;
    let site = site(&owned);
    let mut line = site.lines[0].clone();
    line.origin = superba_core::Origin::parse(&foreign.origin()).unwrap();
    let error = prober()
        .check_key(&site, &line, AuthMode::Pat, &key())
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Invalid { .. }));
    assert!(foreign.recorded().is_empty());
    assert!(owned.recorded().is_empty());
}

#[tokio::test]
async fn check_key_rejects_official_chatgpt_mode_without_requests() {
    let server = MockServer::start(healthy_authenticated).await;
    let site = site(&server);
    let error = prober()
        .check_key(&site, &site.lines[0], AuthMode::OfficialChatGpt, &key())
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Invalid { .. }));
    assert!(server.recorded().is_empty());
}

#[tokio::test]
async fn check_key_rejects_an_empty_key_without_requests() {
    let server = MockServer::start(healthy_authenticated).await;
    let site = site(&server);
    let error = prober()
        .check_key(&site, &site.lines[0], AuthMode::ApiKey, &Secret::new(""))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Invalid { .. }));
    assert!(server.recorded().is_empty());
}

#[tokio::test]
async fn check_authapi_rejects_a_foreign_origin_without_requests() {
    let owned = MockServer::start(healthy_authenticated).await;
    let foreign = MockServer::start(healthy_authenticated).await;
    let site = site(&owned);
    let origin = superba_core::Origin::parse(&foreign.origin()).unwrap();
    let error = prober()
        .check_authapi(&site, &origin, &key())
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Invalid { .. }));
    assert!(foreign.recorded().is_empty());
    assert!(owned.recorded().is_empty());
}

async fn assert_authapi(response: MockResponse, expected: AuthApiHealth) {
    let server = MockServer::start(move |_| response.clone()).await;
    let site = site(&server);
    let health = prober()
        .check_authapi(&site, &site.lines[0].origin, &key())
        .await
        .unwrap();
    assert_eq!(health, expected);
    assert_eq!(server.count(WHOAMI), 1);
    assert_eq!(server.recorded().len(), 1);
    assert_authenticated(&server);
}

#[tokio::test]
async fn check_authapi_accepts_complete_whoami_on_a_site_line() {
    assert_authapi(
        MockResponse::json(200, whoami_body()),
        AuthApiHealth::Healthy,
    )
    .await;
}

#[tokio::test]
async fn check_authapi_requires_the_fedramp_field() {
    let mut body = whoami_body();
    body.as_object_mut()
        .unwrap()
        .remove("chatgpt_account_is_fedramp");
    assert_authapi(
        MockResponse::json(200, body),
        AuthApiHealth::Unhealthy {
            reason: "whoami answer is missing chatgpt_account_is_fedramp".into(),
        },
    )
    .await;
}

#[tokio::test]
async fn check_authapi_rejects_edge_interception() {
    assert_authapi(
        edge_block(),
        AuthApiHealth::Unhealthy {
            reason: "intercepted by the line's edge (HTTP 200 with an l-block header)".into(),
        },
    )
    .await;
}

#[tokio::test]
async fn check_authapi_rejects_401() {
    assert_authapi(
        relay_401(),
        AuthApiHealth::Unhealthy {
            reason: "HTTP 401: missing Authorization".into(),
        },
    )
    .await;
}

async fn assert_keyed_probe(wham_blocked: bool) {
    let server = MockServer::start(move |r| {
        if r.header("authorization").is_none() {
            relay_401()
        } else if wham_blocked && r.path == WHAM {
            edge_block()
        } else {
            healthy_authenticated(r)
        }
    })
    .await;
    let site = site(&server);
    let caps = prober()
        .probe_with_key(&site, &site.lines[0], &key())
        .await
        .unwrap();

    assert!(caps.reachable);
    assert_eq!(caps.key_access, Some(KeyAccess::Ok));
    assert_eq!(caps.pat_ok, !wham_blocked);
    assert!(caps.apikey_ok);
    assert_eq!(caps.blocked_by_edge, wham_blocked);
    assert_eq!(caps.codex_path, Some(CodexPath::BackendApiCodex));
    assert_eq!(caps.allows(AuthMode::Pat), !wham_blocked);
    assert!(caps.allows(AuthMode::ApiKey));
    if wham_blocked {
        assert_detail(&caps, "probe.pat_edge_blocked", &[("endpoint", WHAM)]);
    } else {
        assert_detail(&caps, "probe.ok", &[]);
    }

    let requests = server.recorded();
    let (unauthenticated, authenticated): (Vec<_>, Vec<_>) = requests
        .iter()
        .partition(|r| r.header("authorization").is_none());
    assert_eq!(unauthenticated.len(), 7);
    assert_eq!(authenticated.len(), 3);
    assert_eq!(
        unauthenticated.iter().filter(|r| r.path == MODELS).count(),
        3
    );
    for path in [WHOAMI, WHAM, BACKEND_MODELS, CODEX_V1_MODELS] {
        assert_eq!(unauthenticated.iter().filter(|r| r.path == path).count(), 1);
    }
    for path in [WHOAMI, WHAM, BACKEND_MODELS] {
        assert_eq!(authenticated.iter().filter(|r| r.path == path).count(), 1);
    }
    for request in authenticated {
        assert_eq!(
            request.header("authorization"),
            Some(format!("Bearer {KEY}").as_str())
        );
    }
}

#[tokio::test]
async fn probe_with_key_detects_wham_intercepting_only_authenticated_requests() {
    assert_keyed_probe(true).await;
}

#[tokio::test]
async fn probe_with_key_keeps_fully_healthy_pat_available() {
    assert_keyed_probe(false).await;
}

#[tokio::test]
async fn krill_account_summary_without_billing_endpoints_uses_whoami_and_key_suffix() {
    let server = MockServer::start(healthy_authenticated).await;
    let site = site(&server);
    let summary = prober()
        .account_summary(&site, &site.lines[0], &key())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(summary.email.as_deref(), Some("a@example.com"));
    assert_eq!(summary.key_last4.as_deref(), Some("cdef"));
    assert_eq!(
        summary.plan_name, None,
        "whoami's constant plan must not become an account plan"
    );
    assert_eq!(summary.billing, None);
    assert_eq!(server.count(WHOAMI), 1);
    assert_eq!(
        server.count(BILLING_SUBSCRIPTION) + server.count(BILLING_SUBSCRIPTION_NO_V1),
        2,
        "both billing paths are tried once"
    );
    assert_eq!(server.recorded().len(), 3);
    assert_authenticated(&server);
}

#[tokio::test]
async fn compatible_site_without_billing_endpoints_has_no_summary() {
    let server = MockServer::start(healthy_authenticated).await;
    let site = compatible_site(&server);
    let summary = prober()
        .account_summary(&site, &site.lines[0], &key())
        .await
        .unwrap();
    assert_eq!(summary, None);
    assert_eq!(server.count(WHOAMI), 0, "compatible sites have no whoami");
    assert_eq!(
        server
            .recorded()
            .iter()
            .map(|r| r.path.as_str())
            .collect::<Vec<_>>(),
        [BILLING_SUBSCRIPTION, BILLING_SUBSCRIPTION_NO_V1]
    );
    assert_authenticated(&server);
}

macro_rules! assert_no_key {
    ($value:expr) => {{
        let value = &$value;
        assert!(
            !format!("{value:?}").contains(KEY),
            "key leaked through Debug"
        );
        assert!(
            !serde_json::to_string(value).unwrap().contains(KEY),
            "key leaked through serialization"
        );
    }};
}

fn assert_error_redacted(error: &Error) {
    assert_no_key!(error);
    assert!(
        !error.to_string().contains(KEY),
        "key leaked through Error::Display"
    );
}

fn assert_caps_redacted(caps: &superba_core::LineCaps) {
    assert_no_key!(caps);
    if let Some(detail) = &caps.detail {
        assert!(!detail.to_string().contains(KEY));
    }
    if let Some(access) = &caps.key_access {
        assert!(!access.text().to_string().contains(KEY));
    }
}

fn echoed_401() -> MockResponse {
    MockResponse::json(
        401,
        json!({
            "type": "error",
            "error": {"type": "unauthorized_error", "message": format!("rejected {KEY}")},
        }),
    )
}

#[tokio::test]
async fn echoed_401_is_redacted_from_key_check() {
    let server = MockServer::start(|_| echoed_401()).await;
    let site = site(&server);
    let check = prober()
        .check_key(&site, &site.lines[0], AuthMode::Pat, &key())
        .await
        .unwrap();
    assert_eq!(check.access, KeyAccess::InvalidKey);
    assert!(matches!(
        check.whoami,
        Some(AuthApiHealth::Unhealthy { .. })
    ));
    assert_no_key!(check);
    assert!(!check.access.text().to_string().contains(KEY));
}

#[tokio::test]
async fn echoed_401_is_redacted_from_authapi_health() {
    let server = MockServer::start(|_| echoed_401()).await;
    let site = site(&server);
    let health = prober()
        .check_authapi(&site, &site.lines[0].origin, &key())
        .await
        .unwrap();
    assert!(matches!(health, AuthApiHealth::Unhealthy { .. }));
    assert_no_key!(health);
}

async fn assert_echoed_summary_error_redacted(compatible: bool) {
    let server = MockServer::start(|_| echoed_401()).await;
    let site = if compatible {
        compatible_site(&server)
    } else {
        site(&server)
    };
    let error = prober()
        .account_summary(&site, &site.lines[0], &key())
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Http { status: 401, .. }));
    assert_error_redacted(&error);
    assert_eq!(server.count(WHOAMI), usize::from(!compatible));
    assert_eq!(
        server.count(BILLING_SUBSCRIPTION),
        1,
        "a 401 is final: no fallback path"
    );
    assert_eq!(server.recorded().len(), if compatible { 1 } else { 2 });
}

#[tokio::test]
async fn echoed_401_is_redacted_from_whoami_summary_error() {
    assert_echoed_summary_error_redacted(false).await;
}

#[tokio::test]
async fn echoed_401_is_redacted_from_billing_summary_error() {
    assert_echoed_summary_error_redacted(true).await;
}

#[tokio::test]
async fn echoed_401_is_redacted_from_models_error() {
    let server = MockServer::start(|_| echoed_401()).await;
    let error = prober()
        .list_models(&endpoints(&server).openai_base_url, &key())
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Http { status: 401, .. }));
    assert_error_redacted(&error);
}

#[tokio::test]
async fn echoed_whoami_401_is_redacted_from_caps_and_localized_arguments() {
    let server = MockServer::start(|r| {
        if r.header("authorization").is_none() {
            relay_401()
        } else if r.path == WHOAMI {
            echoed_401()
        } else {
            healthy_authenticated(r)
        }
    })
    .await;
    let site = site(&server);
    let caps = prober()
        .probe_with_key(&site, &site.lines[0], &key())
        .await
        .unwrap();
    assert!(!caps.pat_ok);
    assert_eq!(caps.key_access, Some(KeyAccess::Ok));
    assert_caps_redacted(&caps);
    assert_detail(
        &caps,
        "probe.pat_unavailable",
        &[
            ("endpoint", WHOAMI),
            ("status", "HTTP 401: rejected [redacted]"),
        ],
    );
}

#[tokio::test]
async fn echoed_models_error_is_redacted_from_unknown_key_access_and_caps() {
    let server = MockServer::start(|r| {
        if r.header("authorization").is_none() {
            relay_401()
        } else if r.path == BACKEND_MODELS {
            denial(500, &format!("upstream rejected {KEY}"))
        } else {
            healthy_authenticated(r)
        }
    })
    .await;
    let site = site(&server);
    let prober = prober();
    let check = prober
        .check_key(&site, &site.lines[0], AuthMode::ApiKey, &key())
        .await
        .unwrap();
    let caps = prober
        .probe_with_key(&site, &site.lines[0], &key())
        .await
        .unwrap();
    assert!(matches!(
        check.access,
        KeyAccess::Unknown {
            status: Some(500),
            ..
        }
    ));
    assert_no_key!(check);
    assert!(!check.access.text().to_string().contains(KEY));
    assert_eq!(caps.key_access, Some(check.access));
    assert_caps_redacted(&caps);
}

#[tokio::test]
async fn echoed_wham_error_is_redacted_from_key_check_and_caps() {
    let server = MockServer::start(|r| {
        if r.header("authorization").is_none() {
            relay_401()
        } else if r.path == WHAM {
            denial(403, &format!("rejected {KEY}"))
        } else {
            healthy_authenticated(r)
        }
    })
    .await;
    let site = site(&server);
    let prober = prober();
    let check = prober
        .check_key(&site, &site.lines[0], AuthMode::Pat, &key())
        .await
        .unwrap();
    let caps = prober
        .probe_with_key(&site, &site.lines[0], &key())
        .await
        .unwrap();
    assert!(matches!(
        check.access,
        KeyAccess::Unknown {
            status: Some(403),
            ..
        }
    ));
    assert_no_key!(check);
    assert!(!check.access.text().to_string().contains(KEY));
    assert_caps_redacted(&caps);
    assert_detail(
        &caps,
        "probe.pat_unavailable",
        &[
            ("endpoint", WHAM),
            ("status", "HTTP 403: rejected [redacted]"),
        ],
    );
}

#[tokio::test]
async fn echoed_401_is_redacted_from_responses_probe() {
    let server = MockServer::start(|_| echoed_401()).await;
    let probe = prober()
        .probe_responses(&endpoints(&server), &key(), "gpt-x")
        .await
        .unwrap();
    assert!(!probe.ok);
    assert_eq!(probe.status, Some(401));
    assert_no_key!(probe);
    assert_eq!(
        probe.detail.as_deref(),
        Some("HTTP 401: rejected [redacted]")
    );
}

#[tokio::test]
async fn echoed_sse_failure_is_redacted_from_responses_probe() {
    let server = MockServer::start(|_| MockResponse::sse(format!(
        "event: response.failed\ndata: {}\n\n",
        json!({"type": "response.failed", "response": {"error": {"message": format!("rejected {KEY}")}}})
    ))).await;
    let probe = prober()
        .probe_responses(&endpoints(&server), &key(), "gpt-x")
        .await
        .unwrap();
    assert!(!probe.ok);
    assert_eq!(probe.status, Some(200));
    assert_no_key!(probe);
    assert_eq!(probe.detail.as_deref(), Some("rejected [redacted]"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeouts_do_not_leak_keys_in_results_or_errors() {
    let server = MockServer::start(|r| {
        if r.header("authorization").is_none() {
            relay_401()
        } else {
            echoed_401().with_delay(Duration::from_secs(3))
        }
    })
    .await;
    let site = site(&server);
    let line = &site.lines[0];
    let prober = prober();
    let key = key();
    let endpoints = endpoints(&server);
    let (check, caps, models, responses, summary, health) = tokio::join!(
        prober.check_key(&site, line, AuthMode::Pat, &key),
        prober.probe_with_key(&site, line, &key),
        prober.list_models(&endpoints.openai_base_url, &key),
        prober.probe_responses(&endpoints, &key, "gpt-x"),
        prober.account_summary(&site, line, &key),
        prober.check_authapi(&site, &line.origin, &key),
    );
    let check = check.unwrap();
    assert_eq!(check.access, KeyAccess::Unreachable);
    assert_no_key!(check);
    let caps = caps.unwrap();
    assert_eq!(caps.key_access, Some(KeyAccess::Unreachable));
    assert_caps_redacted(&caps);
    assert_detail(
        &caps,
        "probe.pat_unavailable",
        &[
            ("endpoint", WHOAMI),
            ("status", "no answer before the timeout"),
        ],
    );
    for error in [models.unwrap_err(), summary.unwrap_err()] {
        assert!(matches!(error, Error::Timeout { .. }));
        assert_error_redacted(&error);
    }
    let responses = responses.unwrap();
    assert!(!responses.ok);
    assert_eq!(responses.status, None);
    assert_eq!(
        responses.detail.as_deref(),
        Some("no complete answer before the timeout")
    );
    assert_no_key!(responses);
    let health = health.unwrap();
    assert!(matches!(health, AuthApiHealth::Unhealthy { .. }));
    assert_no_key!(health);
}

#[tokio::test]
async fn edge_blocks_do_not_leak_keys_even_when_the_header_echoes_them() {
    let server = MockServer::start(|r| {
        if r.header("authorization").is_none() {
            relay_401()
        } else {
            edge_block().with_header("l-block", format!("Filter|{KEY}"))
        }
    })
    .await;
    let site = site(&server);
    let line = &site.lines[0];
    let prober = prober();
    let key = key();
    let check = prober
        .check_key(&site, line, AuthMode::Pat, &key)
        .await
        .unwrap();
    assert_eq!(check.access, KeyAccess::EdgeBlocked);
    assert_no_key!(check);
    let caps = prober.probe_with_key(&site, line, &key).await.unwrap();
    assert_eq!(caps.key_access, Some(KeyAccess::EdgeBlocked));
    assert_caps_redacted(&caps);
    assert_detail(&caps, "probe.pat_edge_blocked", &[("endpoint", WHOAMI)]);
    let endpoints = endpoints(&server);
    let models = prober
        .list_models(&endpoints.openai_base_url, &key)
        .await
        .unwrap_err();
    let summary = prober.account_summary(&site, line, &key).await.unwrap_err();
    for error in [models, summary] {
        assert!(matches!(error, Error::Http { status: 200, .. }));
        assert_error_redacted(&error);
    }
    let responses = prober
        .probe_responses(&endpoints, &key, "gpt-x")
        .await
        .unwrap();
    assert!(!responses.ok);
    assert_eq!(responses.status, Some(200));
    assert_no_key!(responses);
    let health = prober
        .check_authapi(&site, &line.origin, &key)
        .await
        .unwrap();
    assert!(matches!(health, AuthApiHealth::Unhealthy { .. }));
    assert_no_key!(health);
}

#[tokio::test]
async fn probe_with_key_restores_pat_after_an_unauthenticated_whoami_failure() {
    // Unauthenticated whoami fails (as a transient upstream error would), while
    // the authenticated requests Codex actually sends all work.
    let server = MockServer::start(|r| {
        if r.header("authorization").is_some() {
            healthy_authenticated(r)
        } else if r.path == WHOAMI {
            MockResponse::json(502, json!({"error": {"message": "bad gateway"}}))
        } else {
            relay_401()
        }
    })
    .await;
    let site = site(&server);
    let prober = prober();
    let unauth = prober.probe(&site, &site.lines[0]).await.unwrap();
    assert!(!unauth.pat_ok, "the unauthenticated view alone says no");
    let caps = prober
        .probe_with_key(&site, &site.lines[0], &key())
        .await
        .unwrap();
    assert!(caps.pat_ok, "{caps:?}");
    assert_eq!(caps.key_access, Some(superba_core::KeyAccess::Ok));
    assert!(caps.allows(superba_core::AuthMode::Pat));
    assert_detail(&caps, "probe.ok", &[]);
}

#[tokio::test]
async fn pat_needs_an_https_line_even_when_its_endpoints_answer() {
    let server = MockServer::start(|r| {
        if r.header("authorization").is_some() {
            healthy_authenticated(r)
        } else {
            relay_401()
        }
    })
    .await;
    let site = site(&server);
    let prober = HttpLineProber::new(ProbeConfig {
        pat_requires_https: true,
        ..probe_config()
    });
    for caps in [
        prober.probe(&site, &site.lines[0]).await.unwrap(),
        prober
            .probe_with_key(&site, &site.lines[0], &key())
            .await
            .unwrap(),
    ] {
        assert!(!caps.pat_ok, "{caps:?}");
        assert!(!caps.allows(superba_core::AuthMode::Pat));
        assert!(caps.allows(superba_core::AuthMode::ApiKey));
        assert_detail(&caps, "probe.pat_needs_https", &[]);
    }
}
