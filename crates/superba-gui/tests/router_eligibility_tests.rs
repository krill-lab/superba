#![allow(missing_docs)]
//! User bug 28 (0.1.0 on macOS): turning on the local router failed and nothing
//! said why: no member had a key the router can send as an API key. The Router
//! page now disables 开启 with the reason under the banner and the direct fixes,
//! and 诊断 shows the router doctor report.

mod router_harness;
mod support;

use std::sync::Arc;

use router_harness::Harness;
use superba_core::{AuthChoice, Locale};
use superba_gui::pages::router::{self, Dialog, State};
use superba_gui::pages::{Action, Page};
use support::fake::{FakeOptions, FakeService};

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

/// The user's state: the Krill profile set to PAT only, no router member.
fn pat_only(service: &FakeService) {
    service.edit_profiles(|set| {
        set.settings.router.members.clear();
        for p in set.profiles.iter_mut().filter(|p| !p.is_builtin()) {
            p.set_auth(AuthChoice::Pat);
        }
    });
}

#[test]
fn without_an_api_key_member_the_page_says_why_and_offers_the_fixes() {
    let service = fake();
    pat_only(&service);
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, service.clone(), true);
    let mut state = State::default();
    let f = h.until(
        |ui, cx, out| router::show(ui, cx, &mut state, out),
        |f| f.has("开启路由需要至少一个用 API key 的成员。"),
    );
    assert!(
        f.has("Krill：认证方式设为仅用个人访问令牌（PAT），路由需要 API key"),
        "{}",
        f.all_text()
    );
    // 为 Krill 填写 API key opens the Providers page key field.
    h.click_text(&f, "为 Krill 填写 API key");
    let f = h.frames(4, |ui, cx, out| router::show(ui, cx, &mut state, out));
    assert!(
        f.actions.iter().any(|a| matches!(
            a,
            Action::PageCommand { page: Page::Providers, command } if command == "key:krill"
        )),
        "{:?}",
        f.actions
    );
    // Nothing tried to start the router.
    assert!(!f.actions.iter().any(|a| matches!(a, Action::Plan(_))));

    // With Auto the key goes to the router as an API key: the panel offers to
    // add the profile as a member, and once it is one the panel is gone.
    service.edit_profiles(|set| {
        for p in set.profiles.iter_mut().filter(|p| !p.is_builtin()) {
            p.set_auth(AuthChoice::Auto);
        }
    });
    h.reload(&service);
    let f = h.until(
        |ui, cx, out| router::show(ui, cx, &mut state, out),
        |f| f.has("添加 Krill 为成员"),
    );
    h.click_text(&f, "添加 Krill 为成员");
    h.until(
        |ui, cx, out| router::show(ui, cx, &mut state, out),
        |_| members(&service) == ["krill"],
    );
    h.reload(&service);
    let f = h.frames(3, |ui, cx, out| router::show(ui, cx, &mut state, out));
    assert!(
        !f.has("开启路由需要至少一个用 API key 的成员。"),
        "{}",
        f.all_text()
    );
}

fn members(service: &FakeService) -> Vec<String> {
    let mut members = Vec::new();
    service.edit_profiles(|set| {
        members = set
            .settings
            .router
            .members
            .iter()
            .map(|m| m.to_string())
            .collect();
    });
    members
}

#[test]
fn english_wording_and_the_diagnostics_dialog() {
    let service = fake();
    pat_only(&service);
    let mut h = Harness::new([1440.0, 900.0], Locale::En, service.clone(), true);
    let mut state = State::default();
    let f = h.until(
        |ui, cx, out| router::show(ui, cx, &mut state, out),
        |f| f.has("Turning on the router needs at least one member that uses an API key."),
    );
    assert!(f.has("Enter an API key for Krill"), "{}", f.all_text());
    // 诊断 from the palette opens the report dialog.
    h.frame(|_, cx, out| router::command(&mut state, cx, "doctor", out));
    assert_eq!(state.dialog, Some(Dialog::Doctor));
    let f = h.until(
        |ui, cx, out| router::show(ui, cx, &mut state, out),
        |f| f.has("Local router diagnostics") && !f.has("Collecting diagnostics…"),
    );
    assert!(f.has("Copy"), "{}", f.all_text());
    h.frame(|_, cx, _| {
        let titles: Vec<String> = router::palette(cx, &state)
            .into_iter()
            .map(|i| i.title)
            .collect();
        assert!(
            titles.contains(&"Local router diagnostics".to_owned()),
            "{titles:?}"
        );
    });
}
