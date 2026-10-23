#![allow(missing_docs)]

//! User bug 108: a provider's line can be disabled on the Providers page. The
//! row keeps its place, is muted with a 已禁用 / Disabled badge instead of the
//! latency and the row action, the last enabled line's switch is locked, and
//! disabling the line in use (direct mode) asks first, then switches to the
//! first enabled line.

mod a11y_harness;
mod router_harness;
mod support;

use std::sync::Arc;

use a11y_harness::AppHarness;
use egui::accesskit::{Role, Toggled};
use router_harness::{Frame, Harness};
use superba_core::{LineId, Locale, ProfileId, SwitchService, catalog};
use superba_gui::i18n::Tr;
use superba_gui::pages::{Action, Page, providers, shared};
use support::fake::{FakeOptions, FakeService};

const SIZES: [[f32; 2]; 2] = [[960.0, 640.0], [1280.0, 800.0]];

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

fn line_name(tr: &Tr, id: &str) -> String {
    tr.text(&catalog::krill_site().line(&LineId::new(id)).unwrap().name)
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// Disable `lines` of the fake's Krill provider.
fn disable(service: &FakeService, lines: &[&str]) {
    rt().block_on(async {
        let set = service.profile_set().await.unwrap();
        let mut p = set.profile(&ProfileId::new("krill")).unwrap().clone();
        let site = catalog::krill_site();
        for l in lines {
            p.set_line_disabled(&site, &LineId::new(*l), true).unwrap();
        }
        service.save_profile(p, None).await.unwrap();
    });
}

fn krill_disabled(service: &FakeService) -> Vec<String> {
    rt().block_on(service.profile_set())
        .unwrap()
        .profile(&ProfileId::new("krill"))
        .unwrap()
        .disabled_lines
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// The texts on the row of the line named `line`.
fn row_texts(frame: &Frame, line: &str) -> Vec<String> {
    let y = frame
        .texts
        .iter()
        .find(|(t, _)| t == line)
        .unwrap_or_else(|| panic!("no line {line:?}:\n{}", frame.all_text()))
        .1
        .center()
        .y;
    frame
        .texts
        .iter()
        .filter(|(_, r)| (r.center().y - y).abs() < 20.0)
        .map(|(t, _)| t.clone())
        .collect()
}

#[test]
fn a_disabled_row_is_muted_keeps_its_place_and_has_no_action() {
    for size in SIZES {
        for locale in [Locale::ZhCn, Locale::En] {
            let tr = Tr::new(locale);
            let service = fake();
            disable(&service, &["cn-fast"]);
            let mut h = Harness::new(size, locale, service.clone(), true)
                .min_height(Page::Providers.min_height());
            let mut state = providers::State::default();
            let use_line = tr.t("connection.lines.use");
            let disabled = tr.t("connection.lines.disabled");
            let f = h.until(
                |ui, cx, out| providers::show(ui, cx, &mut state, out),
                |f| f.has(&use_line) && f.has(&disabled),
            );
            let ctx = format!("{size:?} {locale:?}");
            let cn_fast = row_texts(&f, &line_name(&tr, "cn-fast"));
            assert!(cn_fast.contains(&disabled), "{ctx}: {cn_fast:?}");
            assert!(!cn_fast.contains(&use_line), "{ctx}: {cn_fast:?}");
            // It keeps its place: rank 2 of 3.
            assert!(cn_fast.contains(&"2".to_owned()), "{ctx}: {cn_fast:?}");
            let cdn = row_texts(&f, &line_name(&tr, "cdn"));
            assert!(cdn.contains(&use_line), "{ctx}: {cdn:?}");
            assert!(!cdn.contains(&disabled), "{ctx}");
        }
    }
}

#[test]
fn the_switch_disables_and_enables_a_line_and_the_last_one_is_locked() {
    let service = fake();
    let tr = Tr::new(Locale::ZhCn);
    let mut h = AppHarness::new(service.clone(), Locale::ZhCn, Page::Providers);
    let switch = |line: &str| {
        tr.f(
            "connection.lines.enable_named",
            &[("line", &line_name(&tr, line))],
        )
    };
    let t = h.wait_for(Some(Role::Switch), &switch("cdn"));
    let id = t.find(Some(Role::Switch), &switch("cdn")).unwrap();
    assert_eq!(t.node(id).toggled(), Some(Toggled::True));
    // Not the line in use: a plain save, no switch flow.
    h.click(Some(Role::Switch), &switch("cdn"));
    let t = h.until("cdn is disabled", |t| {
        t.has(&tr.f(
            "connection.lines.disabled_named",
            &[("line", &line_name(&tr, "cdn"))],
        ))
    });
    assert_eq!(krill_disabled(&service), ["cdn"]);
    let id = t.find(Some(Role::Switch), &switch("cdn")).unwrap();
    assert_eq!(t.node(id).toggled(), Some(Toggled::False));
    assert!(
        t.find(
            Some(Role::Button),
            &tr.f(
                "connection.lines.use_named",
                &[("line", &line_name(&tr, "cdn"))]
            )
        )
        .is_none(),
        "a disabled line cannot be switched to"
    );
    for key in ["banner.switch_now", "banner.reapply.now"] {
        assert!(t.find(Some(Role::Button), &tr.t(key)).is_none(), "{key}");
    }
    // cn-fast too: overseas (in use) is then the last enabled line, locked.
    h.click(Some(Role::Switch), &switch("cn-fast"));
    let t = h.until("cn-fast is disabled", |t| {
        t.has(&tr.f(
            "connection.lines.disabled_named",
            &[("line", &line_name(&tr, "cn-fast"))],
        ))
    });
    let last = t.find(Some(Role::Switch), &switch("overseas")).unwrap();
    assert!(t.node(last).is_disabled(), "{}", t.describe(last));
    assert_eq!(t.node(last).toggled(), Some(Toggled::True));
    // Enabled again.
    h.click(Some(Role::Switch), &switch("cdn"));
    h.until("cdn is enabled", |t| {
        t.find(Some(Role::Switch), &switch("cdn"))
            .is_some_and(|id| t.node(id).toggled() == Some(Toggled::True))
    });
    assert_eq!(krill_disabled(&service), ["cn-fast"]);
}

#[test]
fn disabling_the_line_in_use_asks_then_switches_to_the_first_enabled_line() {
    let service = fake();
    let data = router_harness::data_from(&service);
    let site = catalog::krill_site();
    let profile = data.profile(&ProfileId::new("krill")).unwrap().clone();
    let overseas = LineId::new("overseas");
    assert_eq!(
        shared::applied_line(&data, &profile),
        Some(overseas.clone())
    );
    // The line in use: confirm first, the next enabled line is named.
    match shared::toggle_action(&data, &profile, &site, &overseas, true) {
        Some(shared::LineToggle::Confirm { next }) => assert_eq!(next, LineId::new("cn-fast")),
        other => panic!("{other:?}"),
    }
    let actions = shared::disable_and_switch(&profile, &site, &overseas);
    assert!(matches!(
        &actions[..],
        [Action::SaveProfile { profile: saved, .. }, Action::Switch { line: Some(l), .. }]
            if saved.is_line_disabled(&overseas) && l == "cn-fast"
    ));
    // Another line: saved at once.
    assert!(matches!(
        shared::toggle_action(&data, &profile, &site, &LineId::new("cdn"), true),
        Some(shared::LineToggle::Save(Action::SaveProfile { .. }))
    ));

    // In the app: the dialog, then the save and the usual switch flow.
    let tr = Tr::new(Locale::ZhCn);
    let mut h = AppHarness::new(service.clone(), Locale::ZhCn, Page::Providers);
    let name = |l: &str| line_name(&tr, l);
    let switch = tr.f(
        "connection.lines.enable_named",
        &[("line", &name("overseas"))],
    );
    h.wait_for(Some(Role::Switch), &switch);
    h.click(Some(Role::Switch), &switch);
    let confirm = tr.f(
        "connection.lines.disable_active_confirm",
        &[("line", &name("overseas")), ("next", &name("cn-fast"))],
    );
    let t = h.wait_for(Some(Role::Button), &confirm);
    assert!(
        t.has(&tr.f(
            "connection.lines.disable_active_title",
            &[("line", &name("overseas")), ("next", &name("cn-fast"))],
        )),
        "{}",
        t.dump()
    );
    assert!(krill_disabled(&service).is_empty(), "nothing saved before");
    h.click(Some(Role::Button), &confirm);
    h.until("saved", |_| krill_disabled(&service) == ["overseas"]);
}
