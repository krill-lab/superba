//! Catalog transport behavior, including the fields the desktop speed menu uses.
mod common;

use std::time::Duration;

use common::*;
use superba_router::MemberKind;

const CATALOG: &str = r#"{"models":[{"slug":"gpt-test","service_tiers":[{"id":"priority","name":"Fast"},{"id":"ultrafast","name":"Ultrafast"}],"additional_speed_tiers":["ultrafast"],"future_field":{"keep":true}}]}"#;

async fn fixture(mode: Mode) -> (Upstream, Router) {
    let up = upstream(mode).await;
    let router = start(test_config(vec![member(
        "member",
        MemberKind::Krill,
        false,
        vec![line("line", up.base("/backend-api/codex"))],
    )]))
    .await;
    (up, router)
}

async fn get(r: &Router, version: &str) -> reqwest::Response {
    r.http
        .get(r.url(&format!(
            "/backend-api/codex/models?client_version={version}"
        )))
        .bearer_auth(ROUTER_TOKEN)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn concurrent_catalogs_share_one_fetch_and_preserve_the_exact_json() {
    let (up, r) = fixture(Mode::Slow(Duration::from_millis(100), CATALOG)).await;
    let replies = futures_util::future::join_all((0..16).map(|_| get(&r, "0.163.0"))).await;
    for reply in replies {
        assert_eq!(reply.status(), 200);
        assert_eq!(reply.text().await.unwrap(), CATALOG);
    }
    assert_eq!(up.hits(), 1);
    assert_eq!(get(&r, "0.162.1").await.status(), 200);
    assert_eq!(up.hits(), 2, "versions have independent catalogs");
    assert_eq!(
        up.seen()[0].path,
        "/backend-api/codex/models?client_version=0.163.0"
    );
    assert_eq!(
        up.seen()[0].headers["authorization"],
        format!("Bearer {}", key_of("member"))
    );
    let denied = r
        .http
        .get(r.url("/backend-api/codex/models?client_version=0.163.0"))
        .bearer_auth("wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(
        denied.status(),
        401,
        "cached catalogs still require the router token"
    );
    r.handle.shutdown();
}

#[tokio::test]
async fn a_429_uses_the_previous_catalog_and_backs_off() {
    let (up, r) = fixture(Mode::JsonHeader(200, CATALOG, "cache-control", "max-age=0")).await;
    assert_eq!(get(&r, "0.163.0").await.text().await.unwrap(), CATALOG);
    up.set(Mode::JsonHeader(
        429,
        r#"{"error":{"code":"upstream_rate_limit"}}"#,
        "retry-after",
        "20",
    ));
    for _ in 0..5 {
        let reply = get(&r, "0.163.0").await;
        assert_eq!(reply.status(), 200);
        assert_eq!(reply.headers()["x-superba-catalog"], "stale");
        assert_eq!(reply.text().await.unwrap(), CATALOG);
    }
    assert_eq!(up.hits(), 2);
    let cold = get(&r, "0.162.1").await;
    assert_eq!(cold.status(), 429);
    assert!(cold.text().await.unwrap().contains("upstream_rate_limit"));
    assert_eq!(get(&r, "0.162.1").await.status(), 429);
    assert_eq!(up.hits(), 3, "cold failures also coalesce during backoff");
    r.handle.shutdown();
}

#[tokio::test]
async fn etag_validation_keeps_the_body_and_updates_freshness() {
    let (up, r) = fixture(Mode::JsonHeaders(
        200,
        CATALOG,
        vec![("etag", "\"v1\""), ("cache-control", "max-age=0")],
    ))
    .await;
    assert_eq!(get(&r, "0.163.0").await.status(), 200);
    up.set(Mode::JsonHeader(304, "", "cache-control", "max-age=120"));
    let validated = get(&r, "0.163.0").await;
    assert_eq!(validated.status(), 200);
    assert_eq!(validated.headers()["etag"], "\"v1\"");
    assert_eq!(validated.text().await.unwrap(), CATALOG);
    assert_eq!(up.seen()[1].headers["if-none-match"], "\"v1\"");
    assert_eq!(get(&r, "0.163.0").await.text().await.unwrap(), CATALOG);
    assert_eq!(up.hits(), 2);
    r.handle.shutdown();
}

#[tokio::test]
async fn a_public_api_model_list_is_not_cached_as_a_codex_catalog() {
    let (up, r) = fixture(Mode::Json(200, r#"{"data":[{"id":"gpt-test"}]}"#)).await;
    assert_eq!(get(&r, "0.163.0").await.status(), 200);
    up.set(Mode::Json(200, CATALOG));
    assert_eq!(get(&r, "0.163.0").await.text().await.unwrap(), CATALOG);
    assert_eq!(up.hits(), 2);
    r.handle.shutdown();
}

#[tokio::test]
async fn account_rejections_never_use_a_stale_catalog() {
    let (up, r) = fixture(Mode::JsonHeader(200, CATALOG, "cache-control", "max-age=0")).await;
    assert_eq!(get(&r, "0.163.0").await.status(), 200);
    up.set(Mode::Json(401, r#"{"error":{"code":"invalid_key"}}"#));
    assert_eq!(get(&r, "0.163.0").await.status(), 401);
    up.set(Mode::Json(429, "{}"));
    assert_eq!(get(&r, "0.163.0").await.status(), 429);
    r.handle.shutdown();
}

#[tokio::test]
async fn a_fast_request_keeps_service_tier_through_the_router() {
    let (up, r) = fixture(Mode::Json(200, "{}")).await;
    let body = r#"{"model":"gpt-test","service_tier":"priority","input":"hello"}"#;
    assert_eq!(r.post(None, body).await.status(), 200);
    let received: serde_json::Value = serde_json::from_slice(&up.seen()[0].body).unwrap();
    assert_eq!(received["service_tier"], "priority");
    r.handle.shutdown();
}

#[tokio::test]
async fn etags_are_not_sent_to_a_different_member_on_failover() {
    let first = upstream(Mode::JsonHeaders(
        200,
        CATALOG,
        vec![("etag", "\"same-tag\""), ("cache-control", "max-age=0")],
    ))
    .await;
    let other = upstream(Mode::Json(200, r#"{"models":[]}"#)).await;
    let r = start(test_config(vec![
        member(
            "first",
            MemberKind::Krill,
            false,
            vec![line("one", first.base("/backend-api/codex"))],
        ),
        member(
            "other",
            MemberKind::Krill,
            false,
            vec![line("two", other.base("/backend-api/codex"))],
        ),
    ]))
    .await;
    assert_eq!(get(&r, "0.163.0").await.text().await.unwrap(), CATALOG);
    first.set(Mode::Json(503, "{}"));
    assert_eq!(
        get(&r, "0.163.0").await.text().await.unwrap(),
        r#"{"models":[]}"#
    );
    assert_eq!(first.seen()[1].headers["if-none-match"], "\"same-tag\"");
    assert!(!other.seen()[0].headers.contains_key("if-none-match"));
    r.handle.shutdown();
}

#[tokio::test]
async fn no_store_and_must_revalidate_are_respected_on_upstream_429() {
    for control in ["no-store", "max-age=0, must-revalidate"] {
        let (up, r) = fixture(Mode::JsonHeader(200, CATALOG, "cache-control", control)).await;
        assert_eq!(get(&r, "0.163.0").await.status(), 200);
        up.set(Mode::Json(429, "{}"));
        assert_eq!(get(&r, "0.163.0").await.status(), 429);
        r.handle.shutdown();
    }
}

#[tokio::test]
async fn a_member_reload_invalidates_the_catalog_even_with_the_same_client_version() {
    use superba_router::{RouterPaths, RouterServer};
    let up = upstream(Mode::Json(200, CATALOG)).await;
    let cfg = test_config(vec![member(
        "member",
        MemberKind::Krill,
        false,
        vec![line("line", up.base("/backend-api/codex"))],
    )]);
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::from_dir(dir.path().join("router"));
    cfg.save(&paths).unwrap();
    secrets_for(&cfg).save(&paths).unwrap();
    let server = RouterServer::bind_from_files(paths.clone()).await.unwrap();
    let url = format!("http://{}", server.local_addr());
    let admin = server.admin_token().clone();
    let stop = server.shutdown_handle();
    let task = tokio::spawn(server.serve());
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let models = format!("{url}/backend-api/codex/models?client_version=0.163.0");
    assert_eq!(
        http.get(&models)
            .bearer_auth(ROUTER_TOKEN)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        CATALOG
    );
    let mut secrets = secrets_for(&cfg);
    secrets.member_keys.insert(
        "member".into(),
        superba_core::Secret::new("changed-member-key"),
    );
    secrets.save(&paths).unwrap();
    up.set(Mode::Json(200, r#"{"models":[]}"#));
    assert_eq!(
        http.post(format!("{url}/_superba/v1/reload"))
            .bearer_auth(admin.expose())
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        http.get(&models)
            .bearer_auth(ROUTER_TOKEN)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        r#"{"models":[]}"#
    );
    assert_eq!(up.hits(), 2);
    assert_eq!(
        up.seen()[1].headers["authorization"],
        "Bearer changed-member-key"
    );
    stop.shutdown();
    task.await.unwrap().unwrap();
}
