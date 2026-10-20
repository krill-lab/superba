#![allow(missing_docs)]
//! Regressions from the Windows size matrix: inspect painted text against its
//! actual clip, and exercise selecting, editing and saving at the minimum size.

mod a11y_harness;
mod router_harness;
mod support;

use std::sync::Arc;

use a11y_harness::AppHarness;
use egui::{Rect, accesskit::Role};
use router_harness::{Frame, Harness};
use superba_core::{CodexFileKind, Locale, TargetId};
use superba_gui::i18n::Tr;
use superba_gui::pages::{Page, claude, providers, router, superba};
use support::fake::{FakeOptions, FakeService};

const SIZES: [[f32; 2]; 3] = [[960.0, 640.0], [1280.0, 800.0], [1440.0, 900.0]];

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

fn visible_text(frame: &Frame, text: &str) -> Rect {
    let (_, rect, clip) = frame
        .clipped_texts
        .iter()
        .find(|(t, _, _)| t == text)
        .unwrap_or_else(|| panic!("missing {text:?}: {}", frame.all_text()));
    assert!(
        clip.expand(0.5).contains_rect(*rect),
        "{text:?} at {rect:?} is clipped by {clip:?}"
    );
    *rect
}

#[test]
fn claude_setting_names_and_env_badges_have_separate_visible_space() {
    for size in SIZES {
        for locale in [Locale::En, Locale::ZhCn] {
            let mut h = Harness::new(size, locale, fake(), true);
            let mut state = claude::State::default();
            h.until(
                |ui, cx, out| claude::settings::show(ui, cx, &mut state, out),
                |f| f.has(&Tr::new(locale).t("claude.settings.common")),
            );
            state.settings.filter.query = "CLAUDE_CODE_EFFORT_LEVEL".into();
            let f = h.settle(|ui, cx, out| claude::settings::show(ui, cx, &mut state, out));
            let rows = state.settings.rows();
            let title = rows[0].title(locale == Locale::ZhCn);
            let name = visible_text(&f, title);
            if locale == Locale::En {
                assert!(name.width() >= 80.0, "unreadable key: {size:?} {name:?}");
            } else {
                assert!(
                    !f.elided.iter().any(|t| t == title),
                    "short Chinese name is elided"
                );
            }
            let badge = visible_text(&f, &Tr::new(locale).t("claude.settings.badge.env"));
            assert!(!name.intersects(badge), "name overlaps env badge: {size:?}");
        }
    }
}

