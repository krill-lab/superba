#![allow(missing_docs)]
//! User bug 28 (0.1.0 on macOS): turning on the local router failed and nothing
//! said why: no member had a key the router can send as an API key. The Router
//! page now disables 开启 with the reason under the banner and the direct fixes,
//! and 诊断 shows the router doctor report. User bug 39: the gate reads the
//! same snapshot as the member list, so a member added here or with `spb`
//! unlocks 开启 at once.

mod router_harness;
mod support;

use std::sync::Arc;

use router_harness::Harness;
use superba_core::{
    AuthChoice, Locale, ProfileId, RouterInstallRequest, SwitchIntent, SwitchService,
};
use superba_gui::pages::router::backend::{BoxFut, RouterBackend, SystemRouter};
use superba_gui::pages::router::model::{Candidate, Snapshot};
use superba_gui::pages::router::{self, Dialog, State};
use superba_gui::pages::{Action, Page};
use support::fake::{FakeOptions, FakeService};

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

/// The user's state (bug 28): the Krill profile without a key, no router member.
fn keyless(service: &FakeService) {
    service.edit_profiles(|set| {
        set.settings.router.members.clear();
        for p in set.profiles.iter_mut().filter(|p| !p.is_builtin()) {
            p.key_ref = None;
        }
    });
}

#[test]
fn without_an_api_key_member_the_page_says_why_and_offers_the_fixes() {
    let service = fake();
    let keys: Vec<_> = {
        let mut keys = Vec::new();
        service.edit_profiles(|set| {
            keys = set.profiles.iter().map(|p| p.key_ref.clone()).collect();
        });
        keys
    };
    keyless(&service);
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, service.clone(), true);
    let mut state = State::default();
    let f = h.until(
        |ui, cx, out| router::show(ui, cx, &mut state, out),
        |f| f.has("开启路由需要至少一个有 API key 的成员。"),
    );
    assert!(f.has("Krill：还没有填写 API key"), "{}", f.all_text());
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

    // With its key back — and PAT as its direct auth (user bug 78: one
    // `spb switch --auth pat`) — the key goes to the router as an API key:
    // the panel offers to add the profile as a member, and once it is one
    // the panel is gone.
    service.edit_profiles(|set| {
        for (p, k) in set.profiles.iter_mut().zip(&keys) {
            if !p.is_builtin() {
                p.key_ref = k.clone();
                p.set_auth(AuthChoice::Pat);
            }
        }
    });
    h.reload(&service);
    let f = h.until(
        |ui, cx, out| router::show(ui, cx, &mut state, out),
        |f| f.has("添加 Krill 为成员"),
    );
    assert!(
        f.has("Krill：可以作为成员（有 API key）"),
        "{}",
        f.all_text()
    );
    assert!(!f.has("个人访问令牌"), "{}", f.all_text());
    h.click_text(&f, "添加 Krill 为成员");
    h.until(
        |ui, cx, out| router::show(ui, cx, &mut state, out),
        |_| members(&service) == ["krill"],
    );
    h.reload(&service);
    let f = h.frames(3, |ui, cx, out| router::show(ui, cx, &mut state, out));
    assert!(
        !f.has("开启路由需要至少一个有 API key 的成员。"),
        "{}",
        f.all_text()
    );
}

/// Krill with its key and Auto auth (the router sends it as an API key).
fn keyed_auto(service: &FakeService) {
    service.edit_profiles(|set| {
        set.settings.router.members.clear();
        for p in set.profiles.iter_mut().filter(|p| !p.is_builtin()) {
            p.set_auth(AuthChoice::Auto);
        }
    });
}

fn plans_router_on(f: &router_harness::Frame) -> bool {
    f.actions
        .iter()
        .any(|a| matches!(a, Action::Plan(r) if r.intent == SwitchIntent::Router { enable: true }))
}

/// A page whose backend is the fake switch service alone (no service manager:
/// deterministic, the router counts as not answering, so 开启 starts it first).
fn page(service: &Arc<FakeService>) -> State {
    State::with_backend(Arc::new(SystemRouter::for_service(service.clone())))
}

/// User bug 39 (0.1.1-alpha.1 on Windows): the shell's profile set was loaded
/// before Krill became the member (here: by `spb router enable --member`, or an
/// earlier session), so Krill was listed as member #1 while the panel said
/// "开启路由需要至少一个有 API key 的成员。Krill：可以作为成员（有 API key）" with
/// "添加 Krill 为成员", and both 开启 stayed disabled. The page now reads the
/// member list and the gate from the same snapshot.
#[test]
fn a_member_set_outside_the_page_unlocks_turn_on_without_a_reload() {
    let service = fake();
    keyed_auto(&service);
    // The shell data is loaded now, without members (as at app start) ...
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, service.clone(), true);
    // ... then Krill becomes the member and the router is installed.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(service.router_install(RouterInstallRequest {
        members: vec![ProfileId::new("krill")],
    }))
    .unwrap();
    assert!(
        h.data
            .profiles
            .value
            .as_ref()
            .is_some_and(|p| p.settings.router.members.is_empty()),
        "the shell's copy is stale, as in the user's GUI"
    );
    let mut state = page(&service);
    let started = std::time::Instant::now();
    while !state.snapshot().is_some_and(|s| !s.members.is_empty()) {
        assert!(
            started.elapsed().as_secs() < 10,
            "no snapshot with the member"
        );
        h.frame(|ui, cx, out| router::show(ui, cx, &mut state, out));
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let f = h.frames(3, |ui, cx, out| router::show(ui, cx, &mut state, out));
    assert!(f.has("成员与线路顺序"), "{}", f.all_text());
    assert!(
        !f.has("开启路由需要至少一个有 API key 的成员。"),
        "{}",
        f.all_text()
    );
    assert!(!f.has("添加 Krill 为成员"), "{}", f.all_text());
    // 开启 is enabled: it starts the router and plans router mode.
    h.click_text(&f, "开启路由…");
    h.until(
        |ui, cx, out| router::show(ui, cx, &mut state, out),
        plans_router_on,
    );
}

