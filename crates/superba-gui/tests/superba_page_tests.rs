#![allow(missing_docs)]
//! M2 navigation, minimum window, focus deep links and immediate settings saves.
mod a11y_harness;
mod router_harness;
mod support;

use a11y_harness::AppHarness;
use egui::accesskit::Role;
use std::sync::Arc;
use superba_core::{Locale, Settings, SwitchService};
use superba_gui::{
    app::PageStates,
    i18n::Tr,
    pages::{Page, SuperbaSection, superba},
};
use support::fake::{FakeOptions, FakeService};

fn service() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        first_run: false,
        ..Default::default()
    }))
}

#[test]
fn every_section_fits_and_keeps_all_rail_items_visible_at_both_sizes() {
    let data = router_harness::data_from(&service());
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            for window in [[960.0, 640.0], [1280.0, 800.0]] {
                for section in SuperbaSection::ALL {
                    let mut pages = PageStates::default();
                    let mut settings = Settings::default();
                    settings.notify.enabled = true;
                    let report = support::fit::render_fit_min_height(
                        window,
                        locale,
                        theme,
                        &data,
                        Page::Superba(section).min_height(),
                        |ui, cx, out| {
                            pages.show(Page::Superba(section), ui, cx, &mut settings, out)
                        },
                    );
                    assert!(
                        report.fits() && !report.scrolled,
                        "{section:?} {locale:?} {window:?}: {report:?}"
                    );
                    for entry in SuperbaSection::ALL {
                        let label = tr.t(entry.title_key());
                        let r = report
                            .text_shapes
                            .iter()
                            .find(|(s, r, _)| s == &label && r.left() < report.rect.left() + 165.0)
                            .unwrap_or_else(|| {
                                panic!(
                                    "{entry:?} missing in {section:?} at {window:?}: {}",
                                    report.all_text()
                                )
                            });
                        assert!(
                            r.1.bottom() <= report.rect.bottom(),
                            "rail {entry:?} outside viewport: {r:?}"
                        );
                        assert!(!r.2, "rail label elided: {label}");
                    }
                }
            }
        }
    }
}

