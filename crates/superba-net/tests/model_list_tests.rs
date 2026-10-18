mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use pretty_assertions::assert_eq;
use serde_json::json;
use superba_core::{
    CodexVersion, Error, LineProber, ManualClock, ModelList, ModelListRequest, ModelListSource,
    Secret, Site,
};
use superba_net::{HttpLineProber, MODEL_LIST_TTL};
use url::Url;

use support::*;

fn native_body() -> serde_json::Value {
    json!({"models": [
        {"slug": "gpt-6-astra", "display_name": "GPT-6-Astra", "context_window": 272000,
         "max_context_window": 872000, "visibility": "list", "priority": 2,
         "service_tiers": [{"id": "priority", "name": "Fast", "description": "2x"},
                           {"id": "ultrafast", "name": "Ultrafast", "description": "4x"}],
         "prefer_websockets": true},
        {"slug": "gpt-6-luna", "visibility": "list", "priority": 4}
    ]})
}

fn backend_base(server: &MockServer) -> Url {
    endpoints(server).openai_base_url
}

async fn list(
    prober: &HttpLineProber,
    site: &Site,
    base: &Url,
    version: Option<&CodexVersion>,
    refresh: bool,
) -> superba_core::Result<ModelList> {
    let key = key();
    prober
        .model_list(ModelListRequest {
            site,
            base_url: base,
            key: &key,
            client_version: version,
            refresh,
            source: None,
            standard_base: None,
        })
        .await
}

#[tokio::test]
async fn codex_backend_lists_keep_native_entries_and_send_the_whole_client_version() {
    let server = MockServer::start(|r| {
        if r.path == BACKEND_MODELS {
            MockResponse::json(200, native_body()).with_header("ETag", "\"v1\"")
        } else {
            MockResponse::empty(404)
        }
    })
    .await;
    let prober = prober();
    let version = CodexVersion::parse("0.163.0-alpha.10.2").unwrap();
    let l = list(
        &prober,
        &site(&server),
        &backend_base(&server),
        Some(&version),
        false,
    )
    .await
    .unwrap();
    assert_eq!(
        l.source,
        ModelListSource::Upstream {
            path: "/backend-api/codex/models?client_version=0.163.0".into(),
            fetched_at: CHECKED_AT,
            etag: Some("\"v1\"".into()),
        }
    );
    assert_eq!(l.models.len(), 2);
    let astra = &l.models[0];
    assert_eq!(astra.context_window, Some(272_000));
    assert_eq!(astra.max_context_window, Some(872_000));
    assert_eq!(astra.service_tiers.len(), 2);
    assert_eq!(astra.raw.as_ref().unwrap()["prefer_websockets"], true);
    assert_eq!(l.fallback_reason, None);
    let requests = server.recorded();
    assert_eq!(requests.len(), 1);
    assert_client_version(&requests[0], "0.163.0");
    assert_authenticated(&server);
}

#[tokio::test]
async fn compatible_sites_list_openai_models_with_metadata_and_try_without_v1() {
    let server = MockServer::start(|r| match r.path.as_str() {
        "/models" => MockResponse::json(
            200,
            json!({"object": "list", "data": [
                {"id": "acme-coder", "context_length": 131072, "name": "Acme Coder"},
                {"id": "plain"}
            ]}),
        ),
        _ => MockResponse::empty(404),
    })
    .await;
    let site = compatible_site(&server);
    let base = Url::parse(&format!("{}/v1", server.origin())).unwrap();
    let l = list(&prober(), &site, &base, None, false).await.unwrap();
    assert_eq!(
        l.models.iter().map(|m| m.slug.as_str()).collect::<Vec<_>>(),
        ["acme-coder", "plain"]
    );
    assert_eq!(l.models[0].context_window, Some(131_072));
    assert_eq!(l.models[0].display_name.as_deref(), Some("Acme Coder"));
    assert!(l.models[0].raw.is_none());
    match &l.source {
        ModelListSource::Upstream { path, etag, .. } => {
            assert_eq!(path, "/models");
            assert_eq!(etag, &None);
        }
        other => panic!("{other:?}"),
    }
    let paths: Vec<String> = server.recorded().iter().map(|r| r.path.clone()).collect();
    assert_eq!(paths, ["/v1/models", "/models"]);
    assert!(server.recorded().iter().all(|r| r.query.is_none()));
}

