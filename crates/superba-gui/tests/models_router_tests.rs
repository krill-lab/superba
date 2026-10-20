#![allow(missing_docs)]
//! Models page through the router (alpha.8): the router model list modes, one
//! row per model with a providers column, every row's actions whole inside
//! the row (the alpha.7 「测试」 button was cut off), the provider prefixes,
//! the per-model test and the wizard's question.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use superba_core::{CatalogMode, Locale, ModelsView, ModelsViewRequest, SwitchService, Timestamp};
use superba_gui::design::{Palette, fonts, style};
use superba_gui::i18n::Tr;
use superba_gui::pages::models::{self as models_page, State};
use superba_gui::pages::{Action, Cx, Data, Page};
use support::fake::{FakeOptions, FakeService};
use support::fit::{SIZES, render_fit_accesskit, render_fit_min_height};
use support::models_catalog;

/// Every window size the page is checked at, the minimum one first.
const WINDOWS: [[f32; 2]; 3] = [[960.0, 640.0], SIZES[0], SIZES[1]];

fn service() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

async fn data(service: &FakeService) -> Data {
    let mut data = Data {
        sites: service.sites().await.unwrap(),
        ..Data::default()
    };
    data.overview.finish(service.overview().await);
    data.profiles.finish(service.profile_set().await);
    data
}

async fn routed(service: &FakeService, mode: CatalogMode) -> ModelsView {
    let base = service
        .models_view(ModelsViewRequest::default())
        .await
        .unwrap();
    models_catalog::view(base, mode)
}

fn fresh(view: &ModelsView) -> State {
    let mut state = State::default();
    state.set_price_table(None);
    state.set_view(view.clone());
    state
}

#[tokio::test]
async fn every_mode_shows_one_row_per_model_with_its_providers() {
    let fake = service();
    let data = data(&fake).await;
    for mode in CatalogMode::ALL {
        let view = routed(&fake, mode).await;
        for window in WINDOWS {
            for locale in [Locale::En, Locale::ZhCn] {
                let tr = Tr::new(locale);
                let mut state = fresh(&view);
                let report = render_fit_min_height(
                    window,
                    locale,
                    egui::Theme::Light,
                    &data,
                    Page::Models.min_height(),
                    |ui, cx, out| models_page::show(ui, cx, &mut state, out),
                );
                let at = format!("{mode:?} {window:?} {locale:?}");
                assert!(!report.scrolled, "{at}");
                assert!(report.fits(), "{at}: {report:?}");
                assert_eq!(report.beyond_right(), Vec::<&str>::new(), "{at}");
                // No provider-prefixed rows: aliases are published, not listed.
                assert!(
                    !report
                        .texts
                        .iter()
                        .any(|t| t.starts_with("krill/") || t.starts_with("p2/")),
                    "{at}: {:?}",
                    report.texts
                );
                let shown = state.table.visible(view.list.models.len() + 1);
                for row in shown.clone().skip(usize::from(shown.start == 0)) {
                    let slug = &view.list.models[row - 1].slug;
                    let n = report.texts.iter().filter(|t| *t == slug).count();
                    assert_eq!(n, 1, "{at}: {slug} drawn {n} times");
                }
                assert!(report.whole("Krill"), "{at}");
                assert!(report.whole("内网"), "{at}");
                let narrow = window[0] < 1000.0;
                for key in [
                    "models.router.title",
                    "models.col.model",
                    "models.col.providers",
                    if narrow {
                        "models.col.context_short"
                    } else {
                        "models.col.context"
                    },
                    if narrow {
                        "models.col.compact_short"
                    } else {
                        "models.col.compact"
                    },
                    "models.col.actions",
                    "models.router.prefixes",
                ] {
                    assert!(report.whole(&tr.t(key)), "{at}: {key}");
                }
                assert!(
                    report.whole(&models_page::mode_label(&tr, mode)),
                    "{at}: mode"
                );
                // The router publishes its own list: no list source picker,
                // no site picker toggle.
                assert!(
                    !report.texts.contains(&tr.t("models.list_source.label")),
                    "{at}"
                );
                assert!(!report.texts.contains(&tr.t("models.picker_mode")), "{at}");
                assert!(
                    state.table.per_page >= 5,
                    "{at}: {} rows",
                    state.table.per_page
                );
            }
        }
    }
}

#[tokio::test]
async fn codex_mode_lists_only_codexs_own_models_and_extended_adds_the_rest() {
    let fake = service();
    let codex = routed(&fake, CatalogMode::Codex).await;
    let slugs: Vec<&str> = codex.list.models.iter().map(|m| m.slug.as_str()).collect();
    assert_eq!(
        slugs,
        [
            "gpt-5.5",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.4-nano",
            "gpt-5.5-pro"
        ]
    );
    let extended = routed(&fake, CatalogMode::Extended).await;
    assert_eq!(extended.list.models.len(), 12);
    // An estimated window does not cap the user's window, a known one does.
    let estimated = extended.list.model("gpt-5.6").unwrap();
    assert!(estimated.estimated_context_window);
    assert_eq!(estimated.max_window(), None);
    let known = extended.list.model("gpt-5.4-nano").unwrap();
    assert_eq!(known.max_window(), Some(128_000));
}

