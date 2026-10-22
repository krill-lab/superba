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
                for key in [
                    "models.router.title",
                    "models.col.model",
                    "models.col.providers",
                    // Field-only value columns take the short headers.
                    "models.col.context_short",
                    "models.col.compact_short",
                    "models.col.actions",
                    "models.router.prefixes",
                ] {
                    assert!(report.whole(&tr.t(key)), "{at}: {key}");
                }
                assert!(
                    report.whole(&models_page::mode_label(&tr, mode)),
                    "{at}: mode"
                );
                // What the mode does wraps instead of eliding (UI spec §7);
                // in Codex mode, what it leaves out and the one-click fix
                // (user bug 106).
                if mode == CatalogMode::Codex {
                    let n = view.router_catalog.as_ref().unwrap().withheld.len();
                    assert!(n > 0);
                    assert!(
                        report.whole(&tr.f("models.router.withheld_title", &[("n", &n)])),
                        "{at}: withheld"
                    );
                    assert!(
                        report.whole(&tr.t("models.router.withheld_fix")),
                        "{at}: fix"
                    );
                } else {
                    assert!(
                        report.whole(&models_page::mode_help(&tr, mode)),
                        "{at}: mode help"
                    );
                }
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
    // An estimated window is marked in its field (the caption is its tooltip).
    assert!(text.contains('≈'), "{text}");
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
                // A menu option may carry its description on a second line.
                .find(|(t, _)| t == target || t.starts_with(&format!("{target}\n")))
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

/// User bug 106 (a): the default mode withholds the providers' other models;
/// the page says how many next to the mode and switches in one click, then
/// asks Codex to read the new list through the normal apply flow when the
/// provider is the applied one.
#[tokio::test]
async fn codex_mode_says_what_it_withholds_and_switches_in_one_click() {
    let fake = service();
    let data = data(&fake).await;
    let mut view = routed(&fake, CatalogMode::Codex).await;
    let applied = superba_gui::pages::shared::providers::applied_profile_id(&data).cloned();
    if let Some(id) = &applied {
        view.profile_id = id.clone();
    }
    assert!(matches!(
        models_page::guidance(&view, CatalogMode::Codex),
        Some(models_page::Guidance::Withheld(n)) if n > 0
    ));
    fake.set_models_view(view.clone());
    let tr = Tr::new(Locale::ZhCn);
    let fix = tr.t("models.router.withheld_fix");
    let service: Arc<dyn SwitchService> = fake.clone();
    let mut state = fresh(&view);
    let expect_refresh = applied.is_some();
    let actions = drive(&data, service, &mut state, &[fix.as_str()], |_, actions| {
        !fake.router_catalog_saves().is_empty()
            && (!expect_refresh || actions.iter().any(|a| matches!(a, Action::Plan(_))))
    });
    let saves = fake.router_catalog_saves();
    assert_eq!(saves.len(), 1, "{saves:?}");
    assert_eq!(saves[0].mode, CatalogMode::Extended);
    if expect_refresh {
        assert!(
            actions.iter().any(|a| matches!(
                a,
                Action::Plan(r) if r.refresh_models && r.targets.local
            )),
            "{actions:?}"
        );
    }
    // The other modes withhold nothing.
    for mode in [CatalogMode::Extended, CatalogMode::Prefixed] {
        assert_eq!(
            models_page::guidance(&routed(&fake, mode).await, mode),
            None
        );
    }
}

/// User bug 106 (d): a direct connection ignores the router model list. Once
/// the user picked a mode that adds models, the page says so and offers to
/// turn the local router on (a switch, previewed as usual).
#[tokio::test]
async fn a_direct_connection_says_the_mode_needs_the_router() {
    let fake = service();
    let mut data = data(&fake).await;
    let view = fake
        .models_view(ModelsViewRequest::default())
        .await
        .unwrap();
    assert!(view.router_catalog.is_none() && !view.read_only);
    // The default mode changes nothing on a direct connection: no notice.
    assert_eq!(models_page::guidance(&view, CatalogMode::Codex), None);
    assert_eq!(
        models_page::guidance(&view, CatalogMode::Extended),
        Some(models_page::Guidance::Direct)
    );
    if let Some(set) = data.profiles.value.as_mut() {
        set.settings.router.catalog.mode = CatalogMode::Extended;
    }
    let tr = Tr::new(Locale::ZhCn);
    let fix = tr.t("models.router.direct_fix");
    let service: Arc<dyn SwitchService> = fake.clone();
    let mut state = fresh(&view);
    let actions = drive(&data, service, &mut state, &[fix.as_str()], |_, actions| {
        actions.iter().any(|a| matches!(a, Action::Plan(_)))
    });
    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::Plan(r) if r.intent == superba_core::SwitchIntent::Router { enable: true }
        )),
        "{actions:?}"
    );
}

