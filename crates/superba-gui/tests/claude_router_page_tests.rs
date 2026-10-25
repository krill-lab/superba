#![allow(missing_docs)]
//! Claude Code through the local router in the GUI (user request 79): the
//! 本地路由 choice on 连接与模型 is offered once the router is set up and shows
//! the router's address and each provider's connection method; the Router
//! page no longer has a Claude Code section of its own (user bug 117).

mod a11y_harness;
mod router_harness;
mod support;

use std::sync::Arc;

use a11y_harness::AppHarness;
use router_harness::Harness;
use superba_core::{
    ClaudeIneligible, ClaudeRoute, Locale, ProfileId, RouterClaudeMember, RouterClaudeView,
};
use superba_gui::i18n::Tr;
use superba_gui::pages::Page;
use superba_gui::pages::router::{self, State};
use support::fake::{FakeOptions, FakeService};

const URL: &str = "http://127.0.0.1:18431/claude";

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

fn claude_view() -> RouterClaudeView {
    RouterClaudeView {
        route_router: true,
        routed: true,
        base_url: Some(URL.into()),
        members: vec![
            RouterClaudeMember {
                profile_id: ProfileId::new("krill"),
                name: "Krill".into(),
                endpoint: Some("https://api.kbsfdxc.net".into()),
                ineligible: None,
                translated: false,
                preferred: true,
                claude_only: false,
            },
            RouterClaudeMember {
                profile_id: ProfileId::new("plain"),
                name: "Plain".into(),
                endpoint: None,
                ineligible: Some(ClaudeIneligible::NoProtocolRoute),
                translated: false,
                preferred: false,
                claude_only: true,
            },
        ],
    }
}

/// Claude Code has no section of its own on the Router page any more (user
/// bug 117): its entry point is listed under 入口 and its providers, route and
/// connection method are on its 连接与模型 page.
#[test]
fn the_router_page_has_no_separate_claude_code_section() {
    for locale in [Locale::ZhCn, Locale::En] {
        let tr = Tr::new(locale);
        let service = fake();
        service.set_claude_router(Some(url::Url::parse(URL).unwrap()), Some(claude_view()));
        let mut h = Harness::new([1280.0, 900.0], locale, service, true);
        let mut state = State::default();
        let f = h.until_scrolled(
            |ui, cx, out| router::show(ui, cx, &mut state, out),
            |f| f.has(&tr.t("recording.title")),
        );
        // "Claude Code" itself stays: it names the tool's row under 入口.
        for gone in [
            tr.t("router.claude.help"),
            tr.t("router.claude.routed"),
            tr.t("router.claude.claude_only"),
        ] {
            assert!(!f.has(&gone), "{locale:?}: {gone}\n{}", f.all_text());
        }
    }
}

#[test]
fn the_connection_page_offers_the_router_once_it_is_set_up() {
    let tr = Tr::new(Locale::ZhCn);
    let service = fake();
    // Not set up: the choice is there but disabled with the hint.
    let mut h = AppHarness::new(service.clone(), Locale::ZhCn, Page::ClaudeCode);
    let t = h.until("page loaded", |t| {
        t.has_containing(&tr.t("claude.route.router"))
    });
    assert!(!t.has_containing(&tr.f("claude.route.via", &[("url", &URL)])));
    // Set up and chosen: the router's address is shown.
    service.set_claude_router(Some(url::Url::parse(URL).unwrap()), Some(claude_view()));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        use superba_core::SwitchService;
        let mut config = service.claude_view().await.unwrap().config;
        config.route = ClaudeRoute::Router;
        service.save_claude_config(config).await.unwrap();
    });
    let mut h = AppHarness::new(service, Locale::ZhCn, Page::ClaudeCode);
    let via = tr.f("claude.route.via", &[("url", &URL)]);
    let t = h.until("router address", |t| t.has_containing(&via));
    assert!(t.unnamed().is_empty(), "{:?}", t.unnamed());
}

#[test]
fn the_connection_caption_distinguishes_conversion_from_a_missing_direct_endpoint() {
    for locale in [Locale::ZhCn, Locale::En] {
        for route in [ClaudeRoute::Direct, ClaudeRoute::Router] {
            let service = fake();
            service.edit_profiles(|set| {
                let mut provider = set
                    .profiles
                    .iter()
                    .find(|p| p.id.as_str() == "krill")
                    .unwrap()
                    .clone();
                provider.id = ProfileId::new("plain");
                provider.name = "Plain".into();
                provider.site_id = "plain-site".into();
                provider.anthropic = None;
                provider.base_url_override =
                    Some(url::Url::parse("https://plain.example/v1").unwrap());
                set.profiles.push(provider);
            });
            service.set_claude_router(Some(url::Url::parse(URL).unwrap()), Some(claude_view()));
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                use superba_core::SwitchService;
                let mut config = service.claude_view().await.unwrap().config;
                config.provider = Some(ProfileId::new("plain"));
                config.route = route;
                service.save_claude_config(config).await.unwrap();
            });
            let tr = Tr::new(locale);
            let label = if route == ClaudeRoute::Router {
                tr.t("claude_routing.translated")
            } else {
                tr.text(&ClaudeIneligible::NoAnthropicEndpoint.text())
            };
            let mut h = AppHarness::new(service, locale, Page::ClaudeCode);
            h.until("provider connection method", |t| t.has_containing(&label));
        }
    }
}
