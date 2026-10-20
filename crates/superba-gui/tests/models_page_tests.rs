#![allow(missing_docs)]
//! Models page (UX §3.4, plan §5 B6): token parsing, validation, pending edits and
//! the edit they become, the speed control (only offered tiers, per-consumer
//! effect), save -> `save_models` + a switch, the official login read-only, and
//! the Fit layout at both window sizes and locales.

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pretty_assertions::assert_eq;
use superba_core::{
    Locale, ModelOverride, ModelPicker, ModelsView, ModelsViewRequest, PriceEntry, ProfileId,
    SpeedTier, SwitchService, Timestamp, Usd,
};
use superba_gui::design::widgets::token_field::{
    TokenError, TokenInput, parse_input, short_tokens,
};
use superba_gui::design::{Palette, fonts, style};
use superba_gui::i18n::Tr;
use superba_gui::pages::models::model::{
    self, ALL_MODELS, CompactError, DiffField, Pending, Saved, WindowError,
};
use superba_gui::pages::models::{self as models_page, State};
use superba_gui::pages::{Action, Cx, Data};
use support::fake::{FakeOptions, FakeService};
use support::fit::{SIZES, render_fit};

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

async fn krill_view(service: &FakeService) -> ModelsView {
    service
        .models_view(ModelsViewRequest::default())
        .await
        .unwrap()
}

#[tokio::test]
async fn merged_catalog_controls_fit_and_show_pending_evidence() {
    let fake = service();
    let data = data(&fake).await;
    let mut view = krill_view(&fake).await;
    view.per_model = true;
    let slug = view.list.models[0].slug.clone();
    view.router_catalog = Some(superba_core::RouterCatalogView {
        models: vec![superba_core::RouterCatalogModel {
            entry: serde_json::json!({"slug":slug}),
            readiness: superba_core::CatalogReadiness::Pending,
            detail: None,
            sources: vec![superba_core::CatalogProvenance {
                provider: "krill".into(),
                provider_name: "Krill".into(),
                provider_slug: "krill".into(),
                key_scope: "opaque-scope".into(),
                upstream_id: slug.clone(),
                endpoint: "https://example.test/v1/models".into(),
                protocol: "chat_completions".into(),
                layer: superba_core::CatalogLayer::Synthesized,
                evidence: "pending".into(),
                observed_at: 1,
            }],
        }],
        ..Default::default()
    });
    for size in SIZES {
        for locale in [Locale::En, Locale::ZhCn] {
            let mut state = State::default();
            state.set_price_table(None);
            state.set_view(view.clone());
            let report = render_fit(size, locale, egui::Theme::Dark, &data, |ui, cx, out| {
                models_page::show(ui, cx, &mut state, out)
            });
            assert!(report.fits(), "{size:?} {locale:?}: {report:?}");
            let text = report.all_text();
            assert!(
                text.contains(if locale == Locale::En {
                    "Not verified"
                } else {
                    "待验证"
                }),
                "{text}"
            );
            assert!(
                text.contains(if locale == Locale::En {
                    "Via protocol conversion"
                } else {
                    "经协议转换"
                }),
                "{text}"
            );
            assert!(
                text.contains(if locale == Locale::En {
                    "Test"
                } else {
                    "测试"
                }),
                "{text}"
            );
        }
    }
}

fn saved(view: &ModelsView) -> (BTreeMap<String, String>, BTreeMap<String, PriceEntry>) {
    let _ = view;
    (BTreeMap::new(), BTreeMap::new())
}

