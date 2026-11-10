//! Explicit HTTP consent, exercised against the existing local mock upstream.
#![allow(missing_docs)]
mod support;

use serde_json::json;
use superba_core::{
    AuthMode, LineProber, MessagesProbeRequest, ModelListRequest, ModelSource, ResolvedEndpoints,
    Secret,
};
use support::*;
use url::Url;

fn base(server: &MockServer) -> Url {
    // IPv4-mapped IPv6 reaches the local fixture without external DNS/network,
    // but is deliberately not classified as a loopback URL by the safety rule.
    Url::parse(&format!(
        "{}/v1",
        server.origin().replace("127.0.0.1", "[::ffff:127.0.0.1]")
    ))
    .unwrap()
}

#[tokio::test]
async fn unconfirmed_http_sends_no_keys_and_consent_enables_every_probe() {
    let server = MockServer::start(|r| match r.path.as_str() {
        "/v1/models" => MockResponse::json(200, json!({"data": [{"id": "test-model"}]})),
        "/v1/responses" => MockResponse::sse(COMPLETED_SSE),
        "/v1/responses/compact" => MockResponse::json(200, json!({"output": []})),
        "/v1/messages" => MockResponse::json(200, json!({"type": "message", "content": []})),
        _ => MockResponse::empty(404),
    })
    .await;
    let base = base(&server);
    let mut site = superba_net::new_compatible_site("lan".into(), "LAN", &base).unwrap();
    assert!(!site.lines[0].origin.is_loopback());
    let prober = prober();
    let key = key();
    assert!(prober.list_models(&base, &key).await.is_err());
    assert!(
        prober
            .list_models_for_site(&site, &base, &key)
            .await
            .is_err()
    );
    assert!(
        prober
            .check_key(&site, &site.lines[0], AuthMode::ApiKey, &key)
            .await
            .is_err()
    );
    assert!(
        prober
            .account_summary(&site, &site.lines[0], &key)
            .await
            .is_err()
    );
    assert!(ResolvedEndpoints::for_site_base_url(&site, &base).is_err());
    let mut ep =
        ResolvedEndpoints::for_base_url(&Url::parse("https://example.com/v1").unwrap()).unwrap();
    ep.origin = site.lines[0].origin.clone();
    ep.openai_base_url = base.clone();
    assert!(
        prober
            .probe_responses(&ep, &key, "test-model")
            .await
            .is_err()
    );
    assert!(
        prober
            .probe_compaction(&ep, &key, "test-model")
            .await
            .is_err()
    );
    assert!(prober.probe_transports(&ep, &key).await.is_err());
    let endpoint = superba_core::AnthropicEndpoint::new(site.lines[0].origin.url().clone());
    assert!(
        prober
            .probe_messages(MessagesProbeRequest {
                site: Some(&site),
                endpoint: &endpoint,
                key: &key,
                model: "test-model"
            })
            .await
            .is_err()
    );
    assert!(server.recorded().is_empty());

    site.allow_plain_http = true;
    let ep = ResolvedEndpoints::for_site_base_url(&site, &base).unwrap();
    assert!(
        prober
            .check_key(&site, &site.lines[0], AuthMode::ApiKey, &key)
            .await
            .unwrap()
            .access
            .is_ok()
    );
    assert_eq!(
        prober
            .list_models_for_site(&site, &base, &key)
            .await
            .unwrap()[0]
            .id,
        "test-model"
    );
    for source in [None, Some(ModelSource::Standard)] {
        assert!(
            !prober
                .model_list(ModelListRequest {
                    site: &site,
                    base_url: &base,
                    key: &key,
                    client_version: None,
                    refresh: true,
                    source,
                    standard_base: None
                })
                .await
                .unwrap()
                .models
                .is_empty()
        );
    }
    assert!(
        prober
            .probe_responses(&ep, &key, "test-model")
            .await
            .unwrap()
            .ok
    );
    prober
        .probe_compaction(&ep, &key, "test-model")
        .await
        .unwrap();
    prober.probe_transports(&ep, &key).await.unwrap();
    prober
        .account_summary(&site, &site.lines[0], &key)
        .await
        .unwrap();
    assert!(
        prober
            .probe_messages(MessagesProbeRequest {
                site: Some(&site),
                endpoint: &endpoint,
                key: &key,
                model: "test-model"
            })
            .await
            .unwrap()
            .ok
    );
    let recorded = server.recorded();
    assert!(
        recorded
            .iter()
            .any(|r| r.header("upgrade") == Some("websocket")),
        "the WebSocket probe is a same-scheme HTTP Upgrade over http://"
    );
    assert!(
        recorded
            .iter()
            .any(|r| r.header("content-encoding") == Some("zstd"))
    );
    assert!(recorded.iter().all(|r| r.header("authorization") == Some(format!("Bearer {}", key.expose()).as_str())));
    let n = recorded.len();
    // Revocation takes effect even with populated model/account caches.
    site.allow_plain_http = false;
    assert!(
        prober
            .model_list(ModelListRequest {
                site: &site,
                base_url: &base,
                key: &key,
                client_version: None,
                refresh: false,
                source: None,
                standard_base: None
            })
            .await
            .is_err()
    );
    assert!(
        prober
            .account_summary(&site, &site.lines[0], &key)
            .await
            .is_err()
    );
    assert_eq!(server.recorded().len(), n);
}

