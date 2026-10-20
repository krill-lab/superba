#![allow(missing_docs)]
//! Router priority and the collapsed advanced settings at the two review sizes.

mod a11y_harness;
mod router_harness;
mod support;

use std::sync::Arc;

use a11y_harness::AppHarness;
use egui::accesskit::{Action, Role};
use router_harness::Harness;
use superba_core::{Locale, ProfileId};
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
fn both_members_and_line_priority_fit_before_advanced_at_every_review_size() {
    for size in [[1280.0, 800.0], [960.0, 640.0]] {
        for locale in [Locale::En, Locale::ZhCn] {
            let tr = Tr::new(locale);
            for dark in [false, true] {
                let mut h = Harness::new(size, locale, members(), true)
                    .min_height(Page::Router.min_height());
                if dark {
                    h = h.dark();
                }
                let mut state = State::default();
                let f = h.until(
                    |ui, cx, out| router::show(ui, cx, &mut state, out),
                    |f| f.has("Backup provider"),
                );
                assert!(f.fits() && !f.scrolled, "{size:?}: {}", f.all_text());
                assert_eq!(f.scroll_areas, 1);
                assert!(
                    !f.texts
                        .iter()
                        .any(|(text, _)| text == &tr.t("stats_line.title"))
                );
                for name in [
                    tr.t("router.members.title"),
                    "Krill".into(),
                    "Backup provider".into(),
                    tr.t("router.members.primary"),
                    tr.f("router.members.fallback", &[("n", &1)]),
                ] {
                    assert!(
                        f.clipped_texts
                            .iter()
                            .any(|(text, rect, clip)| text == &name
                                && clip.contains_rect(*rect)
                                && f.rect.contains_rect(*rect)),
                        "{size:?} {locale:?} dark={dark}: {name} clipped\n{}",
                        f.all_text()
                    );
                }
                // Both line sequences follow their member header, without entering a second column.
                let first = f.find("Krill").unwrap();
                let backup = f.find("Backup provider").unwrap();
                assert!((first.min.x - backup.min.x).abs() < 1.0);
                assert!(first.max.y < backup.min.y);
                let preferred = tr.f("router.preference.first", &[("line", &"")]);
                assert!(
                    f.texts
                        .iter()
                        .filter(|(text, _)| text.contains(&preferred))
                        .count()
                        >= 2,
                    "both members show their preferred line: {}",
                    f.all_text()
                );
            }
        }
    }
}

#[test]
fn advanced_expands_through_accesskit_and_keeps_existing_action_names() {
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let service = members();
        let mut h = AppHarness::new(service.clone(), locale, Page::Router);
        let advanced = tr.t("router.advanced.title");
        let tree = h.wait_for(Some(Role::DisclosureTriangle), &advanced);
        let id = tree
            .find(Some(Role::DisclosureTriangle), &advanced)
            .unwrap();
        assert_eq!(tree.node(id).is_expanded(), Some(false));
        assert_eq!(
            tree.find_all(Some(Role::Button), &tr.t("router.banner.turn_on"))
                .len(),
            1
        );
        assert!(
            tree.find(Some(Role::Switch), &tr.t("stats_line.title"))
                .is_none()
        );
        assert!(tree.node(id).supports_action(Action::Click));
        h.act(id, Action::Click);
        let tree = h.wait_for(Some(Role::Switch), &tr.t("stats_line.title"));
        assert!(tree.unnamed().is_empty(), "{:?}", tree.unnamed());
        h.click(Some(Role::Switch), &tr.t("stats_line.title"));
        h.until("stats line saved", |_| {
            service.saved_stats().last().is_some_and(|s| s.line.enabled)
        });
        h.click(Some(Role::DisclosureTriangle), &advanced);
        h.until("advanced closed", |tree| {
            tree.find(Some(Role::Switch), &tr.t("stats_line.title"))
                .is_none()
        });
    }
}

