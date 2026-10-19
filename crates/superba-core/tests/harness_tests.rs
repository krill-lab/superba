//! The harness model: ids and protocols, the endpoints a provider offers,
//! the router's ingress, protocol choice, URL and key checks, models from the
//! provider list, and the settings file round trip.

use superba_core::harness::*;
use superba_core::{ModelSpec, Secret, Site, SiteKind, catalog};
use url::Url;

fn u(s: &str) -> Url {
    Url::parse(s).unwrap()
}

fn profile() -> superba_core::Profile {
    serde_json::from_value(
        serde_json::json!({"id":"test", "name":"Test", "site_id":"krill", "auth_mode":"apikey"}),
    )
    .unwrap()
}

fn krill() -> Site {
    catalog::builtin_sites()
        .into_iter()
        .find(|s| s.kind == SiteKind::Krill)
        .unwrap()
}

#[test]
fn ids_and_protocols_parse_their_names() {
    for h in HarnessId::ALL {
        assert_eq!(HarnessId::parse(h.as_str()), Some(h));
        assert_eq!(
            serde_json::to_value(h).unwrap(),
            serde_json::json!(h.as_str())
        );
    }
    assert_eq!(HarnessId::parse("DeepSeek-Harness"), Some(HarnessId::Dsh));
    assert_eq!(HarnessId::parse("claude"), None);
    for (s, p) in [
        ("openai-responses", HarnessProtocol::Responses),
        ("openai-completions", HarnessProtocol::Chat),
        ("openai-chat-completions", HarnessProtocol::Chat),
        ("anthropic-messages", HarnessProtocol::Anthropic),
    ] {
        assert_eq!(HarnessProtocol::parse(s), Some(p));
    }
    assert_eq!(HarnessId::WorkBuddy.protocols(), &[HarnessProtocol::Chat]);
    assert!(HarnessId::Dsh.speaks(HarnessProtocol::Anthropic));
}

#[test]
fn krill_offers_its_line_for_all_three_protocols() {
    let site = krill();
    let origin = u("https://api.krill-code.net");
    let eps = provider_endpoints(
        &site,
        &profile(),
        &origin,
        &u("https://api.krill-code.net/backend-api/codex"),
        None,
    );
    let get = |p| eps.iter().find(|e| e.protocol == p).unwrap();
    assert_eq!(
        get(HarnessProtocol::Responses).base_text(),
        "https://api.krill-code.net/v1"
    );
    assert_eq!(
        get(HarnessProtocol::Chat).base_text(),
        "https://api.krill-code.net/v1"
    );
    assert_eq!(
        get(HarnessProtocol::Anthropic).base_text(),
        "https://api.krill-code.net"
    );
    assert!(eps.iter().all(|e| e.source == EndpointSource::Krill));
    // Declared endpoints win.
    let declared = ProviderProtocols {
        chat: Some(u("https://chat.example/v1")),
        anthropic: None,
    };
    let eps = provider_endpoints(&site, &profile(), &origin, &origin, Some(&declared));
    let chat = eps
        .iter()
        .find(|e| e.protocol == HarnessProtocol::Chat)
        .unwrap();
    assert_eq!(chat.base_text(), "https://chat.example/v1");
    assert_eq!(chat.source, EndpointSource::Declared);
}

#[test]
fn compatible_sites_offer_their_responses_base_and_what_was_declared() {
    let mut site = krill();
    site.kind = SiteKind::Compatible;
    site.lines.clear();
    let base = u("https://api.example.com/v1");
    let eps = provider_endpoints(
        &site,
        &profile(),
        &u("https://api.example.com"),
        &base,
        None,
    );
    assert_eq!(eps.len(), 1);
    assert_eq!(eps[0].protocol, HarnessProtocol::Responses);
    assert_eq!(eps[0].base_text(), "https://api.example.com/v1");
    let declared = ProviderProtocols {
        chat: None,
        anthropic: Some(u("https://api.example.com/anthropic")),
    };
    let eps = provider_endpoints(
        &site,
        &profile(),
        &u("https://api.example.com"),
        &base,
        Some(&declared),
    );
    assert_eq!(
        eps.iter().map(|e| e.protocol).collect::<Vec<_>>(),
        [HarnessProtocol::Responses, HarnessProtocol::Anthropic]
    );
}

