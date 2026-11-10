//! Real config writes, HTTP probing and router forwarding in an isolated home.
#![allow(missing_docs)]
mod fakes;
#[path = "../../superba-net/tests/support/mod.rs"]
mod upstream;

use fakes::{World, krill_profile};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use superba_core::{
    AuthMode, CheckResult, ConfigEngine, ExecuteOptions, LineProber, NoEvents, OutcomeStatus,
    ResolvedEndpoints, RouterInstallRequest, SafeDefaults, Secret, SwitchRequest, SwitchService,
};
use superba_router::{RouterSecrets, RouterServer};
use superba_switch::{Switcher, Tuning};
use upstream::{MockResponse, MockServer};

async fn exercise(routed: bool) {
    let upstream = MockServer::start(|r| match r.path.as_str() {
        "/v1/models" => MockResponse::json(200, json!({"data": [{"id": "gpt-5.5"}]})),
        "/v1/responses" => MockResponse::sse(upstream::COMPLETED_SSE),
        "/v1/responses/compact" => MockResponse::json(200, json!({"output": []})),
        _ => MockResponse::empty(404),
    })
    .await;
    let base = url::Url::parse(&format!(
        "{}/v1",
        upstream.origin().replace("127.0.0.1", "[::ffff:127.0.0.1]")
    ))
    .unwrap();
    let w = World::new();
    // Never enumerate or restart a real app: only the filesystem/config engine,
    // prober and HTTP router are real in this integration test.
    *w.app.session.lock().unwrap() = None;
    let engine = Arc::new(superba_config::TomlConfigEngine::new(w.ctx.clone()));
    let prober = Arc::new(upstream::prober());
    let mut deps = w.deps();
    deps.config = engine.clone();
    deps.prober = prober.clone();
    let sw = Switcher::with_tuning(
        deps,
        Tuning {
            poll: Duration::from_millis(10),
            verify_timeout: Duration::from_secs(2),
            launch_verify_timeout: Duration::from_secs(2),
            stable: Duration::ZERO,
            ..Tuning::default()
        },
    );
    let mut site = superba_switch::new_compatible_site("lan".into(), "LAN", &base).unwrap();
    assert!(!site.lines[0].origin.is_loopback());
    sw.save_site(site.clone()).await.unwrap();
    let mut profile = krill_profile("lan", None, AuthMode::ApiKey);
    profile.site_id = site.id.clone();
    profile.model.model = Some("gpt-5.5".into());
    sw.save_profile(profile, Some(Secret::new(upstream::KEY)))
        .await
        .unwrap();
    assert!(sw.plan(SwitchRequest::profile("lan".into())).await.is_err());
    assert!(
        sw.router_install(RouterInstallRequest {
            members: vec!["lan".into()]
        })
        .await
        .is_err()
    );
    assert!(
        upstream.recorded().is_empty(),
        "unconfirmed site sends no key"
    );
    site.allow_plain_http = true;
    sw.save_site(site.clone()).await.unwrap();

    let mut router = None;
    if routed {
        sw.router_install(RouterInstallRequest {
            members: vec!["lan".into()],
        })
        .await
        .unwrap();
        let mut config = w.router.written.lock().unwrap().last().unwrap().clone();
        assert!(config.members[0].allow_plain_http);
        assert_eq!(config.members[0].lines[0].base_url, base);
        config.listen_port = 0;
        config.use_system_proxy = false;
        config.tuning.active_probes = false;
        // Exercise forwarding without the optional merged catalog's synthetic models.
        config.catalog = None;
        let server = RouterServer::bind(
            config,
            RouterSecrets {
                router_token: Secret::new(fakes::ROUTER_TOKEN),
                member_keys: BTreeMap::from([("lan".into(), Secret::new(upstream::KEY))]),
                official_tokens: Default::default(),
            },
        )
        .await
        .unwrap();
        *w.router.port.lock().unwrap() = Some(server.local_addr().port());
        router = Some(server.shutdown_handle());
        tokio::spawn(server.serve());
        sw.router_set_enabled(true).await.unwrap();
    }
    let plan = sw.plan(SwitchRequest::profile("lan".into())).await.unwrap();
    let outcome = sw
        .execute(
            plan,
            ExecuteOptions {
                allow_full_restart: true,
                ..Default::default()
            },
            Arc::new(SafeDefaults),
            Arc::new(NoEvents),
        )
        .await
        .unwrap();
    assert_eq!(
        outcome.status,
        OutcomeStatus::Switched,
        "{:?}",
        outcome.notices
    );
    assert_eq!(outcome.verification.unwrap().line, CheckResult::Passed);
    let state = engine.read_state(&w.ctx.codex_home()).await.unwrap();
    let written = url::Url::parse(state.openai_base_url.as_deref().unwrap()).unwrap();
    assert_eq!(written.scheme(), "http");
    let key = Secret::new(if routed {
        fakes::ROUTER_TOKEN
    } else {
        upstream::KEY
    });
    let endpoints = if routed {
        assert_eq!(written.host_str(), Some("127.0.0.1"));
        assert_eq!(
            prober.list_models(&written, &key).await.unwrap()[0].id,
            "gpt-5.5"
        );
        ResolvedEndpoints::for_base_url(&written).unwrap()
    } else {
        assert_eq!(written, base);
        assert_eq!(
            prober
                .list_models_for_site(&site, &written, &key)
                .await
                .unwrap()[0]
                .id,
            "gpt-5.5"
        );
        ResolvedEndpoints::for_site_base_url(&site, &written).unwrap()
    };
    assert!(
        prober
            .probe_responses(&endpoints, &key, "gpt-5.5")
            .await
            .unwrap()
            .ok
    );
    let seen = upstream.recorded();
    assert!(seen.iter().any(|r| r.path == "/v1/models"
        && r.header("authorization") == Some(format!("Bearer {}", upstream::KEY).as_str())));
    assert!(seen.iter().any(|r| r.path == "/v1/responses"
        && r.header("authorization") == Some(format!("Bearer {}", upstream::KEY).as_str())));
    if let Some(router) = router {
        router.shutdown();
    }
}

#[tokio::test]
async fn switch_to_http_list_models_and_send_direct() {
    exercise(false).await;
}

#[tokio::test]
async fn switch_to_http_list_models_and_send_through_router() {
    exercise(true).await;
}

#[tokio::test]
async fn probing_an_unconfirmed_plain_http_site_sends_nothing() {
    let upstream = MockServer::start(|_| MockResponse::json(200, json!({"data": []}))).await;
    let base = url::Url::parse(&format!(
        "{}/v1",
        upstream.origin().replace("127.0.0.1", "[::ffff:127.0.0.1]")
    ))
    .unwrap();
    let w = World::new();
    let mut deps = w.deps();
    deps.prober = Arc::new(upstream::prober());
    let sw = Switcher::with_tuning(deps, Tuning::default());
    let site = superba_switch::new_compatible_site("lan".into(), "LAN", &base).unwrap();
    sw.save_site(site.clone()).await.unwrap();
    let caps = sw.probe_lines(&site.id).await.unwrap();
    assert!(caps.iter().all(|c| !c.reachable), "{caps:?}");
    assert!(upstream.recorded().is_empty(), "{:?}", upstream.recorded());
}