/// ui-spec rule 5: one primary button per view. The header's 开启路由… is the
/// primary action only once a member can carry traffic; until then the
/// members section's first "add … as a member" is, and 关闭路由… never is.
#[test]
fn one_primary_action_in_every_router_state() {
    use superba_gui::design::widgets::ButtonKind;
    use superba_gui::pages::router::eligibility::primary_fix;
    use superba_switch::router_doctor::router_candidates;

    assert_eq!(router::switch_kind(false, true), ButtonKind::Primary);
    assert_eq!(router::switch_kind(false, false), ButtonKind::Secondary);
    assert_eq!(router::switch_kind(true, true), ButtonKind::Secondary);
    assert_eq!(router::switch_kind(true, false), ButtonKind::Secondary);

    // Krill with a key (addable) after a provider without one: Krill's button
    // is the primary fix, the key fix stays secondary.
    let service = members();
    service.edit_profiles(|set| {
        set.settings.router.members.clear();
        let mut keyless = set.profile(&ProfileId::new("krill")).unwrap().clone();
        keyless.id = ProfileId::new("plain");
        keyless.name = "Plain".into();
        keyless.key_ref = None;
        set.profiles.insert(0, keyless);
    });
    let mut set = None;
    service.edit_profiles(|s| set = Some(s.clone()));
    let sites = router_harness::data_from(&service).sites;
    let all = router_candidates(&set.unwrap(), &sites);
    let fixable: Vec<_> = all
        .iter()
        .filter(|c| c.addable() || c.eligibility.needs_key())
        .collect();
    let primary = primary_fix(&fixable).expect("an addable provider");
    assert!(fixable[primary].addable());
    assert!(fixable[..primary].iter().all(|c| !c.addable()));
    assert_eq!(primary_fix(&[]), None);
}

#[test]
fn the_conversion_matrix_has_the_same_protocols_down_and_across() {
    use superba_core::{RequestProtocol as P, RouterConversion};
    let c = |from, to| RouterConversion {
        from,
        to,
        enabled: true,
    };
    // Listed in the router's order (client Messages first), shown in one order.
    let conversions = [
        c(P::Messages, P::Responses),
        c(P::Gemini, P::ChatCompletions),
        c(P::Responses, P::Messages),
    ];
    assert_eq!(
        router::ingress::matrix_axes(&conversions),
        [P::Responses, P::ChatCompletions, P::Messages, P::Gemini]
    );
    assert_eq!(
        router::ingress::matrix_axes(&[c(P::Messages, P::Responses)]),
        [P::Responses, P::Messages]
    );
}

/// The whole body scrolls under the header at 960×640: no fixed status card or
/// eligibility panel above it (they took half the window), the fix for "no
/// member" is in view, every section title is drawn once (no column header
/// repeats 入口), and the entry table's counts end under their header.
#[test]
fn the_off_page_guides_to_a_member_in_one_scrolling_column_at_both_sizes() {
    for size in [[1280.0, 800.0], [960.0, 640.0]] {
        for locale in [Locale::En, Locale::ZhCn] {
            let tr = Tr::new(locale);
            for dark in [false, true] {
                let service = Arc::new(FakeService::new(FakeOptions {
                    fast: true,
                    ..Default::default()
                }));
                let mut h =
                    Harness::new(size, locale, service, true).min_height(Page::Router.min_height());
                if dark {
                    h = h.dark();
                }
                let mut state = State::default();
                let add = tr.f("router.eligibility.add", &[("name", &"Krill")]);
                let count = tr.t("router.ingress.col.recent");
                let f = h.until(
                    |ui, cx, out| router::show(ui, cx, &mut state, out),
                    |f| f.has(&add) && f.has(&count) && f.has("0"),
                );
                let what = format!("{size:?} {locale:?} dark={dark}");
                assert_eq!(f.scroll_areas, 1, "{what}");
                assert!(f.fits() && !f.scrolled, "{what}");
                for text in [
                    tr.t("router.banner.turn_on"),
                    tr.t("router.eligibility.none"),
                    add.clone(),
                    tr.t("router.ingress.title"),
                ] {
                    assert!(
                        f.clipped_texts.iter().any(|(t, rect, clip)| t == &text
                            && clip.contains_rect(*rect)
                            && f.rect.contains_rect(*rect)),
                        "{what}: {text} not in view\n{}",
                        f.all_text()
                    );
                }
                for title in [
                    tr.t("router.members.title"),
                    tr.t("router.ingress.title"),
                    tr.t(Page::Router.title_key()),
                ] {
                    assert_eq!(
                        f.texts.iter().filter(|(t, _)| t == &title).count(),
                        1,
                        "{what}: {title} drawn more than once"
                    );
                }
                // The state is said once: "路由已关闭 —— …", not a second label.
                assert_eq!(
                    f.texts
                        .iter()
                        .filter(|(t, _)| t.contains(&tr.t("router.banner.inactive")))
                        .count(),
                    1,
                    "{what}\n{}",
                    f.all_text()
                );
                let header = f.find(&count).unwrap();
                let numbers: Vec<_> = f
                    .texts
                    .iter()
                    .filter(|(t, r)| {
                        t.chars().all(|c| c.is_ascii_digit() || c == ',')
                            && r.min.y > header.max.y
                            && r.max.x > header.min.x
                    })
                    .collect();
                assert_eq!(numbers.len(), 4, "{what}: {numbers:?}");
                for (n, r) in numbers {
                    assert!(
                        (r.max.x - header.max.x).abs() < 0.5,
                        "{what}: {n} ends at {} under a header ending at {}",
                        r.max.x,
                        header.max.x
                    );
                }
            }
        }
    }
}