#[test]
fn claude_pending_controls_do_not_overlap_the_pager() {
    for size in SIZES {
        for locale in [Locale::En, Locale::ZhCn] {
            let tr = Tr::new(locale);
            let mut h = Harness::new(size, locale, fake(), true);
            let mut state = claude::State::default();
            h.until(
                |ui, cx, out| claude::settings::show(ui, cx, &mut state, out),
                |f| f.has(&tr.t("claude.settings.common")),
            );
            state
                .settings
                .edit("model", Some(serde_json::json!("test-model")));
            let save = tr.t("claude.settings.save");
            let f = h.until_scrolled(
                |ui, cx, out| claude::settings::show(ui, cx, &mut state, out),
                |f| {
                    f.clipped_texts
                        .iter()
                        .any(|(t, r, clip)| t == &save && clip.contains_rect(*r))
                },
            );
            let labels = [
                tr.f("claude.settings.pending", &[("n", &1)]),
                tr.t("claude.settings.diff"),
                tr.t("claude.settings.discard"),
                save,
            ];
            for label in labels {
                let control = visible_text(&f, &label);
                for (other, rect, _) in &f.clipped_texts {
                    if other != &label {
                        assert!(
                            !control.intersects(*rect),
                            "{label:?} overlaps {other:?} at {size:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn narrow_claude_settings_can_select_edit_close_reopen_and_save() {
    for locale in [Locale::En, Locale::ZhCn] {
        let service = fake();
        service.set_codex_file(TargetId::local(), CodexFileKind::ClaudeSettings, Some("{}"));
        let tr = Tr::new(locale);
        let mut h = AppHarness::new(service.clone(), locale, Page::ClaudeSettings);
        h.resize(SIZES[0]);
        h.wait_for(Some(Role::ListItem), "model");
        h.click(Some(Role::ListItem), "model");
        h.wait_for(Some(Role::TextInput), "model");
        h.set_value("model", "test-model");
        h.click(Some(Role::Button), &tr.t("codex_settings.pending.close"));
        let t = h.settle();
        assert!(t.find(Some(Role::TextInput), "model").is_none());
        h.click(Some(Role::ListItem), "model");
        let t = h.wait_for(Some(Role::TextInput), "model");
        let field = t.find(Some(Role::TextInput), "model").unwrap();
        assert_eq!(t.node(field).value(), Some("test-model"));
        h.key(egui::Key::Escape);
        h.click(Some(Role::Button), &tr.t("claude.settings.next"));
        h.click(Some(Role::Button), &tr.t("claude.settings.prev"));
        h.click(Some(Role::Button), &tr.t("claude.settings.save"));
        h.until("saved model", |_| {
            service
                .codex_file_text(&TargetId::local(), CodexFileKind::ClaudeSettings)
                .is_some_and(|s| s.contains("test-model"))
        });
        let saved = service
            .codex_file_text(&TargetId::local(), CodexFileKind::ClaudeSettings)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&saved).unwrap()["model"],
            "test-model"
        );
        // Expanding the window docks the same selected, saved setting again.
        let t = h.resize(SIZES[1]);
        assert!(t.find(Some(Role::TextInput), "model").is_some());
    }
}

#[test]
fn installer_explanation_and_source_choices_do_not_overlap() {
    for size in SIZES {
        for locale in [Locale::En, Locale::ZhCn] {
            let tr = Tr::new(locale);
            let mut h = Harness::new(size, locale, fake(), true);
            let mut settings = superba_core::Settings::default();
            // The actual Windows-only row is public so other hosts can test it.
            let f = h.settle(|ui, cx, _| {
                superba::network::windows_app_source_row(ui, cx, &mut settings);
            });
            let desc = visible_text(&f, &tr.t("prefs.network.app_source_desc"));
            for key in [
                "app_source_official_first",
                "app_source_official_only",
                "app_source_store_only",
            ] {
                let choice = visible_text(&f, &tr.t(&format!("prefs.network.{key}")));
                assert!(
                    !desc.intersects(choice),
                    "installer description overlaps {key} at {size:?}"
                );
            }
        }
    }
}

#[test]
fn provider_header_and_lines_actions_do_not_cover_labels() {
    for size in SIZES {
        for locale in [Locale::En, Locale::ZhCn] {
            let tr = Tr::new(locale);
            let mut h = Harness::new(size, locale, fake(), true);
            let mut state = providers::State::default();
            let f = h.settle(|ui, cx, out| providers::show(ui, cx, &mut state, out));
            let title = visible_text(&f, &tr.t("providers.lines.title"));
            let probe = visible_text(&f, &tr.t("action.probe"));
            assert!(
                !title.intersects(probe),
                "lines title overlaps Test lines at {size:?}"
            );
            let switch = visible_text(&f, &tr.t("providers.use"));
            let applied: Vec<_> = f
                .clipped_texts
                .iter()
                .filter(|(t, _, _)| t == &tr.t("providers.applied"))
                .collect();
            assert!(applied.len() >= 2, "both list and detail show In use");
            for (_, badge, clip) in applied {
                assert!(clip.expand(0.5).contains_rect(*badge));
                assert!(
                    !badge.intersects(switch),
                    "In use overlaps Switch at {size:?}"
                );
            }
        }
    }
}

#[test]
fn router_actions_and_lower_settings_remain_reachable_when_narrow() {
    for size in SIZES {
        for locale in [Locale::En, Locale::ZhCn] {
            let tr = Tr::new(locale);
            let mut h = Harness::new(size, locale, fake(), true);
            let mut state = router::State::default();
            h = h.min_height(Page::Router.min_height());
            // No member yet: the members section offers the fix (add Krill)
            // next to the header's 开启路由….
            let add = tr.f("router.eligibility.add", &[("name", &"Krill")]);
            let f = h.until(
                |ui, cx, out| router::show(ui, cx, &mut state, out),
                |f| f.has(&add),
            );
            for text in [tr.t("router.banner.turn_on"), add] {
                visible_text(&f, &text);
            }
            assert!(
                !f.texts
                    .iter()
                    .any(|(text, _)| text == &tr.t("stats_line.title")),
                "Advanced starts closed"
            );
            let title = tr.t("router.advanced.title");
            let f = h.until_scrolled(
                |ui, cx, out| router::show(ui, cx, &mut state, out),
                |f| {
                    f.clipped_texts
                        .iter()
                        .any(|(text, rect, clip)| text == &title && clip.contains_rect(*rect))
                },
            );
            h.click_text(&f, &title);
            h.settle(|ui, cx, out| router::show(ui, cx, &mut state, out));
            for key in ["stats_line.title", "router.doctor.open"] {
                let f = h.until_scrolled(
                    |ui, cx, out| router::show(ui, cx, &mut state, out),
                    |f| {
                        f.clipped_texts
                            .iter()
                            .any(|(t, r, clip)| t == &tr.t(key) && clip.contains_rect(*r))
                    },
                );
                visible_text(&f, &tr.t(key));
                assert_eq!(f.scroll_areas, 1);
            }
        }
    }
}