/// Adding the member from the panel unlocks 开启 on the spot (the shell's
/// profile set is reloaded too, but the page does not wait for it).
#[test]
fn adding_krill_from_the_panel_unlocks_turn_on_without_a_reload() {
    let service = fake();
    keyed_auto(&service);
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, service.clone(), true);
    let mut state = page(&service);
    let f = h.until(
        |ui, cx, out| router::show(ui, cx, &mut state, out),
        |f| f.has("添加 Krill 为成员"),
    );
    assert!(f.has("开启路由需要至少一个有 API key 的成员。"));
    assert!(
        f.has("Krill：可以作为成员（有 API key）"),
        "{}",
        f.all_text()
    );
    h.click_text(&f, "添加 Krill 为成员");
    // The add asks the shell to reload its data (the palette, the Usage page).
    h.until(
        |ui, cx, out| router::show(ui, cx, &mut state, out),
        |f| f.actions.iter().any(|a| matches!(a, Action::Refresh)),
    );
    assert_eq!(members(&service), ["krill"]);
    // No h.reload: the page alone must agree with itself.
    let f = h.until(
        |ui, cx, out| router::show(ui, cx, &mut state, out),
        |f| !f.has("开启路由需要至少一个有 API key 的成员。") && f.has("成员与线路顺序"),
    );
    assert!(!f.has("添加 Krill 为成员"), "{}", f.all_text());
    h.click_text(&f, "开启路由…");
    h.until(
        |ui, cx, out| router::show(ui, cx, &mut state, out),
        plans_router_on,
    );
}

/// The production backend over the fake service, with the router program on
/// a Parallels shared folder (what `default_backend` finds for a Superba run
/// from `\\Mac\Home\Downloads\windows\`).
struct OnNetworkShare(SystemRouter);

impl RouterBackend for OnNetworkShare {
    fn snapshot(&self) -> BoxFut<Snapshot> {
        let inner = self.0.snapshot();
        Box::pin(async move {
            let mut s = inner.await?;
            s.network_exe = Some(r"\\Mac\Home\Downloads\windows\superba-router.exe".into());
            Ok(s)
        })
    }
    fn set_order(&self, ids: Vec<String>) -> BoxFut<()> {
        self.0.set_order(ids)
    }
    fn add_member(&self, candidate: Candidate) -> BoxFut<()> {
        self.0.add_member(candidate)
    }
    fn remove_member(&self, id: String) -> BoxFut<()> {
        self.0.remove_member(id)
    }
    fn install(&self) -> BoxFut<()> {
        self.0.install()
    }
    fn restart(&self) -> BoxFut<()> {
        self.0.restart()
    }
    fn uninstall(&self) -> BoxFut<()> {
        self.0.uninstall()
    }
}

/// User bug 39: the Run-key service of a Superba run from a network share
/// fails at sign-in when the share is not connected yet; the page says so
/// under the service and suggests the MSI.
#[test]
fn a_router_program_on_a_network_share_is_flagged_under_the_service() {
    let service = fake();
    keyed_auto(&service);
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, service.clone(), true);
    let mut state = State::with_backend(Arc::new(OnNetworkShare(SystemRouter::for_service(
        service.clone(),
    ))));
    let f = h.until_scrolled(
        |ui, cx, out| router::show(ui, cx, &mut state, out),
        |f| f.has("路由程序在网络共享上"),
    );
    assert!(
        f.has(r"\\Mac\Home\Downloads\windows\superba-router.exe"),
        "{}",
        f.all_text()
    );
    assert!(f.has("请用 MSI 安装 Superba"), "{}", f.all_text());
    // English.
    let mut h = Harness::new([1280.0, 800.0], Locale::En, service.clone(), true);
    let mut state = State::with_backend(Arc::new(OnNetworkShare(SystemRouter::for_service(
        service.clone(),
    ))));
    h.until_scrolled(
        |ui, cx, out| router::show(ui, cx, &mut state, out),
        |f| f.has("The router program is on a network share") && f.has("MSI"),
    );
    // A local program: no warning.
    let mut h = Harness::new([1280.0, 800.0], Locale::ZhCn, service.clone(), true);
    let mut state = page(&service);
    let f = h.until_scrolled(
        |ui, cx, out| router::show(ui, cx, &mut state, out),
        |f| f.has("成员与线路顺序"),
    );
    assert!(!f.has("网络共享"), "{}", f.all_text());
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
    keyless(&service);
    let mut h = Harness::new([1440.0, 900.0], Locale::En, service.clone(), true);
    let mut state = State::default();
    let f = h.until(
        |ui, cx, out| router::show(ui, cx, &mut state, out),
        |f| f.has("Turning on the router needs at least one member with an API key."),
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
}