/// The search field clears with its × and with Esc.
#[test]
fn search_field_clears_with_its_button_and_escape() {
    use superba_gui::design::widgets::SearchField;
    let ctx = egui::Context::default();
    let mut text = String::from("deepseek");
    let run = |text: &mut String, events: Vec<egui::Event>, focus: bool| {
        let mut rect = egui::Rect::NOTHING;
        let out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 100.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                let r = SearchField::new(text, "test-search", "Search", "Clear")
                    .width(200.0)
                    .show(ui);
                if focus {
                    r.request_focus();
                }
                rect = r.rect;
            },
        );
        out.drop_without_applying_deltas();
        rect
    };
    let rect = run(&mut text, Vec::new(), false);
    // × sits inside the field's right edge.
    let at = egui::pos2(rect.right() - 5.0 - 9.0, rect.center().y);
    run(&mut text, vec![egui::Event::PointerMoved(at)], false);
    let press = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    run(&mut text, vec![press(true)], false);
    run(&mut text, vec![press(false)], false);
    assert_eq!(text, "");
    // Esc in the focused field.
    text.push_str("gpt");
    run(&mut text, Vec::new(), true);
    run(&mut text, Vec::new(), false);
    let esc = egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Default::default(),
    };
    run(&mut text, vec![esc], false);
    assert_eq!(text, "");
}

/// User requirement 115: through the router every row has its price with
/// where it comes from; at 960 px the price folds into the model cell.
#[tokio::test]
async fn routed_rows_show_their_price_and_fold_it_when_narrow() {
    let fake = service();
    let data = data(&fake).await;
    let view = routed(&fake, CatalogMode::Extended).await;
    let table = fake.price_table().await.unwrap();
    for (window, wide) in [([1440.0, 900.0], true), ([960.0, 640.0], false)] {
        for locale in [Locale::En, Locale::ZhCn] {
            let tr = Tr::new(locale);
            let mut state = fresh(&view);
            state.set_price_table(Some(table.clone()));
            // The priced fixture model on the first page.
            state.query = "astra".into();
            let report = render_fit_min_height(
                window,
                locale,
                egui::Theme::Light,
                &data,
                Page::Models.min_height(),
                |ui, cx, out| models_page::show(ui, cx, &mut state, out),
            );
            let at = format!("{window:?} {locale:?}");
            assert!(report.fits(), "{at}: {report:?}");
            assert_eq!(report.beyond_right(), Vec::<&str>::new(), "{at}");
            assert_eq!(
                report.texts.contains(&tr.t("models.col.price")),
                wide,
                "{at}: price column"
            );
            let astra = "5.00 · 0.50 · 40.00";
            assert!(
                report.texts.iter().any(|t| t.contains(astra)),
                "{at}: {:?}",
                report.texts
            );
            // The page's way to the ratios and calibration.
            assert!(report.whole(&tr.t("models.prices.open")), "{at}");
        }
    }
}

/// 「价格与校准…」 lists each key-based provider's ratio; editing and saving
/// stores it with the same settings the cost estimates read.
#[tokio::test]
async fn price_ratios_are_edited_on_the_models_page() {
    let fake = service();
    let data = data(&fake).await;
    let view = routed(&fake, CatalogMode::Extended).await;
    fake.set_models_view(view.clone());
    let tr = Tr::new(Locale::ZhCn);
    let service: Arc<dyn SwitchService> = fake.clone();
    let mut state = fresh(&view);
    let open = tr.t("models.prices.open");
    let edit = tr.t("prices.edit");
    let save = tr.t("prices.save");
    drive(
        &data,
        service,
        &mut state,
        &[open.as_str(), edit.as_str(), save.as_str()],
        |_, _| !fake.saved_stats().is_empty(),
    );
    assert_eq!(fake.saved_stats().len(), 1);
    assert!(state.price_tools.open, "the dialog stays open");
}
