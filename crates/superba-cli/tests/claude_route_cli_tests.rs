//! `spb claude route` and `spb claude status` with the local router (user
//! request 79): the route needs the router set up; status names the route,
//! the router's base URL and every provider Claude Code uses there, with the
//! reason one cannot serve it.

#![allow(missing_docs)]

mod common;

use common::*;
use superba_core::{
    ClaudeIneligible, ClaudeRoute, ClaudeView, ProfileId, RouterClaudeMember, RouterClaudeView,
};

fn view(router: Option<&str>) -> ClaudeView {
    serde_json::from_value(serde_json::json!({
        "config_dir": "/home/u/.claude",
        "settings_path": "/home/u/.claude/settings.json",
        "file": {"kind": "valid"},
        "config": {"provider": "work"},
        "provider": "work",
        "router_base_url": router,
    }))
    .unwrap()
}

#[tokio::test]
async fn the_router_route_needs_the_router_and_is_saved_with_a_hint_to_apply() {
    let service = FakeService::new();
    *service.claude.lock().unwrap() = Some(view(None));
    let (code, _, err) = invoke(service.clone(), &["claude", "route", "router"]).await;
    assert_ne!(code, 0);
    assert!(err.contains("spb router install"), "{err}");
    assert!(!service.calls().names.contains(&"save_claude_config"));

    *service.claude.lock().unwrap() = Some(view(Some("http://127.0.0.1:18431/claude")));
    let (code, out, _) = invoke(service.clone(), &["claude", "route", "router"]).await;
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("spb claude apply"), "{out}");
    assert!(out.contains("http://127.0.0.1:18431/claude"), "{out}");
    let saved = service.claude.lock().unwrap().clone().unwrap();
    assert_eq!(saved.config.route, ClaudeRoute::Router);

    let (code, _, _) = invoke(service.clone(), &["claude", "route", "direct"]).await;
    assert_eq!(code, 0);
    let saved = service.claude.lock().unwrap().clone().unwrap();
    assert_eq!(saved.config.route, ClaudeRoute::Direct);
}

#[tokio::test]
async fn status_lists_claude_codes_providers_on_the_router_with_their_reason() {
    let service = FakeService::new();
    let mut v = view(Some("http://127.0.0.1:18431/claude"));
    v.config.route = ClaudeRoute::Router;
    *service.claude.lock().unwrap() = Some(v);
    service.v2.lock().unwrap().router.claude = Some(RouterClaudeView {
        route_router: true,
        routed: true,
        base_url: Some("http://127.0.0.1:18431/claude".into()),
        members: vec![
            RouterClaudeMember {
                profile_id: ProfileId::new("work"),
                name: "Work".into(),
                endpoint: Some("https://api.krill-code.net".into()),
                ineligible: None,
                preferred: true,
                claude_only: false,
            },
            RouterClaudeMember {
                profile_id: ProfileId::new("plain"),
                name: "Plain".into(),
                endpoint: None,
                ineligible: Some(ClaudeIneligible::NoAnthropicEndpoint),
                preferred: false,
                claude_only: false,
            },
        ],
    });
    let (code, out, _) = invoke(service.clone(), &["claude", "status"]).await;
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("the local router (http://127.0.0.1:18431/claude)"),
        "{out}"
    );
    assert!(
        out.contains("✓ Work (Claude Code's provider)  https://api.krill-code.net"),
        "{out}"
    );
    assert!(
        out.contains(
            "✗ Plain  This provider has no Anthropic endpoint; protocol conversion comes in 0.1.2"
        ),
        "{out}"
    );
    // spb router status shows them too.
    let (code, out, _) = invoke(service, &["router", "status"]).await;
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("Providers for Claude Code"), "{out}");
    assert!(out.contains("Plain (plain)"), "{out}");
}