#[test]
fn token_input_accepts_counts_suffixes_and_percentages_where_allowed() {
    assert_eq!(parse_input("", false), Ok(TokenInput::Empty));
    assert_eq!(parse_input("  ", true), Ok(TokenInput::Empty));
    assert_eq!(
        parse_input("400000", false),
        Ok(TokenInput::Tokens(400_000))
    );
    assert_eq!(parse_input("400k", false), Ok(TokenInput::Tokens(400_000)));
    assert_eq!(parse_input("272K", false), Ok(TokenInput::Tokens(272_000)));
    assert_eq!(parse_input("1m", false), Ok(TokenInput::Tokens(1_000_000)));
    assert_eq!(
        parse_input("1.5m", false),
        Ok(TokenInput::Tokens(1_500_000))
    );
    assert_eq!(
        parse_input("117,283", false),
        Ok(TokenInput::Tokens(117_283))
    );
    assert_eq!(parse_input("80%", true), Ok(TokenInput::Percent(80)));
    assert_eq!(parse_input("80%", false), Err(TokenError::Format));
    for bad in ["0", "abc", "1.5", "-4k", "150%", "0%", "k"] {
        assert_eq!(parse_input(bad, true), Err(TokenError::Format), "{bad}");
    }
}

#[test]
fn short_tokens_use_k_and_m_only_when_exact() {
    assert_eq!(short_tokens(400_000), "400K");
    assert_eq!(short_tokens(272_000), "272K");
    assert_eq!(short_tokens(1_000_000), "1M");
    assert_eq!(short_tokens(1_500_000), "1.5M");
    assert_eq!(short_tokens(1_050_000), "1.05M");
    assert_eq!(short_tokens(117_283), "117,283");
    assert_eq!(short_tokens(999), "999");
    // What the field shows parses back to the same value.
    for n in [400_000, 1_050_000, 117_283, 16_000] {
        assert_eq!(
            parse_input(&short_tokens(n), false),
            Ok(TokenInput::Tokens(n))
        );
    }
}

#[test]
fn windows_need_the_minimum_and_stay_under_the_model_maximum() {
    assert_eq!(
        model::check_window(15_999, Some(272_000), false),
        Err(WindowError::TooSmall(16_000))
    );
    assert_eq!(
        model::check_window(400_000, Some(272_000), false),
        Err(WindowError::OverMax(272_000))
    );
    assert_eq!(
        model::check_window(272_000, Some(272_000), false),
        Ok(272_000)
    );
    // The all-models value is clamped per model by Codex: any value above 16K.
    assert_eq!(
        model::check_window(2_000_000, Some(272_000), true),
        Ok(2_000_000)
    );
    assert_eq!(
        model::check_window(1_000, None, true),
        Err(WindowError::TooSmall(16_000))
    );
}

#[test]
fn compaction_takes_tokens_or_a_share_and_reports_the_90_percent_clamp() {
    assert_eq!(model::compact_tokens(TokenInput::Empty, None), Ok(None));
    assert_eq!(
        model::compact_tokens(TokenInput::Tokens(300_000), None),
        Ok(Some(300_000))
    );
    assert_eq!(
        model::compact_tokens(TokenInput::Percent(80), Some(400_000)),
        Ok(Some(320_000))
    );
    assert_eq!(
        model::compact_tokens(TokenInput::Percent(80), None),
        Err(CompactError::NeedsWindow)
    );
    let spec = service()
        .model_fixture()
        .into_iter()
        .find(|m| m.slug == "gpt-6-astra")
        .unwrap();
    // Default: 90 % of the window.
    assert_eq!(
        model::effective_compact(&spec, &ModelOverride::default()),
        Some((244_800, false))
    );
    let o = ModelOverride {
        context_window: Some(400_000),
        auto_compact: Some(380_000),
        ..ModelOverride::default()
    };
    assert_eq!(model::effective_compact(&spec, &o), Some((360_000, true)));
}