/// Every button and check box of every table row lies whole inside its row,
/// and no button label is cut by its cell (alpha.7: 「测试」 below the row).
#[tokio::test]
async fn row_actions_are_whole_inside_their_rows_at_every_size() {
    let fake = service();
    let data = data(&fake).await;
    for mode in CatalogMode::ALL {
        let view = routed(&fake, mode).await;
        for window in WINDOWS {
            for locale in [Locale::En, Locale::ZhCn] {
                let tr = Tr::new(locale);
                let mut state = fresh(&view);
                let report = render_fit_accesskit(
                    window,
                    locale,
                    egui::Theme::Dark,
                    &data,
                    Page::Models.min_height(),
                    |ui, cx, out| models_page::show(ui, cx, &mut state, out),
                );
                let at = format!("{mode:?} {window:?} {locale:?}");
                let rows = report.nodes_of(Role::Row);
                assert!(rows.len() >= 5, "{at}: {} rows", rows.len());
                let test = tr.t("models.catalog.test");
                let mut tests = 0;
                for (role, name, rect) in &report.nodes {
                    if !matches!(role, Role::Button | Role::CheckBox) {
                        continue;
                    }
                    let Some((_, row)) = rows.iter().find(|(_, r)| r.contains(rect.center()))
                    else {
                        continue;
                    };
                    assert!(
                        row.expand(0.5).contains_rect(*rect),
                        "{at}: {role:?} {name:?} {rect:?} not inside its row {row:?}"
                    );
                    // Row actions keep the small control height.
                    if *name == test || *role == Role::CheckBox {
                        assert!(
                            rect.height() >= superba_gui::design::tokens::size::CONTROL_SM - 0.5,
                            "{at}: {name:?} is {} px high",
                            rect.height()
                        );
                    }
                    if *name == test {
                        tests += 1;
                    }
                }
                // One test button per model row on the page.
                let models = state.table.visible(view.list.models.len() + 1).len() - 1;
                assert_eq!(tests, models, "{at}");
                // Their labels are painted whole inside their clip rectangles.
                for (text, rect, clip) in &report.clipped {
                    if *text == test {
                        assert!(
                            clip.expand(0.5).contains_rect(*rect),
                            "{at}: {text} cut: {rect:?} in {clip:?}"
                        );
                    }
                }
                // Visibility toggles are named check boxes.
                let toggles = report.nodes_of(Role::CheckBox);
                assert!(
                    toggles.iter().any(|(n, _)| n.contains("gpt-5.5")),
                    "{at}: {toggles:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn chips_name_each_providers_source_and_upstream_id() {
    let fake = service();
    let data = data(&fake).await;
    let view = routed(&fake, CatalogMode::Prefixed).await;
    let catalog = view.router_catalog.as_ref().unwrap();
    let tr = Tr::new(Locale::ZhCn);
    with_cx(&data, &tr, |cx| {
        let row = catalog.model("gpt-5.5").unwrap();
        let krill = superba_gui::pages::models::chip_lines(cx, &row.sources[0]).join("\n");
        assert!(krill.contains("Krill · Codex 目录"), "{krill}");
        assert!(krill.contains("上游 id：gpt-5.5"), "{krill}");
        assert!(krill.contains("固定入口：krill/gpt-5.5"), "{krill}");
        let office = superba_gui::pages::models::chip_lines(cx, &row.sources[1]).join("\n");
        assert!(office.contains("内网 · 标准列表（已知模型）"), "{office}");
        let synth = catalog.model("gpt-5.6").unwrap();
        let line = superba_gui::pages::models::chip_lines(cx, &synth.sources[0]).join("\n");
        assert!(line.contains("合成"), "{line}");
        assert!(line.contains("测试通过后"), "{line}");
    });
    // The page shows the pending model's state and its estimate.
    let mut state = fresh(&routed(&fake, CatalogMode::Extended).await);
    let report = render_fit_min_height(
        SIZES[1],
        Locale::ZhCn,
        egui::Theme::Light,
        &data,
        Page::Models.min_height(),
        |ui, cx, out| models_page::show(ui, cx, &mut state, out),
    );
    let text = report.all_text();
    assert!(text.contains("待验证"), "{text}");
    assert!(text.contains("估算值"), "{text}");
}

fn with_cx<T>(data: &Data, tr: &Tr, f: impl FnOnce(&Cx<'_>) -> T) -> T {
    let report = fonts::FontReport::default();
    let service = support::idle_service();
    let cx = Cx {
        tr,
        pal: &superba_gui::design::tokens::LIGHT,
        data,
        busy: false,
        now: Timestamp::now(),
        fonts: &report,
        service: &service,
        log: &[],
    };
    f(&cx)
}

/// Run the page against `service`: settle, then click each text in turn (its
/// last drawing: an open menu's option over the field that shows it), then
/// run until `done` (at most 3 s).
fn drive(
    data: &Data,
    service: Arc<dyn SwitchService>,
    state: &mut State,
    targets: &[&str],
    mut done: impl FnMut(&State, &[Action]) -> bool,
) -> Vec<Action> {
    let ctx = egui::Context::default();
    let report = fonts::FontReport::default();
    ctx.set_fonts(fonts::definitions(&report));
    style::install(&ctx, Some(egui::Theme::Light));
    let tr = Tr::new(Locale::ZhCn);
    let mut actions = Vec::new();
    let start = Instant::now();
    let mut frame = 0usize;
    let mut texts: Vec<(String, egui::Rect)> = Vec::new();
    let mut next = 0;
    let mut pressed: Option<egui::Pos2> = None;
    while start.elapsed() < Duration::from_secs(3) {
        let mut events = Vec::new();
        if let Some(pos) = pressed.take() {
            events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            });
        } else if frame >= 2 && frame.is_multiple_of(3) && next < targets.len() {
            let target = targets[next];
            let pos = texts
                .iter()
                .rev()
                .find(|(t, _)| t == target)
                .map(|(_, r)| r.center())
                .unwrap_or_else(|| panic!("{target} not drawn: {texts:?}"));
            events.push(egui::Event::PointerMoved(pos));
            events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            });
            pressed = Some(pos);
            next += 1;
        }
        frame += 1;
        let out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 800.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                let cx = Cx {
                    tr: &tr,
                    pal: Palette::of(ui.ctx()),
                    data,
                    busy: false,
                    now: Timestamp::now(),
                    fonts: &report,
                    service: &service,
                    log: &[],
                };
                // Where the shell puts the page (as `support::fit` does).
                let central = egui::Rect::from_min_max(
                    egui::pos2(superba_gui::design::tokens::size::SIDEBAR, 0.0),
                    egui::pos2(1280.0, 800.0),
                );
                let page = superba_gui::design::layout::page_rect(central, true);
                let mut area = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(page)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                superba_gui::design::layout::fit_area(&mut area, Page::Models.min_height(), |ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    models_page::show(ui, &cx, state, &mut actions);
                });
            },
        );
        texts = out
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((
                    t.galley.text().to_owned(),
                    t.galley.rect.translate(t.pos.to_vec2()),
                )),
                _ => None,
            })
            .collect();
        out.drop_without_applying_deltas();
        if next == targets.len() && pressed.is_none() && done(state, &actions) {
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    actions
}

