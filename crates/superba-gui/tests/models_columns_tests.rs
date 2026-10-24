#![allow(missing_docs)]
//! Models table columns (user report, alpha.9): on a custom site with long
//! model ids and upstream names the upstream picker ran past its column and
//! was clipped where the actions column starts, the upstream names were
//! elided inside fixed-width fields, and the value columns spent their width
//! on 「默认」 chips. At 960x640, 1280x800 and 1440x900, in both locales:
//! the actions sit fully inside the table, an elided upstream name keeps its
//! full text for hover and screen readers, and nothing runs past the right
//! edge.

mod support;

use std::sync::Arc;

use egui::accesskit::Role;
use superba_core::{Locale, ModelsViewRequest, SiteKind, SwitchService};
use superba_gui::i18n::Tr;
use superba_gui::pages::Data;
use superba_gui::pages::Page;
use superba_gui::pages::models::{self as models_page, State};
use support::fake::{FakeOptions, FakeService};
use support::fit::{SIZES, render_fit_accesskit};

const WINDOWS: [[f32; 2]; 3] = [[960.0, 640.0], SIZES[0], SIZES[1]];
const SPARK: &str = "gpt-5.3-codex-spark";
const LONG: &str = "claude-opus-4-7-20260901-thinking-extended-context-1m";
const SPARK_UP: &str = "gpt-5.3-codex-spark-internal-preview-2026";

/// The Krill view moved onto a custom site (「内网」) whose list carries long
/// ids, two of them mapped to long upstream names.
async fn custom(standard_only: bool) -> (Data, superba_core::ModelsView) {
    let fake = Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }));
    let mut data = Data {
        sites: fake.sites().await.unwrap(),
        ..Data::default()
    };
    data.overview.finish(fake.overview().await);
    data.profiles.finish(fake.profile_set().await);
    let mut view = fake
        .models_view(ModelsViewRequest::default())
        .await
        .unwrap();
    let mut site = data
        .sites
        .iter()
        .find(|s| s.id == view.site_id)
        .unwrap()
        .clone();
    site.id = "intranet".into();
    site.kind = SiteKind::Compatible;
    site.model_map = [
        (SPARK.to_owned(), SPARK_UP.to_owned()),
        (LONG.to_owned(), LONG.to_owned() + "-upstream"),
    ]
    .into();
    data.sites.push(site);
    view.site_id = "intranet".into();
    let template = view.list.models[0].clone();
    for slug in [SPARK, LONG] {
        let mut m = template.clone();
        m.slug = slug.to_owned();
        m.display_name = None;
        view.list.models.insert(0, m);
    }
    if standard_only {
        for m in &mut view.list.models {
            m.service_tiers.clear();
        }
    }
    (data, view)
}

