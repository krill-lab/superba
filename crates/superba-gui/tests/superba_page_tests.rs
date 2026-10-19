#![allow(missing_docs)]
//! Tabbed Superba shell, anchored settings, minimum window and accessible navigation.
mod a11y_harness;
#[path = "unit/font_diagnostics.rs"]
mod font_diagnostics;
mod router_harness;
mod support;

use a11y_harness::AppHarness;
use egui::accesskit::Role;
use std::sync::Arc;
use superba_core::{Locale, Settings, SwitchService};
use superba_gui::{
    app::PageStates,
    i18n::Tr,
    pages::{
        Page, SuperbaSection,
        superba::{self, SuperbaTab},
    },
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
fn every_section_fits_and_keeps_all_five_tabs_on_one_line() {
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
                    let mut tab_y: Option<f32> = None;
                    for entry in SuperbaTab::ALL {
                        let label = tr.t(entry.title_key());
                        let r = report
                            .text_shapes
                            .iter()
                            .find(|(s, r, _)| s == &label && r.top() < report.rect.top() + 110.0)
                            .unwrap_or_else(|| {
                                panic!(
                                    "{entry:?} missing in {section:?} at {window:?}: {}",
                                    report.all_text()
                                )
                            });
                        assert!(
                            r.1.bottom() <= report.rect.bottom(),
                            "tab {entry:?} outside viewport: {r:?}"
                        );
                        assert!(!r.2, "tab label elided: {label}");
                        assert!(
                            r.1.left() >= report.rect.left() && r.1.right() <= report.rect.right(),
                            "{entry:?} outside page: {r:?}"
                        );
                        if let Some(y) = tab_y {
                            assert!((r.1.center().y - y).abs() < 1.0, "tabs wrapped");
                        }
                        tab_y = Some(r.1.center().y);
                    }
                }
            }
        }
    }
}

