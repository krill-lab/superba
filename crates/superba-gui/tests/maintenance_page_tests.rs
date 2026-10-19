#![allow(missing_docs)]
//! M5: installed-tool checks, backup restore gates and the actual loaded layouts.
mod a11y_harness;
mod router_harness;
mod support;

use a11y_harness::{AppHarness, Tree};
use egui::accesskit::{Action as A11yAction, Role};
use std::sync::Arc;
use superba_core::{CodexFileKind, Locale, Settings, TargetId, harness::HarnessId};
use superba_gui::{
    app::PageStates,
    i18n::Tr,
    pages::{Page, SuperbaSection},
};
use support::fake::{FakeOptions, FakeService};

fn service(fast: bool) -> Arc<FakeService> {
    let s = Arc::new(FakeService::new(FakeOptions {
        fast,
        first_run: false,
        ..Default::default()
    }));
    s.maintenance_fixture();
    s
}

fn loaded_tool_rows(h: &mut AppHarness, tr: &Tr) -> Tree {
    // A Claude row can precede the overview and doctor results. Those reads
    // change the layout and its auto-generated AccessKit ids, so acquire the
    // action target only after all three independent reads have rendered.
    h.until("Codex, installed tools and diagnostics", |t| {
        ["Codex,", "WorkBuddy,"].into_iter().all(|prefix| {
            t.with_role(Role::Button)
                .into_iter()
                .any(|id| t.name(id).is_some_and(|n| n.starts_with(prefix)))
        }) && t
            .find(Some(Role::Button), &tr.t("diag.run"))
            .is_some_and(|id| t.node(id).supports_action(A11yAction::Click))
    })
}

#[test]
fn tool_rows_expose_install_file_errors_and_drift_in_both_languages() {
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let mut h = AppHarness::new(
            service(true),
            locale,
            Page::Superba(SuperbaSection::Diagnostics),
        );
        let t = loaded_tool_rows(&mut h, &tr);
        let names: Vec<_> = t
            .nodes
            .keys()
            .filter(|id| t.node(**id).role() == Role::Button)
            .filter_map(|id| t.name(*id))
            .collect();
        for tool in [
            "Codex",
            "Claude Code",
            "dsh",
            "opencode",
            "ZCode",
            "WorkBuddy",
        ] {
            assert_eq!(
                names
                    .iter()
                    .filter(|n| n.starts_with(&format!("{tool},")))
                    .count(),
                1,
                "{tool}: {names:?}"
            );
        }
        assert!(
            !names.iter().any(|n| n.starts_with("pi,")),
            "uninstalled tool must be omitted"
        );
        let dsh = names.iter().find(|n| n.starts_with("dsh,")).unwrap();
        assert!(dsh.contains(&tr.f("superba.diagnostics.drift", &[("n", &2)])));
        let zcode = names.iter().find(|n| n.starts_with("ZCode,")).unwrap();
        assert!(zcode.contains(&tr.f(
            "superba.diagnostics.invalid",
            &[("line", &7), ("column", &3)]
        )));
        assert!(!t.dump().contains("secret-must-not-appear"));
        assert!(t.unnamed().is_empty(), "{:?}", t.unnamed());
        let id = t.find(Some(Role::Button), dsh).unwrap();
        h.act(id, A11yAction::Expand);
        let t = h.settle();
        assert!(t.has(&tr.f("superba.diagnostics.configure", &[("tool", &"dsh")])));
        assert_eq!(t.node(id).is_expanded(), Some(true));
        h.act(id, A11yAction::Collapse);
        let t = h.settle();
        assert_eq!(t.node(id).is_expanded(), Some(false));
    }
}

#[test]
fn claude_row_keeps_the_endpoint_check() {
    // Also exercise the real asynchronous loading order, without fast reads.
    for fast in [true, false] {
        let mut h = AppHarness::new(
            service(fast),
            Locale::En,
            Page::Superba(SuperbaSection::Diagnostics),
        );
        let t = loaded_tool_rows(&mut h, &Tr::new(Locale::En));
        let id = t
            .with_role(Role::Button)
            .into_iter()
            .find(|id| t.name(*id).is_some_and(|n| n.starts_with("Claude Code,")))
            .unwrap();
        assert_eq!(t.node(id).is_expanded(), Some(false));
        assert!(t.node(id).supports_action(A11yAction::Expand));
        h.act(id, A11yAction::Expand);
        let t = h.settle();
        assert_eq!(
            t.nodes.get(&id).and_then(|n| n.is_expanded()),
            Some(true),
            "Claude row did not expand (fast={fast}):\n{}",
            t.dump()
        );
        h.click(Some(Role::Button), "Check Claude Code endpoint");
        let t = h.wait_for(None, "HTTP 200 · 214 ms");
        assert!(t.unnamed().is_empty(), "{:?}", t.unnamed());
    }
}