#[test]
fn anthropic_uses_the_shared_preset_and_provider_override() {
    use superba_core::claude::{AnthropicEndpoint, PRESET_KRILL_CODEX, anthropic_preset};

    let site = krill();
    let origin = u("https://api.krill-code.net");
    let mut p = profile();
    for endpoint in [
        anthropic_preset(PRESET_KRILL_CODEX)
            .unwrap()
            .endpoint(Some(&origin))
            .unwrap(),
        AnthropicEndpoint::new(u("https://messages.example.com/anthropic")),
    ] {
        p.anthropic = Some(endpoint.clone());
        let eps = provider_endpoints(&site, &p, &origin, &origin, None);
        let anthropic = eps
            .iter()
            .find(|e| e.protocol == HarnessProtocol::Anthropic)
            .unwrap();
        assert_eq!(anthropic.base_url, endpoint.base_url);
        assert_eq!(anthropic.source, EndpointSource::Provider);
        // OpenAI clients still use the all-models endpoint.
        assert_eq!(eps[0].base_text(), "https://api.krill-code.net/v1");

        let declared = ProviderProtocols {
            chat: None,
            anthropic: Some(u("https://explicit.example.com/messages")),
        };
        let eps = provider_endpoints(&site, &p, &origin, &origin, Some(&declared));
        let anthropic = eps
            .iter()
            .find(|e| e.protocol == HarnessProtocol::Anthropic)
            .unwrap();
        assert_eq!(anthropic.base_url, declared.anthropic.unwrap());
        assert_eq!(anthropic.source, EndpointSource::Declared);
    }
}

#[test]
fn compatible_sites_use_the_same_suggested_anthropic_preset_as_claude_code() {
    let mut site = krill();
    site.kind = SiteKind::Compatible;
    site.lines.clear();
    let mut p = profile();
    p.base_url_override = Some(u("https://api.deepseek.com/v1"));
    let eps = provider_endpoints(
        &site,
        &p,
        &u("https://api.deepseek.com"),
        p.base_url_override.as_ref().unwrap(),
        None,
    );
    let anthropic = eps
        .iter()
        .find(|e| e.protocol == HarnessProtocol::Anthropic)
        .unwrap();
    assert_eq!(anthropic.base_text(), "https://api.deepseek.com/anthropic");
    assert_eq!(anthropic.source, EndpointSource::Provider);
}

#[test]
fn the_router_ingress_per_protocol() {
    let r = u("http://127.0.0.1:18731");
    assert_eq!(
        routed_endpoint(&r, HarnessProtocol::Chat)
            .unwrap()
            .base_text(),
        "http://127.0.0.1:18731/openai/v1"
    );
    assert_eq!(
        routed_endpoint(&r, HarnessProtocol::Anthropic)
            .unwrap()
            .base_text(),
        "http://127.0.0.1:18731/claude"
    );
    assert_eq!(
        routed_endpoint(&u("http://127.0.0.1:18731/"), HarnessProtocol::Responses)
            .unwrap()
            .base_text(),
        "http://127.0.0.1:18731/openai/v1"
    );
}

#[test]
fn protocol_choice_prefers_responses_and_respects_the_harness() {
    use HarnessProtocol as P;
    let all = [P::Responses, P::Chat, P::Anthropic];
    assert_eq!(pick_protocol(HarnessId::Pi, None, &all), Some(P::Responses));
    assert_eq!(
        pick_protocol(HarnessId::WorkBuddy, None, &all),
        Some(P::Chat)
    );
    assert_eq!(
        pick_protocol(HarnessId::WorkBuddy, None, &[P::Responses]),
        None
    );
    assert_eq!(
        pick_protocol(HarnessId::Dsh, Some(P::Anthropic), &all),
        Some(P::Anthropic)
    );
    assert_eq!(
        pick_protocol(HarnessId::Dsh, Some(P::Chat), &[P::Responses]),
        None
    );
    assert_eq!(
        pick_protocol(HarnessId::WorkBuddy, Some(P::Responses), &all),
        None
    );
}

#[test]
fn base_urls_are_https_or_loopback_http_without_query() {
    assert!(check_base_url("https://api.example.com/v1").is_ok());
    assert!(check_base_url("http://127.0.0.1:8080/v1").is_ok());
    assert!(check_base_url("http://localhost:8080").is_ok());
    assert!(check_base_url("http://[::1]:8080").is_ok());
    assert_eq!(
        check_base_url("http://example.com/v1"),
        Err("harness.url.http")
    );
    assert_eq!(check_base_url("https://x/v1?a=1"), Err("harness.url.query"));
    assert_eq!(check_base_url("ftp://x"), Err("harness.url.invalid"));
    assert_eq!(check_base_url(" "), Err("harness.url.empty"));
    assert!(!base_url_problem("harness.url.http").fallback.is_empty());
}

