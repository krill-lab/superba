mod support;

use std::time::{Duration, Instant};

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use superba_core::{Error, LineProber};
use url::Url;

use support::*;

#[tokio::test]
async fn list_models_reads_backend_catalog_with_client_version_and_bearer_key() {
    let server = MockServer::start(|r| {
        if r.path == BACKEND_MODELS {
            MockResponse::json(
                200,
                json!({"models": [
                    {"slug": "a", "display_name": "A"}, {"slug": "b"},
                ]}),
            )
        } else {
            MockResponse::empty(404)
        }
    })
    .await;
    let prober = prober();
    let base = endpoints(&server).openai_base_url;
    let models = prober.list_models(&base, &key()).await.unwrap();

    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(models[0].display_name.as_deref(), Some("A"));
    assert_eq!(models[1].display_name, None);
    let requests = server.recorded();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, BACKEND_MODELS);
    assert!(requests[0].body.is_empty());
    assert_client_version(&requests[0], &prober.config().client_version);
    assert_authenticated(&server);
}

#[tokio::test]
async fn list_models_reads_openai_catalog_on_codex_v1_without_client_version() {
    let server = MockServer::start(|r| {
        if r.path == CODEX_V1_MODELS {
            MockResponse::json(
                200,
                json!({"object": "list", "data": [{"id": "a"}, {"id": "b"}]}),
            )
        } else {
            MockResponse::empty(404)
        }
    })
    .await;
    let base = Url::parse(&format!("{}/codex/v1", server.origin())).unwrap();
    let models = prober().list_models(&base, &key()).await.unwrap();

    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert!(models.iter().all(|m| m.display_name.is_none()));
    let requests = server.recorded();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, CODEX_V1_MODELS);
    assert_eq!(requests[0].query, None);
    assert_authenticated(&server);
}

#[tokio::test]
async fn list_models_preserves_forbidden_status_and_message() {
    let server =
        MockServer::start(|_| MockResponse::json(403, json!({"error": {"message": "denied"}})))
            .await;
    let base = endpoints(&server).openai_base_url;
    let error = prober().list_models(&base, &key()).await.unwrap_err();
    assert_eq!(
        error,
        Error::Http {
            url: format!("{}{BACKEND_MODELS}", server.origin()),
            status: 403,
            message: "denied".into(),
        }
    );
    assert_authenticated(&server);
}

#[tokio::test]
async fn list_models_rejects_edge_interception_even_with_status_200() {
    let server = MockServer::start(|_| edge_block()).await;
    let error = prober()
        .list_models(&endpoints(&server).openai_base_url, &key())
        .await
        .unwrap_err();
    assert_eq!(
        error,
        Error::Http {
            url: format!("{}{BACKEND_MODELS}", server.origin()),
            status: 200,
            message: "intercepted by the line's edge (HTTP 200 with an l-block header)".into(),
        }
    );
}

#[tokio::test]
async fn list_models_treats_invalid_json_200_as_an_interception() {
    let server = MockServer::start(|_| {
        MockResponse {
            body: b"{not valid JSON".to_vec(),
            ..MockResponse::empty(200)
        }
        .with_header("Content-Type", "application/json")
    })
    .await;
    let error = prober()
        .list_models(&endpoints(&server).openai_base_url, &key())
        .await
        .unwrap_err();
    // JSON endpoint interception is checked before catalog parsing; a 2xx body
    // that is not JSON is classified as an edge answer, even with a JSON header.
    assert_eq!(
        error,
        Error::Http {
            url: format!("{}{BACKEND_MODELS}", server.origin()),
            status: 200,
            message: "intercepted by the line's edge (HTTP 200 with a non-JSON body)".into(),
        }
    );
}

#[tokio::test]
async fn list_models_reports_parse_error_for_json_without_a_catalog() {
    let server = MockServer::start(|_| MockResponse::json(200, json!({"unexpected": []}))).await;
    let error = prober()
        .list_models(&endpoints(&server).openai_base_url, &key())
        .await
        .unwrap_err();
    assert_eq!(
        error,
        Error::Parse {
            what: "model catalog".into(),
            message: "models answer has neither `models` nor `data`".into(),
        }
    );
}