#[tokio::test]
async fn http_consent_keeps_redirects_credentials_pat_and_other_origins_blocked() {
    let destination = MockServer::start(|_| MockResponse::empty(200)).await;
    let location = format!("{}/v1/models", destination.origin());
    let server =
        MockServer::start(move |_| MockResponse::empty(302).with_header("Location", &location))
            .await;
    let base = base(&server);
    let mut site = superba_net::new_compatible_site("lan".into(), "LAN", &base).unwrap();
    site.allow_plain_http = true;
    let p = prober();
    let key = Secret::new(KEY);
    assert!(p.list_models_for_site(&site, &base, &key).await.is_err());
    assert!(destination.recorded().is_empty());
    let mut foreign = base.clone();
    foreign.set_port(Some(base.port().unwrap() + 1)).unwrap();
    assert!(p.list_models_for_site(&site, &foreign, &key).await.is_err());
    let mut credentials = base.clone();
    credentials.set_username("user").unwrap();
    assert!(
        p.list_models_for_site(&site, &credentials, &key)
            .await
            .is_err()
    );
    let mut pat = ResolvedEndpoints::for_site_base_url(&site, &base).unwrap();
    pat.chatgpt_base_url = Some(base.clone());
    assert!(p.probe_responses(&pat, &key, "test-model").await.is_err());
    site.kind = superba_core::SiteKind::Krill;
    assert!(p.list_models_for_site(&site, &base, &key).await.is_err());
}

#[tokio::test]
async fn an_unconfirmed_plain_http_line_gets_no_request_at_all() {
    // Not even the unauthenticated latency and endpoint probes the providers
    // page and auto-probe run: the line stays unreachable without network I/O.
    let server = MockServer::start(|r| match r.path.as_str() {
        "/v1/models" => MockResponse::json(200, json!({"data": [{"id": "test-model"}]})),
        _ => MockResponse::empty(401),
    })
    .await;
    let base = base(&server);
    let mut site = superba_net::new_compatible_site("lan".into(), "LAN", &base).unwrap();
    let prober = prober();
    let line = site.lines[0].clone();
    let caps = prober.probe(&site, &line).await.unwrap();
    assert!(!caps.reachable, "{caps:?}");
    assert!(server.recorded().is_empty(), "{:?}", server.recorded());
    // Consent turns the same probe on.
    site.allow_plain_http = true;
    let caps = prober.probe(&site, &line).await.unwrap();
    assert!(caps.reachable, "{caps:?}");
    assert!(!server.recorded().is_empty());
    assert!(
        server
            .recorded()
            .iter()
            .all(|r| r.header("authorization").is_none())
    );
}