#[tokio::test]
async fn pending_edits_count_diff_and_become_one_models_edit() {
    let service = service();
    let view = krill_view(&service).await;
    let (map, prices) = saved(&view);
    let saved = Saved {
        view: &view,
        model_map: &map,
        prices: &prices,
    };
    let mut p = Pending::default();
    assert_eq!(p.count(&saved), 0);
    let mut d = p.defaults(&saved);
    d.speed = Some(SpeedTier::Fast);
    d.model = Some("gpt-6-astra".into());
    p.set_defaults(&saved, d);
    p.set_override(
        &saved,
        "gpt-6-astra",
        ModelOverride {
            context_window: Some(400_000),
            ..ModelOverride::default()
        },
    );
    p.set_price(
        &saved,
        "gpt-5.5",
        Some(PriceEntry {
            input: Usd::from_dollars(1.25).unwrap(),
            cached_input: Some(Usd::from_dollars(0.125).unwrap()),
            cache_write: None,
            output: Usd::from_dollars(10.0).unwrap(),
            long_context: Vec::new(),
            tiers: BTreeMap::new(),
            tier_prices: BTreeMap::new(),
            source: None,
            rev: None,
        }),
    );
    // Two defaults (model, speed), one override, one price.
    assert_eq!(p.count(&saved), 4);
    assert!(p.overridden(&saved, "gpt-6-astra"));
    assert!(p.touched("gpt-6-astra"));
    let edit = p.edit(&saved);
    assert_eq!(edit.profile_id, Some(ProfileId::new("krill")));
    assert_eq!(
        edit.defaults.as_ref().and_then(|d| d.speed),
        Some(SpeedTier::Fast)
    );
    assert_eq!(
        edit.set.get("gpt-6-astra").and_then(|o| o.context_window),
        Some(400_000)
    );
    // Prices travel in the router's stats settings, not in the models edit.
    assert!(!edit.set.contains_key("gpt-5.5"));
    let diff = p.diff(&saved);
    assert!(diff.iter().any(|l| l.slug == ALL_MODELS
        && l.field == DiffField::Speed
        && l.after.as_deref() == Some("fast")));
    assert!(diff.iter().any(|l| l.slug == "gpt-6-astra"
        && l.field == DiffField::Window
        && l.after.as_deref() == Some("400K")));
    assert!(
        diff.iter()
            .any(|l| l.field == DiffField::Price
                && l.after.as_deref() == Some("1.25 · 0.125 · 10.00"))
    );
    // Editing back to the saved value drops the change.
    p.set_override(&saved, "gpt-6-astra", ModelOverride::default());
    p.set_price(&saved, "gpt-5.5", None);
    p.set_defaults(&saved, view.defaults.clone());
    assert!(p.is_empty(), "{p:?}");
}

#[test]
fn upstream_names_are_sent_only_when_changed_and_cleared_with_an_empty_name() {
    let service = service();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let view = rt.block_on(krill_view(&service));
    let map = BTreeMap::from([("gpt-5.5".to_owned(), "openai/gpt-5.5".to_owned())]);
    let prices = BTreeMap::new();
    let saved = Saved {
        view: &view,
        model_map: &map,
        prices: &prices,
    };
    let mut p = Pending::default();
    let mut o = p.override_of(&saved, "gpt-5.5");
    assert_eq!(o.upstream_name.as_deref(), Some("openai/gpt-5.5"));
    o.context_window = Some(200_000);
    p.set_override(&saved, "gpt-5.5", o);
    let edit = p.edit(&saved);
    assert_eq!(
        edit.set["gpt-5.5"].upstream_name, None,
        "unchanged: not sent"
    );
    p.reset(&saved, "gpt-5.5");
    let edit = p.edit(&saved);
    assert_eq!(edit.set["gpt-5.5"].upstream_name.as_deref(), Some(""));
    assert!(!p.overridden(&saved, "gpt-5.5"));
}

#[test]
fn hidden_models_and_the_filter_shape_the_rows() {
    let service = service();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let view = rt.block_on(krill_view(&service));
    let (map, prices) = saved(&view);
    let saved = Saved {
        view: &view,
        model_map: &map,
        prices: &prices,
    };
    let p = Pending::default();
    let shown = model::rows(&saved, &p, false, "");
    let all = model::rows(&saved, &p, true, "");
    assert_eq!(all.len(), view.list.models.len());
    assert_eq!(shown.len() + model::hidden_count(&saved, &p), all.len());
    let astra = model::rows(&saved, &p, false, "astra");
    assert_eq!(
        astra.iter().map(|m| m.slug.as_str()).collect::<Vec<_>>(),
        vec!["gpt-6-astra"]
    );
    // Display names match too.
    assert_eq!(model::rows(&saved, &p, false, "GPT-6 Sol").len(), 1);
}