#[test]
fn brand_header_keeps_version_channel_and_status_on_one_line() {
    use superba_core::{SelfUpdateStatus, SelfUpdateView, Timestamp};
    let mut data = router_harness::data_from(&service());
    data.selfupdate.finish(Ok(SelfUpdateView {
        current: env!("CARGO_PKG_VERSION").into(),
        status: SelfUpdateStatus::UpToDate,
        checked_at: Some(Timestamp::from_millis(
            Timestamp::now().as_millis() - 180_000,
        )),
        ..Default::default()
    }));
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            for window in [[960.0, 640.0], [1280.0, 800.0]] {
                let mut pages = PageStates::default();
                let mut settings = Settings::default();
                let report = support::fit::render_fit_min_height(
                    window,
                    locale,
                    theme,
                    &data,
                    Page::Superba(SuperbaSection::Home).min_height(),
                    |ui, cx, out| {
                        pages.show(
                            Page::Superba(SuperbaSection::Home),
                            ui,
                            cx,
                            &mut settings,
                            out,
                        )
                    },
                );
                let title = report
                    .text_shapes
                    .iter()
                    .find(|(s, _, _)| s == "Superba")
                    .unwrap()
                    .1;
                for label in [
                    format!("v{}", env!("CARGO_PKG_VERSION")),
                    tr.t("updates.superba.channel_stable"),
                ] {
                    let (_, rect, elided) = report
                        .text_shapes
                        .iter()
                        .find(|(s, _, _)| s == &label)
                        .unwrap_or_else(|| panic!("{label}: {}", report.all_text()));
                    assert!(!elided);
                    assert!(
                        (rect.center().y - title.center().y).abs() < 5.0,
                        "{label} wrapped at {window:?}: {rect:?} / {title:?}"
                    );
                    assert!(rect.right() <= report.rect.right() + 0.5);
                }
                let (_, rect, elided) = report
                    .text_shapes
                    .iter()
                    .find(|(s, _, _)| s.starts_with(&tr.t("superba.home.current")))
                    .unwrap();
                assert!(
                    !elided && rect.height() < 25.0,
                    "status wrapped at {window:?}: {rect:?}"
                );
                assert!((rect.center().y - title.center().y).abs() < 5.0);
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
                    // The longer data section can end at a fractional scroll
                    // offset. Match the half-pixel tolerance of the fit tests.
                    .is_some_and(|r| r.y0 >= 49.5 && r.y1 <= 626.5)
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
fn tabs_have_names_roles_and_keyboard_navigation_into_the_panel() {
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let mut h = AppHarness::new(service(), locale, Page::Superba(SuperbaSection::General));
        h.resize([960.0, 640.0]);
        let t = h.settle();
        let list = t
            .find(Some(Role::TabList), "Superba")
            .expect("named tab list");
        let tabs: Vec<_> = t
            .node(list)
            .children()
            .iter()
            .filter(|id| t.node(**id).role() == Role::Tab)
            .copied()
            .collect();
        assert_eq!(tabs.len(), 5, "{}", t.dump());
        for (id, tab) in tabs.iter().zip(SuperbaTab::ALL) {
            assert_eq!(t.name(*id), Some(tr.t(tab.title_key())));
            assert!(t.node(*id).supports_action(egui::accesskit::Action::Click));
        }
        h.act(tabs[2], egui::accesskit::Action::Focus);
        h.settle();
        let t = h.key(egui::Key::ArrowRight);
        assert_eq!(
            t.name(t.focus),
            Some(tr.t(SuperbaTab::Diagnostics.title_key()))
        );
        assert_eq!(t.node(t.focus).is_selected(), Some(true));
        let t = h.key(egui::Key::ArrowDown);
        assert_eq!(t.node(t.focus).role(), Role::Group, "{}", t.dump());
        let t = h.key(egui::Key::Tab);
        assert_ne!(t.node(t.focus).role(), Role::Tab, "{}", t.dump());
        let t = h.key(egui::Key::Escape);
        assert_eq!(t.node(t.focus).role(), Role::Tab);
        assert_eq!(
            t.name(t.focus),
            Some(tr.t(SuperbaTab::Diagnostics.title_key()))
        );
        let t = h.key(egui::Key::ArrowLeft);
        assert_eq!(
            t.name(t.focus),
            Some(tr.t(SuperbaTab::Settings.title_key()))
        );
        let t = h.key(egui::Key::Home);
        assert_eq!(t.name(t.focus), Some(tr.t(SuperbaTab::Home.title_key())));
        let t = h.key(egui::Key::End);
        assert_eq!(t.name(t.focus), Some(tr.t(SuperbaTab::Data.title_key())));
    }
}

fn at_anchor(t: &a11y_harness::Tree, tr: &Tr, section: SuperbaSection) -> bool {
    let name = tr.t(section.title_key());
    let active = t
        .find(Some(Role::Button), &name)
        .is_some_and(|id| t.node(id).is_selected() == Some(true));
    let header = t.find_all(Some(Role::Label), &name).into_iter().any(|id| {
        t.node(id)
            .bounds()
            .is_some_and(|r| r.y0 > 150.0 && r.y1 < 350.0)
    });
    active && header
}

#[test]
fn every_settings_deep_link_opens_its_anchor_in_the_settings_tab() {
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        for section in [
            SuperbaSection::General,
            SuperbaSection::Appearance,
            SuperbaSection::Network,
            SuperbaSection::Notify,
        ] {
            for window in [[960.0, 640.0], [1280.0, 800.0]] {
                let mut h = AppHarness::new(service(), locale, Page::Superba(section));
                h.resize(window);
                let t = h.until("initial deep link", |t| at_anchor(t, &tr, section));
                assert!(
                    t.find(
                        Some(Role::TabPanel),
                        &tr.t(SuperbaTab::Settings.title_key())
                    )
                    .is_some()
                );
                assert!(t.unnamed().is_empty(), "{:?}", t.unnamed());
            }
        }
    }
}

#[test]
fn jump_directory_tracks_scroll_and_sidebar_and_comma_restore_anchors() {
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let mut h = AppHarness::new(service(), locale, Page::Superba(SuperbaSection::General));
        h.resize([960.0, 640.0]);
        for section in [
            SuperbaSection::Notify,
            SuperbaSection::Appearance,
            SuperbaSection::Network,
        ] {
            h.click(Some(Role::Button), &tr.t(section.title_key()));
            h.until("jump", |t| at_anchor(t, &tr, section));
        }
        h.scroll_page(-1200.0);
        h.until("scroll spy", |t| {
            t.find(
                Some(Role::Button),
                &tr.t(SuperbaSection::Notify.title_key()),
            )
            .is_some_and(|id| t.node(id).is_selected() == Some(true))
        });
        h.go(Page::Overview, &tr);
        h.click(Some(Role::Tab), "Superba");
        h.until("sidebar restores anchor", |t| {
            at_anchor(t, &tr, SuperbaSection::Notify)
        });
        h.shortcut(egui::Key::Comma);
        h.until("comma opens general", |t| {
            at_anchor(t, &tr, SuperbaSection::General)
        });
        // The current Page can already be General after manually scrolling away.
        h.click(
            Some(Role::Button),
            &tr.t(SuperbaSection::Notify.title_key()),
        );
        h.until("notify again", |t| {
            at_anchor(t, &tr, SuperbaSection::Notify)
        });
        h.shortcut(egui::Key::Comma);
        h.until("same-page comma", |t| {
            at_anchor(t, &tr, SuperbaSection::General)
        });
    }
}

