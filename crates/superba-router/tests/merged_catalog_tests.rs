mod common;

use std::sync::Arc;

use common::*;
use serde_json::{Value, json};
use superba_core::{CatalogLayer, CatalogReadiness, ModelOverride, RouterCatalogView};
use superba_router::merged_catalog::{CatalogConfig, MemberCatalog, provider_slug};
use superba_router::{MemberKind, RouterConfig};

fn native(id: &str) -> Value {
    json!({"slug":id,"display_name":id.to_uppercase(),"supported_reasoning_levels":[],"shell_type":"unified_exec",
        "visibility":"list","supported_in_api":true,"priority":1,"support_verbosity":false,
        "truncation_policy":{"mode":"bytes","limit":10000},"experimental_supported_tools":[],
        "base_instructions":"Use tools when necessary.","context_window":128000,
        "service_tiers":[{"id":"priority","name":"Fast","future":{"retain":true}}],
        "additional_speed_tiers":["fast","ultrafast"],"future_model_field":{"retain":true}})
}

fn responder(codex_status: u16, standard_status: u16, passes: bool) -> Mode {
    Mode::PathReply(Arc::new(move |path, _, body| {
        if path == "/backend-api/codex/models" {
            return (codex_status, json!({"models":[native("gpt-test")]}));
        }
        if path == "/v1/models" {
            return (
                standard_status,
                json!({"data":[{"id":"deepseek-test","context_length":96000}]}),
            );
        }
        let req: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
        let output = if !passes {
            json!([])
        } else if let Some(value) = req["input"]
            .as_array()
            .and_then(|v| v.iter().find(|v| v["type"] == "function_call_output"))
        {
            json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":value["output"]}]}])
        } else if req["tool_choice"]["type"] == "function" {
            json!([{"type":"function_call","call_id":"call_test","name":"get_probe_value","arguments":"{}"}])
        } else {
            json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":"OK"}]}])
        };
        (
            200,
            json!({"id":"resp_test","status":"completed","output":output}),
        )
    }))
}

fn config(ups: &[(&str, &Upstream)]) -> RouterConfig {
    let mut config = test_config(
        ups.iter()
            .map(|(id, up)| {
                member(
                    id,
                    MemberKind::Krill,
                    false,
                    vec![line("one", up.base("/backend-api/codex"))],
                )
            })
            .collect(),
    );
    config.catalog = Some(CatalogConfig {
        bundled: vec![native("gpt-test"), native("fallback")],
        members: ups
            .iter()
            .map(|(id, _)| {
                (
                    id.to_string(),
                    MemberCatalog {
                        name: id.to_string(),
                        ..Default::default()
                    },
                )
            })
            .collect(),
        ..Default::default()
    });
    config
}