#[tokio::test]
async fn list_models_refuses_non_loopback_http_before_network_io() {
    // Input validation rejects this URL before DNS or an HTTP connection.
    let base = Url::parse("http://example.com/v1").unwrap();
    let error = prober().list_models(&base, &key()).await.unwrap_err();
    assert_eq!(
        error,
        Error::Invalid {
            message: "refusing to send a key over a non-https URL: http://example.com/v1".into(),
        }
    );
}

#[tokio::test]
async fn list_models_never_follows_redirects_to_another_origin() {
    let destination =
        MockServer::start(|_| MockResponse::json(200, json!({"models": [{"slug": "redirected"}]})))
            .await;
    let location = format!("{}{BACKEND_MODELS}", destination.origin());
    let server =
        MockServer::start(move |_| MockResponse::empty(302).with_header("Location", &location))
            .await;
    let error = prober()
        .list_models(&endpoints(&server).openai_base_url, &key())
        .await
        .unwrap_err();

    assert!(matches!(error, Error::Http { status: 302, .. }));
    assert_eq!(server.count(BACKEND_MODELS), 1);
    assert_authenticated(&server);
    assert!(destination.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn responses_completed_stream_and_compact_succeed_with_codex_post_bodies() {
    let server = MockServer::start(healthy_authenticated).await;
    let probe = prober()
        .probe_responses(&endpoints(&server), &key(), "gpt-x")
        .await
        .unwrap();

    assert!(probe.ok);
    assert_eq!(probe.status, Some(200));
    assert!(probe.latency_ms.is_some());
    assert_eq!(probe.compact_ok, Some(true));
    assert_eq!(probe.detail, None);
    let requests = server.recorded();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].path, RESPONSES);
    assert_eq!(requests[1].path, COMPACT);
    assert_eq!(requests[0].peer_addr, requests[1].peer_addr);
    for request in &requests {
        assert_eq!(request.method, "POST");
        assert_eq!(request.query, None);
        assert_eq!(request.header("content-type"), Some("application/json"));
        assert_eq!(
            request
                .header("content-length")
                .unwrap()
                .parse::<usize>()
                .unwrap(),
            request.body.len()
        );
    }
    assert_authenticated(&server);
    assert_eq!(requests[0].header("accept"), Some("text/event-stream"));
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["model"], "gpt-x");
    assert_eq!(body["stream"], true);
    assert_eq!(body["store"], false);
    assert_eq!(body["input"][0]["role"], "user");
    assert_eq!(body["input"][0]["content"][0]["text"], "ping");
    let compact: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(requests[1].header("accept"), Some("application/json"));
    assert_eq!(compact["model"], "gpt-x");
    assert_eq!(compact["input"].as_array().unwrap().len(), 2);
    assert!(compact.get("stream").is_none());
}

#[tokio::test]
async fn responses_failed_event_reports_its_error_and_skips_compact() {
    let server = MockServer::start(|r| {
        if r.path == RESPONSES {
            MockResponse::sse("event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"model not found\"}}}\n\n")
        } else {
            healthy_authenticated(r)
        }
    }).await;
    let probe = prober()
        .probe_responses(&endpoints(&server), &key(), "gpt-x")
        .await
        .unwrap();
    assert!(!probe.ok);
    assert_eq!(probe.status, Some(200));
    assert!(probe.latency_ms.is_some());
    assert_eq!(probe.detail.as_deref(), Some("model not found"));
    assert_eq!(probe.compact_ok, None);
    assert_eq!(server.count(RESPONSES), 1);
    assert_eq!(server.count(COMPACT), 0);
    assert_authenticated(&server);
}

#[tokio::test]
async fn responses_401_reports_failure_and_skips_compact() {
    let server = MockServer::start(|_| relay_401()).await;
    let probe = prober()
        .probe_responses(&endpoints(&server), &key(), "gpt-x")
        .await
        .unwrap();
    assert!(!probe.ok);
    assert_eq!(probe.status, Some(401));
    assert!(probe.latency_ms.is_some());
    assert_eq!(probe.compact_ok, None);
    assert_eq!(
        probe.detail.as_deref(),
        Some("HTTP 401: missing Authorization")
    );
    assert_eq!(server.count(COMPACT), 0);
    assert_authenticated(&server);
}