/// Nothing in the page's column, Advanced open, reaches past its right edge
/// into the scroll bar's gutter (the reply preview and a section's wrapped
/// caption made the column wider, and the switches and 查看… beside them
/// went under the scroll bar at 960×640).
#[test]
fn advanced_settings_stay_inside_the_column_at_both_sizes() {
    use superba_gui::design::tokens::focus::BLEED;
    for size in [[1280.0, 800.0], [960.0, 640.0]] {
        for locale in [Locale::En, Locale::ZhCn] {
            let tr = Tr::new(locale);
            let mut h =
                Harness::new(size, locale, members(), true).min_height(Page::Router.min_height());
            let mut state = State::default();
            let title = tr.t("router.advanced.title");
            let f = h.until_scrolled(
                |ui, cx, out| router::show(ui, cx, &mut state, out),
                |f| {
                    f.clipped_texts
                        .iter()
                        .any(|(t, r, clip)| t == &title && clip.contains_rect(*r))
                },
            );
            h.click_text(&f, &title);
            let mut seen_service = false;
            for _ in 0..30 {
                let f = h.settle(|ui, cx, out| router::show(ui, cx, &mut state, out));
                // The column is clipped to its rectangle plus the focus bleed
                // (table cells to their cell); the page header's controls to
                // the page. Its right edge: the widest clip inside the page.
                let inside = |clip: &egui::Rect| clip.right() < f.rect.right() - 0.5;
                let column = f
                    .clipped_texts
                    .iter()
                    .map(|(_, _, clip)| *clip)
                    .chain(f.outlines.iter().map(|(_, clip)| *clip))
                    .filter(inside)
                    .map(|clip| clip.right())
                    .fold(f32::MIN, f32::max)
                    - BLEED;
                for (rect, clip) in &f.outlines {
                    if inside(clip) && rect.intersects(*clip) {
                        assert!(
                            rect.right() <= column + 0.5,
                            "{size:?} {locale:?}: a control at {rect:?} past the column ({column})"
                        );
                    }
                }
                for (text, rect, clip) in &f.clipped_texts {
                    if inside(clip) && rect.intersects(*clip) {
                        assert!(
                            rect.right() <= column + 0.5,
                            "{size:?} {locale:?}: {text:?} at {rect:?} past the column ({column})"
                        );
                    }
                }
                seen_service |= f.has(&tr.t("router.process"));
                if seen_service && f.has(&tr.t("router.service.state")) {
                    break;
                }
                h.scroll_at(
                    egui::pos2(f.rect.min.x + 100.0, f.rect.center().y),
                    egui::vec2(0.0, -200.0),
                );
            }
            assert!(seen_service, "{size:?} {locale:?}: scrolled to the service");
        }
    }
}