#[tokio::test]
async fn picking_a_mode_saves_it_and_a_test_reports_as_a_toast() {
    let fake = service();
    let data = data(&fake).await;
    let view = routed(&fake, CatalogMode::Codex).await;
    fake.set_models_view(view.clone());
    let tr = Tr::new(Locale::ZhCn);
    let service: Arc<dyn SwitchService> = fake.clone();
    // The mode dropdown, then the extended mode in its menu.
    let mut state = fresh(&view);
    drive(
        &data,
        service.clone(),
        &mut state,
        &["仅 Codex 模型", "Codex + 供应商模型"],
        |_, _| !fake.router_catalog_saves().is_empty(),
    );
    let saves = fake.router_catalog_saves();
    assert_eq!(saves.len(), 1, "{saves:?}");
    assert_eq!(saves[0].mode, CatalogMode::Extended);
    // The other router settings are kept.
    assert_eq!(saves[0].provider_slugs["office"], "p2");
    // Test: the last row's button; the result comes back as a toast.
    let mut state = fresh(&view);
    let test = tr.t("models.catalog.test");
    let actions = drive(
        &data,
        service,
        &mut state,
        &[test.as_str()],
        |_, actions| actions.iter().any(|a| matches!(a, Action::Toast { .. })),
    );
    assert_eq!(fake.router_tests(), ["gpt-5.5-pro"]);
    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::Toast { text, .. } if text.contains("gpt-5.5-pro") && text.contains("Krill")
        )),
        "{actions:?}"
    );
}

#[test]
fn slugs_the_editor_accepts_are_the_routers() {
    for ok in ["krill", "p2", "my-relay", "a1"] {
        assert!(models_page::valid_slug(ok), "{ok}");
    }
    for bad in [
        "",
        "Krill",
        "内网",
        "-a",
        "a-",
        "a--b",
        "a/b",
        &"a".repeat(49),
    ] {
        assert!(!models_page::valid_slug(bad), "{bad}");
    }
}
