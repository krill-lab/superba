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
fn simple_router_navigation_and_advanced_button_open_the_corresponding_mode() {
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let service = members();
        service.edit_profiles(|set| {
            set.settings.simple_mode = true;
            set.settings.reduce_motion = true;
        });
        let mut h = AppHarness::new(service, locale, Page::Overview);
        h.click(Some(Role::Tab), &tr.t(Page::Router.title_key()));
        let tree = h.wait_for(Some(Role::Switch), &tr.t("router.mode_label"));
        assert!(tree.has(&tr.t("router.simple.description")));
        assert!(tree.has(&tr.t("router.simple.usage_off")));
        assert!(
            tree.find(Some(Role::Switch), &tr.t("router.failover.enabled"))
                .is_none()
        );
        h.click(Some(Role::Button), &tr.t("router.simple.full_mode"));
        h.wait_for(Some(Role::Switch), &tr.t("router.failover.enabled"));
    }
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

#[test]
fn disabling_failover_exposes_exit_buttons_and_keeps_the_rules() {
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let service = members();
        service.edit_profiles(|set| {
            set.profiles
                .iter_mut()
                .find(|p| p.id.as_str() == "krill")
                .unwrap()
                .id = ProfileId::new("Krill/Mixed");
            set.settings.router.members[0] = ProfileId::new("Krill/Mixed");
        });
        let mut h = AppHarness::new(service.clone(), locale, Page::Router);
        let toggle = tr.t("router.failover.enabled");
        h.wait_for(Some(Role::Switch), &toggle);
        h.click(Some(Role::Switch), &toggle);
        h.until("failover disabled", |_| !service.router_failover().enabled);
        let initial = service.router_failover().fixed_route;
        let tree = h.wait_for(Some(Role::Button), &tr.t("router.failover.select_line"));
        assert!(!tree.has(&tr.t("router.members.up")));
        assert!(!tree.has(&tr.t("router.members.down")));
        assert!(!tree.has(&tr.t("router.members.primary")));
        assert!(tree.has(&tr.t("router.members.fixed_title")));
        h.click(Some(Role::Button), &tr.t("router.failover.select_line"));
        h.until("exit saved", |_| {
            service.router_failover().fixed_route != initial
        });
        assert_eq!(
            service
                .router_failover()
                .fixed_route
                .as_ref()
                .unwrap()
                .member_id,
            "krill-mixed"
        );
        let selected = service.router_failover().fixed_route;
        h.click(Some(Role::Switch), &toggle);
        h.until("failover enabled", |_| service.router_failover().enabled);
        let rules = service.router_failover();
        assert_eq!(rules.fixed_route, selected);
        assert_eq!(rules.failover_statuses, [502, 503, 504]);
        assert_eq!(rules.cooldown_secs, 5);
        h.click(Some(Role::Switch), &toggle);
        h.until("fixed exit retained", |_| {
            !service.router_failover().enabled
        });
        assert_eq!(service.router_failover().fixed_route, selected);
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

/// The field named `name`, scrolled into view through AccessKit (what UI
/// Automation's ScrollItemPattern sends); returns its on-screen center.
fn field_center(h: &mut AppHarness, name: &str) -> egui::Pos2 {
    let tree = h.wait_for(Some(Role::TextInput), name);
    let id = tree.find(Some(Role::TextInput), name).unwrap();
    h.act(id, egui::accesskit::Action::ScrollIntoView);
    let tree = h.settle();
    let id = tree.find(Some(Role::TextInput), name).unwrap();
    let b = tree.node(id).bounds().expect("the field has bounds");
    let c = egui::pos2(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32);
    assert!(
        (0.0..1280.0).contains(&c.x) && (0.0..800.0).contains(&c.y),
        "{name} is not on screen after ScrollIntoView: {c:?}"
    );
    c
}

/// A real double-click at `pos` (press, release, press, release).
fn double_click(h: &mut AppHarness, pos: egui::Pos2) {
    let button = |pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    h.input(vec![
        vec![egui::Event::PointerMoved(pos)],
        vec![button(true)],
        vec![button(false)],
        vec![button(true)],
        vec![button(false)],
    ]);
}

/// The value the field named `name` shows.
fn field_value(h: &mut AppHarness, name: &str) -> String {
    let tree = h.wait_for(Some(Role::TextInput), name);
    let id = tree.find(Some(Role::TextInput), name).unwrap();
    tree.node(id).value().unwrap_or_default().to_owned()
}

/// alpha.10 (Windows re-test CDW2): the cooldowns by a real keyboard —
/// double-click, select all, type, Enter; and type then Tab out — and by
/// UI Automation's SetValue, which focuses the element first. Each value is
/// saved to the router configuration and is what a new window reads.
#[test]
fn the_cooldowns_save_from_real_keys_and_a_focused_set_value_and_survive_a_reload() {
    for locale in [Locale::ZhCn, Locale::En] {
        let tr = Tr::new(locale);
        let service = members();
        let first = tr.t("router.failover.cooldown");
        let max = tr.t("router.failover.max_cooldown");
        let sticky = tr.t("router.failover.sticky");
        {
            let mut h = AppHarness::new(service.clone(), locale, Page::Router);
            // Double-click, Ctrl+A, type 7, Enter.
            let at = field_center(&mut h, &first);
            double_click(&mut h, at);
            h.shortcut(egui::Key::A);
            h.type_text("7");
            h.key(egui::Key::Enter);
            h.until("first cooldown 7 saved", |_| {
                service.router_failover().cooldown_secs == 7
            });
            assert_eq!(field_value(&mut h, &first), "7");

            // Click, select all, type 90, Tab out.
            let at = field_center(&mut h, &max);
            double_click(&mut h, at);
            h.shortcut(egui::Key::A);
            h.type_text("90");
            h.key(egui::Key::Tab);
            h.until("longest cooldown 90 saved", |_| {
                service.router_failover().max_cooldown_secs == 90
            });

            // UI Automation: SetFocus, then ValuePattern.SetValue; the
            // field keeps focus and the value is saved anyway.
            let tree = h.wait_for(Some(Role::TextInput), &max);
            let id = tree.find(Some(Role::TextInput), &max).unwrap();
            h.act(id, egui::accesskit::Action::Focus);
            h.settle();
            h.set_value(&max, "900");
            h.until("longest cooldown 900 saved while focused", |_| {
                service.router_failover().max_cooldown_secs == 900
            });
            // Keys typed after it go to that field and save again.
            h.shortcut(egui::Key::A);
            h.type_text("1200");
            h.key(egui::Key::Enter);
            h.until("longest cooldown 1200 saved", |_| {
                service.router_failover().max_cooldown_secs == 1200
            });
            assert_eq!(service.router_failover().cooldown_secs, 7);

            // A numeric SetValue (RangeValuePattern) on a focused field.
            let tree = h.wait_for(Some(Role::TextInput), &sticky);
            let id = tree.find(Some(Role::TextInput), &sticky).unwrap();
            h.act(id, egui::accesskit::Action::Focus);
            h.settle();
            h.input(vec![vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::SetValue,
                    target_tree: egui::accesskit::TreeId::ROOT,
                    target_node: id,
                    data: Some(egui::accesskit::ActionData::NumericValue(45.0)),
                },
            )]]);
            h.until("sticky 45 saved", |_| {
                service.router_failover().sticky_idle_minutes == 45
            });
        }
        // A new window reads the saved rules.
        let mut h = AppHarness::new(service.clone(), locale, Page::Router);
        h.until("the saved rules are shown after a reload", |t| {
            let v = |n: &str| {
                t.find(Some(Role::TextInput), n)
                    .and_then(|id| t.node(id).value().map(str::to_owned))
            };
            v(&first).as_deref() == Some("7")
                && v(&max).as_deref() == Some("1200")
                && v(&sticky).as_deref() == Some("45")
        });
    }
}
