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