/// Render the page with `service` for a few frames until `done` holds (or 3 s).
fn drive(
    data: &Data,
    service: Arc<dyn SwitchService>,
    state: &mut State,
    locale: Locale,
    mut done: impl FnMut(&State, &[Action]) -> bool,
) -> (Vec<Action>, String) {
    let ctx = egui::Context::default();
    let report = fonts::FontReport::default();
    ctx.set_fonts(fonts::definitions(&report));
    style::install(&ctx, Some(egui::Theme::Light));
    let tr = Tr::new(locale);
    let mut actions = Vec::new();
    let mut text = String::new();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        let out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1060.0, 760.0),
                )),
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
                models_page::show(ui, &cx, state, &mut actions);
            },
        );
        text = out
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some(t.galley.text().to_owned()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        out.drop_without_applying_deltas();
        if done(state, &actions) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    (actions, text)
}

fn with_cx<T>(data: &Data, service: &Arc<dyn SwitchService>, f: impl FnOnce(&Cx<'_>) -> T) -> T {
    let tr = Tr::new(Locale::En);
    let report = fonts::FontReport::default();
    let cx = Cx {
        tr: &tr,
        pal: &superba_gui::design::tokens::LIGHT,
        data,
        busy: false,
        now: Timestamp::now(),
        fonts: &report,
        service,
        log: &[],
    };
    f(&cx)
}

#[tokio::test]
async fn save_and_apply_calls_save_models_then_plans_a_switch_of_the_applied_provider() {
    let fake = service();
    let data = data(&fake).await;
    // API-key mode: per-model values reach Codex (with the fixture's personal
    // access token they would not, and nothing would restart).
    let mut view = krill_view(&fake).await;
    view.per_model = true;
    let service: Arc<dyn SwitchService> = fake.clone();
    let mut state = State::default();
    state.set_price_table(None);
    state.set_view(view.clone());
    let (map, prices) = saved(&view);
    let saved = Saved {
        view: &view,
        model_map: &map,
        prices: &prices,
    };
    state.pending.set_override(
        &saved,
        "gpt-6-astra",
        ModelOverride {
            context_window: Some(400_000),
            auto_compact: Some(300_000),
            ..ModelOverride::default()
        },
    );
    with_cx(&data, &service, |cx| {
        assert_eq!(models_page::status(&state).pending, 1);
        models_page::command(&mut state, cx, "save", &mut Vec::new());
    });
    let (actions, _) = drive(&data, service, &mut state, Locale::En, |_, a| {
        a.iter().any(|a| matches!(a, Action::Switch { .. }))
    });
    let edits = fake.models_edits();
    assert_eq!(edits.len(), 1, "{edits:?}");
    assert_eq!(
        edits[0].set["gpt-6-astra"],
        ModelOverride {
            context_window: Some(400_000),
            auto_compact: Some(300_000),
            ..ModelOverride::default()
        }
    );
    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::Switch { profile, line: None, auth: None } if profile.as_str() == "krill"
        )),
        "{actions:?}"
    );
    assert!(actions.iter().any(|a| matches!(a, Action::Refresh)));
    assert_eq!(models_page::status(&state).pending, 0);
}

#[tokio::test]
async fn speed_control_offers_only_listed_tiers_and_says_where_the_tier_applies() {
    let fake = service();
    let data = data(&fake).await;
    let mut view = krill_view(&fake).await;
    // Only Fast is offered by this list.
    for m in &mut view.list.models {
        m.service_tiers.retain(|t| t.id == "priority");
    }
    view.speed.offered = view.list.offered_tiers();
    api_key_speed(&mut view, false);
    view.defaults.speed = Some(SpeedTier::Fast);
    let service: Arc<dyn SwitchService> = fake.clone();
    let mut state = State::default();
    state.set_price_table(None);
    state.set_view(view);
    let (_, text) = drive(&data, service, &mut state, Locale::ZhCn, |_, _| true);
    assert!(text.contains("Fast"), "{text}");
    assert!(!text.contains("Ultrafast"), "{text}");
    assert!(text.contains("速度"), "{text}");
    assert!(
        text.contains("命令行：生效 · 桌面 app：API Key 模式下桌面 app 强制使用标准档"),
        "{text}"
    );
}