#[tokio::test]
async fn long_ids_keep_the_actions_inside_and_upstream_names_readable() {
    for standard_only in [false, true] {
        let (data, view) = custom(standard_only).await;
        for window in WINDOWS {
            for locale in [Locale::En, Locale::ZhCn] {
                let tr = Tr::new(locale);
                let at = format!("{window:?} {locale:?} standard_only={standard_only}");
                let mut state = State::default();
                state.set_price_table(None);
                state.set_view(view.clone());
                let report = render_fit_accesskit(
                    window,
                    locale,
                    egui::Theme::Light,
                    &data,
                    Page::Models.min_height(),
                    |ui, cx, out| models_page::show(ui, cx, &mut state, out),
                );
                let right = report.rect.max.x + 0.5;
                // Nothing runs past the right edge, and no text is cut by
                // its clip rectangle without being elided.
                assert_eq!(report.beyond_right(), Vec::<&str>::new(), "{at}");
                for ((text, r, elided), clip) in report.text_shapes.iter().zip(&report.text_clips) {
                    if r.intersects(*clip) && r.max.y > clip.min.y + 1.0 {
                        assert!(
                            *elided || r.max.x <= clip.max.x + 1.0,
                            "{at}: {text:?} clipped at {clip:?} ({r:?})"
                        );
                    }
                }
                // The actions header is whole and inside the table.
                let actions = tr.t("models.col.actions");
                let header = report
                    .text_shapes
                    .iter()
                    .find(|(t, _, e)| *t == actions && !e)
                    .unwrap_or_else(|| panic!("{at}: actions header"))
                    .1;
                assert!(header.max.x <= right, "{at}: header {header:?}");
                // Both mapped rows have a reset button: fully inside the
                // table, under the actions header, not overlapped.
                let resets: Vec<_> = report
                    .nodes
                    .iter()
                    .filter(|(role, name, _)| {
                        *role == Role::Button
                            && [SPARK, LONG]
                                .iter()
                                .any(|m| **name == tr.f("models.reset_of", &[("model", m)]))
                    })
                    .collect();
                assert_eq!(resets.len(), 2, "{at}: {:?}", report.nodes);
                for (_, name, r) in &resets {
                    assert!(r.width() >= 20.0, "{at}: {name} {r:?}");
                    assert!(r.max.x <= right - 12.0, "{at}: {name} {r:?}");
                    assert!(
                        r.min.x >= header.min.x - 16.0,
                        "{at}: {name} {r:?} {header:?}"
                    );
                }
                // The upstream pickers end before the actions column.
                let pick = tr.t("models.upstream.pick");
                let picks: Vec<_> = report
                    .nodes
                    .iter()
                    .filter(|(_, n, _)| n.contains(&pick))
                    .collect();
                assert!(!picks.is_empty(), "{at}");
                for (_, _, r) in &picks {
                    assert!(r.max.x <= header.min.x - 4.0, "{at}: picker {r:?}");
                }
                // An upstream name that is elided keeps its full text in the
                // cell's name (and tooltip); 同名 rows say so.
                let upstream = tr.t("models.col.upstream");
                for full in [SPARK_UP.to_owned(), LONG.to_owned() + "-upstream"] {
                    let cell = report
                        .nodes
                        .iter()
                        .find(|(_, n, _)| n.starts_with(&upstream) && n.ends_with(&full));
                    assert!(cell.is_some(), "{at}: no full name {full}");
                    let shown = report
                        .text_shapes
                        .iter()
                        .find(|(t, _, _)| full.starts_with(t.trim_end_matches('…')) && t.len() > 6);
                    assert!(shown.is_some(), "{at}: {full} not drawn");
                }
                assert!(report.whole(&tr.t("models.upstream.same")), "{at}");
                // The 默认 state is the fields' placeholder: no separate chip
                // next to each field.
                let default = tr.t("models.tag.default");
                let chips = report.texts.iter().filter(|t| **t == default).count();
                assert!(chips <= 1, "{at}: {chips} default chips");
                // Speed tiers collapse when no model has a faster tier.
                let speed = report.texts.contains(&tr.t("models.col.speed"));
                if standard_only {
                    assert!(!speed, "{at}: empty speed column");
                }
            }
        }
    }
}

#[test]
fn a_fixed_column_is_reserved_before_the_others_shrink() {
    use superba_gui::design::widgets::Table;
    let row = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 40.0));
    // Minimums of 600 px in a 400 px row: the fixed actions column keeps its
    // 80 px and ends at the row's padding; the rest shrink to fit.
    let t = Table::new(&[1.0, 0.0, 0.0])
        .min_widths(&[200.0, 320.0, 80.0])
        .fixed(&[2]);
    let cells = t.cells(row);
    let last = cells[2];
    assert!((last.max.x - 388.0).abs() < 0.5, "{cells:?}");
    assert!((last.width() - 80.0).abs() < 0.5, "{cells:?}");
    assert!(
        cells.windows(2).all(|w| w[0].max.x <= w[1].min.x),
        "{cells:?}"
    );
    // Without `fixed` every minimum shrinks alike, still inside the row.
    let loose = Table::new(&[1.0, 0.0, 0.0]).min_widths(&[200.0, 320.0, 80.0]);
    let cells = loose.cells(row);
    assert!(
        cells[2].max.x <= 388.5 && cells[2].width() < 79.0,
        "{cells:?}"
    );
}
