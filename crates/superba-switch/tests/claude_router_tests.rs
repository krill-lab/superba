//! Claude Code through the local router (user request 79, §6), with the real
//! settings.json writer and a fake router: the route needs the router set up;
//! applying it writes the router's base URL, Claude Code's own harness token
//! and the gateway hint headers, after the router got its Claude Code section
//! (members with Anthropic endpoints, routes for the provider's models,
//! aliases for Claude Code's own ids); going direct restores the provider's
//! values; a provider only Claude Code uses joins as a Claude-only member;
//! ineligible providers carry their reason; uninstalling the router and a
//! router that stays down move Claude Code back to its provider and say so.

#![allow(missing_docs)]

mod fakes;

use std::sync::Arc;
use std::time::Duration;

use fakes::*;
use serde_json::Value;
use superba_core::claude::AnthropicEndpoint;
use superba_core::{
    ClaudeConfig, ClaudeModels, ClaudePlanKind, ClaudeRoute, ClaudeSlot, DataDir, LocalContext,
    MapEnv, ProfileId, RequestHarness, RouterInstallRequest, RouterWatchState, Severity, SlotModel,
    SwitchService,
};
use superba_router::claude::AnthropicAuth as RouterAuth;
use superba_switch::Switcher;

const ROUTER_URL: &str = "http://127.0.0.1:18431/claude";

fn world() -> World {
    let mut w = World::new();
    w.ctx = LocalContext::new(
        w.tmp.path().join("home"),
        DataDir::new(w.tmp.path().join("data")),
        Arc::new(MapEnv::new().with("SUPERBA_SECRET_BACKEND", "file")),
    );
    std::fs::create_dir_all(w.tmp.path().join("home")).unwrap();
    w
}