#[test]
fn backup_restore_requires_selection_and_confirmation_and_cancel_writes_nothing() {
    let s = service(true);
    let mut h = AppHarness::new(s.clone(), Locale::En, Page::Superba(SuperbaSection::Data));
    h.click(Some(Role::Button), "Choose a dsh backup to restore");
    assert!(s.harness_restores().is_empty());
    h.click(Some(Role::Button), "Restore… · dsh · backup-dsh");
    assert!(s.harness_restores().is_empty());
    let t = h.click(Some(Role::Button), "Cancel");
    assert!(s.harness_restores().is_empty());
    assert!(!t.has("Restore a dsh backup?"));
    h.click(Some(Role::Button), "Restore… · dsh · backup-dsh");
    h.click(Some(Role::Button), "Restore this file");
    h.wait_for(None, "Restored dsh's file. New sessions will use it.");
    assert_eq!(
        s.harness_restores(),
        vec![(HarnessId::Dsh, "backup-dsh".into())]
    );
}

#[test]
fn backup_store_failure_is_visible_while_other_stores_remain_usable() {
    let s = service(true);
    s.fail_harness_backups(HarnessId::Pi);
    let mut h = AppHarness::new(s, Locale::En, Page::Superba(SuperbaSection::Data));
    let t = h.until("failure", |t| t.dump().contains("backup index unreadable"));
    assert!(t.has("View pi backups"));
    h.click(Some(Role::Button), "View dsh backups");
    let t = h.wait_for(Some(Role::Button), "Restore… · dsh · backup-dsh");
    assert!(t.unnamed().is_empty(), "{:?}", t.unnamed());
}

#[test]
fn backup_deep_link_opens_the_selected_remote_home_instead_of_the_last_editor() {
    let s = service(true);
    let mut h = router_harness::Harness::new([1280.0, 800.0], Locale::En, s, true);
    let mut pages = PageStates::default();
    let mut requested = false;
    h.settle(|_ui, cx, out| {
        if !requested {
            pages.command(
                Page::Settings,
                cx,
                "files-backups:auth.json@remote-demo",
                out,
            );
            requested = true;
        }
    });
    assert_eq!(pages.settings.files.target(), TargetId::new("remote-demo"));
    assert_eq!(pages.settings.files.kind(), CodexFileKind::AuthJson);
    assert!(matches!(
        pages.settings.files.dialog,
        Some(superba_gui::pages::settings::files::Dialog::Backups)
    ));
}

#[test]
fn loaded_maintenance_pages_fit_960_and_1280_in_both_themes_and_languages() {
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        for dark in [false, true] {
            for window in [[960.0, 640.0], [1280.0, 800.0]] {
                for section in [SuperbaSection::Diagnostics, SuperbaSection::Data] {
                    let mut h = router_harness::Harness::new(window, locale, service(true), true)
                        .min_height(Page::Superba(section).min_height());
                    if dark {
                        h = h.dark();
                    }
                    let mut pages = PageStates::default();
                    let mut settings = Settings::default();
                    let page = Page::Superba(section);
                    let f = h.until(
                        |ui, cx, out| pages.show(page, ui, cx, &mut settings, out),
                        |f| {
                            if section == SuperbaSection::Diagnostics {
                                f.has(&tr.t("superba.diagnostics.codex_valid"))
                            } else {
                                f.has(&tr.t("prefs.data.move"))
                            }
                        },
                    );
                    assert!(f.fits() && !f.scrolled, "{section:?} {window:?}: {f:?}");
                    for entry in superba_gui::pages::superba::SuperbaTab::ALL {
                        assert!(f.has(&tr.t(entry.title_key())), "missing tab {entry:?}");
                    }
                    let mut frames = vec![f];
                    for _ in 0..12 {
                        let rect = frames[0].rect;
                        h.scroll_at(
                            egui::pos2(rect.right() - 40.0, rect.center().y),
                            egui::vec2(0.0, -240.0),
                        );
                        frames.push(
                            h.settle(|ui, cx, out| pages.show(page, ui, cx, &mut settings, out)),
                        );
                    }
                    assert!(
                        frames.iter().any(|f| f.has("WorkBuddy")),
                        "last tool never reachable"
                    );
                    let last = if section == SuperbaSection::Diagnostics {
                        "diag.log.title"
                    } else {
                        "settings.uninstall.title"
                    };
                    assert!(
                        frames.iter().any(|f| f.has(&tr.t(last))),
                        "last section never reachable"
                    );
                    if section == SuperbaSection::Diagnostics && window[0] == 960.0 {
                        let note = tr.t("superba.diagnostics.plain_key");
                        let visible_notes: Vec<_> = frames
                            .iter()
                            .flat_map(|f| &f.clipped_texts)
                            .filter(|(text, rect, clip)| {
                                text == &note
                                    && clip.height() >= rect.height() + 12.0
                                    && rect.center().y > clip.top()
                            })
                            .collect();
                        assert!(!visible_notes.is_empty(), "no in-row note was painted");
                        for (_, rect, clip) in visible_notes {
                            assert!(
                                rect.bottom() <= clip.bottom() + 0.5,
                                "note clipped inside tool row: {rect:?} {clip:?}"
                            );
                        }
                    }
                    for f in &frames {
                        assert!(f.fits() && !f.scrolled);
                        for (text, rect) in &f.texts {
                            assert!(
                                rect.right() <= f.rect.right() + 1.0,
                                "{section:?} {window:?}: {text} at {rect:?} > {:?}",
                                f.rect
                            );
                        }
                    }
                }
            }
        }
    }
}
