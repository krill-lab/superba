#![allow(missing_docs)]
//! M4: eight rows, named controls, isolated drafts, reviewed writes and fit.
mod a11y_harness;
mod router_harness;
mod support;
use a11y_harness::{AppHarness, Tree};
use egui::accesskit::Role;
use std::sync::Arc;
use superba_core::{Locale, harness::*};
use superba_gui::{
    AppOptions,
    i18n::Tr,
    pages::{
        Page,
        tools::{self, Selection, State},
    },
};
use support::fake::{FakeOptions, FakeService};
fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}
fn row(t: &Tree, name: &str) -> egui::accesskit::NodeId {
    t.with_role(Role::ListItem)
        .into_iter()
        .find(|id| {
            t.name(*id)
                .is_some_and(|s| s.starts_with(&format!("{name},")))
        })
        .unwrap_or_else(|| panic!("missing row {name}: {}", t.dump()))
}
#[test]
fn eight_rows_and_controls_are_named_in_both_languages_and_sizes() {
    for locale in [Locale::ZhCn, Locale::En] {
        let tr = Tr::new(locale);
        let svc = fake();
        let mut h = AppHarness::new(svc.clone(), locale, Page::Tools);
        for width in [1280.0, 960.0] {
            h.resize([width, if width == 960.0 { 640.0 } else { 800.0 }]);
            let t = h.until("tool model list", |t| t.has("gpt-6-astra"));
            for name in [
                "Codex",
                "Claude Code",
                "dsh",
                "opencode",
                "pi",
                "ZCode",
                "WorkBuddy",
                "Gemini CLI",
            ] {
                row(&t, name);
            }
            assert!(t.unnamed().is_empty(), "{:?}", t.unnamed());
            assert!(
                t.find(Some(Role::ComboBox), &tr.t("tools.provider"))
                    .is_some(),
                "{}",
                t.dump()
            );
            assert_eq!(
                svc.harness_counts(),
                (0, 0),
                "reading page never saves or applies"
            );
        }
    }
}
#[test]
fn drafts_survive_row_changes_and_writes_require_the_review_confirmation() {
    let svc = fake();
    let tr = Tr::new(Locale::En);
    let mut h = AppHarness::new(svc.clone(), Locale::En, Page::Tools);
    let t = h.until("models", |t| t.has("gpt-6-astra"));
    let model = t.find(Some(Role::CheckBox), "gpt-6-astra").unwrap();
    h.click_id(model);
    assert_eq!(svc.harness_counts(), (0, 0));
    let t = h.frame();
    h.click_id(row(&t, "pi"));
    let t = h.frame();
    h.click_id(row(&t, "opencode"));
    let t = h.until("draft retained", |t| {
        t.find(Some(Role::CheckBox), "gpt-6-astra").is_some()
    });
    let node = t.node(t.find(Some(Role::CheckBox), "gpt-6-astra").unwrap());
    assert_eq!(node.toggled(), Some(egui::accesskit::Toggled::False));
    h.click(Some(Role::Button), &tr.t("tools.diff"));
    let t = h.until("review", |t| t.has(&tr.t("tools.review_apply")));
    assert!(t.has(&tr.t("tools.take_over")), "{}", t.dump());
    assert_eq!(
        svc.harness_counts(),
        (0, 0),
        "preview must not save settings or write tool files"
    );
    h.click(Some(Role::Button), &tr.t("tools.cancel"));
    assert_eq!(svc.harness_counts(), (0, 0));
    h.click(Some(Role::Button), &tr.t("tools.apply"));
    h.until("second review", |t| t.has(&tr.t("tools.confirm")));
    h.click(Some(Role::Button), &tr.t("tools.confirm"));
    h.until("applied", |_| svc.harness_counts().1 == 1);
}
#[test]
fn model_failure_is_visible_and_fetch_models_retries() {
    let svc = fake();
    svc.fail_harness_models(true);
    let mut h = AppHarness::new(svc.clone(), Locale::En, Page::Tools);
    h.until("model failure", |t| {
        t.has_containing("Model server unavailable")
    });
    svc.fail_harness_models(false);
    h.click(Some(Role::Button), "Fetch models");
    h.until("retry models", |t| t.has("gpt-6-astra"));
    assert_eq!(svc.harness_counts(), (0, 0));
}
#[test]
fn summaries_offer_their_own_pages_and_no_tool_apply() {
    let tr = Tr::new(Locale::En);
    let mut h = AppHarness::new(fake(), Locale::En, Page::Tools);
    h.until("loaded", |t| t.has("gpt-6-astra"));
    for (name, key) in [
        ("Codex", "tools.open_codex"),
        ("Claude Code", "tools.open_claude"),
    ] {
        let t = h.frame();
        h.click_id(row(&t, name));
        let t = h.until("summary", |t| t.has(&tr.t(key)));
        assert!(!t.has(&tr.t("tools.apply")));
        assert!(t.unnamed().is_empty(), "{:?}", t.unnamed());
    }
}
#[test]
fn all_eight_details_fit_1280_and_960_without_horizontal_overflow() {
    let svc = fake();
    let data = router_harness::data_from(&svc);
    for locale in [Locale::En, Locale::ZhCn] {
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            for width in [1280.0, 960.0] {
                for selected in [Selection::Codex, Selection::Claude]
                    .into_iter()
                    .chain(HarnessId::ALL.map(Selection::Tool))
                {
                    let mut state = State::default();
                    state.selected = selected;
                    for tool in HarnessId::ALL {
                        state.set_view(svc.tool_fixture(tool));
                    }
                    let report = support::fit::render_fit(
                        [width, 800.0],
                        locale,
                        theme,
                        &data,
                        |ui, cx, out| tools::show(ui, cx, &mut state, out),
                    );
                    assert!(
                        report.fits(),
                        "{selected:?} {width} {locale:?}: {} > {}",
                        report.used_bottom,
                        report.rect.max.y
                    );
                    assert!(
                        report.beyond_right().is_empty(),
                        "{selected:?} {width} {locale:?}: {:?}",
                        report.beyond_right()
                    );
                }
            }
        }
    }
}
#[test]
fn dark_theme_preserves_accessible_tool_names() {
    let mut h = AppHarness::with_options(
        fake(),
        AppOptions {
            theme: Some(egui::Theme::Dark),
            locale: Some(Locale::ZhCn),
            page: Some(Page::Tools),
            ..Default::default()
        },
    );
    let t = h.until("tools", |t| t.has("gpt-6-astra"));
    assert!(t.unnamed().is_empty(), "{:?}", t.unnamed());
}

#[test]
fn gemini_row_names_its_route_conversion_and_shared_files() {
    for locale in [Locale::En, Locale::ZhCn] {
        let svc = fake();
        svc.routed_tool_fixture(HarnessId::Gemini, true);
        let mut h = AppHarness::new(svc.clone(), locale, Page::Tools);
        let t = h.until("loaded", |t| t.has("gpt-6-astra"));
        h.click_id(row(&t, "Gemini CLI"));
        let translated = if locale == Locale::ZhCn {
            "经协议转换"
        } else {
            "Via protocol conversion"
        };
        let t = h.until("Gemini route", |t| t.has(translated));
        assert!(t.has_containing("/gemini"), "{}", t.dump());
        assert!(t.has_containing(".gemini/settings.json"));
        assert!(t.has_containing(".gemini/.env"));
        assert!(t.unnamed().is_empty());
        assert_eq!(svc.harness_counts(), (0, 0));
    }
}