/// The speed effects of an API-key profile (the fixture's Krill uses a personal
/// access token), as the service computes them: per tier, with `suffix` = the
/// site maps tier model names.
fn api_key_speed(view: &mut ModelsView, suffix: bool) {
    let model = view.defaults.model.clone();
    view.speed.tiers = superba_switch::models::tier_effects(
        &view.list,
        |_| None,
        model.as_deref(),
        false,
        false,
        suffix,
    );
    let (cli, app) = view.speed.effect(view.defaults.speed.unwrap_or_default());
    (view.speed.cli, view.speed.app) = (cli.clone(), app.clone());
}

#[tokio::test]
async fn a_tier_picked_but_not_saved_shows_its_own_effect() {
    // Review ux-visual #0: an API-key profile saved at Standard. Picking Fast
    // says at once that the desktop app runs Standard, not "applies" (the
    // saved Standard's effect) until the next save.
    let fake = service();
    let data = data(&fake).await;
    let mut view = krill_view(&fake).await;
    view.defaults.speed = None;
    api_key_speed(&mut view, false);
    assert_eq!(view.speed.app, superba_core::SpeedEffect::Applies);
    // The page reloads its view: the service answers with this one.
    fake.set_models_view(view.clone());
    let service: Arc<dyn SwitchService> = fake.clone();
    let mut state = State::default();
    state.set_price_table(None);
    state.set_view(view.clone());
    let (_, text) = drive(&data, service.clone(), &mut state, Locale::ZhCn, |_, _| {
        true
    });
    assert!(text.contains("标准：Codex 不发送速度档位"), "{text}");
    let (map, prices) = saved(&view);
    let saved = Saved {
        view: &view,
        model_map: &map,
        prices: &prices,
    };
    let mut defaults = state.pending.defaults(&saved);
    defaults.speed = Some(SpeedTier::Fast);
    state.pending.set_defaults(&saved, defaults);
    assert_eq!(models_page::status(&state).pending, 1);
    let (_, text) = drive(&data, service.clone(), &mut state, Locale::ZhCn, |_, _| {
        true
    });
    assert!(
        text.contains("命令行：生效 · 桌面 app：API Key 模式下桌面 app 强制使用标准档"),
        "{text}"
    );
    assert!(!text.contains("桌面 app：生效"), "{text}");
    let (_, text) = drive(&data, service, &mut state, Locale::En, |_, _| true);
    assert!(
        text.contains(
            "Terminal Codex: Applies · desktop app: The desktop app runs Standard in API-key mode"
        ),
        "{text}"
    );
}

#[tokio::test]
async fn speed_tooltips_tell_each_tiers_effect_and_label_the_providers_words() {
    // Review ux-visual #1: the tier-model-name caveat only where the app gets
    // the tier through a model name; the provider's description is labelled.
    let fake = service();
    let mut view = krill_view(&fake).await;
    view.defaults.model = Some("gpt-6-astra".into());
    view.defaults.speed = None;
    for m in &mut view.list.models {
        for t in &mut m.service_tiers {
            t.description = "2x speed, increased usage".into();
        }
    }
    let tr = Tr::new(Locale::ZhCn);
    let caveat = tr.t("models.speed.app_model_tip");
    let defaults = view.defaults.clone();
    // Standard explains nothing.
    assert_eq!(
        models_page::speed_tooltip(&tr, &view, &defaults, SpeedTier::Standard),
        None
    );
    // API key, no tier model names: the app runs Standard.
    api_key_speed(&mut view, false);
    let tip = models_page::speed_tooltip(&tr, &view, &defaults, SpeedTier::Fast).unwrap();
    assert!(
        tip.contains("供应商说明：2x speed, increased usage"),
        "{tip}"
    );
    assert!(
        tip.contains("API Key 模式下桌面 app 强制使用标准档"),
        "{tip}"
    );
    assert!(!tip.contains(&caveat), "{tip}");
    // Tier model names: the caveat and the name, renamed with a pending model.
    api_key_speed(&mut view, true);
    let tip = models_page::speed_tooltip(&tr, &view, &defaults, SpeedTier::Ultrafast).unwrap();
    assert!(tip.contains(&caveat), "{tip}");
    assert!(tip.contains("gpt-6-astra-ultrafast"), "{tip}");
    let mut pending = defaults.clone();
    pending.model = Some("gpt-6-sol".into());
    let tip = models_page::speed_tooltip(&tr, &view, &pending, SpeedTier::Fast).unwrap();
    assert!(tip.contains("gpt-6-sol-fast"), "{tip}");
    // A personal access token: the app's own menu.
    view.speed.tiers = superba_switch::models::tier_effects(
        &view.list,
        |_| None,
        Some("gpt-6-astra"),
        false,
        true,
        false,
    );
    let tip = models_page::speed_tooltip(&tr, &view, &defaults, SpeedTier::Fast).unwrap();
    assert!(tip.contains("在 app 自己的速度菜单中设置"), "{tip}");
    assert!(!tip.contains(&caveat), "{tip}");
}

