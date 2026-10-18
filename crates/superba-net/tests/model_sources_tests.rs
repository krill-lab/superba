//! Model lists by explicit source (user bug 79, design §2a): the Codex list
//! and the standard `/v1/models` list (OpenAI and Anthropic shapes, Anthropic
//! paging with `after_id`), read alone or both at once and merged by id (the
//! Codex metadata wins), each with its own cache entry, a failing list
//! reported while the other is used; Anthropic endpoints get `x-api-key` and
//! `anthropic-version`; the messages check.

mod support;

use pretty_assertions::assert_eq;
use serde_json::json;
use superba_core::{
    AnthropicAuth, AnthropicEndpoint, LineProber, MessagesProbeRequest, ModelList, ModelListKind,
    ModelListRequest, ModelSource, Site,
};
use superba_net::HttpLineProber;
use url::Url;

use support::*;

fn native_body() -> serde_json::Value {
    json!({"models": [
        {"slug": "gpt-6-sol", "display_name": "GPT-6-Sol", "context_window": 500000,
         "supported_reasoning_levels": [{"effort": "high"}, {"effort": "xhigh"}],
         "visibility": "list"},
        {"slug": "gpt-6-luna", "visibility": "list"}
    ]})
}

fn openai_body() -> serde_json::Value {
    json!({"object": "list", "data": [
        {"id": "gpt-6-sol", "object": "model", "owned_by": "system"},
        {"id": "deepseek-v4", "object": "model", "owned_by": "system"},
        {"id": "claude-sonnet-5-5", "object": "model"}
    ]})
}

async fn list(
    prober: &HttpLineProber,
    site: &Site,
    base: &Url,
    source: ModelSource,
    standard: Option<&Url>,
) -> ModelList {
    let key = key();
    prober
        .model_list(ModelListRequest {
            site,
            base_url: base,
            key: &key,
            client_version: None,
            refresh: false,
            source: Some(source),
            standard_base: standard,
        })
        .await
        .unwrap()
}

fn ids(l: &ModelList) -> Vec<(&str, Vec<ModelListKind>)> {
    l.models
        .iter()
        .map(|m| (m.slug.as_str(), m.lists.clone()))
        .collect()
}

#[tokio::test]
async fn auto_merges_both_lists_and_keeps_the_codex_metadata() {
    let server = MockServer::start(|r| match r.path.as_str() {
        BACKEND_MODELS => MockResponse::json(200, native_body()),
        MODELS => MockResponse::json(200, openai_body()),
        _ => MockResponse::empty(404),
    })
    .await;
    let prober = prober();
    let base = endpoints(&server).openai_base_url;
    let l = list(&prober, &site(&server), &base, ModelSource::Auto, None).await;
    use ModelListKind::{Codex, Standard};
    assert_eq!(
        ids(&l),
        vec![
            ("gpt-6-sol", vec![Codex, Standard]),
            ("gpt-6-luna", vec![Codex]),
            ("deepseek-v4", vec![Standard]),
            ("claude-sonnet-5-5", vec![Standard]),
        ]
    );
    let sol = l.model("gpt-6-sol").unwrap();
    assert_eq!(sol.display_name.as_deref(), Some("GPT-6-Sol"));
    assert_eq!(sol.context_window, Some(500_000));
    assert_eq!(sol.reasoning_levels.len(), 2);
    assert_eq!(l.sources.len(), 2);
    assert!(l.sources.iter().all(|s| s.error.is_none()));
    assert_eq!(l.sources[0].count, 2);
    assert_eq!(l.sources[1].count, 3);
    // Each list once; a second read comes from each list's own cache entry.
    let again = list(&prober, &site(&server), &base, ModelSource::Auto, None).await;
    assert_eq!(ids(&again), ids(&l));
    assert_eq!(server.count(BACKEND_MODELS), 1);
    assert_eq!(server.count(MODELS), 1);
    // Only one list: the other is not asked.
    let codex = list(&prober, &site(&server), &base, ModelSource::Codex, None).await;
    assert_eq!(codex.models.len(), 2);
    assert_eq!(codex.sources.len(), 1);
    let standard = list(&prober, &site(&server), &base, ModelSource::Standard, None).await;
    assert_eq!(standard.models.len(), 3);
    assert_eq!(server.count(BACKEND_MODELS), 1);
    assert_eq!(server.count(MODELS), 1);
}

#[tokio::test]
async fn one_failing_list_is_reported_and_the_other_used() {
    let server = MockServer::start(|r| match r.path.as_str() {
        BACKEND_MODELS => MockResponse::json(500, json!({"error": {"message": "boom"}})),
        MODELS => MockResponse::json(200, openai_body()),
        _ => MockResponse::empty(404),
    })
    .await;
    let prober = prober();
    let base = endpoints(&server).openai_base_url;
    let l = list(&prober, &site(&server), &base, ModelSource::Auto, None).await;
    assert_eq!(l.models.len(), 3);
    let codex = l
        .sources
        .iter()
        .find(|s| s.kind == ModelListKind::Codex)
        .unwrap();
    let err = codex.error.as_ref().expect("the Codex list failed");
    assert!(err.to_string().contains("boom"), "{err}");
    let standard = l
        .sources
        .iter()
        .find(|s| s.kind == ModelListKind::Standard)
        .unwrap();
    assert!(standard.error.is_none());
    assert_eq!(standard.path.as_deref(), Some("/v1/models"));
}