#[tokio::test]
async fn a_site_without_a_list_is_unsupported_and_a_401_is_an_invalid_key() {
    let server = MockServer::start(|r| match r.path.as_str() {
        "/v1/models" => MockResponse::empty(405),
        "/models" => MockResponse {
            body: b"<!doctype html><html>app</html>".to_vec(),
            ..MockResponse::empty(200)
        }
        .with_header("Content-Type", "text/html"),
        _ => MockResponse::empty(404),
    })
    .await;
    let compat = compatible_site(&server);
    let base = Url::parse(&format!("{}/v1", server.origin())).unwrap();
    let err = list(&prober(), &compat, &base, None, false)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");

    let rejecting = MockServer::start(|_| relay_401()).await;
    let err = list(
        &prober(),
        &site(&rejecting),
        &backend_base(&rejecting),
        None,
        false,
    )
    .await
    .unwrap_err();
    match err {
        Error::Http {
            status, message, ..
        } => {
            assert_eq!(status, 401);
            assert!(!message.contains(KEY));
        }
        other => panic!("{other:?}"),
    }

    let blocked = MockServer::start(|_| edge_block()).await;
    let err = list(
        &prober(),
        &site(&blocked),
        &backend_base(&blocked),
        None,
        false,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, Error::Http { .. }), "{err:?}");
}

#[tokio::test]
async fn lists_are_reused_for_ten_minutes_then_revalidated_with_the_etag() {
    let hits = Arc::new(AtomicUsize::new(0));
    let h = Arc::clone(&hits);
    let server = MockServer::start(move |r| {
        h.fetch_add(1, Ordering::SeqCst);
        if r.header("if-none-match") == Some("\"v1\"") {
            MockResponse::empty(304)
        } else {
            MockResponse::json(200, native_body()).with_header("ETag", "\"v1\"")
        }
    })
    .await;
    let clock = Arc::new(ManualClock::new(CHECKED_AT));
    let prober = HttpLineProber::new(probe_config()).with_clock(clock.clone());
    let (site, base) = (site(&server), backend_base(&server));

    let first = list(&prober, &site, &base, None, false).await.unwrap();
    clock.advance(Duration::from_secs(60));
    let cached = list(&prober, &site, &base, None, false).await.unwrap();
    assert_eq!(cached, first, "served from the cache");
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    clock.advance(MODEL_LIST_TTL);
    let revalidated = list(&prober, &site, &base, None, false).await.unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert_eq!(revalidated.models, first.models);
    match revalidated.source {
        ModelListSource::Upstream { fetched_at, .. } => assert!(fetched_at > CHECKED_AT),
        other => panic!("{other:?}"),
    }
    let last = server.recorded().pop().unwrap();
    assert_eq!(last.header("if-none-match"), Some("\"v1\""));

    // Refresh always asks (conditionally).
    list(&prober, &site, &base, None, true).await.unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 3);

    // Another key is another cache entry.
    let other = Secret::new("sk-superba-test-other-0000000000");
    prober
        .model_list(ModelListRequest {
            site: &site,
            base_url: &base,
            key: &other,
            client_version: None,
            refresh: false,
            source: None,
            standard_base: None,
        })
        .await
        .unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn the_disk_cache_survives_a_new_prober_and_never_holds_the_key() {
    let server = MockServer::start(|_| {
        // A server that echoes the key in its answer: scrubbed before caching.
        MockResponse::json(
            200,
            json!({"models": [{"slug": "m", "description": format!("for {KEY}")}]}),
        )
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let make = || {
        HttpLineProber::new(probe_config())
            .with_clock(Arc::new(ManualClock::new(CHECKED_AT)))
            .with_model_cache_dir(dir.path().join("models"))
    };
    let (site, base) = (site(&server), backend_base(&server));
    let a = list(&make(), &site, &base, None, false).await.unwrap();
    let b = list(&make(), &site, &base, None, false).await.unwrap();
    assert_eq!(a, b);
    assert_eq!(server.recorded().len(), 1);
    let mut files = 0;
    for e in std::fs::read_dir(dir.path().join("models")).unwrap() {
        let e = e.unwrap();
        files += 1;
        let name = e.file_name().to_string_lossy().into_owned();
        assert!(!name.contains(KEY) && !name.starts_with(".tmp"));
        let text = std::fs::read_to_string(e.path()).unwrap();
        assert!(!text.contains(KEY), "{text}");
    }
    assert_eq!(files, 1);
}

#[tokio::test]
async fn a_key_is_never_sent_to_a_foreign_origin() {
    let server = MockServer::start(|_| MockResponse::json(200, native_body())).await;
    let other = MockServer::start(|_| MockResponse::json(200, native_body())).await;
    let err = list(
        &prober(),
        &site(&other),
        &backend_base(&server),
        None,
        false,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err:?}");
    assert_eq!(server.recorded().len(), 0);
    assert_eq!(other.recorded().len(), 0);
}