async fn models(r: &Router) -> Value {
    r.http
        .get(r.url("/models?client_version=0.163.0"))
        .bearer_auth(ROUTER_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn report(r: &Router) -> RouterCatalogView {
    r.http
        .get(r.url("/_superba/v1/catalog"))
        .bearer_auth(&r.admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn probe(r: &Router, model: &str) -> superba_core::RouterModelTest {
    r.http
        .post(r.url("/_superba/v1/catalog/test"))
        .bearer_auth(&r.admin_token)
        .json(&json!({"model":model}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn listed(v: &Value, id: &str) -> bool {
    v["models"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["slug"] == id)
}

#[tokio::test]
async fn merge_replaces_l0_whole_retains_speed_fields_and_reports_pending_sources() {
    let up = upstream(responder(200, 200, true)).await;
    let mut cfg = config(&[("krill", &up)]);
    cfg.catalog.as_mut().unwrap().bundled[0]["obsolete_builtin_field"] = json!(true);
    let r = start(cfg).await;
    let v = models(&r).await;
    let entry = v["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["slug"] == "gpt-test")
        .unwrap();
    assert_eq!(entry, &native("gpt-test"));
    assert!(listed(&v, "fallback") && listed(&v, "krill/gpt-test"));
    assert!(!listed(&v, "deepseek-test"));
    let view = report(&r).await;
    assert_eq!(view.sources.len(), 2);
    let pending = view
        .models
        .iter()
        .find(|m| m.entry["slug"] == "deepseek-test")
        .unwrap();
    assert_eq!(pending.readiness, CatalogReadiness::Pending);
    assert_eq!(pending.entry["apply_patch_tool_type"], "freeform");
    assert_eq!(pending.entry["context_window"], 96000);
    assert_eq!(pending.sources.len(), 2);
    assert!(
        pending
            .sources
            .iter()
            .all(|s| s.provider == "krill" && s.key_scope.len() == 16 && s.observed_at > 0)
    );
    assert!(
        !serde_json::to_string(&view)
            .unwrap()
            .contains(&key_of("krill"))
    );
    r.handle.shutdown();
}

#[tokio::test]
async fn catalogs_are_single_flight_per_member_and_layer() {
    let a = upstream(responder(200, 200, true)).await;
    let b = upstream(responder(200, 200, true)).await;
    let r = start(config(&[("a", &a), ("b", &b)])).await;
    let replies = futures_util::future::join_all((0..20).map(|_| models(&r))).await;
    assert!(
        replies
            .iter()
            .all(|v| listed(v, "a/gpt-test") && listed(v, "b/gpt-test"))
    );
    for up in [&a, &b] {
        assert_eq!(up.hits(), 2);
    }
    r.handle.shutdown();
}

#[tokio::test]
async fn successful_tool_roundtrip_publishes_model_and_routes_only_to_standard_endpoint() {
    let up = upstream(responder(200, 200, true)).await;
    let other = upstream(Mode::Json(200, r#"{"models":[],"data":[]}"#)).await;
    let r = start(config(&[("krill", &up), ("other", &other)])).await;
    assert!(!listed(&models(&r).await, "deepseek-test"));
    let test = probe(&r, "krill/deepseek-test").await;
    assert!(test.passed, "{}", test.detail);
    assert!(probe(&r, "deepseek-test").await.passed);
    assert_eq!(
        up.seen().iter().filter(|s| s.method == "POST").count(),
        2,
        "probe cached"
    );
    let published = models(&r).await;
    assert!(listed(&published, "deepseek-test") && listed(&published, "krill/deepseek-test"));
    for id in ["deepseek-test", "krill/deepseek-test"] {
        assert_eq!(
            r.post(None, &json!({"model":id,"input":"hello"}).to_string())
                .await
                .status(),
            200
        );
        let seen = up.seen().last().unwrap().clone();
        assert_eq!(seen.path, "/v1/responses");
        assert_eq!(
            serde_json::from_slice::<Value>(&seen.body).unwrap()["model"],
            "deepseek-test"
        );
    }
    assert!(other.seen().iter().all(|s| s.method == "GET"));
    assert_eq!(
        r.post(None, r#"{"model":"krill/gpt-test","input":"hello"}"#)
            .await
            .status(),
        200
    );
    assert_eq!(
        up.seen().last().unwrap().path,
        "/backend-api/codex/responses"
    );
    r.handle.shutdown();
}

#[tokio::test]
async fn failed_probes_remain_visible_in_admin_and_are_not_forwarded() {
    let up = upstream(responder(200, 200, false)).await;
    let r = start(config(&[("krill", &up)])).await;
    assert!(!probe(&r, "deepseek-test").await.passed);
    assert!(!listed(&models(&r).await, "deepseek-test"));
    let hits = up.hits();
    assert_eq!(
        r.post(None, r#"{"model":"deepseek-test","input":"private"}"#)
            .await
            .status(),
        409
    );
    assert_eq!(up.hits(), hits);
    let view = report(&r).await;
    assert!(
        view.models
            .iter()
            .any(|m| m.entry["slug"] == "deepseek-test" && m.readiness == CatalogReadiness::Failed)
    );
    r.handle.shutdown();
}

#[tokio::test]
async fn namespace_pins_provider_while_bare_models_keep_failover_order() {
    let first = upstream(responder(200, 200, true)).await;
    let second = upstream(responder(200, 200, true)).await;
    let r = start(config(&[("first", &first), ("second", &second)])).await;
    models(&r).await;
    first.set(Mode::Json(503, "{}"));
    assert_eq!(
        r.post(None, r#"{"model":"first/gpt-test","input":"hello"}"#)
            .await
            .status(),
        502
    );
    assert_eq!(second.hits(), 2, "no cross-provider request");
    assert_eq!(
        r.post(None, r#"{"model":"gpt-test","input":"hello"}"#)
            .await
            .status(),
        200
    );
    assert_eq!(second.hits(), 3);
    r.handle.shutdown();
}

#[tokio::test]
async fn namespace_can_fail_over_between_its_own_lines_and_strips_zstd_prefix() {
    let first = upstream(Mode::Json(503, "{}")).await;
    let second = upstream(responder(200, 200, true)).await;
    let other = upstream(responder(200, 200, true)).await;
    let mut cfg = config(&[("krill", &first), ("other", &other)]);
    cfg.members[0]
        .lines
        .push(line("two", second.base("/backend-api/codex")));
    let r = start(cfg).await;
    models(&r).await;
    let body =
        zstd::stream::encode_all(&br#"{"model":"krill/gpt-test","input":"hello"}"#[..], 3).unwrap();
    let answer = r
        .http
        .post(r.url("/backend-api/codex/responses"))
        .bearer_auth(ROUTER_TOKEN)
        .header("content-encoding", "zstd")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(answer.status(), 200);
    assert_eq!(other.hits(), 2);
    let last = second.seen().last().unwrap().clone();
    assert_eq!(
        serde_json::from_slice::<Value>(&last.body).unwrap()["model"],
        "gpt-test"
    );
    r.handle.shutdown();
}

#[tokio::test]
async fn list_failures_are_separate_and_never_masquerade_as_empty_success() {
    for (l1, l2) in [(401, 200), (200, 429)] {
        let up = upstream(responder(l1, l2, true)).await;
        let r = start(config(&[("krill", &up)])).await;
        let view = report(&r).await;
        let codex = view
            .sources
            .iter()
            .find(|s| s.layer == CatalogLayer::Codex)
            .unwrap();
        let standard = view
            .sources
            .iter()
            .find(|s| s.layer == CatalogLayer::Standard)
            .unwrap();
        assert_eq!(codex.error.is_some(), l1 != 200);
        assert_eq!(standard.error.is_some(), l2 != 200);
        assert!(codex.observed_at.is_some() == (l1 == 200));
        assert!(standard.observed_at.is_some() == (l2 == 200));
        report(&r).await;
        assert_eq!(up.hits(), 2, "success and failure both cached");
        r.handle.shutdown();
    }
}

#[tokio::test]
async fn overrides_affect_published_values_and_prefix_visibility_does_not_change_routing() {
    let up = upstream(responder(200, 200, true)).await;
    let mut cfg = config(&[("krill", &up)]);
    let c = cfg.catalog.as_mut().unwrap();
    c.settings.show_prefixed = false;
    c.members.get_mut("krill").unwrap().overrides.insert(
        "gpt-test".into(),
        ModelOverride {
            context_window: Some(64000),
            auto_compact: Some(60000),
            hidden: true,
            ..Default::default()
        },
    );
    let r = start(cfg).await;
    let v = models(&r).await;
    assert!(!listed(&v, "krill/gpt-test"));
    let entry = v["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["slug"] == "gpt-test")
        .unwrap();
    assert_eq!(entry["context_window"], 64000);
    assert_eq!(entry["auto_compact_token_limit"], 57600);
    assert_eq!(entry["visibility"], "hide");
    for prefix in ["krill", "krill-usage", "krill-balance"] {
        assert_eq!(
            r.post(
                None,
                &json!({"model":format!("{prefix}/gpt-test"),"input":"hello"}).to_string()
            )
            .await
            .status(),
            200
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&up.seen().last().unwrap().body).unwrap()["model"],
            "gpt-test"
        );
    }
    assert_eq!(
        r.post(None, r#"{"model":"unknown/gpt-test","input":"hello"}"#)
            .await
            .status(),
        409
    );
    r.handle.shutdown();
}

#[tokio::test]
async fn disabled_conversion_models_are_marked_as_needing_translation() {
    let up = upstream(responder(200, 200, true)).await;
    let mut cfg = config(&[("krill", &up)]);
    cfg.openai = Some(superba_router::openai::OpenAiConfig {
        members: [(
            "krill".into(),
            superba_router::openai::OpenAiMember {
                chat: Some(superba_router::openai::OpenAiEndpoint::at("/v1")),
                ..Default::default()
            },
        )]
        .into(),
        ..Default::default()
    });
    for to in superba_router::translate::ir::Protocol::ALL {
        if to != superba_router::translate::ir::Protocol::Responses {
            cfg.translation
                .disabled
                .insert(superba_router::translate::routing::Conversion {
                    from: superba_router::translate::ir::Protocol::Responses,
                    to,
                });
        }
    }
    let r = start(cfg).await;
    let view = report(&r).await;
    assert!(
        view.models
            .iter()
            .any(|m| m.entry["slug"] == "deepseek-test"
                && m.readiness == CatalogReadiness::NeedsTranslation
                && m.sources.iter().all(|s| s.protocol == "unavailable"))
    );
    assert!(!probe(&r, "deepseek-test").await.passed);
    assert!(!listed(&models(&r).await, "deepseek-test"));
    assert_eq!(up.hits(), 2);
    r.handle.shutdown();
}

#[tokio::test]
async fn model_and_admin_endpoints_require_their_own_tokens() {
    let up = upstream(responder(200, 200, true)).await;
    let r = start(config(&[("krill", &up)])).await;
    for path in [
        "/models",
        "/backend-api/codex/models",
        "/_superba/v1/catalog",
    ] {
        assert_eq!(r.http.get(r.url(path)).send().await.unwrap().status(), 401);
    }
    assert_eq!(
        r.http
            .post(r.url("/_superba/v1/catalog/test"))
            .bearer_auth(ROUTER_TOKEN)
            .json(&json!({"model":"deepseek-test"}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(up.hits(), 0);
    r.handle.shutdown();
}

#[test]
fn provider_names_are_ascii_unique_and_stable_across_priority_changes() {
    assert_eq!(provider_slug("  DeepSeek / Team A  "), "deepseek-team-a");
    assert_eq!(provider_slug("中文"), "provider");
    let url = "http://127.0.0.1:1234/v1".parse().unwrap();
    let mut cfg = test_config(vec![
        member("b", MemberKind::Compatible, false, vec![line("l", url)]),
        member(
            "a",
            MemberKind::Compatible,
            false,
            vec![line("l", "http://127.0.0.1:1235/v1".parse().unwrap())],
        ),
    ]);
    for member in &mut cfg.members {
        member.name = "Same Name".into();
    }
    let c = CatalogConfig::default();
    let slugs = c.slugs(&cfg);
    assert_eq!(slugs["a"], "same-name");
    assert_eq!(slugs["b"], "same-name-2");
    cfg.members.reverse();
    assert_eq!(slugs, c.slugs(&cfg));
    for member in &mut cfg.members {
        member.name = "a".repeat(64);
    }
    let mut chosen = c;
    chosen.settings.provider_slugs = chosen.slugs(&cfg);
    assert_ne!(
        chosen.settings.provider_slugs["a"],
        chosen.settings.provider_slugs["b"]
    );
    assert!(
        chosen.validate(&cfg).is_ok(),
        "generated slugs must be saveable"
    );
}

#[tokio::test]
async fn transient_list_failures_serve_stale_but_auth_rejections_clear_evidence() {
    let body = Box::leak(
        json!({"models":[native("gpt-test")],"data":[{"id":"deepseek-test"}]})
            .to_string()
            .into_boxed_str(),
    );
    let up = upstream(Mode::JsonHeader(200, body, "cache-control", "max-age=0")).await;
    let r = start(config(&[("krill", &up)])).await;
    report(&r).await;
    up.set(Mode::Json(503, "{}"));
    let stale = report(&r).await;
    assert!(stale.sources.iter().all(|s| s.stale && s.error.is_some()));
    assert!(
        stale
            .models
            .iter()
            .any(|m| m.entry["slug"] == "deepseek-test")
    );
    r.handle.shutdown();
    let up = upstream(Mode::JsonHeader(200, body, "cache-control", "max-age=0")).await;
    let r = start(config(&[("krill", &up)])).await;
    report(&r).await;
    up.set(Mode::Json(401, "{}"));
    let denied = report(&r).await;
    assert!(
        denied
            .sources
            .iter()
            .all(|s| !s.stale && s.observed_at.is_none() && s.error.is_some())
    );
    assert!(
        !denied
            .models
            .iter()
            .any(|m| m.entry["slug"] == "deepseek-test")
    );
    r.handle.shutdown();
}

#[tokio::test]
async fn settings_reload_preserves_probes_but_key_changes_invalidate_them() {
    let up = upstream(responder(200, 200, true)).await;
    let mut cfg = config(&[("krill", &up)]);
    let temp = tempfile::tempdir().unwrap();
    let paths = superba_router::RouterPaths::from_dir(temp.path().join("router"));
    cfg.save(&paths).unwrap();
    let mut secrets = secrets_for(&cfg);
    secrets.save(&paths).unwrap();
    let server = superba_router::RouterServer::bind_from_files(paths.clone())
        .await
        .unwrap();
    let r = Router {
        addr: server.local_addr(),
        admin_token: server.admin_token().expose().to_owned(),
        handle: server.shutdown_handle(),
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
    };
    let task = tokio::spawn(server.serve());
    assert!(probe(&r, "deepseek-test").await.passed);
    cfg.catalog.as_mut().unwrap().settings.show_prefixed = false;
    cfg.save(&paths).unwrap();
    assert_eq!(
        r.http
            .post(r.url("/_superba/v1/reload"))
            .bearer_auth(&r.admin_token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert!(listed(&models(&r).await, "deepseek-test"));
    secrets.member_keys.insert(
        "krill".into(),
        superba_core::Secret::new("different-test-key"),
    );
    secrets.save(&paths).unwrap();
    assert_eq!(
        r.http
            .post(r.url("/_superba/v1/reload"))
            .bearer_auth(&r.admin_token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert!(!listed(&models(&r).await, "deepseek-test"));
    r.handle.shutdown();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn visibility_override_can_reveal_models_hidden_upstream() {
    let up = upstream(Mode::PathReply(Arc::new(|_, _, _| {
        let mut hidden = native("gpt-test");
        hidden["visibility"] = json!("hide");
        (200, json!({"models":[hidden],"data":[]}))
    })))
    .await;
    let mut cfg = config(&[("krill", &up)]);
    cfg.catalog
        .as_mut()
        .unwrap()
        .settings
        .visibility
        .insert("gpt-test".into(), true);
    cfg.catalog
        .as_mut()
        .unwrap()
        .settings
        .visibility
        .insert("krill/gpt-test".into(), false);
    let r = start(cfg).await;
    let v = models(&r).await;
    let entry = |id: &str| {
        v["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["slug"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(entry("gpt-test")["visibility"], "list");
    assert_eq!(entry("krill/gpt-test")["visibility"], "hide");
    assert_eq!(
        entry("gpt-test")["service_tiers"],
        native("gpt-test")["service_tiers"]
    );
    r.handle.shutdown();
}

#[tokio::test]
async fn compatible_standard_provider_works_without_a_codex_catalog() {
    let up = upstream(responder(200, 200, true)).await;
    let mut cfg = config(&[("deepseek", &up)]);
    cfg.members[0].kind = MemberKind::Compatible;
    cfg.members[0].lines[0].base_url = up.base("/v1");
    let r = start(cfg).await;
    let view = report(&r).await;
    assert!(
        view.sources
            .iter()
            .any(|s| s.layer == CatalogLayer::Codex && s.error.is_some())
    );
    assert!(
        view.sources
            .iter()
            .any(|s| s.layer == CatalogLayer::Standard && s.error.is_none())
    );
    assert!(probe(&r, "deepseek/deepseek-test").await.passed);
    assert!(listed(&models(&r).await, "deepseek/deepseek-test"));
    assert_eq!(
        r.post(None, r#"{"model":"deepseek/deepseek-test","input":"hi"}"#)
            .await
            .status(),
        200
    );
    let seen = up.seen();
    let sent = seen.last().unwrap();
    assert_eq!(sent.path, "/v1/responses");
    assert_eq!(
        serde_json::from_slice::<Value>(&sent.body).unwrap()["model"],
        "deepseek-test"
    );
    r.handle.shutdown();
}