#[tokio::test]
async fn anthropic_lists_are_paged_and_asked_with_anthropic_headers() {
    let server = MockServer::start(|r| {
        if r.path != "/anthropic/v1/models" {
            return MockResponse::empty(404);
        }
        let query = r.query.clone().unwrap_or_default();
        if query.contains("after_id=m2") {
            MockResponse::json(
                200,
                json!({"data": [{"id": "m3", "display_name": "Model 3", "type": "model",
                                  "created_at": "2026-01-01T00:00:00Z"}],
                       "has_more": false, "first_id": "m3", "last_id": "m3"}),
            )
        } else {
            MockResponse::json(
                200,
                json!({"data": [
                    {"id": "m1", "display_name": "Model 1", "type": "model", "created_at": "2026-01-01T00:00:00Z"},
                    {"id": "m2", "display_name": "Model 2", "type": "model", "created_at": "2026-01-01T00:00:00Z"}],
                       "has_more": true, "first_id": "m1", "last_id": "m2"}),
            )
        }
    })
    .await;
    let prober = prober();
    let anthropic = Url::parse(&format!("{}/anthropic", server.origin())).unwrap();
    let site = compatible_site(&server);
    let base = Url::parse(&format!("{}/v1", server.origin())).unwrap();
    let l = list(
        &prober,
        &site,
        &base,
        ModelSource::Standard,
        Some(&anthropic),
    )
    .await;
    assert_eq!(
        l.models
            .iter()
            .map(|m| (m.slug.as_str(), m.display_name.as_deref()))
            .collect::<Vec<_>>(),
        vec![
            ("m1", Some("Model 1")),
            ("m2", Some("Model 2")),
            ("m3", Some("Model 3"))
        ]
    );
    let recorded = server.recorded();
    assert_eq!(recorded.len(), 2);
    for r in &recorded {
        assert_eq!(r.header("x-api-key"), Some(KEY));
        assert_eq!(r.header("anthropic-version"), Some("2023-06-01"));
        assert_eq!(
            r.header("authorization").map(str::to_owned),
            Some(format!("Bearer {KEY}"))
        );
    }
    assert!(
        recorded[1]
            .query
            .as_deref()
            .is_some_and(|q| q.contains("after_id=m2")),
        "{:?}",
        recorded[1].query
    );
    // A site without a Codex list reports that and keeps the standard list.
    let auto = list(&prober, &site, &base, ModelSource::Auto, Some(&anthropic)).await;
    assert_eq!(auto.models.len(), 3);
    let codex = auto
        .sources
        .iter()
        .find(|s| s.kind == ModelListKind::Codex)
        .unwrap();
    assert!(codex.error.is_some());
}

#[tokio::test]
async fn the_key_goes_only_to_the_site_or_the_vouched_endpoint() {
    let server = MockServer::start(|_| MockResponse::json(200, openai_body())).await;
    let other = MockServer::start(|_| MockResponse::json(200, openai_body())).await;
    let prober = prober();
    let site = compatible_site(&server);
    let base = Url::parse(&format!("{}/v1", other.origin())).unwrap();
    let key = key();
    let err = prober
        .model_list(ModelListRequest {
            site: &site,
            base_url: &base,
            key: &key,
            client_version: None,
            refresh: false,
            source: Some(ModelSource::Standard),
            standard_base: None,
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("refusing"), "{err}");
    assert_eq!(other.recorded().len(), 0);
}

#[tokio::test]
async fn the_messages_check_sends_one_token_the_way_claude_code_does() {
    let server = MockServer::start(|r| {
        if r.path == "/api/anthropic/v1/messages" {
            MockResponse::json(
                200,
                json!({"id": "msg_1", "type": "message", "role": "assistant",
                       "content": [{"type": "text", "text": "OK"}], "stop_reason": "max_tokens"}),
            )
        } else {
            MockResponse::json(404, json!({"error": {"message": "no route"}}))
        }
    })
    .await;
    let prober = prober();
    let key = key();
    let mut endpoint =
        AnthropicEndpoint::new(Url::parse(&format!("{}/api/anthropic", server.origin())).unwrap());
    let p = prober
        .probe_messages(MessagesProbeRequest {
            endpoint: &endpoint,
            key: &key,
            model: "gpt-6-sol",
        })
        .await
        .unwrap();
    assert!(p.ok, "{p:?}");
    assert_eq!(p.status, Some(200));
    let r = &server.recorded()[0];
    assert_eq!(r.method, "POST");
    assert_eq!(
        r.header("authorization").map(str::to_owned),
        Some(format!("Bearer {KEY}"))
    );
    assert_eq!(r.header("x-api-key"), None);
    assert_eq!(r.header("anthropic-version"), Some("2023-06-01"));
    let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(body["max_tokens"], 1);
    assert_eq!(body["model"], "gpt-6-sol");
    // x-api-key style; a failing endpoint reports its error without the key.
    endpoint.auth = AnthropicAuth::ApiKey;
    endpoint.base_url = Url::parse(&format!("{}/wrong", server.origin())).unwrap();
    let p = prober
        .probe_messages(MessagesProbeRequest {
            endpoint: &endpoint,
            key: &key,
            model: "gpt-6-sol",
        })
        .await
        .unwrap();
    assert!(!p.ok);
    assert_eq!(p.status, Some(404));
    assert_eq!(p.message.as_deref(), Some("no route"));
    let r = &server.recorded()[1];
    assert_eq!(r.header("authorization"), None);
    assert_eq!(r.header("x-api-key"), Some(KEY));
}
