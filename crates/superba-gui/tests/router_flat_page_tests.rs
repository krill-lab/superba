#![allow(missing_docs)]
//! The flattened Router page (user bugs 116 and 117): 「高级」 and 「协议转换」
//! are plain sections (no disclosure), the service and the router process sit
//! under the status line, the recent routing events and the separate Claude
//! Code section are gone, and the failover and recovery rules are settings.
//! Checked at 1280×800 and 960×640, light and dark, Chinese and English.

mod a11y_harness;
mod router_harness;
mod support;

use std::sync::Arc;

use a11y_harness::AppHarness;
use egui::accesskit::Role;
use router_harness::Harness;
use superba_core::{Locale, ProfileId, RouterFailover};
use superba_gui::i18n::Tr;
use superba_gui::pages::Page;
use superba_gui::pages::router::{self, State};
use support::fake::{FakeOptions, FakeService};

fn members() -> Arc<FakeService> {
    let service = Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }));
    service.edit_profiles(|set| {
        let mut backup = set.profile(&ProfileId::new("krill")).unwrap().clone();
        backup.id = ProfileId::new("backup");
        backup.name = "Backup provider".into();
        set.profiles.push(backup);
        set.settings.router.members = vec![ProfileId::new("krill"), ProfileId::new("backup")];
    });
    service
}

#[test]
fn no_disclosure_for_advanced_or_conversion_and_their_controls_are_named() {
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let mut h = AppHarness::new(members(), locale, Page::Router);
        // The stats line switch under 「高级」 is in the tree without opening anything.
        let tree = h.wait_for(Some(Role::Switch), &tr.t("stats_line.title"));
        for name in [
            tr.t("router.advanced.title"),
            tr.t("router.ingress.translation"),
            tr.t("router.members.rules"),
        ] {
            assert!(
                tree.find(Some(Role::DisclosureTriangle), &name).is_none(),
                "{locale:?}: {name} is still a disclosure"
            );
        }
        // The service actions keep their names.
        assert!(
            tree.find(Some(Role::Button), &tr.t("router.doctor.open"))
                .is_some()
        );
        // The failover rules are controls with names.
        for name in [
            tr.t("router.failover.cooldown"),
            tr.t("router.failover.max_cooldown"),
            tr.t("router.failover.sticky"),
        ] {
            assert!(tree.has(&name), "{locale:?}: {name}\n{}", tree.dump());
        }
        assert!(
            tree.find(Some(Role::Switch), &tr.t("router.failover.return"))
                .is_some()
        );
        assert_eq!(
            tree.find_all(Some(Role::Button), &tr.t("router.banner.turn_on"))
                .len(),
            1
        );
        assert!(tree.unnamed().is_empty(), "{:?}", tree.unnamed());
    }
}

#[test]
fn the_service_sits_under_the_status_line_and_events_and_claude_are_gone() {
    for size in [[1280.0, 800.0], [960.0, 640.0]] {
        for locale in [Locale::En, Locale::ZhCn] {
            for dark in [false, true] {
                let tr = Tr::new(locale);
                let mut h = Harness::new(size, locale, members(), true)
                    .min_height(Page::Router.min_height());
                if dark {
                    h = h.dark();
                }
                let mut state = State::default();
                let f = h.until(
                    |ui, cx, out| router::show(ui, cx, &mut state, out),
                    |f| f.has("Backup provider") && f.has(&tr.t("router.service_label")),
                );
                let text = f.all_text();
                let status = f.find(&tr.t("router.banner.off"));
                let service = f.find(&tr.t("router.service_label")).unwrap();
                let members_title = f.find(&tr.t("router.members.title")).unwrap();
                // Under the status line, above the members, indented to its text.
                if let Some(status) = status {
                    assert!(service.min.y >= status.max.y - 1.0, "{text}");
                    assert!(service.min.y - status.max.y < 40.0, "{text}");
                }
                assert!(service.max.y < members_title.min.y, "{text}");
                // The process and the diagnostics action are on the same strip.
                let process = f.find(&tr.t("router.process")).unwrap();
                assert!(process.min.y < members_title.min.y);
                assert!(f.has(&tr.t("router.doctor.open")), "{text}");
                // No separate service section, no events, no Claude Code section.
                assert!(!f.has(&tr.t("router.events.title")), "{text}");
                // ("Claude Code" itself stays: its row under 入口.)
                assert!(!f.has(&tr.t("router.claude.help")), "{text}");
                assert!(
                    f.texts
                        .iter()
                        .filter(|(t, _)| t == &tr.t("router.service_label"))
                        .count()
                        == 1,
                    "{text}"
                );
                // 960×640: the page fits the window (one scrolling column, no
                // horizontal overflow).
                assert!(f.fits(), "{size:?} {locale:?} dark={dark}: {text}");
                assert_eq!(f.scroll_areas, 1);
                for (t, r) in &f.texts {
                    assert!(
                        r.max.x <= f.rect.max.x + 0.5,
                        "{size:?} {locale:?}: {t:?} past the right edge"
                    );
                }
            }
        }
    }
}