#[tokio::test]
async fn the_official_login_is_read_only_without_a_speed_control() {
    let fake = service();
    let data = data(&fake).await;
    let view = fake
        .models_view(ModelsViewRequest {
            profile_id: Some(ProfileId::new("official")),
            refresh: false,
        })
        .await
        .unwrap();
    assert!(view.read_only);
    let service: Arc<dyn SwitchService> = fake.clone();
    let mut state = State::default();
    state.set_price_table(None);
    state.set_view(view);
    let (_, text) = drive(&data, service, &mut state, Locale::ZhCn, |_, _| true);
    assert!(
        text.contains("官方登录使用 ChatGPT 自己的模型列表"),
        "{text}"
    );
    assert!(text.contains("官方登录：显示内置列表（只读）"), "{text}");
    assert!(!text.contains("命令行："), "{text}");
    // Saving does nothing on a read-only view.
    let svc: Arc<dyn SwitchService> = fake.clone();
    with_cx(&data, &svc, |cx| {
        models_page::command(&mut state, cx, "save", &mut Vec::new())
    });
    assert!(fake.models_edits().is_empty());
}

#[tokio::test]
async fn models_fit_both_sizes_and_locales_with_at_least_eleven_rows() {
    let fake = service();
    let data = data(&fake).await;
    let view = krill_view(&fake).await;
    for size in SIZES {
        for locale in [Locale::En, Locale::ZhCn] {
            let mut state = State::default();
            state.set_price_table(None);
            state.set_view(view.clone());
            let report = render_fit(size, locale, egui::Theme::Dark, &data, |ui, cx, out| {
                models_page::show(ui, cx, &mut state, out);
            });
            assert!(!report.scrolled, "{size:?} {locale:?}");
            assert_eq!(report.scroll_areas, 0, "{size:?} {locale:?}");
            assert!(report.fits(), "{size:?} {locale:?}: {report:?}");
            let min = if size[1] >= 900.0 { 13 } else { 11 };
            assert!(
                state.table.per_page >= min,
                "{size:?} {locale:?}: {} rows",
                state.table.per_page
            );
            let text = report.all_text();
            assert!(text.contains("gpt-6-astra"), "{text}");
            assert!(text.contains("872K"), "max window shown: {text}");
        }
    }
}

