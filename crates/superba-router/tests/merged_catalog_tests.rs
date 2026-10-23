mod common;

use std::sync::Arc;

use common::*;
use serde_json::{Value, json};
use superba_core::{CatalogLayer, CatalogMode, CatalogReadiness, ModelOverride, RouterCatalogView};
use superba_router::merged_catalog::{CatalogConfig, MemberCatalog, domain_slug, provider_slug};
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
        // Most tests check the pinned aliases; the mode tests set their own.
        settings: superba_core::RouterCatalogSettings {
            mode: CatalogMode::Prefixed,
            ..Default::default()
        },
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
    // One source per provider: the layer its metadata came from.
    assert_eq!(pending.sources.len(), 1);
    assert_eq!(pending.sources[0].layer, CatalogLayer::Synthesized);
    assert_eq!(
        pending.sources[0].readiness,
        Some(CatalogReadiness::Pending)
    );
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
    c.settings.mode = CatalogMode::Extended;
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
    // An unknown namespace is refused; it never falls back to every provider.
    assert_eq!(
        r.post(None, r#"{"model":"unknown/gpt-test","input":"hello"}"#)
            .await
            .status(),
        404
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
    assert_eq!(provider_slug("中文"), "");
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
    cfg.catalog.as_mut().unwrap().settings.mode = CatalogMode::Extended;
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
    // The single row's setting applies to every alias; alias keys are not read.
    assert_eq!(entry("gpt-test")["visibility"], "list");
    assert_eq!(entry("krill/gpt-test")["visibility"], "list");
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

// ---------------------------------------------------------------------------
// Router model list modes (alpha.8): one row per model, the published set of
// each mode, exact metadata reuse, pinned aliases and canonical overrides.
// ---------------------------------------------------------------------------

/// A complete Codex entry with its own window, reasoning levels and tiers.
fn known(id: &str, window: u64) -> Value {
    let mut entry = native(id);
    entry["context_window"] = json!(window);
    entry["supported_reasoning_levels"] =
        json!([{"effort":"low","description":"Fast"},{"effort":"xhigh","description":"Deep"}]);
    entry["apply_patch_tool_type"] = json!("freeform");
    entry
}

/// Krill-like member: a Codex catalog with one model the installed Codex
/// does not know. Office-like member: a standard list only.
fn office_and_krill(krill: &Upstream, office: &Upstream, mode: CatalogMode) -> RouterConfig {
    let mut cfg = test_config(vec![
        member(
            "krill",
            MemberKind::Krill,
            false,
            vec![line("one", krill.base("/backend-api/codex"))],
        ),
        member(
            "office",
            MemberKind::Compatible,
            false,
            vec![line("one", office.base("/v1"))],
        ),
    ]);
    cfg.catalog = Some(CatalogConfig {
        settings: superba_core::RouterCatalogSettings {
            mode,
            ..Default::default()
        },
        bundled: vec![native("gpt-test"), known("gpt-5.4-nano", 400_000)],
        members: [
            (
                "krill".to_string(),
                MemberCatalog {
                    name: "Krill".into(),
                    ..Default::default()
                },
            ),
            (
                "office".to_string(),
                MemberCatalog {
                    name: "内网".into(),
                    ..Default::default()
                },
            ),
        ]
        .into(),
    });
    cfg
}

fn office_reply(standard: Value) -> Mode {
    Mode::PathReply(Arc::new(move |path, _, body| {
        if path == "/v1/models" {
            return (200, standard.clone());
        }
        let req: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
        let output = if let Some(value) = req["input"]
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
            json!({"id":"resp_office","status":"completed","output":output}),
        )
    }))
}

fn krill_reply() -> Mode {
    Mode::PathReply(Arc::new(|path, _, _| {
        if path == "/backend-api/codex/models" {
            return (
                200,
                json!({"models":[native("gpt-test"), known("gpt-l1-only", 600_000)]}),
            );
        }
        if path == "/v1/models" {
            return (200, json!({"data":[]}));
        }
        (
            200,
            json!({"id":"resp_krill","status":"completed","output":[
                {"type":"message","role":"assistant","content":[{"type":"output_text","text":"OK"}]}]}),
        )
    }))
}

fn office_list() -> Value {
    json!({"data":[{"id":"gpt-test"},{"id":"gpt-5.4-nano"},{"id":"gpt-l1-only"},
        {"id":"deepseek-x"},{"id":"gpt-9-unknown"}]})
}

fn slugs_of(v: &Value) -> Vec<String> {
    v["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["slug"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn codex_mode_publishes_exactly_the_installed_codex_models_once() {
    let krill = upstream(krill_reply()).await;
    let office = upstream(office_reply(office_list())).await;
    let r = start(office_and_krill(&krill, &office, CatalogMode::Codex)).await;
    assert_eq!(slugs_of(&models(&r).await), ["gpt-5.4-nano", "gpt-test"]);
    let view = report(&r).await;
    let ids: Vec<_> = view.models.iter().map(|m| m.id().to_owned()).collect();
    assert_eq!(ids, ["gpt-5.4-nano", "gpt-test"], "one row per model");
    assert!(view.aliases.is_empty());
    let row = view.model("gpt-test").unwrap();
    let layers: Vec<_> = row
        .sources
        .iter()
        .map(|s| (s.provider.as_str(), s.layer))
        .collect();
    assert_eq!(
        layers,
        [
            ("krill", CatalogLayer::Codex),
            ("office", CatalogLayer::StandardKnown)
        ]
    );
    // Aliases are computed (the page shows them) but not published.
    assert_eq!(row.sources[0].alias.as_deref(), Some("krill/gpt-test"));
    assert_eq!(row.sources[1].alias.as_deref(), Some("p2/gpt-test"));
    assert_eq!(view.settings.provider_slugs["office"], "p2");
    // The mode decides what is offered, not what routes.
    assert_eq!(
        r.post(None, r#"{"model":"gpt-l1-only","input":"hello"}"#)
            .await
            .status(),
        200
    );
    r.handle.shutdown();
}

#[tokio::test]
async fn known_standard_ids_reuse_full_metadata_and_unknown_ids_get_family_budgets() {
    let krill = upstream(krill_reply()).await;
    let office = upstream(office_reply(office_list())).await;
    let r = start(office_and_krill(&krill, &office, CatalogMode::Extended)).await;
    let view = report(&r).await;
    // L0 match: the installed Codex's complete entry, ready on a native endpoint.
    let nano = view.model("gpt-5.4-nano").unwrap();
    assert_eq!(nano.entry, known("gpt-5.4-nano", 400_000));
    let office_nano = nano
        .sources
        .iter()
        .find(|s| s.provider == "office")
        .unwrap();
    assert_eq!(office_nano.layer, CatalogLayer::StandardKnown);
    assert_eq!(office_nano.readiness, Some(CatalogReadiness::Ready));
    assert!(!nano.estimated());
    // L1 match from another provider: its metadata, but this provider still
    // has to pass its own tool probe.
    let l1 = view.model("gpt-l1-only").unwrap();
    assert_eq!(l1.entry["context_window"], 600_000);
    assert_eq!(l1.readiness, CatalogReadiness::Ready);
    let office_l1 = l1.sources.iter().find(|s| s.provider == "office").unwrap();
    assert_eq!(office_l1.layer, CatalogLayer::StandardKnown);
    assert_eq!(office_l1.readiness, Some(CatalogReadiness::Pending));
    // Unknown ids: family operating budgets, never written as a maximum.
    for (id, window) in [("gpt-9-unknown", 272_000), ("deepseek-x", 64_000)] {
        let row = view.model(id).unwrap();
        assert_eq!(row.entry["context_window"], window, "{id}");
        assert!(row.entry.get("max_context_window").is_none(), "{id}");
        assert!(row.estimated(), "{id}");
        assert_eq!(row.readiness, CatalogReadiness::Pending);
    }
    let published = slugs_of(&models(&r).await);
    assert_eq!(published, ["gpt-5.4-nano", "gpt-l1-only", "gpt-test"]);
    // A passing test publishes a pending model; still no aliases.
    let test = probe(&r, "deepseek-x").await;
    assert!(test.passed, "{}", test.detail);
    let published = slugs_of(&models(&r).await);
    assert!(published.contains(&"deepseek-x".to_owned()));
    assert!(published.iter().all(|s| !s.contains('/')));
    r.handle.shutdown();
}

#[tokio::test]
async fn prefixed_mode_adds_pinned_aliases_without_extra_rows() {
    let krill = upstream(krill_reply()).await;
    let office = upstream(office_reply(office_list())).await;
    let r = start(office_and_krill(&krill, &office, CatalogMode::Prefixed)).await;
    let extended_rows = 5;
    let view = report(&r).await;
    assert_eq!(view.models.len(), extended_rows, "aliases are not rows");
    let published = models(&r).await;
    let slugs = slugs_of(&published);
    for alias in [
        "krill/gpt-test",
        "p2/gpt-test",
        "krill/gpt-5.4-nano",
        "p2/gpt-5.4-nano",
        "krill/gpt-l1-only",
    ] {
        assert!(slugs.contains(&alias.to_owned()), "{alias}: {slugs:?}");
        assert!(view.aliases.contains_key(alias), "{alias}");
    }
    // Unverified targets get no alias until their own test passes.
    for missing in ["p2/gpt-l1-only", "p2/deepseek-x", "deepseek-x"] {
        assert!(!slugs.contains(&missing.to_owned()), "{missing}");
    }
    let entry = |id: &str| {
        published["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["slug"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(entry("krill/gpt-test")["display_name"], "GPT-TEST · Krill");
    assert_eq!(entry("p2/gpt-test")["display_name"], "GPT-TEST · 内网");
    // The alias keeps everything else of its provider's entry.
    let mut expected = native("gpt-test");
    expected["slug"] = json!("krill/gpt-test");
    expected["display_name"] = json!("GPT-TEST · Krill");
    assert_eq!(entry("krill/gpt-test"), expected);
    r.handle.shutdown();
}

#[tokio::test]
async fn bare_ids_fail_over_in_member_order_and_aliases_stay_pinned() {
    let krill = upstream(krill_reply()).await;
    let office = upstream(office_reply(office_list())).await;
    let r = start(office_and_krill(&krill, &office, CatalogMode::Prefixed)).await;
    models(&r).await;
    krill.set(Mode::Json(503, "{}"));
    let office_posts = || office.seen().iter().filter(|s| s.method == "POST").count();
    assert_eq!(
        r.post(None, r#"{"model":"krill/gpt-test","input":"hi"}"#)
            .await
            .status(),
        502
    );
    assert_eq!(office_posts(), 0, "a pinned alias never fails over");
    assert_eq!(
        r.post(None, r#"{"model":"gpt-test","input":"hi"}"#)
            .await
            .status(),
        200
    );
    assert_eq!(
        office_posts(),
        1,
        "the bare id fails over to the next member"
    );
    assert_eq!(
        r.post(None, r#"{"model":"p2/gpt-test","input":"hi"}"#)
            .await
            .status(),
        200
    );
    let last = office.seen().last().unwrap().clone();
    assert_eq!(last.path, "/v1/responses");
    assert_eq!(
        serde_json::from_slice::<Value>(&last.body).unwrap()["model"],
        "gpt-test"
    );
    r.handle.shutdown();
}

#[tokio::test]
async fn canonical_overrides_apply_to_the_bare_id_and_every_alias() {
    let krill = upstream(krill_reply()).await;
    let office = upstream(office_reply(office_list())).await;
    let mut cfg = office_and_krill(&krill, &office, CatalogMode::Prefixed);
    let catalog = cfg.catalog.as_mut().unwrap();
    // A provider's own setting is the target default ...
    catalog.members.get_mut("krill").unwrap().overrides.insert(
        "gpt-test".into(),
        ModelOverride {
            context_window: Some(64_000),
            ..Default::default()
        },
    );
    // ... and the single row's explicit setting wins everywhere.
    catalog.settings.overrides.insert(
        "gpt-test".into(),
        ModelOverride {
            context_window: Some(100_000),
            hidden: true,
            ..Default::default()
        },
    );
    let r = start(cfg).await;
    let published = models(&r).await;
    for id in ["gpt-test", "krill/gpt-test", "p2/gpt-test"] {
        let entry = published["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["slug"] == id)
            .unwrap();
        assert_eq!(entry["context_window"], 100_000, "{id}");
        assert_eq!(entry["visibility"], "hide", "{id}");
    }
    r.handle.shutdown();
}

#[tokio::test]
async fn single_provider_gets_an_alias_and_real_slash_ids_win_over_generated_ones() {
    let office = upstream(office_reply(
        json!({"data":[{"id":"gpt-test"},{"id":"p1/gpt-test"}]}),
    ))
    .await;
    let mut cfg = test_config(vec![member(
        "office",
        MemberKind::Compatible,
        false,
        vec![line("one", office.base("/v1"))],
    )]);
    cfg.catalog = Some(CatalogConfig {
        settings: superba_core::RouterCatalogSettings {
            mode: CatalogMode::Prefixed,
            ..Default::default()
        },
        bundled: vec![native("gpt-test")],
        members: [(
            "office".to_string(),
            MemberCatalog {
                name: "内网".into(),
                ..Default::default()
            },
        )]
        .into(),
    });
    let r = start(cfg).await;
    let view = report(&r).await;
    let row = view.model("gpt-test").unwrap();
    // The generated alias would shadow a real upstream id: not generated.
    assert_eq!(row.sources[0].alias, None);
    let real = view.model("p1/gpt-test").unwrap();
    assert_eq!(real.sources[0].upstream_id, "p1/gpt-test");
    assert_eq!(real.sources[0].layer, CatalogLayer::Synthesized);
    assert!(view.aliases.is_empty());
    r.handle.shutdown();
    // Without the collision a single provider still gets its alias.
    let office = upstream(office_reply(json!({"data":[{"id":"gpt-test"}]}))).await;
    let mut cfg2 = test_config(vec![member(
        "office",
        MemberKind::Compatible,
        false,
        vec![line("one", office.base("/v1"))],
    )]);
    let mut catalog = CatalogConfig {
        bundled: vec![native("gpt-test")],
        ..Default::default()
    };
    catalog.settings.mode = CatalogMode::Prefixed;
    cfg2.catalog = Some(catalog);
    let r = start(cfg2).await;
    assert!(slugs_of(&models(&r).await).contains(&"office/gpt-test".to_owned()));
    r.handle.shutdown();
}

#[tokio::test]
async fn a_canonical_test_checks_each_provider_and_says_which() {
    let krill = upstream(krill_reply()).await;
    let office = upstream(office_reply(office_list())).await;
    let r = start(office_and_krill(&krill, &office, CatalogMode::Extended)).await;
    let test = probe(&r, "gpt-l1-only").await;
    assert!(test.passed, "{}", test.detail);
    assert!(
        test.detail.contains("Krill:") && test.detail.contains("内网:"),
        "{}",
        test.detail
    );
    // The office target passed its tool probe: it is ready now.
    let view = report(&r).await;
    let office_l1 = view
        .model("gpt-l1-only")
        .unwrap()
        .sources
        .iter()
        .find(|s| s.provider == "office")
        .unwrap()
        .clone();
    assert_eq!(office_l1.readiness, Some(CatalogReadiness::Ready));
    // A pinned alias tests its provider only.
    let before = krill.seen().len();
    assert!(probe(&r, "p2/gpt-test").await.passed);
    assert_eq!(krill.seen().len(), before);
    r.handle.shutdown();
}

#[test]
fn slugs_use_the_saved_value_an_ascii_name_the_domain_then_a_number() {
    let at = |url: &str| line("l", url.parse().unwrap());
    let mut cfg = test_config(vec![
        member(
            "a",
            MemberKind::Compatible,
            false,
            vec![at("https://api.foo.com/v1")],
        ),
        member(
            "b",
            MemberKind::Compatible,
            false,
            vec![at("http://10.0.0.7:8080/v1")],
        ),
        member(
            "c",
            MemberKind::Compatible,
            false,
            vec![at("https://llm.bar.com.cn/v1")],
        ),
        member(
            "d",
            MemberKind::Compatible,
            false,
            vec![at("https://api.baz.co.uk/v1")],
        ),
        member(
            "e",
            MemberKind::Compatible,
            false,
            vec![at("https://api.foo.com/v1")],
        ),
    ]);
    for m in &mut cfg.members {
        m.name = "内网".into();
    }
    let mut c = CatalogConfig::default();
    let slugs = c.slugs(&cfg);
    assert_eq!(slugs["a"], "foo");
    assert_eq!(slugs["b"], "p2", "an IP address has no domain");
    assert_eq!(slugs["c"], "bar");
    assert_eq!(slugs["d"], "baz");
    assert_eq!(slugs["e"], "foo-2");
    // An ASCII name comes first; a saved slug before everything.
    cfg.members[0].name = "My Relay".into();
    c.settings
        .provider_slugs
        .insert("b".into(), "office".into());
    // alpha.7's placeholder for a name without ASCII letters is not kept.
    c.settings
        .provider_slugs
        .insert("c".into(), "provider".into());
    let slugs = c.slugs(&cfg);
    assert_eq!(slugs["a"], "my-relay");
    assert_eq!(slugs["b"], "office");
    assert_eq!(slugs["c"], "bar");
    assert_eq!(
        domain_slug(&"http://localhost:3000/v1".parse().unwrap()),
        None
    );
    assert_eq!(
        domain_slug(&"https://xn--fiq228c.com/v1".parse().unwrap()),
        None
    );
    assert_eq!(
        domain_slug(&"https://api.deepseek.com/v1".parse().unwrap()).as_deref(),
        Some("deepseek")
    );
}

/// User bug 106 end to end, through the router's own files: a third-party
/// model (`deepseek-v4.1-flash` in a provider's `/v1/models`) passes its tool
/// probe. In Codex mode it is withheld (and the view says so); switching the
/// mode publishes it at once, shown in the picker; Responses answers carry the
/// list's tag so Codex refetches the list; the pass survives a router restart.
#[tokio::test]
async fn bug106_a_passed_provider_model_reaches_codex_after_a_mode_switch() {
    const ID: &str = "deepseek-v4.1-flash";
    let krill = upstream(krill_reply()).await;
    // The provider marks it hidden in its own list metadata: a verified model
    // is still shown unless the user hides it here.
    let office = upstream(office_reply(
        json!({"data":[{"id":ID,"visibility":"hide"}]}),
    ))
    .await;
    let mut cfg = office_and_krill(&krill, &office, CatalogMode::Codex);
    let temp = tempfile::tempdir().unwrap();
    let paths = superba_router::RouterPaths::from_dir(temp.path().join("router"));
    cfg.save(&paths).unwrap();
    let secrets = secrets_for(&cfg);
    secrets.save(&paths).unwrap();
    async fn boot(paths: &superba_router::RouterPaths) -> (Router, tokio::task::JoinHandle<()>) {
        let server = superba_router::RouterServer::bind_from_files(paths.clone())
            .await
            .unwrap();
        let r = Router {
            addr: server.local_addr(),
            admin_token: server.admin_token().expose().to_owned(),
            handle: server.shutdown_handle(),
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
        };
        let task = tokio::spawn(async move { server.serve().await.unwrap() });
        (r, task)
    }
    async fn reload(r: &Router) {
        let status = r
            .http
            .post(r.url("/_superba/v1/reload"))
            .bearer_auth(&r.admin_token)
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, 200);
    }
    async fn served(r: &Router) -> (Value, String) {
        let resp = r
            .http
            .get(r.url("/models?client_version=0.163.0"))
            .bearer_auth(ROUTER_TOKEN)
            .send()
            .await
            .unwrap();
        let etag = resp.headers()["etag"].to_str().unwrap().to_owned();
        (resp.json().await.unwrap(), etag)
    }
    async fn models_etag(r: &Router) -> String {
        let resp = r
            .post(None, r#"{"model":"gpt-test","input":"hello"}"#)
            .await;
        assert_eq!(resp.status(), 200);
        resp.headers()["x-models-etag"].to_str().unwrap().to_owned()
    }
    let (r, task) = boot(&paths).await;

    // Codex mode: withheld, and the view names it for the Models page.
    let (list, codex_tag) = served(&r).await;
    assert!(!listed(&list, ID));
    let view = report(&r).await;
    assert!(view.model(ID).is_none());
    assert!(view.withheld.iter().any(|w| w == ID), "{:?}", view.withheld);
    // The test still runs (the page offers it) and passes...
    let test = probe(&r, ID).await;
    assert!(test.passed, "{}", test.detail);
    assert_eq!(test.detail, "Two-turn function call passed");
    // ...but Codex mode keeps it out.
    assert!(!listed(&served(&r).await.0, ID));
    assert_eq!(models_etag(&r).await, codex_tag);

    // Provider models: published right away, shown in the picker.
    cfg.catalog.as_mut().unwrap().settings.mode = CatalogMode::Extended;
    cfg.save(&paths).unwrap();
    reload(&r).await;
    // Before Codex fetched the new list, Responses name another list.
    assert_ne!(models_etag(&r).await, codex_tag);
    let (list, extended_tag) = served(&r).await;
    let entry = list["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["slug"] == ID)
        .expect("published in provider-models mode")
        .clone();
    assert_eq!(entry["visibility"], "list");
    assert_ne!(extended_tag, codex_tag);
    assert_eq!(models_etag(&r).await, extended_tag);
    assert!(report(&r).await.withheld.is_empty());

    // Prefixed: the bare id and the pinned alias.
    cfg.catalog.as_mut().unwrap().settings.mode = CatalogMode::Prefixed;
    cfg.save(&paths).unwrap();
    reload(&r).await;
    let list = served(&r).await.0;
    assert!(listed(&list, ID));
    assert!(listed(&list, &format!("p2/{ID}")), "{list}");

    // A router restart keeps the pass (it is on disk, keyed to the key).
    r.handle.shutdown();
    task.await.unwrap();
    let (r, task) = boot(&paths).await;
    assert!(listed(&served(&r).await.0, ID));

    // The user's own choice to hide it wins.
    cfg.catalog
        .as_mut()
        .unwrap()
        .settings
        .visibility
        .insert(ID.into(), false);
    cfg.save(&paths).unwrap();
    reload(&r).await;
    let list = served(&r).await.0;
    let entry = list["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["slug"] == ID)
        .unwrap()
        .clone();
    assert_eq!(entry["visibility"], "hide");
    r.handle.shutdown();
    task.await.unwrap();
}