#[test]
fn every_section_title_is_flat_and_in_order_at_the_end_of_the_page() {
    for size in [[1280.0, 800.0], [960.0, 640.0]] {
        for locale in [Locale::En, Locale::ZhCn] {
            let tr = Tr::new(locale);
            let mut h =
                Harness::new(size, locale, members(), true).min_height(Page::Router.min_height());
            let mut state = State::default();
            // Scrolling down reaches each title in turn: they come in this
            // order, each a plain title (nothing to open on the way).
            for name in [
                tr.t("router.ingress.translation"),
                tr.t("router.failover.title"),
                tr.t("router.advanced.title"),
                tr.t("stats_line.title"),
                tr.t("recording.title"),
            ] {
                h.until_scrolled(
                    |ui, cx, out| router::show(ui, cx, &mut state, out),
                    |f| {
                        f.clipped_texts
                            .iter()
                            .any(|(t, r, clip)| t == &name && clip.contains_rect(*r))
                    },
                );
            }
        }
    }
}

#[test]
fn the_failover_rules_are_settings_that_save() {
    // Today's rules are the defaults.
    let d = RouterFailover::default();
    assert_eq!(
        (d.cooldown_secs, d.max_cooldown_secs, d.sticky_idle_minutes),
        (5, 60, 120)
    );
    assert_eq!(d.failover_statuses, [502, 503, 504]);
    assert!(!d.return_to_preferred);
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let service = members();
        let mut h = AppHarness::new(service.clone(), locale, Page::Router);
        let name = tr.t("router.failover.return");
        h.wait_for(Some(Role::Switch), &name);
        h.click(Some(Role::Switch), &name);
        h.until("return to preferred saved", |_| {
            service.router_failover().return_to_preferred
        });
        // A status: 429 now fails over too.
        let code = tr.f("router.failover.status", &[("code", &429)]);
        h.click(Some(Role::CheckBox), &code);
        h.until("429 saved", |_| {
            service.router_failover().failover_statuses == [429, 502, 503, 504]
        });
        assert!(service.router_failover().return_to_preferred);
    }
}

/// alpha.9: the cooldown fields take a value from UI Automation's
/// ValuePattern.SetValue (text) and from the keyboard, and their steppers
/// change it by one; out-of-range input is clamped.
#[test]
fn the_cooldown_fields_take_typed_and_assistive_values() {
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let service = members();
        let mut h = AppHarness::new(service.clone(), locale, Page::Router);
        let first = tr.t("router.failover.cooldown");
        h.wait_for(Some(Role::TextInput), &first);
        h.set_value(&first, "7");
        h.until("first cooldown 7 saved", |_| {
            service.router_failover().cooldown_secs == 7
        });
        h.click(Some(Role::Button), &format!("{first} +"));
        h.until("stepped to 8", |_| {
            service.router_failover().cooldown_secs == 8
        });
        let max = tr.t("router.failover.max_cooldown");
        h.set_value(&max, "999999");
        let top = *RouterFailover::MAX_COOLDOWN.end();
        h.until("max cooldown clamped", |_| {
            service.router_failover().max_cooldown_secs == top
        });
        // Typed: focus the field, type, Enter.
        let sticky = tr.t("router.failover.sticky");
        let tree = h.wait_for(Some(Role::TextInput), &sticky);
        let id = tree.find(Some(Role::TextInput), &sticky).unwrap();
        h.act(id, egui::accesskit::Action::Focus);
        h.settle();
        h.shortcut(egui::Key::A);
        h.type_text("30");
        h.key(egui::Key::Enter);
        h.until("sticky 30 saved", |_| {
            service.router_failover().sticky_idle_minutes == 30
        });
    }
}