fn modal_has(t: &a11y_harness::Tree, name: &str) -> bool {
    let dialog = t
        .with_role(Role::Dialog)
        .into_iter()
        .next()
        .expect("palette dialog");
    let mut nodes = vec![dialog];
    while let Some(id) = nodes.pop() {
        if t.name(id).as_deref() == Some(name) {
            return true;
        }
        nodes.extend(t.node(id).children());
    }
    false
}

#[test]
fn settings_search_entry_only_lists_settings_and_global_shortcut_resets_scope() {
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let mut h = AppHarness::new(service(), locale, Page::Superba(SuperbaSection::General));
        let t = h.click(Some(Role::Button), &tr.t("superba.search"));
        assert!(modal_has(&t, &tr.t("superba.tab.settings")));
        for key in [
            "palette.group.navigate",
            "palette.group.switch",
            "palette.group.commands",
        ] {
            assert!(!modal_has(&t, &tr.t(key)), "{key}: {}", t.dump());
        }
        h.set_value(&tr.t("palette.placeholder"), "prefs.data.dir");
        h.key(egui::Key::Enter);
        h.until("search across tabs", |t| {
            t.name(t.focus) == Some(tr.t("prefs.data.dir"))
        });
        let t = h.shortcut(egui::Key::K);
        assert!(t.has(&tr.t("palette.group.switch")), "{}", t.dump());
    }
}

#[test]
fn preference_edits_still_emit_an_immediate_save() {
    let mut h = router_harness::Harness::new([960.0, 640.0], Locale::En, service(), true);
    let mut pages = PageStates::default();
    let mut settings = Settings {
        reduce_motion: true,
        ..Settings::default()
    };
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

#[test]
fn tabs_do_not_repeat_page_titles_and_settings_keep_compact_anchor_titles() {
    let data = router_harness::data_from(&service());
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        for window in [[960.0, 640.0], [1280.0, 800.0]] {
            for section in [
                SuperbaSection::Updates,
                SuperbaSection::Diagnostics,
                SuperbaSection::Data,
                SuperbaSection::General,
            ] {
                let mut pages = PageStates::default();
                let mut settings = Settings::default();
                let report = support::fit::render_fit_min_height(
                    window,
                    locale,
                    egui::Theme::Light,
                    &data,
                    Page::Superba(section).min_height(),
                    |ui, cx, out| pages.show(Page::Superba(section), ui, cx, &mut settings, out),
                );
                let name = tr.t(section.title_key());
                let titles: Vec<_> = report
                    .text_shapes
                    .iter()
                    .filter(|(s, r, _)| s == &name && r.top() < report.rect.top() + 250.0)
                    .collect();
                if section == SuperbaSection::Data {
                    assert!(titles.is_empty(), "duplicate data page title: {titles:?}");
                } else if section == SuperbaSection::General {
                    // The directory and one anchor, with no duplicate General subsection.
                    assert_eq!(titles.len(), 2, "{titles:?}");
                    assert!(
                        titles.iter().all(|(_, r, _)| r.height() < 29.0),
                        "oversized anchor: {titles:?}"
                    );
                } else {
                    assert_eq!(
                        titles.len(),
                        1,
                        "only the tab should carry this title: {titles:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn updates_inventory_uses_the_full_panel_width() {
    use superba_gui::design::{layout, tokens::size};
    for locale in [Locale::En, Locale::ZhCn] {
        for window in [[960.0, 640.0], [1280.0, 800.0], [1440.0, 900.0]] {
            let mut h = AppHarness::new(service(), locale, Page::Superba(SuperbaSection::Updates));
            h.resize(window);
            let tree = h.until("update inventory", |t| {
                t.with_role(Role::Button)
                    .into_iter()
                    .any(|id| t.name(id).is_some_and(|n| n.starts_with("Superba, ")))
            });
            let row = tree
                .with_role(Role::Button)
                .into_iter()
                .find(|id| tree.name(*id).is_some_and(|n| n.starts_with("Superba, ")))
                .unwrap();
            let rect = tree.node(row).bounds().unwrap();
            let central = egui::Rect::from_min_max(
                egui::pos2(size::SIDEBAR, 0.0),
                egui::pos2(window[0], window[1]),
            );
            let panel = layout::page_rect(central, true);
            assert!(
                (rect.x0 - f64::from(panel.left())).abs() < 2.0,
                "{rect:?} / {panel:?}"
            );
            assert!(
                f64::from(panel.right()) - rect.x1 <= 20.0,
                "inventory leaves unused width: {rect:?} / {panel:?}"
            );
            assert!(
                rect.x1 <= f64::from(panel.right()),
                "inventory overflows: {rect:?}"
            );
        }
    }
}