#[tokio::test]
async fn responses_success_with_missing_compact_reports_compact_false() {
    let server = MockServer::start(|r| {
        if r.path == COMPACT {
            MockResponse::empty(404)
        } else {
            healthy_authenticated(r)
        }
    })
    .await;
    let probe = prober()
        .probe_responses(&endpoints(&server), &key(), "gpt-x")
        .await
        .unwrap();
    assert!(probe.ok);
    assert_eq!(probe.status, Some(200));
    assert!(probe.latency_ms.is_some());
    assert_eq!(probe.compact_ok, Some(false));
    assert_eq!(probe.detail, None);
    assert_eq!(server.count(RESPONSES), 1);
    assert_eq!(server.count(COMPACT), 1);
    assert_authenticated(&server);
}

#[tokio::test]
async fn responses_rejects_an_empty_model_without_sending_a_request() {
    let server = MockServer::start(healthy_authenticated).await;
    for model in ["", " \t "] {
        let error = prober()
            .probe_responses(&endpoints(&server), &key(), model)
            .await
            .unwrap_err();
        assert!(matches!(error, Error::Invalid { .. }));
    }
    assert!(server.recorded().is_empty());
}

#[tokio::test]
async fn responses_timeout_reports_no_complete_answer_and_skips_compact() {
    let server = MockServer::start(|r| {
        if r.path == RESPONSES {
            MockResponse::sse(COMPLETED_SSE).with_delay(Duration::from_secs(3))
        } else {
            healthy_authenticated(r)
        }
    })
    .await;
    let prober = prober();
    let endpoints = endpoints(&server);
    let key = key();
    let started = Instant::now();
    let probe = tokio::time::timeout(
        Duration::from_millis(2750),
        prober.probe_responses(&endpoints, &key, "gpt-x"),
    )
    .await
    .expect("Responses should respect its configured timeout")
    .unwrap();

    assert!(started.elapsed() < Duration::from_millis(2750));
    assert!(!probe.ok);
    assert_eq!(probe.status, None);
    assert_eq!(probe.latency_ms, None);
    assert_eq!(probe.compact_ok, None);
    assert_eq!(
        probe.detail.as_deref(),
        Some("no complete answer before the timeout")
    );
    assert_eq!(server.count(RESPONSES), 1);
    assert_eq!(server.count(COMPACT), 0);
    assert_authenticated(&server);
}

#[tokio::test]
async fn responses_stream_held_open_after_completion_still_succeeds() {
    let server = MockServer::start(|r| match r.path.as_str() {
        RESPONSES => MockResponse::sse(COMPLETED_SSE).held_open(),
        COMPACT => MockResponse::json(200, json!({"output": []})),
        _ => MockResponse::empty(404),
    })
    .await;
    let started = Instant::now();
    let probe = prober()
        .probe_responses(&endpoints(&server), &key(), "gpt-x")
        .await
        .unwrap();
    assert!(probe.ok, "{probe:?}");
    assert_eq!(probe.compact_ok, Some(true));
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "the probe must stop reading after the terminal event, not wait for the timeout"
    );
}

#[tokio::test]
async fn responses_refuse_a_base_url_off_the_endpoints_origin() {
    let declared = MockServer::start(healthy_authenticated).await;
    let other = MockServer::start(healthy_authenticated).await;
    let mut endpoints = endpoints(&declared);
    endpoints.openai_base_url =
        Url::parse(&format!("{}/backend-api/codex", other.origin())).unwrap();
    let error = prober()
        .probe_responses(&endpoints, &key(), "gpt-x")
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Invalid { .. }), "{error:?}");
    assert!(declared.recorded().is_empty());
    assert!(other.recorded().is_empty());
}

#[tokio::test]
async fn list_models_refuses_a_base_url_carrying_the_key() {
    let server = MockServer::start(healthy_authenticated).await;
    for base in [
        format!("{}/v1?api_key={KEY}", server.origin()),
        format!("{}/{KEY}/v1", server.origin()),
    ] {
        let error = prober()
            .list_models(&Url::parse(&base).unwrap(), &key())
            .await
            .unwrap_err();
        assert!(matches!(error, Error::Invalid { .. }), "{error:?}");
        assert!(!error.to_string().contains(KEY));
        assert!(!format!("{error:?}").contains(KEY));
    }
    assert!(server.recorded().is_empty());
}
