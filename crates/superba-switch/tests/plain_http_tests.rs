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

/// Write `config` (as the switcher handed it to the router) where a running
/// router reads it, the way `RouterManager::rewrite` does before a reload.
fn store_router_config(
    paths: &superba_router::paths::RouterPaths,
    config: &superba_router::RouterConfig,
) {
    let mut config = config.clone();
    config.listen_port = 0;
    config.use_system_proxy = false;
    config.tuning.active_probes = false;
    config.catalog = None;
    let secrets = RouterSecrets {
        router_token: Secret::new(fakes::ROUTER_TOKEN),
        member_keys: config
            .members
            .iter()
            .map(|m| (m.id.clone(), Secret::new(upstream::KEY)))
            .collect(),
        official_tokens: Default::default(),
    };
    config.save(paths).unwrap();
    secrets.save(paths).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoking_consent_takes_the_member_out_of_the_running_router_before_saving() {
    let serve = |r: &upstream::RecordedRequest| match r.path.as_str() {
        "/v1/models" => MockResponse::json(200, json!({"data": [{"id": "gpt-5.5"}]})),
        "/v1/responses" => MockResponse::sse(upstream::COMPLETED_SSE),
        _ => MockResponse::empty(404),
    };
    let lan_up = MockServer::start(serve).await;
    let local_up = MockServer::start(serve).await;
    let lan_base = url::Url::parse(&format!(
        "{}/v1",
        lan_up.origin().replace("127.0.0.1", "[::ffff:127.0.0.1]")
    ))
    .unwrap();
    let local_base = url::Url::parse(&format!("{}/v1", local_up.origin())).unwrap();
    let w = World::new();
    *w.app.session.lock().unwrap() = None;
    let prober = Arc::new(upstream::prober());
    let mut deps = w.deps();
    deps.prober = prober.clone();
    let sw = Switcher::with_tuning(deps, Tuning::default());
    let mut lan = superba_switch::new_compatible_site("lan".into(), "LAN", &lan_base).unwrap();
    lan.allow_plain_http = true;
    let local = superba_switch::new_compatible_site("local".into(), "Local", &local_base).unwrap();
    for site in [&lan, &local] {
        sw.save_site(site.clone()).await.unwrap();
        let mut profile = krill_profile(site.id.as_str(), None, AuthMode::ApiKey);
        profile.site_id = site.id.clone();
        profile.model.model = Some("gpt-5.5".into());
        sw.save_profile(profile, Some(Secret::new(upstream::KEY)))
            .await
            .unwrap();
    }
    sw.router_install(RouterInstallRequest {
        members: vec!["lan".into(), "local".into()],
    })
    .await
    .unwrap();

    // The running router reads its configuration from files and reloads.
    let dir = tempfile::tempdir().unwrap();
    let paths = superba_router::paths::RouterPaths::new(&superba_core::DataDir::new(dir.path()));
    store_router_config(
        &paths,
        &w.router.written.lock().unwrap().last().unwrap().clone(),
    );
    let server = RouterServer::bind_from_files(paths.clone()).await.unwrap();
    let port = server.local_addr().port();
    let admin =
        superba_router::client::AdminClient::new(port, server.admin_token().clone()).unwrap();
    let handle = server.shutdown_handle();
    tokio::spawn(server.serve());
    let router = ResolvedEndpoints::for_base_url(
        &url::Url::parse(&format!(
            "http://127.0.0.1:{port}{}",
            superba_router::ROUTER_BASE_PATH
        ))
        .unwrap(),
    )
    .unwrap();
    let token = Secret::new(fakes::ROUTER_TOKEN);
    let send = || prober.probe_responses(&router, &token, "gpt-5.5");
    let responses = |up: &MockServer| {
        up.recorded()
            .iter()
            .filter(|r| r.path == "/v1/responses")
            .count()
    };
    assert!(send().await.unwrap().ok);
    assert_eq!(responses(&lan_up), 1, "the first member is the LAN one");

    // Revoked consent (e.g. a re-import of the same provider) while the
    // router cannot be rewritten: nothing is saved, consent stays as it was.
    let mut revoked = lan.clone();
    revoked.allow_plain_http = false;
    let writes = w.router.written.lock().unwrap().len();
    *w.router.fail_rewrite.lock().unwrap() = Some(superba_core::Error::other("router down"));
    assert!(sw.save_site(revoked.clone()).await.is_err());
    *w.router.fail_rewrite.lock().unwrap() = None;
    assert_eq!(w.router.written.lock().unwrap().len(), writes);
    let stored = sw.sites().await.unwrap();
    assert!(
        stored
            .iter()
            .find(|s| s.id == lan.id)
            .unwrap()
            .allow_plain_http
    );

    // With the router reachable, the LAN member leaves its configuration first.
    sw.save_site(revoked).await.unwrap();
    let written = w.router.written.lock().unwrap().last().unwrap().clone();
    let ids: Vec<&str> = written.members.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["local"]);
    assert!(written.members.iter().all(|m| !m.allow_plain_http));
    let set = sw.profile_set().await.unwrap();
    assert_eq!(
        set.settings.router.members,
        vec![superba_core::ProfileId::new("local")]
    );
    assert!(
        !set.custom_sites
            .iter()
            .find(|s| s.id == lan.id)
            .unwrap()
            .allow_plain_http
    );

    // The running router reloads that configuration: no further request
    // reaches the plain-http upstream.
    store_router_config(&paths, &written);
    admin.reload().await.unwrap();
    for _ in 0..3 {
        assert!(send().await.unwrap().ok);
    }
    assert_eq!(responses(&lan_up), 1, "{:?}", lan_up.recorded());
    assert_eq!(responses(&local_up), 3);
    handle.shutdown();
}

#[tokio::test]
async fn revocation_is_refused_while_the_routers_configuration_cannot_be_read() {
    // The router still routes the provider, but its endpoint (configuration
    // or keys) cannot be read: nothing can be rewritten, so nothing is saved.
    let base = url::Url::parse("http://192.168.1.20:8080/v1").unwrap();
    let w = World::new();
    let sw = Switcher::with_tuning(w.deps(), Tuning::default());
    let mut site = superba_switch::new_compatible_site("lan".into(), "LAN", &base).unwrap();
    site.allow_plain_http = true;
    sw.save_site(site.clone()).await.unwrap();
    let mut profile = krill_profile("lan", None, AuthMode::ApiKey);
    profile.site_id = site.id.clone();
    sw.save_profile(profile, Some(Secret::new(upstream::KEY)))
        .await
        .unwrap();
    sw.router_install(RouterInstallRequest {
        members: vec!["lan".into(), "work".into()],
    })
    .await
    .unwrap();
    *w.router.port.lock().unwrap() = None;
    site.allow_plain_http = false;
    assert!(sw.save_site(site.clone()).await.is_err());
    let stored = sw.sites().await.unwrap();
    assert!(
        stored
            .iter()
            .find(|s| s.id == site.id)
            .unwrap()
            .allow_plain_http
    );
}
