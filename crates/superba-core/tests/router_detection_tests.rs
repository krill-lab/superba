//! Router identity requires the recorded endpoint and applied mode (user bug 101).
#![allow(missing_docs)]
use superba_core::router::{codex_uses_router, matches_router_endpoint};
use superba_core::{ActiveState, AuthMode, AuthShape, ConfigState, Timestamp};
use url::Url;

fn state(base: &str) -> ConfigState {
    ConfigState {
        codex_home: "/isolated/codex".into(),
        config_sha256: None,
        auth_sha256: None,
        parse_error: None,
        model_provider: Some("openai".into()),
        provider_table_present: false,
        openai_base_url: Some(base.into()),
        chatgpt_base_url: None,
        model: None,
        model_owned: false,
        reasoning_effort: None,
        profile_override: None,
        cli_auth_credentials_store: None,
        auth: AuthShape::ApiKey {
            key_last4: "test".into(),
        },
        active: Some(ActiveState {
            profile_id: Some("local".into()),
            site_id: Some("local".into()),
            line_id: None,
            auth_mode: Some(AuthMode::ApiKey),
            generation: 1,
            applied_at: Timestamp(0),
        }),
        drift: Vec::new(),
        auth_drift: None,
        handed_off: None,
        since_release: Vec::new(),
        since_release_auth: None,
        journal_pending: false,
    }
}

#[test]
fn router_mode_requires_enabled_recorded_endpoint_and_applied_token_mode() {
    let endpoint = Url::parse("http://127.0.0.1:18431/backend-api/codex").unwrap();
    let mut config = state(endpoint.as_str());
    assert!(codex_uses_router(&config, true, Some(&endpoint)));
    assert!(!codex_uses_router(&config, false, Some(&endpoint)));
    assert!(!codex_uses_router(&config, true, None));
    config.active.as_mut().unwrap().auth_mode = Some(AuthMode::OfficialChatGpt);
    assert!(!codex_uses_router(&config, true, Some(&endpoint)));
    config.active = None;
    assert!(!codex_uses_router(&config, true, Some(&endpoint)));
    config = state(endpoint.as_str());
    config.model_provider = Some("custom".into());
    assert!(!codex_uses_router(&config, true, Some(&endpoint)));
}

#[test]
fn other_local_ports_paths_and_stale_router_ports_are_not_current_routing() {
    let endpoint = Url::parse("http://127.0.0.1:18431/backend-api/codex").unwrap();
    for observed in [
        "http://127.0.0.1:11434/backend-api/codex",
        "http://localhost:1234/v1",
        "http://[::1]:18431/backend-api/codex",
        "http://127.0.0.1:18431/openai/v1",
        "http://127.0.0.1:18431/backend-api/codex/other",
        "https://127.0.0.1:18431/backend-api/codex",
    ] {
        assert!(
            !codex_uses_router(&state(observed), true, Some(&endpoint)),
            "{observed}"
        );
    }
    let moved = Url::parse("http://127.0.0.1:19440/backend-api/codex").unwrap();
    assert!(!codex_uses_router(
        &state(endpoint.as_str()),
        true,
        Some(&moved)
    ));
    assert!(codex_uses_router(
        &state(moved.as_str()),
        true,
        Some(&moved)
    ));
}

#[test]
fn exact_ingresses_work_on_recorded_loopback_and_wsl_listeners() {
    // WSL NAT/private and mirrored/loopback origins follow the same exact rule.
    for host in [
        "127.0.0.1",
        "localhost",
        "[::1]",
        "172.24.16.1",
        "10.0.0.1",
        "100.64.0.1",
    ] {
        for path in ["/backend-api/codex", "/openai/v1"] {
            let endpoint = Url::parse(&format!("http://{host}:18431{path}")).unwrap();
            assert!(matches_router_endpoint(endpoint.as_str(), &endpoint));
            assert!(matches_router_endpoint(&format!("{endpoint}/"), &endpoint));
            assert!(!matches_router_endpoint(
                &format!("http://{host}:11434{path}"),
                &endpoint
            ));
            assert!(!matches_router_endpoint(
                &format!("http://172.24.16.2:18431{path}"),
                &endpoint
            ));
        }
    }
}

#[test]
fn matching_rejects_credentials_queries_fragments_and_bad_urls() {
    let endpoint = Url::parse("http://localhost/backend-api/codex").unwrap();
    assert!(matches_router_endpoint(
        "http://localhost:80/backend-api/codex/",
        &endpoint
    ));
    for observed in [
        "not a URL",
        "http://user@localhost/backend-api/codex",
        "http://localhost/backend-api/codex?other=1",
        "http://localhost/backend-api/codex#other",
    ] {
        assert!(!matches_router_endpoint(observed, &endpoint));
    }
}