fn settings(w: &World) -> Value {
    let path = w
        .tmp
        .path()
        .join("home")
        .join(".claude")
        .join("settings.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// `work` (Krill) for Claude Code: opus and haiku slots, the default model.
fn config(route: ClaudeRoute) -> ClaudeConfig {
    let mut models = ClaudeModels {
        main: Some("gpt-6-sol".into()),
        ..ClaudeModels::default()
    };
    models
        .slots
        .insert(ClaudeSlot::Opus, SlotModel::new("gpt-6-sol"));
    models
        .slots
        .insert(ClaudeSlot::Haiku, SlotModel::new("gpt-5.4-mini"));
    let mut c = ClaudeConfig {
        provider: Some(ProfileId::new("work")),
        route,
        ..ClaudeConfig::default()
    };
    c.models.insert(ProfileId::new("work"), models);
    c
}

/// A second provider on a custom site, with or without an Anthropic endpoint.
fn add_provider(w: &World, id: &str, anthropic: Option<&str>) {
    let mut set = w.profiles.set.lock().unwrap();
    let mut p = set
        .profiles
        .iter()
        .find(|p| p.id == ProfileId::new("work"))
        .unwrap()
        .clone();
    p.id = ProfileId::new(id);
    p.name = id.to_uppercase();
    p.site_id = format!("site-{id}").as_str().into();
    p.line_id = None;
    p.key_ref = Some(superba_core::KeyRef::Keyring {
        service: "superba".into(),
        account: format!("profile:{id}"),
    });
    p.base_url_override = Some(url::Url::parse(&format!("https://{id}.example.org/v1")).unwrap());
    p.anthropic = anthropic.map(|u| {
        let mut e = AnthropicEndpoint::new(url::Url::parse(u).unwrap());
        e.auth = superba_core::AnthropicAuth::ApiKey;
        e
    });
    set.profiles.push(p);
    set.custom_sites.push(
        superba_net::new_compatible_site(
            format!("site-{id}").as_str().into(),
            &id.to_uppercase(),
            &url::Url::parse(&format!("https://{id}.example.org/v1")).unwrap(),
        )
        .unwrap(),
    );
    w.secrets
        .map
        .lock()
        .unwrap()
        .insert(format!("profile:{id}"), format!("sk-{id}-key-0000"));
}

async fn install(sw: &Switcher, members: &[&str]) {
    sw.router_install(RouterInstallRequest {
        members: members.iter().map(|m| ProfileId::new(*m)).collect(),
    })
    .await
    .unwrap();
}

async fn apply(sw: &Switcher) {
    let plan = sw.claude_plan(ClaudePlanKind::Apply).await.unwrap();
    sw.claude_apply(plan, false).await.unwrap();
}

fn last_section(w: &World) -> superba_router::claude::ClaudeConfig {
    w.router
        .written
        .lock()
        .unwrap()
        .last()
        .and_then(|c| c.claude.clone())
        .expect("a router configuration with a claude section")
}

#[tokio::test]
async fn router_mode_needs_the_router_and_writes_its_url_and_claude_codes_token() {
    let w = world();
    let sw = w.switcher();
    // Not set up yet: the route is not offered, a plan is refused.
    let view = sw
        .save_claude_config(config(ClaudeRoute::Router))
        .await
        .unwrap();
    assert_eq!(view.router_base_url, None);
    assert!(
        view.notes
            .iter()
            .any(|n| n.text.key == "switch.claude.router.not_set_up")
    );
    assert!(sw.claude_plan(ClaudePlanKind::Apply).await.is_err());

    install(&sw, &["work"]).await;
    let view = sw.claude_view().await.unwrap();
    assert_eq!(view.router_base_url.unwrap().as_str(), ROUTER_URL);
    apply(&sw).await;

    let s = settings(&w);
    assert_eq!(s["env"]["ANTHROPIC_BASE_URL"], ROUTER_URL);
    let token = superba_router::token::harness_token(
        ROUTER_TOKEN.as_bytes(),
        RequestHarness::ClaudeCode,
        None,
    )
    .unwrap();
    assert_eq!(s["env"]["ANTHROPIC_AUTH_TOKEN"], token.as_str());
    assert_ne!(s["env"]["ANTHROPIC_AUTH_TOKEN"], KEY);
    assert_eq!(s["env"]["CLAUDE_CODE_GATEWAY_HINT_HEADERS"], "1");
    assert_eq!(s["env"]["ANTHROPIC_DEFAULT_OPUS_MODEL"], "gpt-6-sol");

    // The router got its Claude Code section before settings.json pointed at it.
    let section = last_section(&w);
    let work = &section.members["work"];
    let endpoint = work.anthropic.as_ref().unwrap();
    assert_eq!(work.auth, RouterAuth::Bearer);
    assert!(!work.claude_only);
    assert!(endpoint.lines.is_empty(), "a path below the lines' origin");
    assert_eq!(section.order, ["work"]);
    let routed: Vec<&str> = section.routes.iter().map(|r| r.model.as_str()).collect();
    assert!(routed.contains(&"gpt-6-sol") && routed.contains(&"gpt-5.4-mini"));
    let haiku = section
        .aliases
        .iter()
        .find(|a| a.from == "claude-*haiku*")
        .unwrap();
    assert_eq!(haiku.to, "gpt-5.4-mini");
    assert!(
        section
            .aliases
            .iter()
            .any(|a| a.from == "claude-*opus*" && a.to == "gpt-6-sol")
    );

    // Seen as routed by the overview (the shell watches the router) and the
    // Router page.
    assert!(sw.overview().await.unwrap().claude_routed);
    let router = sw.router_status().await.unwrap();
    assert!(!router.members.is_empty());
    for member in &router.members {
        assert_eq!(member.protocols.len(), 3);
        assert_eq!(
            member.protocols[2].protocol,
            superba_core::RequestProtocol::Gemini
        );
    }
    let claude = router.claude.unwrap();
    assert!(claude.routed && claude.route_router);
    assert_eq!(claude.base_url.as_deref(), Some(ROUTER_URL));
    assert_eq!(claude.members.len(), 1);
    assert!(claude.members[0].preferred);
}

#[tokio::test]
async fn going_direct_restores_the_providers_values_and_drops_the_section() {
    let w = world();
    let sw = w.switcher();
    install(&sw, &["work"]).await;
    sw.save_claude_config(config(ClaudeRoute::Router))
        .await
        .unwrap();
    apply(&sw).await;
    assert_eq!(settings(&w)["env"]["ANTHROPIC_BASE_URL"], ROUTER_URL);

    sw.save_claude_config(config(ClaudeRoute::Direct))
        .await
        .unwrap();
    apply(&sw).await;
    let s = settings(&w);
    assert_eq!(s["env"]["ANTHROPIC_BASE_URL"], OVERSEAS);
    assert_eq!(s["env"]["ANTHROPIC_AUTH_TOKEN"], KEY);
    assert!(s["env"].get("CLAUDE_CODE_GATEWAY_HINT_HEADERS").is_none());
    assert!(last_section(&w).members.is_empty());
    assert!(!sw.overview().await.unwrap().claude_routed);
    assert!(sw.router_status().await.unwrap().claude.is_none());
}

#[tokio::test]
async fn a_provider_only_claude_code_uses_joins_the_router_and_others_show_why_not() {
    let w = world();
    add_provider(&w, "deepseek", Some("https://api.deepseek.com/anthropic"));
    add_provider(&w, "plain", None);
    let sw = w.switcher();
    install(&sw, &["work", "plain"]).await;
    let mut c = config(ClaudeRoute::Router);
    c.provider = Some(ProfileId::new("deepseek"));
    c.models.insert(
        ProfileId::new("deepseek"),
        c.models[&ProfileId::new("work")].clone(),
    );
    sw.save_claude_config(c).await.unwrap();
    apply(&sw).await;

    let written = w.router.written.lock().unwrap().last().cloned().unwrap();
    let ids: Vec<&str> = written.members.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        ["work", "plain", "deepseek"],
        "Claude-only member added"
    );
    assert!(written.serves_codex("work") && !written.serves_codex("deepseek"));
    let section = written.claude.unwrap();
    assert_eq!(
        section.order,
        ["deepseek", "work", "plain"],
        "its own provider first"
    );
    assert!(
        section.members["plain"].anthropic.is_none(),
        "Responses-only member is served through conversion"
    );
    let ds = &section.members["deepseek"];
    assert!(ds.claude_only);
    assert_eq!(ds.auth, RouterAuth::XApiKey);
    let route = section
        .routes
        .iter()
        .find(|r| r.model == "gpt-6-sol")
        .unwrap();
    let targets: Vec<&str> = route.targets.iter().map(|t| t.member.as_str()).collect();
    assert_eq!(targets, ["deepseek", "work", "plain"]);

    let claude = sw.router_status().await.unwrap().claude.unwrap();
    let names: Vec<(&str, Option<superba_core::ClaudeIneligible>, bool)> = claude
        .members
        .iter()
        .map(|m| (m.profile_id.as_str(), m.ineligible, m.claude_only))
        .collect();
    assert!(
        claude
            .members
            .iter()
            .find(|m| m.profile_id.as_str() == "plain")
            .unwrap()
            .translated
    );
    assert_eq!(
        names,
        [
            ("deepseek", None, true),
            ("work", None, false),
            ("plain", None, false),
        ]
    );
}

#[tokio::test]
async fn uninstalling_the_router_moves_claude_code_back_to_its_provider() {
    let w = world();
    let sw = w.switcher();
    install(&sw, &["work"]).await;
    sw.save_claude_config(config(ClaudeRoute::Router))
        .await
        .unwrap();
    apply(&sw).await;
    let view = sw.router_uninstall().await.unwrap();
    assert!(
        view.notes
            .iter()
            .any(|n| n.text.key == "switch.claude.router.fell_back"
                && n.severity == Severity::Warning),
        "{:?}",
        view.notes
    );
    let s = settings(&w);
    assert_eq!(s["env"]["ANTHROPIC_BASE_URL"], OVERSEAS);
    assert_eq!(s["env"]["ANTHROPIC_AUTH_TOKEN"], KEY);
    assert_eq!(
        sw.claude_view().await.unwrap().config.route,
        ClaudeRoute::Direct
    );
}

#[tokio::test]
async fn a_router_that_stays_down_moves_claude_code_back_and_says_so() {
    let w = world();
    let sw = w.switcher();
    install(&sw, &["work"]).await;
    sw.save_claude_config(config(ClaudeRoute::Router))
        .await
        .unwrap();
    apply(&sw).await;
    // Codex is direct: the watchdog still watches for Claude Code.
    assert_eq!(
        sw.router_watch().await.unwrap().state,
        RouterWatchState::Healthy
    );
    *w.router.up.lock().unwrap() = false;
    let tick = sw.router_watch().await.unwrap();
    assert!(
        matches!(tick.state, RouterWatchState::Suspect { .. }),
        "{tick:?}"
    );
    w.clock.advance(Duration::from_secs(25));
    let tick = sw.router_watch().await.unwrap();
    assert!(
        matches!(tick.state, RouterWatchState::FellBack { .. }),
        "{tick:?}"
    );
    assert!(
        tick.notes
            .iter()
            .any(|n| n.text.key == "switch.claude.router.fell_back"),
        "{tick:?}"
    );
    assert_eq!(settings(&w)["env"]["ANTHROPIC_BASE_URL"], OVERSEAS);
    // Nothing goes through the router any more.
    assert_eq!(
        sw.router_watch().await.unwrap().state,
        RouterWatchState::Inactive
    );
}

#[tokio::test]
async fn claude_code_follows_a_router_that_moved_to_another_port() {
    let w = world();
    let sw = w.switcher();
    install(&sw, &["work"]).await;
    sw.save_claude_config(config(ClaudeRoute::Router))
        .await
        .unwrap();
    apply(&sw).await;
    *w.router.port.lock().unwrap() = Some(19_440);
    let tick = sw.router_watch().await.unwrap();
    assert!(
        tick.notes
            .iter()
            .any(|n| n.text.key == "switch.claude.router.moved"),
        "{tick:?}"
    );
    assert_eq!(
        settings(&w)["env"]["ANTHROPIC_BASE_URL"],
        "http://127.0.0.1:19440/claude"
    );
}

#[tokio::test]
async fn conversion_switches_and_endpoints_survive_member_rewrites() {
    use superba_router::openai::OpenAiEndpoint;
    use superba_router::translate::{
        ir::Protocol,
        routing::{Conversion, GeminiConfig, GeminiMember},
    };
    let w = world();
    add_provider(&w, "plain", None);
    let sw = w.switcher();
    install(&sw, &["work", "plain"]).await;
    let conversion = Conversion {
        from: Protocol::Messages,
        to: Protocol::Responses,
    };
    {
        let mut written = w.router.written.lock().unwrap();
        let cfg = written.last_mut().unwrap();
        cfg.translation.disabled.insert(conversion);
        cfg.gemini = Some(GeminiConfig {
            members: [(
                "work".into(),
                GeminiMember {
                    endpoint: Some(OpenAiEndpoint::at("/v1beta")),
                    ..Default::default()
                },
            )]
            .into(),
            ..Default::default()
        });
    }
    sw.save_claude_config(config(ClaudeRoute::Router))
        .await
        .unwrap();
    let cfg = w.router.written.lock().unwrap().last().cloned().unwrap();
    assert!(cfg.translation.disabled.contains(&conversion));
    assert!(cfg.gemini_member("work").unwrap().endpoint.is_some());
    assert!(cfg.claude_eligibility("plain").is_err());
    let view = sw.router_status().await.unwrap().claude.unwrap();
    let plain = view
        .members
        .iter()
        .find(|m| m.profile_id.as_str() == "plain")
        .unwrap();
    assert_eq!(
        plain.ineligible,
        Some(superba_core::ClaudeIneligible::NoProtocolRoute)
    );
    assert!(!plain.translated);
}

#[tokio::test]
async fn a_responses_only_preferred_provider_can_join_for_claude_code() {
    let w = world();
    add_provider(&w, "plain", None);
    let sw = w.switcher();
    install(&sw, &["work"]).await;
    let mut c = config(ClaudeRoute::Router);
    c.provider = Some(ProfileId::new("plain"));
    sw.save_claude_config(c).await.unwrap();
    apply(&sw).await;
    let cfg = w.router.written.lock().unwrap().last().cloned().unwrap();
    assert!(!cfg.serves_codex("plain"));
    assert!(cfg.claude_eligibility("plain").is_ok());
    let view = sw.router_status().await.unwrap().claude.unwrap();
    let plain = view
        .members
        .iter()
        .find(|m| m.profile_id.as_str() == "plain")
        .unwrap();
    assert!(plain.translated && plain.claude_only && plain.ineligible.is_none());
    assert_eq!(settings(&w)["env"]["ANTHROPIC_BASE_URL"], ROUTER_URL);
}