#[test]
fn home_fact_explanations_and_updates_subtitle_are_never_elided() {
    let data = router_harness::data_from(&service());
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            for window in [[960.0, 640.0], [1280.0, 800.0]] {
                for (section, keys) in [
                    (
                        SuperbaSection::Home,
                        &[
                            "superba.home.backups_desc",
                            "superba.home.router_desc",
                            "superba.home.cli_desc",
                        ][..],
                    ),
                    (
                        SuperbaSection::Updates,
                        &[Page::Superba(SuperbaSection::Updates).subtitle_key()][..],
                    ),
                ] {
                    let mut pages = PageStates::default();
                    let mut settings = Settings::default();
                    let report = support::fit::render_fit_min_height(
                        window,
                        locale,
                        theme,
                        &data,
                        Page::Superba(section).min_height(),
                        |ui, cx, out| {
                            pages.show(Page::Superba(section), ui, cx, &mut settings, out)
                        },
                    );
                    for key in keys {
                        let expected = tr.t(key);
                        let (_, rect, elided) = report
                            .text_shapes
                            .iter()
                            .find(|(text, _, _)| text == &expected)
                            .unwrap_or_else(|| panic!("{key} missing: {}", report.all_text()));
                        assert!(!elided, "{key} elided at {window:?} {locale:?} {theme:?}");
                        assert!(
                            rect.left() >= report.rect.left()
                                && rect.right() <= report.rect.right() + 0.5,
                            "{key} outside page: {rect:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn palette_opens_and_focuses_every_setting_without_changing_it() {
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let mut h = AppHarness::new(service(), locale, Page::Overview);
        h.resize([960.0, 640.0]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let before = rt.block_on(h.service.profile_set()).unwrap().settings;
        for (section, key) in superba::search::ROWS {
            if *key == "prefs.network.app_source" && !cfg!(windows) {
                continue;
            }
            h.shortcut(egui::Key::K);
            h.set_value(&tr.t("palette.placeholder"), key);
            h.key(egui::Key::Enter);
            let tree = h.until(key, |t| {
                t.nodes
                    .get(&t.focus)
                    .and_then(|n| n.bounds())
                    .is_some_and(|r| r.y0 >= 50.0 && r.y1 <= 626.0)
            });
            let label = tr.t(superba::search::label_key(key));
            assert_eq!(
                tree.name(tree.focus).as_deref(),
                Some(label.as_str()),
                "{locale:?} {section:?} {key}:\n{}",
                tree.dump()
            );
            let rect = tree.node(tree.focus).bounds().expect("row bounds");
            assert!(
                rect.y1 >= 50.0 && rect.y0 < 610.0,
                "{key}: row not scrolled into view: {rect:?}"
            );
            assert!(tree.unnamed().is_empty(), "{key}: {:?}", tree.unnamed());
        }
        assert_eq!(
            rt.block_on(h.service.profile_set()).unwrap().settings,
            before
        );
    }
}

#[test]
fn rail_arrows_keep_focus_and_right_and_escape_move_between_panes() {
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let mut h = AppHarness::new(service(), locale, Page::Superba(SuperbaSection::General));
        h.resize([960.0, 640.0]);
        let t = h.wait_for(Some(Role::Tab), &tr.t(SuperbaSection::General.title_key()));
        h.act(
            t.find(Some(Role::Tab), &tr.t(SuperbaSection::General.title_key()))
                .unwrap(),
            egui::accesskit::Action::Focus,
        );
        h.settle();
        let t = h.key(egui::Key::ArrowDown);
        assert_eq!(
            t.name(t.focus),
            Some(tr.t(SuperbaSection::Appearance.title_key()))
        );
        assert_eq!(t.node(t.focus).is_selected(), Some(true));
        let t = h.key(egui::Key::ArrowRight);
        assert_eq!(t.node(t.focus).role(), Role::Group, "{}", t.dump());
        let t = h.key(egui::Key::Tab);
        assert_ne!(
            t.node(t.focus).role(),
            Role::Tab,
            "Tab enters the detail: {}",
            t.dump()
        );
        let t = h.key(egui::Key::Escape);
        assert_eq!(t.node(t.focus).role(), Role::Tab);
        assert_eq!(
            t.name(t.focus),
            Some(tr.t(SuperbaSection::Appearance.title_key()))
        );
        let t = h.key(egui::Key::ArrowUp);
        assert_eq!(
            t.name(t.focus),
            Some(tr.t(SuperbaSection::General.title_key()))
        );
    }
}

#[test]
fn sidebar_remembers_section_and_comma_opens_general() {
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let mut h = AppHarness::new(service(), locale, Page::Overview);
        h.go(Page::Superba(SuperbaSection::Network), &tr);
        h.go(Page::Overview, &tr);
        let t = h.click(Some(Role::Tab), "Superba");
        let network = t
            .find(Some(Role::Tab), &tr.t(SuperbaSection::Network.title_key()))
            .unwrap();
        assert_eq!(t.node(network).is_selected(), Some(true), "{}", t.dump());
        let t = h.shortcut(egui::Key::Comma);
        let general = t
            .find(Some(Role::Tab), &tr.t(SuperbaSection::General.title_key()))
            .unwrap();
        assert_eq!(t.node(general).is_selected(), Some(true), "{}", t.dump());
        assert!(t.has(&tr.t("settings.safe_mode.title")));
    }
}

#[test]
fn preference_edits_still_emit_an_immediate_save() {
    let mut h = router_harness::Harness::new([960.0, 640.0], Locale::En, service(), true);
    let mut pages = PageStates::default();
    let mut settings = Settings::default();
    let page = Page::Superba(SuperbaSection::Appearance);
    let f = h.settle(|ui, cx, out| pages.show(page, ui, cx, &mut settings, out));
    h.click_text(&f, "Light");
    let f = h.until(
        |ui, cx, out| pages.show(page, ui, cx, &mut settings, out),
        |f| {
            f.actions
                .iter()
                .any(|a| matches!(a, superba_gui::pages::Action::SaveSettings))
        },
    );
    assert_eq!(settings.theme, superba_core::Theme::Light);
    assert!(
        f.actions
            .iter()
            .any(|a| matches!(a, superba_gui::pages::Action::SaveSettings))
    );
}