#[tokio::test]
async fn models_fit_the_minimum_window_without_cutting_a_column() {
    // User bug 25: at 960x640 the table's columns ran past the page edge and
    // the model column got no width. Narrow tables fold the speed column into
    // the model cell.
    let fake = service();
    let data = data(&fake).await;
    let view = krill_view(&fake).await;
    for locale in [Locale::En, Locale::ZhCn] {
        let tr = Tr::new(locale);
        let mut state = State::default();
        state.set_price_table(None);
        state.set_view(view.clone());
        let report = render_fit(
            [960.0, 640.0],
            locale,
            egui::Theme::Light,
            &data,
            |ui, cx, out| {
                models_page::show(ui, cx, &mut state, out);
            },
        );
        // 640 px is below the Fit minimum: the page scrolls vertically, but
        // nothing may run past its right edge.
        assert_eq!(report.beyond_right(), Vec::<&str>::new(), "{locale:?}");
        // The strip's labels are not pushed past the left edge either.
        let left: Vec<&str> = report
            .text_shapes
            .iter()
            .filter(|(_, r, _)| r.min.x < report.rect.min.x - 0.5)
            .map(|(t, _, _)| t.as_str())
            .collect();
        assert!(left.is_empty(), "{locale:?}: {left:?}");
        for key in ["models.default_model", "models.effort"] {
            assert!(report.whole(&tr.t(key)), "{locale:?}: {key}");
        }
        for slug in ["gpt-6-astra", "gpt-5.6-luna", "gpt-5.5-mini"] {
            assert!(report.whole(slug), "{locale:?}: {slug} elided or missing");
        }
        for key in [
            "models.col.model",
            "models.col.context",
            "models.col.compact",
            "models.col.price",
        ] {
            assert!(report.whole(&tr.t(key)), "{locale:?}: header {key}");
        }
        assert!(
            !report.texts.contains(&tr.t("models.col.speed")),
            "{locale:?}"
        );
        assert!(
            report.all_text().contains("GPT-6 Astra · Fast · Ultrafast"),
            "{locale:?}: {}",
            report.all_text()
        );
    }
    // The wide table keeps its speed column.
    let mut state = State::default();
    state.set_price_table(None);
    state.set_view(view.clone());
    let report = render_fit(
        SIZES[0],
        Locale::En,
        egui::Theme::Light,
        &data,
        |ui, cx, out| {
            models_page::show(ui, cx, &mut state, out);
        },
    );
    assert!(report.whole("Speed tiers"), "{}", report.all_text());
    assert_eq!(report.beyond_right(), Vec::<&str>::new());
}

#[tokio::test]
async fn palette_offers_context_entries_and_commands_focus_the_row() {
    let fake = service();
    let data = data(&fake).await;
    let view = krill_view(&fake).await;
    let service: Arc<dyn SwitchService> = fake.clone();
    let mut state = State::default();
    state.set_price_table(None);
    state.set_view(view.clone());
    with_cx(&data, &service, |cx| {
        let items = models_page::palette(cx, &state);
        let titles: Vec<&str> = items.iter().map(|i| i.title.as_str()).collect();
        assert!(
            titles.contains(&"Set the context window of gpt-6-astra"),
            "{titles:?}"
        );
        // A hidden model's command shows hidden models and selects its row.
        models_page::command(&mut state, cx, "context:codex-auto-review", &mut Vec::new());
    });
    assert!(state.show_hidden);
    let index = view
        .list
        .models
        .iter()
        .position(|m| m.slug == "codex-auto-review")
        .unwrap();
    assert_eq!(state.table.selected, Some(index + 1));
}

#[tokio::test]
async fn invalid_input_blocks_saving_and_names_the_limit() {
    let fake = service();
    let data = data(&fake).await;
    let view = krill_view(&fake).await;
    let service: Arc<dyn SwitchService> = fake.clone();
    let mut state = State::default();
    state.set_price_table(None);
    state.set_view(view);
    // gpt-5.5 has a 272K maximum.
    *state.buffer_mut("gpt-5.5", model::Field::Window) = "400k".into();
    with_cx(&data, &service, |cx| {
        state.commit_field(cx, "gpt-5.5", model::Field::Window)
    });
    assert_eq!(
        state.invalid().values().next().map(String::as_str),
        Some("Above this model's maximum 272K")
    );
    with_cx(&data, &service, |cx| {
        models_page::command(&mut state, cx, "save", &mut Vec::new())
    });
    assert!(fake.models_edits().is_empty());
    // A valid value clears the error and becomes a pending edit.
    *state.buffer_mut("gpt-5.5", model::Field::Window) = "200k".into();
    with_cx(&data, &service, |cx| {
        state.commit_field(cx, "gpt-5.5", model::Field::Window)
    });
    assert!(state.invalid().is_empty());
    assert_eq!(models_page::status(&state).pending, 1);
    let _ = ModelPicker::CodexDefault;
}