#[test]
fn opencode_versions_name_their_major() {
    assert_eq!(OpencodeMajor::of_version("1.19.3"), Some(OpencodeMajor::V1));
    assert_eq!(
        OpencodeMajor::of_version("opencode v2.0.32"),
        Some(OpencodeMajor::V2)
    );
    assert_eq!(
        OpencodeMajor::of_version("v2.1.0\n"),
        Some(OpencodeMajor::V2)
    );
    assert_eq!(OpencodeMajor::of_version("3.0.0"), None);
    assert_eq!(OpencodeMajor::parse("v1"), Some(OpencodeMajor::V1));
}

#[test]
fn models_from_the_provider_list() {
    let spec: ModelSpec = serde_json::from_value(serde_json::json!({
        "slug": "gpt-5.6-luna", "display_name": "GPT-5.6 Luna", "context_window": 272000,
        "reasoning_levels": [{ "effort": "Low" }, { "effort": "medium" }, { "effort": "none" }, { "effort": "medium" }],
        "default_reasoning_level": "medium", "input_modalities": ["text", "image"]
    }))
    .unwrap();
    let m = HarnessModel::from_spec(&spec);
    assert_eq!(m.name.as_deref(), Some("GPT-5.6 Luna"));
    assert_eq!(m.reasoning_levels, ["low", "medium", "none"]);
    assert_eq!(m.named_levels(), ["low", "medium"]);
    assert_eq!(m.default_reasoning.as_deref(), Some("medium"));
    assert!(m.images && m.reasons());
    let plain: ModelSpec =
        serde_json::from_value(serde_json::json!({ "slug": "x", "display_name": "x" })).unwrap();
    let p = HarnessModel::from_spec(&plain);
    assert!(p.name.is_none() && !p.reasons() && !p.images && p.context_window.is_none());
}

fn target(route: HarnessRoute) -> HarnessTarget {
    HarnessTarget {
        claude: None,
        harness: HarnessId::Pi,
        provider: None,
        provider_name: "Krill".into(),
        endpoint: routed_endpoint(&u("http://127.0.0.1:1"), HarnessProtocol::Responses).unwrap(),
        key: Secret::new("sk-secret-value-123456"),
        models: vec![HarnessModel::id_only("a"), HarnessModel::id_only("b")],
        default_model: Some("missing".into()),
        route,
        options: HarnessOptions::default(),
    }
}

#[test]
fn targets_name_themselves_and_never_print_their_key() {
    let t = target(HarnessRoute::Direct);
    assert_eq!(t.display_name(), "Superba · Krill");
    assert_eq!(
        target(HarnessRoute::Routed).display_name(),
        "Superba · Krill (router)"
    );
    // A default that is not one of the models falls back to the first.
    assert_eq!(t.default_model_id(), Some("a"));
    assert!(!format!("{t:?}").contains("secret-value"));
}

#[test]
fn key_text_checks() {
    assert!(check_key_text(&Secret::new("sk-abc_DEF.123")).is_ok());
    for bad in ["a b", "a\"b", "a\\b", "a\u{7}b"] {
        assert!(check_key_text(&Secret::new(bad)).is_err(), "{bad:?}");
    }
    assert_eq!(check_key_text(&Secret::new("")), Err("harness.key.empty"));
}

#[test]
fn pi_skips_its_version_check_when_superba_manages_updates() {
    assert_eq!(pi_launch_env(true), vec![(ENV_PI_SKIP_VERSION_CHECK, "1")]);
    assert!(pi_launch_env(false).is_empty());
}

#[test]
fn the_settings_file_round_trips() {
    let mut c = HarnessConfig::default();
    c.harnesses.insert(
        HarnessId::OpenCode,
        HarnessSettings {
            provider: Some(superba_core::ProfileId::new("krill")),
            protocol: Some(HarnessProtocol::Chat),
            models: vec!["m".into()],
            route: HarnessRoute::Routed,
            router_url: Some(u("http://127.0.0.1:2")),
            options: HarnessOptions {
                opencode_major: Some(OpencodeMajor::V1),
                workbuddy_editions: vec![WorkBuddyEdition::Intl],
                dsh_telemetry: DshTelemetry::Off,
            },
            ..HarnessSettings::default()
        },
    );
    let text = serde_json::to_string(&c).unwrap();
    assert!(text.contains("\"opencode\":"), "{text}");
    let back: HarnessConfig = serde_json::from_str(&text).unwrap();
    assert_eq!(back, c);
    assert_eq!(c.settings(HarnessId::Pi), HarnessSettings::default());
}
