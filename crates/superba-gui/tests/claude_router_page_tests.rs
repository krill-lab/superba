#![allow(missing_docs)]
//! Claude Code through the local router in the GUI (user request 79): the
//! 本地路由 choice on 连接与模型 is offered once the router is set up and shows
//! the router's address; the Router page lists Claude Code's providers with
//! the reason one cannot serve it.

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
                claude_only: true,
            },
        ],
    }
}

#[test]
fn the_router_page_lists_claude_codes_providers_with_their_reason() {
    for locale in [Locale::ZhCn, Locale::En] {
        let tr = Tr::new(locale);
        let service = fake();
        service.set_claude_router(Some(url::Url::parse(URL).unwrap()), Some(claude_view()));
        let mut h = Harness::new([1280.0, 900.0], locale, service.clone(), true);
        let mut state = State::default();
        let title = tr.t("router.claude.routed");
        let f = h.until(
            |ui, cx, out| router::show(ui, cx, &mut state, out),
            |f| f.has(&title),
        );
        assert!(f.has(URL), "{}", f.all_text());
        assert!(f.has("Plain"), "{}", f.all_text());
        assert!(f.has(&tr.t("router.claude.preferred")), "{}", f.all_text());
        assert!(
            f.has(&tr.t("router.claude.claude_only")),
            "{}",
            f.all_text()
        );
        assert!(f.has("https://api.krill-code.net"), "{}", f.all_text());
        // The window chrome reduces the column's visible height. The last
        // member's reason may be below the fold; it must remain reachable
        // by scrolling the router column.
        let reason = tr.text(&ClaudeIneligible::NoAnthropicEndpoint.text());
        h.scroll_at(f.find("Plain").unwrap().center(), egui::vec2(0.0, -160.0));
        h.until(
            |ui, cx, out| router::show(ui, cx, &mut state, out),
            |f| f.has(&reason),
        );
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
