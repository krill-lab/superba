//! Drawing of the Models page: header (provider, search, refresh), the source
//! strip with default model / effort / speed, the per-consumer speed effect line,
//! the paged two-line table, the bottom row (toggles, catalog note) and the
//! dialogs (diff, price override).

use std::collections::BTreeMap;

use egui::{Align, Layout, RichText, Sense, TextEdit, Ui, vec2};
use superba_core::{
    CatalogFileStatus, LimitError, LimitSource, ModelDefaults, ModelLimits, ModelListSource,
    ModelOverride, ModelPicker, ModelSpec, ModelsView, PriceEntry, SpeedEffect, SpeedTier, Usd,
};

use super::model::{
    self, ALL_MODELS, CompactError, DiffField, Field, Saved, WindowError, price_text,
};
use super::{PriceDialog, State, model_map, saved_prices};
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::icons::Icon;
use crate::design::layout::{self, Track};
use crate::design::tokens::{size, text};
use crate::design::widgets::token_field::{
    TokenError, TokenField, TokenInput, exact_tokens, parse_input, short_tokens, token_label,
};
use crate::design::widgets::{
    Button, Dropdown, IconButton, PagedTable, PendingAction, PendingBar, PendingLabels, SpeedTiers,
    Status, StatusDot, Table, Text, TierText, Toggle, dialog,
};
use crate::i18n::Tr;
use crate::pages::shared::providers::{self, Provider, ProviderKind};
use crate::pages::{Action, Cx, Page, empty_state, error_state, page_header};

/// Reasoning efforts offered when the model list does not say.
const EFFORTS: [&str; 5] = ["minimal", "low", "medium", "high", "xhigh"];

/// Width of the context window / auto-compact fields.
const FIELD_W: f32 = 76.0;
/// The same in a narrow table (room for the tag and caption next to it).
const FIELD_W_NARROW: f32 = 72.0;

/// Tables narrower than this drop the speed column (the 960 px window).
const NARROW_TABLE: f32 = 860.0;

/// Pages narrower than this show the list source on a row of its own.
const NARROW_STRIP: f32 = 860.0;

/// Display name of a provider in pickers and texts.
pub(super) fn provider_label(cx: &Cx<'_>, p: &Provider) -> String {
    match p.kind {
        ProviderKind::Official => cx.tr.t("providers.official.name"),
        _ => p
            .profile
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| cx.tr.text(&p.site.name)),
    }
}

/// The whole page.
pub(super) fn page(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    header(ui, cx, state, out);
    let Some(view) = state.view.clone() else {
        match &state.error {
            Some(e) => error_state(ui, cx, &cx.tr.t("models.load_failed"), e),
            None => empty_state(
                ui,
                cx,
                Icon::Model,
                &cx.tr.t("models.loading.title"),
                &cx.tr.t("models.loading.body"),
            ),
        }
        return;
    };
    let map = model_map(cx, &view);
    let prices = saved_prices(cx, &view.profile_id);
    let saved = Saved {
        view: &view,
        model_map: &map,
        prices: &prices,
    };
    let body = ui.available_rect_before_wrap();
    // A narrow page (the 960 px window) puts the list source on a row of its
    // own above the default model / effort / speed controls (user bug 25).
    let narrow = body.width() < NARROW_STRIP;
    let mut tracks = vec![
        Track::Fixed(size::CONTROL),
        Track::Fixed(20.0),
        Track::Fill,
        Track::Fixed(26.0),
    ];
    if narrow {
        tracks.insert(0, Track::Fixed(20.0));
    }
    let rects = layout::row_rects(body, &tracks, 6.0);
    let rows = if narrow {
        layout::fixed(ui, rects[0], |ui| {
            ui.horizontal(|ui| source_line(ui, cx, &saved));
        });
        &rects[1..]
    } else {
        &rects[..]
    };
    layout::fixed(ui, rows[0], |ui| strip(ui, cx, state, &saved, !narrow));
    layout::fixed(ui, rows[1], |ui| effect_line(ui, cx, state, &saved, out));
    layout::fixed(ui, rows[2], |ui| table(ui, cx, state, &saved));
    layout::fixed(ui, rows[3], |ui| bottom(ui, cx, state, &saved));
    if state.diff_open {
        diff_dialog(ui.ctx(), cx, state, &saved);
    }
    if state.price.is_some() {
        price_dialog(ui.ctx(), cx, state, &saved);
    }
}

/// Page header: provider picker, search and refresh on the right.
fn header(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let list = providers::with_profiles(cx.data);
    let current = state
        .view
        .as_ref()
        .map(|v| v.profile_id.clone())
        .or_else(|| state.profile.clone());
    let mut picked = None;
    let mut refresh = false;
    // Narrower pickers on a narrow page (the 960 px window).
    let narrow = ui.available_width() < NARROW_STRIP;
    page_header(ui, cx, Page::Models, |ui| {
        let loading = state.task.is_some();
        refresh = ui
            .add(
                Button::new(cx.tr.t("models.refresh"))
                    .icon(Icon::Refresh)
                    .small()
                    .loading(loading && state.refreshing)
                    .enabled(!loading && state.view.is_some()),
            )
            .clicked();
        source_picker(ui, cx, current.as_ref(), narrow, out);
        let name = list
            .iter()
            .find(|p| p.profile_id() == current.as_ref())
            .map(|p| provider_label(cx, p))
            .unwrap_or_default();
        Dropdown::new("models-provider")
            .label(cx.tr.t("models.provider"))
            .selected_text(name)
            .width(if narrow { 140.0 } else { 170.0 })
            .show_ui(ui, |ui| {
                for p in &list {
                    let Some(id) = p.profile_id() else {
                        continue;
                    };
                    let mut label = provider_label(cx, p);
                    if p.applied {
                        label = format!("{label} · {}", cx.tr.t("providers.applied"));
                    }
                    if ui
                        .selectable_label(current.as_ref() == Some(id), label)
                        .clicked()
                    {
                        picked = Some(id.clone());
                    }
                }
            });
        ui.label(Text::small(cx.tr.t("models.provider"), cx.pal));
        ui.add_space(8.0);
        let r = ui
            .add(
                TextEdit::singleline(&mut state.query)
                    .id(egui::Id::unique("models-search"))
                    .hint_text(cx.tr.t("models.search"))
                    .desired_width(if narrow { 120.0 } else { 170.0 })
                    .margin(vec2(10.0, 5.0)),
            )
            .accessible_name(cx.tr.t("models.search"));
        if state.focus_search {
            r.request_focus();
            state.focus_search = false;
        }
        if r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            state.query.clear();
        }
    });
    if refresh {
        state.task = None;
        state.load(cx, true);
    }
    if let Some(id) = picked {
        state.select_profile(cx, id);
    }
}

/// Which of the provider's lists the page reads (user bug 79, design §2a):
/// the provider's default (the list its base URL serves), both, the Codex
/// list or the standard list. Saved with the provider.
fn source_picker(
    ui: &mut Ui,
    cx: &Cx<'_>,
    profile: Option<&superba_core::ProfileId>,
    narrow: bool,
    out: &mut Vec<Action>,
) {
    use superba_core::ModelSource;
    let Some(p) = profile.and_then(|id| cx.data.profile(id)) else {
        return;
    };
    if p.builtin.is_some() || p.key_ref.is_none() {
        return;
    }
    let label = |s: Option<ModelSource>| {
        cx.tr.t(match s {
            None => "models.list_source.default",
            Some(ModelSource::Auto) => "models.list_source.auto",
            Some(ModelSource::Codex) => "models.list_source.codex",
            Some(ModelSource::Standard) => "models.list_source.standard",
        })
    };
    let mut pick = None;
    Dropdown::new("models-list-source")
        .label(cx.tr.t("models.list_source.label"))
        .selected_text(label(p.model_source))
        .width(if narrow { 110.0 } else { 140.0 })
        .show_ui(ui, |ui| {
            for s in [
                None,
                Some(ModelSource::Auto),
                Some(ModelSource::Codex),
                Some(ModelSource::Standard),
            ] {
                if ui
                    .selectable_label(p.model_source == s, label(s))
                    .on_hover_text(cx.tr.t("models.list_source.tip"))
                    .clicked()
                {
                    pick = Some(s);
                }
            }
        });
    ui.label(Text::small(cx.tr.t("models.list_source.label"), cx.pal));
    ui.add_space(8.0);
    if let Some(s) = pick
        && s != p.model_source
    {
        let mut next = p.clone();
        next.model_source = s;
        out.push(Action::SaveProfile {
            profile: Box::new(next),
            key: None,
        });
    }
}

/// The list source: status dot and text (hover: why a fallback list is used).
fn source_line(ui: &mut Ui, cx: &Cx<'_>, saved: &Saved<'_>) {
    let (status, text, tip) = source_text(cx, saved.view);
    ui.add(StatusDot::new(status));
    let r = ui.add(egui::Label::new(Text::small(text, cx.pal).color(cx.pal.text)).truncate());
    if let Some(tip) = tip {
        r.on_hover_text(tip);
    }
}

/// Source strip: where the list comes from (unless `with_source` is false: a
/// narrow page shows it on a row of its own), default model, effort and speed.
/// The controls are drawn right-aligned first; the source text gets the rest.
fn strip(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, saved: &Saved<'_>, with_source: bool) {
    let view = saved.view;
    let read_only = view.read_only || cx.busy || state.saving.is_some();
    let rect = ui.max_rect();
    // Narrower pickers on a narrow page (the 960 px window).
    let narrow = rect.width() < NARROW_STRIP;
    let mut defaults = state.pending.defaults(saved);
    let before = defaults.clone();
    // Label + control pairs, right-aligned on the strip's center line.
    let controls = crate::design::widgets::FormRow::unlabeled()
        .right()
        .height(rect.height())
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if !view.read_only {
                speed_control(ui, cx, view, &mut defaults, read_only);
                // The tiers name themselves; a narrow strip has no room for it.
                if !narrow {
                    ui.label(Text::small(cx.tr.t("models.speed.label"), cx.pal));
                }
                ui.add_space(6.0);
            }
            effort_picker(ui, cx, view, &mut defaults, read_only, narrow);
            ui.label(Text::small(cx.tr.t("models.effort"), cx.pal));
            ui.add_space(6.0);
            model_picker(ui, cx, view, &mut defaults, read_only, narrow);
            ui.label(Text::small(cx.tr.t("models.default_model"), cx.pal));
        })
        .controls_rect;
    if defaults != before {
        state.pending.set_defaults(saved, defaults);
    }
    if !with_source {
        return;
    }
    let left = egui::Rect::from_min_max(
        rect.min,
        egui::pos2((controls.min.x - 16.0).max(rect.min.x), rect.max.y),
    );
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(left)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.set_clip_rect(left.intersect(ui.clip_rect()));
            source_line(ui, cx, saved);
        },
    );
}

/// Status, text and tooltip of the list source.
fn source_text(cx: &Cx<'_>, view: &ModelsView) -> (Status, String, Option<String>) {
    let n = view.list.models.len();
    let tip = view
        .list
        .fallback_reason
        .as_ref()
        .map(|e| crate::pages::error_text(cx.tr, e));
    let (status, text) = match &view.list.source {
        ModelListSource::Upstream {
            path, fetched_at, ..
        } => (
            Status::Ok,
            cx.tr.f(
                "models.source.upstream",
                &[("path", path), ("n", &n), ("age", &cx.ago(*fetched_at))],
            ),
        ),
        ModelListSource::Feed {
            codex_version,
            mirror,
            fetched_at,
            ..
        } => (
            Status::Warn,
            cx.tr.f(
                "models.source.builtin",
                &[
                    ("version", codex_version),
                    ("mirror", mirror),
                    (
                        "date",
                        &fetched_at
                            .map(|t| t.utc_date_string())
                            .unwrap_or_else(|| "—".into()),
                    ),
                ],
            ),
        ),
        ModelListSource::Snapshot { codex_version } => (
            Status::Warn,
            cx.tr
                .f("models.source.snapshot", &[("version", codex_version)]),
        ),
        ModelListSource::Bundled { codex_version } => (
            Status::Warn,
            cx.tr
                .f("models.source.bundled", &[("version", codex_version)]),
        ),
        ModelListSource::OfficialReadOnly => (Status::Idle, cx.tr.t("models.source.official")),
        ModelListSource::Union { members } => (
            Status::Ok,
            cx.tr
                .f("models.source.union", &[("members", &members.join(", "))]),
        ),
        _ => (Status::Idle, cx.tr.f("models.source.other", &[("n", &n)])),
    };
    (status, text, tip)
}

fn model_picker(
    ui: &mut Ui,
    cx: &Cx<'_>,
    view: &ModelsView,
    defaults: &mut ModelDefaults,
    read_only: bool,
    narrow: bool,
) {
    let codex_default = cx.tr.t("models.codex_default");
    let shown = match &defaults.model {
        Some(m) => RichText::new(m).font(mono_font(text::SMALL, Weight::Regular)),
        None => RichText::new(&codex_default),
    };
    ui.add_enabled_ui(!read_only, |ui| {
        Dropdown::new(("models-default", &view.profile_id))
            .label(cx.tr.t("models.default_model"))
            .selected_text(shown)
            .width(if narrow { 112.0 } else { 150.0 })
            .menu_height(360.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut defaults.model, None, &codex_default);
                let mut seen = false;
                for m in view
                    .list
                    .models
                    .iter()
                    .filter(|m| !model::hidden(m, &ModelOverride::default()))
                {
                    seen |= defaults.model.as_deref() == Some(m.slug.as_str());
                    ui.selectable_value(&mut defaults.model, Some(m.slug.clone()), &m.slug)
                        .on_hover_text(m.label());
                }
                if let Some(other) = defaults.model.clone().filter(|_| !seen) {
                    ui.selectable_value(&mut defaults.model, Some(other.clone()), other);
                }
            });
    });
}

fn effort_picker(
    ui: &mut Ui,
    cx: &Cx<'_>,
    view: &ModelsView,
    defaults: &mut ModelDefaults,
    read_only: bool,
    narrow: bool,
) {
    let codex_default = cx.tr.t("models.codex_default");
    let levels: Vec<String> = defaults
        .model
        .as_deref()
        .and_then(|m| view.list.model(m))
        .map(|m| {
            m.reasoning_levels
                .iter()
                .map(|l| l.effort.clone())
                .collect()
        })
        .filter(|l: &Vec<String>| !l.is_empty())
        .unwrap_or_else(|| EFFORTS.iter().map(|e| (*e).to_owned()).collect());
    let shown = defaults
        .reasoning_effort
        .clone()
        .unwrap_or_else(|| codex_default.clone());
    ui.add_enabled_ui(!read_only, |ui| {
        Dropdown::new(("models-effort", &view.profile_id))
            .label(cx.tr.t("models.effort"))
            .selected_text(shown)
            .width(if narrow { 84.0 } else { 96.0 })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut defaults.reasoning_effort, None, &codex_default);
                for l in &levels {
                    ui.selectable_value(&mut defaults.reasoning_effort, Some(l.clone()), l);
                }
                if let Some(other) = defaults
                    .reasoning_effort
                    .clone()
                    .filter(|e| !levels.contains(e))
                {
                    ui.selectable_value(&mut defaults.reasoning_effort, Some(other.clone()), other);
                }
            });
    });
}

/// The profile speed (tiers some listed model offers; Standard always).
fn speed_control(
    ui: &mut Ui,
    cx: &Cx<'_>,
    view: &ModelsView,
    defaults: &mut ModelDefaults,
    read_only: bool,
) {
    let mut tier = defaults.speed.unwrap_or_default();
    let before = tier;
    let mut control = SpeedTiers::new(
        ("models-speed", &view.profile_id),
        &mut tier,
        &view.speed.offered,
    )
    .small()
    .label(cx.tr.t("models.speed.label"))
    .enabled(!read_only)
    .not_offered(cx.tr.t("models.speed.none"));
    for t in SpeedTier::ALL {
        let tooltip = speed_tooltip(cx.tr, view, defaults, t);
        control = control.text(
            t,
            TierText {
                label: cx.tr.text(&t.text()),
                tooltip,
            },
        );
    }
    control.show(ui);
    if tier != before {
        defaults.speed = Some(tier);
    }
}

/// Left: the first validation error, else what saving does (restart or not and
/// why), else saved values Codex does not use yet (with "apply now"), else the
/// PAT scope; right: where the speed applies.
fn effect_line(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    saved: &Saved<'_>,
    out: &mut Vec<Action>,
) {
    let view = saved.view;
    let errors = all_errors(cx, state, saved);
    let active = view_applied(cx, view);
    let provider = super::provider_name(cx, &view.profile_id);
    let rect = ui.max_rect();
    // Right: where the tier picked applies, saved or not (ux-visual #0). It
    // gets its width first (at most 60 % of the row), so a long note on the
    // left truncates instead of pushing it past the page edge (960 px).
    let mut left = rect;
    if !view.read_only {
        let defaults = state.pending.defaults(saved);
        let tier = defaults.speed.unwrap_or_default();
        let (cli, app) = model::speed_effect(view, &defaults, tier);
        let text = if tier == SpeedTier::Standard {
            cx.tr.t("models.speed.standard_effect")
        } else {
            effect_text(cx, &cli, &app)
        };
        let galley = egui::WidgetText::from(Text::caption(text.clone(), cx.pal)).into_galley(
            ui,
            Some(egui::TextWrapMode::Extend),
            f32::INFINITY,
            egui::FontSelection::Default,
        );
        let width = (galley.size().x + 2.0).min(rect.width() * 0.6);
        let right = egui::Rect::from_min_max(egui::pos2(rect.max.x - width, rect.min.y), rect.max);
        left.max.x = (right.min.x - 16.0).max(rect.min.x);
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(right)
                .layout(Layout::right_to_left(Align::Center)),
            |ui| {
                let r = ui.add(egui::Label::new(Text::caption(text.clone(), cx.pal)).truncate());
                if matches!(app, SpeedEffect::ViaModelName { .. }) {
                    r.on_hover_text(format!("{text}\n{}", cx.tr.t("models.speed.app_model_tip")));
                }
            },
        );
    }
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(left)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.set_clip_rect(left.intersect(ui.clip_rect()));
            if let Some(((slug, _), message)) = errors.iter().next() {
                let who = if slug.is_empty() {
                    cx.tr.t("models.all")
                } else {
                    slug.clone()
                };
                ui.add(StatusDot::new(Status::Error));
                ui.add(
                    egui::Label::new(
                        Text::small(format!("{who}: {message}"), cx.pal).color(cx.pal.red),
                    )
                    .truncate(),
                );
            } else if !view.read_only && !state.pending.is_empty() {
                let edit = state.pending.edit(saved);
                let key = match (active, model::affects_codex(&edit, view.per_model)) {
                    (true, true) => "models.after_save.restart",
                    (true, false) => "models.after_save.no_restart",
                    (false, _) => "models.after_save.next_switch",
                };
                ui.add(StatusDot::new(Status::Warn));
                ui.add(
                    egui::Label::new(Text::small(
                        cx.tr.f(key, &[("provider", &provider)]),
                        cx.pal,
                    ))
                    .truncate(),
                );
            } else if active
                && !view.read_only
                && !cx.busy
                && let n @ 1.. = model::not_applied_count(saved, &state.pending)
            {
                ui.add(StatusDot::new(Status::Warn));
                ui.label(Text::small(
                    cx.tr.f("models.not_applied", &[("n", &n)]),
                    cx.pal,
                ));
                let r = ui
                    .add_enabled(
                        !cx.busy && state.saving.is_none(),
                        egui::Label::new(
                            Text::small(cx.tr.t("models.apply_now"), cx.pal).color(cx.pal.link),
                        )
                        .sense(Sense::click()),
                    )
                    .on_hover_text(cx.tr.f("models.apply_now_tip", &[("provider", &provider)]));
                if r.clicked() {
                    out.push(Action::Switch {
                        profile: view.profile_id.clone(),
                        line: None,
                        auth: None,
                    });
                }
            } else if !view.read_only && !view.per_model {
                ui.add(StatusDot::new(Status::Idle));
                ui.add(
                    egui::Label::new(Text::small(
                        cx.tr.f("models.pat_scope", &[("provider", &provider)]),
                        cx.pal,
                    ))
                    .truncate(),
                );
            }
        },
    );
}

/// "命令行：生效 · 桌面 app：…" for a tier's two effects.
fn effect_text(cx: &Cx<'_>, cli: &SpeedEffect, app: &SpeedEffect) -> String {
    effect_line_text(cx.tr, cli, app)
}

fn effect_line_text(tr: &Tr, cli: &SpeedEffect, app: &SpeedEffect) -> String {
    tr.f(
        "models.speed.effect",
        &[
            ("cli", &tr.text(&cli.text())),
            ("app", &tr.text(&app.text())),
        ],
    )
}

/// Tooltip of a tier in the speed control (`None` for Standard): the
/// provider's description of the tier (labelled: it is the provider's text, in
/// its language), the billing note and what that tier does in terminal Codex
/// and the desktop app; the tier-model-name caveat only where the app gets the
/// tier through a model name (review ux-visual #1).
pub fn speed_tooltip(
    tr: &Tr,
    view: &ModelsView,
    defaults: &ModelDefaults,
    tier: SpeedTier,
) -> Option<String> {
    if tier == SpeedTier::Standard {
        return None;
    }
    let mut tip = Vec::new();
    let description = tier.request_value().and_then(|id| {
        view.list
            .models
            .iter()
            .flat_map(|m| &m.service_tiers)
            .find(|t| SpeedTier::from_catalog_id(&t.id) == SpeedTier::from_catalog_id(id))
            .map(|t| t.description.trim().to_owned())
            .filter(|d| !d.is_empty())
    });
    if let Some(d) = description {
        tip.push(tr.f("models.speed.provider_note", &[("text", &d)]));
    }
    tip.push(tr.t("models.speed.billing_note"));
    let (cli, app) = model::speed_effect(view, defaults, tier);
    tip.push(effect_line_text(tr, &cli, &app));
    if matches!(app, SpeedEffect::ViaModelName { .. }) {
        tip.push(tr.t("models.speed.app_model_tip"));
    }
    Some(tip.join("\n"))
}

/// Every field that cannot be saved, translated: text that is not a value
/// (kept in the field) and values that do not fit together
/// ([`model::limit_errors`]).
fn all_errors(cx: &Cx<'_>, state: &State, saved: &Saved<'_>) -> BTreeMap<(String, Field), String> {
    let mut out = state.invalid.clone();
    for (slug, field, e) in model::limit_errors(saved, &state.pending) {
        out.entry((slug, field))
            .or_insert_with(|| limit_text(cx, &e));
    }
    out
}

/// The message of a window / compaction error.
fn limit_text(cx: &Cx<'_>, e: &LimitError) -> String {
    match *e {
        LimitError::WindowTooSmall { min } => cx
            .tr
            .f("models.invalid.too_small", &[("min", &short_tokens(min))]),
        LimitError::WindowOverMax { max } if max >= superba_core::MAX_CONTEXT_WINDOW => cx
            .tr
            .f("models.invalid.over_limit", &[("max", &short_tokens(max))]),
        LimitError::WindowOverMax { max } => cx
            .tr
            .f("models.invalid.over_max", &[("max", &short_tokens(max))]),
        LimitError::CompactTooSmall { min } => cx.tr.f(
            "models.invalid.compact_too_small",
            &[("min", &short_tokens(min))],
        ),
        LimitError::CompactNotBelowWindow { window } => cx.tr.f(
            "models.invalid.compact_not_below",
            &[("window", &short_tokens(window))],
        ),
    }
}

/// One table row: the all-models row (index 0) or a model.
enum RowRef<'a> {
    All,
    Model(&'a ModelSpec),
}

/// The paged model table with the pager and the pending bar in its footer.
fn table(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, saved: &Saved<'_>) {
    let view = saved.view;
    let models = model::rows(saved, &state.pending, state.show_hidden, &state.query);
    let total = models.len() + 1;
    let custom = !saved.model_map.is_empty()
        || providers::site(cx.data, &view.site_id)
            .is_some_and(|s| s.kind == superba_core::SiteKind::Compatible);
    let last_col = if custom {
        cx.tr.t("models.col.upstream")
    } else {
        cx.tr.t("models.col.price")
    };
    // Below NARROW_TABLE (the 960 px minimum window) the speed column folds
    // into the model cell's second line and the value columns narrow, so the
    // model column keeps room for its slug (user bug 25).
    let narrow = ui.available_width() < NARROW_TABLE;
    let mut headers = vec![
        cx.tr.t("models.col.model"),
        cx.tr.t("models.col.context"),
        cx.tr.t("models.col.compact"),
        cx.tr.t("models.col.speed"),
        last_col,
        String::new(),
    ];
    let columns = if narrow {
        headers.remove(3);
        Table::new(&[1.0, 0.0, 0.0, 0.0, 0.0]).min_widths(&[0.0, 150.0, 164.0, 158.0, 28.0])
    } else {
        Table::new(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0])
            .min_widths(&[0.0, 176.0, 176.0, 132.0, 176.0, 28.0])
    };
    let read_only = view.read_only || cx.busy || state.saving.is_some();
    let count = state.pending.count(saved);
    let active = view_applied(cx, view);
    let labels = PendingLabels {
        count: cx.tr.f("models.pending.count", &[("n", &count)]),
        diff: cx.tr.t("models.pending.diff"),
        discard: cx.tr.t("models.pending.discard"),
        save: if active && model::affects_codex(&state.pending.edit(saved), view.per_model) {
            cx.tr.t("models.pending.save_apply")
        } else {
            cx.tr.t("models.pending.save")
        },
    };
    let errors = all_errors(cx, state, saved);
    let blocked = !errors.is_empty() || cx.busy || view.read_only;
    let saving = state.saving.is_some();
    let save_error = state.save_error.clone();
    let mut action = PendingAction::None;
    let mut edits: Vec<Edit> = Vec::new();
    // A focus request (palette) applies when its row is drawn this frame.
    let focus = state.focus.take();
    let State {
        table,
        buffers,
        rejected,
        pending,
        prices,
        ..
    } = state;
    PagedTable::new(("models", &view.profile_id), columns, total, table)
        .label(cx.tr.t(Page::Models.title_key()))
        .header(headers)
        .row_height(size::ROW_TABLE2)
        .keys_when_idle(true)
        .footer(|ui| {
            action = PendingBar::new(count, labels)
                .saving(saving)
                .enabled(!blocked)
                .error(save_error)
                .show(ui);
        })
        .show(ui, |ui, row, col| {
            let r = if row == 0 {
                RowRef::All
            } else {
                RowRef::Model(models[row - 1])
            };
            let mut rc = RowCx {
                cx,
                saved,
                pending,
                buffers,
                rejected,
                errors: &errors,
                read_only,
                custom,
                active,
                narrow,
                edits: &mut edits,
                focus: focus.as_ref(),
                table: prices.as_ref(),
            };
            // The narrow table has no speed column (index 3).
            let col = if narrow && col >= 3 { col + 1 } else { col };
            cell(ui, &mut rc, &r, col);
        });
    for e in edits {
        apply_edit(cx, state, saved, e);
    }
    match action {
        PendingAction::Save => state.save(cx),
        PendingAction::Discard => {
            state.pending = model::Pending::default();
            state.buffers.clear();
            state.invalid.clear();
            state.rejected.clear();
            state.save_error = None;
        }
        PendingAction::Diff => state.diff_open = true,
        PendingAction::None => {}
    }
}

fn view_applied(cx: &Cx<'_>, view: &ModelsView) -> bool {
    providers::applied_profile_id(cx.data) == Some(&view.profile_id)
}

/// An edit made in a cell this frame.
enum Edit {
    /// Committed text of a field.
    Text {
        slug: String,
        field: Field,
        text: String,
    },
    /// Reset a row.
    Reset(String),
    /// Open the price editor.
    Price(String),
}

struct RowCx<'a, 'b> {
    cx: &'a Cx<'b>,
    saved: &'a Saved<'a>,
    pending: &'a model::Pending,
    buffers: &'a mut std::collections::HashMap<(String, Field), String>,
    rejected: &'a std::collections::HashMap<(String, Field), String>,
    errors: &'a BTreeMap<(String, Field), String>,
    read_only: bool,
    custom: bool,
    /// The provider is the applied one.
    active: bool,
    /// No speed column: the model cell names the faster tiers.
    narrow: bool,
    edits: &'a mut Vec<Edit>,
    focus: Option<&'a (String, Field)>,
    table: Option<&'a superba_core::PriceTable>,
}

fn cell(ui: &mut Ui, rc: &mut RowCx<'_, '_>, row: &RowRef<'_>, col: usize) {
    let cx = rc.cx;
    match (row, col) {
        (RowRef::All, 0) => name_cell(ui, rc, ALL_MODELS, |ui| {
            ui.label(Text::strong(cx.tr.t("models.all"), cx.pal));
            ui.label(Text::caption(cx.tr.t("models.all_hint"), cx.pal));
        }),
        (RowRef::All, 1) => {
            let d = rc.pending.defaults(rc.saved);
            let shown = Shown {
                value: d.context_window,
                initial: d.context_window.map(short_tokens).unwrap_or_default(),
                placeholder: "—".to_owned(),
                source: source_of(d.context_window.is_some()),
                caption: d
                    .context_window
                    .map(|_| (cx.tr.t("models.all_capped"), false)),
                hover: all_hover(cx, d.context_window, Field::Window),
                enabled: true,
            };
            value_cell(ui, rc, ALL_MODELS, Field::Window, shown);
        }
        (RowRef::All, 2) => {
            let d = rc.pending.defaults(rc.saved);
            let shown = Shown {
                value: d.auto_compact,
                initial: d.auto_compact.map(short_tokens).unwrap_or_default(),
                placeholder: cx.tr.t("models.compact_default_short"),
                source: source_of(d.auto_compact.is_some()),
                caption: d
                    .auto_compact
                    .map(|_| (cx.tr.t("models.all_compact_capped"), false)),
                hover: all_hover(cx, d.auto_compact, Field::Compact),
                enabled: true,
            };
            value_cell(ui, rc, ALL_MODELS, Field::Compact, shown);
        }
        (RowRef::All, 3) => {
            let tier = rc.pending.defaults(rc.saved).speed.unwrap_or_default();
            ui.label(Text::small(cx.tr.text(&tier.text()), cx.pal));
        }
        (RowRef::All, 5) => reset_button(ui, rc, ALL_MODELS),
        (RowRef::All, _) => {}
        (RowRef::Model(m), 0) => {
            let o = rc.pending.override_of(rc.saved, &m.slug);
            let mut name = o
                .display_name
                .clone()
                .unwrap_or_else(|| m.label().to_owned());
            if model::hidden(m, &o) {
                name = format!("{name} · {}", cx.tr.t("models.hidden"));
            }
            if rc.narrow {
                for t in model::tiers(m)
                    .into_iter()
                    .filter(|t| *t != SpeedTier::Standard)
                {
                    name = format!("{name} · {}", cx.tr.text(&t.text()));
                }
            }
            name_cell(ui, rc, &m.slug, |ui| {
                let r = ui.add(
                    egui::Label::new(
                        RichText::new(&m.slug)
                            .font(mono_font(text::SMALL, Weight::Medium))
                            .color(cx.pal.text),
                    )
                    .truncate(),
                );
                if let Some(d) = &m.description {
                    r.on_hover_text(d);
                }
                // Which lists named the model (explicit list sources).
                let mut name = name;
                if !m.lists.is_empty() {
                    let lists: Vec<String> = m
                        .lists
                        .iter()
                        .map(|k| {
                            cx.tr.t(match k {
                                superba_core::ModelListKind::Codex => {
                                    "models.list_source.codex_short"
                                }
                                superba_core::ModelListKind::Standard => {
                                    "models.list_source.standard_short"
                                }
                            })
                        })
                        .collect();
                    name = format!("{name} · {}", lists.join(" + "));
                }
                ui.add(egui::Label::new(Text::caption(name, cx.pal)).truncate());
            });
        }
        (RowRef::Model(m), 1) => {
            let l = model::limits(rc.saved, rc.pending, m);
            let o = rc.pending.override_of(rc.saved, &m.slug);
            let shown = Shown {
                value: l.window,
                initial: l.window.map(short_tokens).unwrap_or_default(),
                placeholder: "—".to_owned(),
                source: l.window_source,
                caption: l
                    .max
                    .map(|max| (cx.tr.f("models.max", &[("max", &token_label(max))]), false)),
                hover: window_hover(rc, m, &l, &o),
                enabled: rc.saved.view.per_model,
            };
            value_cell(ui, rc, &m.slug, Field::Window, shown);
        }
        (RowRef::Model(m), 2) => {
            let l = model::limits(rc.saved, rc.pending, m);
            let o = rc.pending.override_of(rc.saved, &m.slug);
            let initial = match l.compact_source {
                LimitSource::Custom => l.compact_limit,
                _ => l.compact,
            };
            let caption = if l.clamped {
                Some((cx.tr.t("models.compact_capped"), true))
            } else {
                l.compact_percent()
                    .map(|p| (cx.tr.f("models.compact_share", &[("percent", &p)]), false))
            };
            let shown = Shown {
                value: l.compact,
                initial: initial.map(short_tokens).unwrap_or_default(),
                placeholder: "—".to_owned(),
                source: l.compact_source,
                caption,
                hover: compact_hover(rc, &l, &o),
                enabled: rc.saved.view.per_model,
            };
            value_cell(ui, rc, &m.slug, Field::Compact, shown);
        }
        (RowRef::Model(m), 3) => {
            // Standard is always there: the column names the faster tiers.
            let tiers: Vec<SpeedTier> = model::tiers(m)
                .into_iter()
                .filter(|t| *t != SpeedTier::Standard)
                .collect();
            if tiers.is_empty() {
                ui.label(Text::small("—", cx.pal));
            } else {
                let names: Vec<String> = tiers.iter().map(|t| cx.tr.text(&t.text())).collect();
                let r = ui.add(egui::Label::new(Text::small(names.join(" · "), cx.pal)).truncate());
                let tips: Vec<String> = m
                    .service_tiers
                    .iter()
                    .filter(|t| !t.description.is_empty())
                    .map(|t| {
                        format!(
                            "{}: {}",
                            if t.name.is_empty() { &t.id } else { &t.name },
                            t.description
                        )
                    })
                    .collect();
                if !tips.is_empty() {
                    r.on_hover_text(tips.join("\n"));
                }
            }
        }
        (RowRef::Model(m), 4) if rc.custom => {
            let o = rc.pending.override_of(rc.saved, &m.slug);
            let value = o.upstream_name.clone().unwrap_or_default();
            upstream(ui, rc, &m.slug, value);
        }
        (RowRef::Model(m), 4) => {
            let shown = model::shown_price(rc.pending, rc.saved, rc.table, &m.slug);
            let (label, color) = match &shown {
                Some((p, true)) => (price_text(p), cx.pal.text),
                Some((p, false)) => (price_text(p), cx.pal.text_subtle),
                None => ("—".to_owned(), cx.pal.text_subtle),
            };
            let r = ui.add(
                egui::Label::new(
                    RichText::new(label)
                        .font(mono_font(text::CAPTION, Weight::Regular))
                        .color(color),
                )
                .truncate()
                .sense(if rc.read_only {
                    Sense::hover()
                } else {
                    Sense::click()
                }),
            );
            let r = r.on_hover_text(cx.tr.t("models.price.tip"));
            if r.clicked() {
                rc.edits.push(Edit::Price(m.slug.clone()));
            }
        }
        (RowRef::Model(m), 5) => reset_button(ui, rc, &m.slug),
        _ => {}
    }
}

fn source_of(set: bool) -> LimitSource {
    if set {
        LimitSource::Custom
    } else {
        LimitSource::Model
    }
}

/// The model column: marker, the name lines on the left and the row's state
/// on the right.
fn name_cell(ui: &mut Ui, rc: &RowCx<'_, '_>, slug: &str, name: impl FnOnce(&mut Ui)) {
    let rect = ui.max_rect();
    let state = model::row_state(rc.saved, rc.pending, slug, rc.active);
    let badge = ui
        .scope_builder(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::right_to_left(Align::Center)),
            |ui| state_badge(ui, rc, state),
        )
        .inner;
    let right = badge.map_or(rect.max.x, |r| r.min.x - 8.0);
    let left = egui::Rect::from_min_max(rect.min, egui::pos2(right.max(rect.min.x), rect.max.y));
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(left)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.set_clip_rect(left.intersect(ui.clip_rect()));
            marker(ui, rc, slug);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                name(ui);
            });
        },
    );
}

/// The row's state as a small pill (nothing when there is nothing to say).
fn state_badge(ui: &mut Ui, rc: &RowCx<'_, '_>, state: model::RowState) -> Option<egui::Rect> {
    use model::RowState as S;
    let cx = rc.cx;
    let provider = || super::provider_name(cx, &rc.saved.view.profile_id);
    let (key, color, tip) = match state {
        S::Quiet => return None,
        S::Unsaved => (
            "models.state.unsaved",
            cx.pal.amber,
            cx.tr.t("models.state.unsaved_tip"),
        ),
        S::NotApplied => (
            "models.state.not_applied",
            cx.pal.link,
            cx.tr
                .f("models.state.not_applied_tip", &[("provider", &provider())]),
        ),
        S::Applied => (
            "models.state.applied",
            cx.pal.green,
            cx.tr.t("models.state.applied_tip"),
        ),
        S::NextSwitch => (
            "models.state.next_switch",
            cx.pal.text_muted,
            cx.tr
                .f("models.state.next_switch_tip", &[("provider", &provider())]),
        ),
        S::NotUsed => (
            "models.state.not_used",
            cx.pal.text_muted,
            cx.tr
                .f("models.pat_per_model", &[("provider", &provider())]),
        ),
    };
    let r = pill(ui, cx, &cx.tr.t(key), color);
    let rect = r.rect;
    r.on_hover_text(tip);
    Some(rect)
}

/// A small tinted pill with caption text.
fn pill(ui: &mut Ui, cx: &Cx<'_>, label: &str, color: egui::Color32) -> egui::Response {
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_owned(), font(text::CAPTION, Weight::Medium), color);
    let size = vec2(galley.size().x + 12.0, 18.0);
    let (rect, r) = ui.allocate_exact_size(size, Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(
            rect,
            crate::design::tokens::radius::PILL,
            cx.pal.tint(color),
        );
        ui.painter().galley(
            egui::pos2(rect.min.x + 6.0, rect.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
    }
    r
}

/// ● overridden (amber while pending).
fn marker(ui: &mut Ui, rc: &RowCx<'_, '_>, slug: &str) {
    let (r, _) = ui.allocate_exact_size(vec2(8.0, 16.0), Sense::hover());
    if rc.pending.touched(slug) {
        ui.painter()
            .circle_filled(r.center(), 3.0, rc.cx.pal.status(Status::Warn));
    } else if rc.pending.overridden(rc.saved, slug) {
        ui.painter().circle_filled(r.center(), 3.0, rc.cx.pal.text);
    }
}

fn reset_button(ui: &mut Ui, rc: &mut RowCx<'_, '_>, slug: &str) {
    if rc.read_only || !rc.pending.overridden(rc.saved, slug) {
        return;
    }
    if ui
        .add(IconButton::new(Icon::Restore, rc.cx.tr.t("models.reset")).size(24.0))
        .clicked()
    {
        rc.edits.push(Edit::Reset(slug.to_owned()));
    }
}

/// What a window / compaction cell shows.
struct Shown {
    /// The value Codex uses (`None` = unknown, or unset on the all-models row).
    value: Option<u64>,
    /// Exact text an edit starts from.
    initial: String,
    /// Shown without a value.
    placeholder: String,
    /// Where the value comes from (the tag).
    source: LimitSource,
    /// Second line next to the tag (amber when true).
    caption: Option<(String, bool)>,
    /// Tooltip.
    hover: String,
    /// Per-model values reach Codex (false with a personal access token).
    enabled: bool,
}

/// A value cell: the field (a `k`/`M` label until edited; the exact value on
/// hover), then the tag (默认 / 所有模型 / 自定义) over a caption.
fn value_cell(ui: &mut Ui, rc: &mut RowCx<'_, '_>, slug: &str, field: Field, shown: Shown) {
    let cx = rc.cx;
    let key = (slug.to_owned(), field);
    let error = rc.errors.get(&key).cloned();
    let label = shown
        .value
        .map(token_label)
        .unwrap_or_else(|| shown.placeholder.clone());
    // Rejected text stays in the field (red) until it is fixed or cleared.
    let rejected = rc.rejected.get(&key).cloned();
    let muted =
        rejected.is_none() && (shown.source != LimitSource::Custom || shown.value.is_none());
    let (initial, label) = match rejected {
        Some(text) => (text.clone(), text),
        None => (shown.initial, label),
    };
    let buffer = rc.buffers.entry(key.clone()).or_default();
    let f = TokenField::new(("models", slug, field), buffer, initial)
        .display(label)
        .muted(muted)
        .hint(if field == Field::Compact {
            "80%"
        } else {
            "400k"
        })
        .width(if rc.narrow { FIELD_W_NARROW } else { FIELD_W })
        .error(error.is_some())
        .enabled(!rc.read_only && shown.enabled);
    let id = f.id();
    let r = f.show(ui);
    if rc.focus == Some(&key) {
        ui.memory_mut(|m| m.request_focus(id));
    }
    let tip = match (&error, shown.enabled) {
        (Some(e), _) => format!("{e}\n\n{}", shown.hover),
        (None, false) => format!(
            "{}\n\n{}",
            shown.hover,
            cx.tr.f(
                "models.pat_per_model",
                &[(
                    "provider",
                    &super::provider_name(cx, &rc.saved.view.profile_id)
                )]
            )
        ),
        (None, true) => shown.hover,
    };
    // Disabled fields (personal access token, busy) explain themselves too.
    r.response
        .on_hover_text(tip.clone())
        .on_disabled_hover_text(tip);
    if r.committed {
        let text = rc.buffers.get(&key).cloned().unwrap_or_default();
        rc.edits.push(Edit::Text {
            slug: slug.to_owned(),
            field,
            text,
        });
    }
    ui.add_space(6.0);
    // The tag over the caption, centred on the field.
    let block = if shown.caption.is_some() { 36.0 } else { 18.0 };
    let pad = ((ui.available_height() - block) / 2.0).max(0.0);
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), ui.available_height()),
        Layout::top_down(Align::Min),
        |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.add_space(pad);
            let (tag, color) = match shown.source {
                LimitSource::Custom => ("models.tag.custom", cx.pal.link),
                LimitSource::AllModels => ("models.tag.all", cx.pal.text_muted),
                LimitSource::Model => ("models.tag.default", cx.pal.text_subtle),
            };
            pill(ui, cx, &cx.tr.t(tag), color);
            if let Some((caption, amber)) = shown.caption {
                let t = Text::caption(caption, cx.pal);
                ui.add(egui::Label::new(if amber { t.color(cx.pal.amber) } else { t }).truncate());
            }
        },
    );
}

/// "200,000 tokens" with the source in brackets.
fn exact_line(cx: &Cx<'_>, value: u64, source: LimitSource) -> String {
    let source = cx.tr.t(match source {
        LimitSource::Custom => "models.tag.custom",
        LimitSource::AllModels => "models.tag.all",
        LimitSource::Model => "models.tag.default",
    });
    cx.tr.f(
        "models.exact",
        &[("value", &exact_tokens(value)), ("source", &source)],
    )
}

fn window_hover(rc: &RowCx<'_, '_>, m: &ModelSpec, l: &ModelLimits, o: &ModelOverride) -> String {
    let cx = rc.cx;
    let mut lines = Vec::new();
    match l.window {
        Some(w) => lines.push(exact_line(cx, w, l.window_source)),
        None => lines.push(cx.tr.t("models.window_unknown")),
    }
    if let Some(d) = m.resolved_context_window() {
        lines.push(
            cx.tr
                .f("models.hover.model_default", &[("value", &exact_tokens(d))]),
        );
    }
    if let Some(max) = l.max {
        lines.push(
            cx.tr
                .f("models.hover.max", &[("value", &exact_tokens(max))]),
        );
    }
    if let Some(u) = l.usable_window() {
        lines.push(cx.tr.f(
            "models.hover.usable",
            &[("percent", &l.usable_percent), ("value", &exact_tokens(u))],
        ));
    }
    if !rc.saved.view.per_model
        && let Some(c) = o.context_window
    {
        lines.push(
            cx.tr
                .f("models.hover.unused", &[("value", &exact_tokens(c))]),
        );
    }
    if rc.saved.view.per_model && !rc.read_only {
        lines.push(cx.tr.t("models.hover.edit_window"));
    }
    lines.join("\n")
}

fn compact_hover(rc: &RowCx<'_, '_>, l: &ModelLimits, o: &ModelOverride) -> String {
    let cx = rc.cx;
    let mut lines = Vec::new();
    match l.compact {
        Some(c) => lines.push(exact_line(cx, c, l.compact_source)),
        None => lines.push(cx.tr.t("models.window_unknown")),
    }
    if let (true, Some(limit), Some(c)) = (l.clamped, l.compact_limit, l.compact) {
        lines.push(cx.tr.f(
            "models.hover.clamped",
            &[("limit", &exact_tokens(limit)), ("value", &exact_tokens(c))],
        ));
    }
    if l.compact_source == LimitSource::Model && l.compact_limit.is_none() {
        lines.push(cx.tr.t("models.compact_default"));
    }
    if !rc.saved.view.per_model
        && let Some(c) = o.auto_compact
    {
        lines.push(
            cx.tr
                .f("models.hover.unused", &[("value", &exact_tokens(c))]),
        );
    }
    if rc.saved.view.per_model && !rc.read_only {
        lines.push(cx.tr.t("models.hover.edit_compact"));
    }
    lines.join("\n")
}

fn all_hover(cx: &Cx<'_>, value: Option<u64>, field: Field) -> String {
    let mut lines = Vec::new();
    if let Some(v) = value {
        lines.push(exact_line(cx, v, LimitSource::Custom));
    }
    lines.push(cx.tr.t(if field == Field::Window {
        "models.hover.all_window"
    } else {
        "models.hover.all_compact"
    }));
    lines.join("\n")
}

/// Upstream model name of a custom site's model, with the listed ids to pick.
fn upstream(ui: &mut Ui, rc: &mut RowCx<'_, '_>, slug: &str, value: String) {
    let key = (slug.to_owned(), Field::Upstream);
    let id = egui::Id::unique(("models-upstream", slug));
    let editing_id = id.with("editing");
    let was_editing = ui.data(|d| d.get_temp::<bool>(editing_id)).unwrap_or(false);
    let buffer = rc.buffers.entry(key.clone()).or_default();
    if !was_editing && !ui.memory(|m| m.has_focus(id)) {
        // Not editing: show the stored name, never a copy from an older view.
        buffer.clone_from(&value);
    }
    let r = ui
        .add_enabled(
            !rc.read_only,
            TextEdit::singleline(buffer)
                .id(id)
                .font(egui::TextStyle::Monospace)
                .hint_text(slug)
                .desired_width(128.0)
                .margin(vec2(8.0, 4.0)),
        )
        .accessible_name(format!("{} · {slug}", rc.cx.tr.t("models.col.upstream")));
    let mut picked = None;
    let ids: Vec<&str> = rc
        .saved
        .view
        .list
        .models
        .iter()
        .map(|m| m.slug.as_str())
        .collect();
    ui.add_enabled_ui(!rc.read_only && !ids.is_empty(), |ui| {
        Dropdown::new(("models-upstream-pick", slug))
            .label(rc.cx.tr.t("models.upstream.pick"))
            .selected_text("")
            .width(20.0)
            .menu_height(300.0)
            .show_ui(ui, |ui| {
                for id in &ids {
                    if ui.selectable_label(value == *id, *id).clicked() {
                        picked = Some((*id).to_owned());
                    }
                }
            })
            .response
            .on_hover_text(rc.cx.tr.t("models.upstream.pick"));
    });
    if r.has_focus() {
        ui.data_mut(|d| d.insert_temp(editing_id, true));
    } else if was_editing {
        ui.data_mut(|d| d.remove::<bool>(editing_id));
    }
    let committed =
        (r.lost_focus() && rc.buffers.get(&key).is_some_and(|b| *b != value)) || picked.is_some();
    if committed {
        let text = picked.unwrap_or_else(|| rc.buffers.get(&key).cloned().unwrap_or_default());
        rc.buffers.insert(key, text.clone());
        rc.edits.push(Edit::Text {
            slug: slug.to_owned(),
            field: Field::Upstream,
            text,
        });
    }
}

/// Apply a committed cell edit to the pending edits (with validation).
fn apply_edit(cx: &Cx<'_>, state: &mut State, saved: &Saved<'_>, edit: Edit) {
    match edit {
        Edit::Reset(slug) => {
            state.pending.reset(saved, &slug);
            for f in [Field::Window, Field::Compact, Field::Upstream] {
                state.buffers.remove(&(slug.clone(), f));
                state.invalid.remove(&(slug.clone(), f));
                state.rejected.remove(&(slug.clone(), f));
            }
        }
        Edit::Price(slug) => {
            let current = state.pending.price_of(saved, &slug);
            let text = |v: Option<Usd>| v.map(model::dollars).unwrap_or_default();
            state.price = Some(PriceDialog {
                input: text(current.as_ref().map(|p| p.input)),
                cached: text(current.as_ref().and_then(|p| p.cached_input)),
                output: text(current.as_ref().map(|p| p.output)),
                slug,
                error: None,
            });
        }
        Edit::Text { slug, field, text } => commit_text(cx, state, saved, &slug, field, &text),
    }
}

/// Record a committed field of the loaded view (see [`State::commit_field`]).
pub(super) fn apply_text(cx: &Cx<'_>, state: &mut State, slug: &str, field: Field, text: String) {
    let Some(view) = state.view.clone() else {
        return;
    };
    let map = model_map(cx, &view);
    let prices = saved_prices(cx, &view.profile_id);
    let saved = Saved {
        view: &view,
        model_map: &map,
        prices: &prices,
    };
    commit_text(cx, state, &saved, slug, field, &text);
}

fn commit_text(
    cx: &Cx<'_>,
    state: &mut State,
    saved: &Saved<'_>,
    slug: &str,
    field: Field,
    text: &str,
) {
    let key = (slug.to_owned(), field);
    state.invalid.remove(&key);
    state.rejected.remove(&key);
    match commit(cx, state, saved, slug, field, text) {
        Ok(()) => {
            // The field shows the stored value next frame.
            if field != Field::Upstream {
                state.buffers.remove(&key);
            }
        }
        Err(message) => {
            // The field keeps the rejected text (red) until it is fixed,
            // cleared or the edits are discarded.
            state.invalid.insert(key.clone(), message);
            state.rejected.insert(key, text.trim().to_owned());
        }
    }
}

/// Validate and record one field; the error message is translated. A value
/// equal to what the model would have without its own value records no
/// override (typing the default back is the same as clearing the field).
fn commit(
    cx: &Cx<'_>,
    state: &mut State,
    saved: &Saved<'_>,
    slug: &str,
    field: Field,
    text: &str,
) -> Result<(), String> {
    let all = slug == ALL_MODELS;
    let spec = saved.spec(slug);
    if field == Field::Upstream {
        let mut o = state.pending.override_of(saved, slug);
        let t = text.trim();
        o.upstream_name = (!t.is_empty() && t != slug).then(|| t.to_owned());
        if o.upstream_name.is_none() && saved.override_of(slug).upstream_name.is_some() {
            o.upstream_name = Some(String::new());
        }
        state.pending.set_override(saved, slug, o);
        return Ok(());
    }
    let format = |_| {
        cx.tr.t(if field == Field::Compact {
            "models.invalid.format"
        } else {
            "models.invalid.format_window"
        })
    };
    let input = parse_input(text, field == Field::Compact).map_err(format)?;
    let mut d = state.pending.defaults(saved);
    let mut o = state.pending.override_of(saved, slug);
    match field {
        Field::Window => {
            let value = match input {
                TokenInput::Empty => None,
                TokenInput::Tokens(n) => Some(
                    model::check_window(n, spec.and_then(model::max_of), all).map_err(
                        |e| match e {
                            WindowError::TooSmall(min) => {
                                limit_text(cx, &LimitError::WindowTooSmall { min })
                            }
                            WindowError::OverMax(max) => {
                                limit_text(cx, &LimitError::WindowOverMax { max })
                            }
                        },
                    )?,
                ),
                TokenInput::Percent(_) => return Err(format(TokenError::Format)),
            };
            if all {
                d.context_window = value;
                state.pending.set_defaults(saved, d);
            } else {
                let inherited = spec.and_then(|m| {
                    let without = ModelOverride {
                        context_window: None,
                        ..o.clone()
                    };
                    superba_core::model_limits(m, Some(&without), &d).window
                });
                o.context_window = value.filter(|v| Some(*v) != inherited);
                state.pending.set_override(saved, slug, o);
            }
        }
        Field::Compact => {
            let window = if all {
                d.context_window
            } else {
                spec.and_then(|m| model::limits(saved, &state.pending, m).window)
            };
            let value =
                model::compact_tokens(input, window).map_err(|CompactError::NeedsWindow| {
                    cx.tr.t("models.invalid.percent_needs_window")
                })?;
            if let Some(v) = value {
                superba_core::check_compact(v, window).map_err(|e| limit_text(cx, &e))?;
            }
            if all {
                d.auto_compact = value;
                state.pending.set_defaults(saved, d);
            } else {
                let inherited = spec.and_then(|m| {
                    let without = ModelOverride {
                        auto_compact: None,
                        ..o.clone()
                    };
                    superba_core::model_limits(m, Some(&without), &d).compact
                });
                o.auto_compact = value.filter(|v| Some(*v) != inherited);
                state.pending.set_override(saved, slug, o);
            }
        }
        Field::Upstream => {}
    }
    Ok(())
}

/// Toggles on the left (hidden models, picker mode), the catalog note on the right.
fn bottom(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, saved: &Saved<'_>) {
    let view = saved.view;
    ui.horizontal(|ui| {
        ui.set_min_height(24.0);
        ui.spacing_mut().item_spacing.x = 6.0;
        let hidden = model::hidden_count(saved, &state.pending);
        if hidden > 0 || state.show_hidden {
            ui.add(Toggle::new(
                &mut state.show_hidden,
                cx.tr.t("models.show_hidden"),
            ));
            ui.label(Text::small(
                cx.tr.f("models.show_hidden_n", &[("n", &hidden)]),
                cx.pal,
            ));
            ui.add_space(12.0);
        }
        if view.picker_available && !view.read_only {
            let mut on = state.pending.picker(saved) == ModelPicker::Provider;
            let before = on;
            ui.add(
                Toggle::new(&mut on, cx.tr.t("models.picker_mode"))
                    .enabled(!cx.busy && state.saving.is_none()),
            );
            ui.label(Text::small(cx.tr.t("models.picker_mode"), cx.pal))
                .on_hover_text(cx.tr.t("models.picker_mode_tip"));
            if on != before {
                state.pending.set_picker(
                    saved,
                    if on {
                        ModelPicker::Provider
                    } else {
                        ModelPicker::CodexDefault
                    },
                );
            }
            ui.add_space(12.0);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (status, line, all) = note_text(cx, view);
            let r = ui.add(egui::Label::new(Text::caption(line, cx.pal)).truncate());
            if !all.is_empty() {
                r.on_hover_text(all.join("\n"));
            }
            ui.add(StatusDot::new(status));
        });
    });
}

/// The first note to show in the bottom row, its status and every note.
fn note_text(cx: &Cx<'_>, view: &ModelsView) -> (Status, String, Vec<String>) {
    let mut all: Vec<String> = Vec::new();
    if view.read_only {
        let t = cx.tr.t("models.official_readonly");
        return (Status::Idle, t.clone(), vec![t]);
    }
    let file = match &view.catalog_file {
        CatalogFileStatus::NotNeeded => None,
        other => Some(cx.localize(&other.text())),
    };
    let warn = super::warning(&view.notes)
        || matches!(view.catalog_file, CatalogFileStatus::Failed { .. });
    all.extend(view.notes.iter().map(|n| cx.localize(&n.text)));
    all.extend(file.clone());
    all.push(cx.tr.t("models.catalog_note"));
    let first = view
        .notes
        .iter()
        .find(|n| n.severity != superba_core::Severity::Info)
        .map(|n| cx.localize(&n.text))
        .or(file)
        .or_else(|| view.notes.first().map(|n| cx.localize(&n.text)))
        .unwrap_or_else(|| cx.tr.t("models.catalog_note"));
    (if warn { Status::Warn } else { Status::Idle }, first, all)
}

fn field_label(cx: &Cx<'_>, f: DiffField) -> String {
    cx.tr.t(match f {
        DiffField::Model => "models.default_model",
        DiffField::Effort => "models.effort",
        DiffField::Speed => "models.speed.label",
        DiffField::Window => "models.col.context",
        DiffField::Compact => "models.col.compact",
        DiffField::Upstream => "models.col.upstream",
        DiffField::Name => "models.diff.name",
        DiffField::Hidden => "models.diff.hidden",
        DiffField::Picker => "models.picker_mode",
        DiffField::Price => "models.col.price",
    })
}

fn diff_dialog(ctx: &egui::Context, cx: &Cx<'_>, state: &mut State, saved: &Saved<'_>) {
    let lines = state.pending.diff(saved);
    let mut close = false;
    let r = dialog(ctx, egui::Id::unique("models-diff"), 560.0, |ui| {
        ui.label(Text::h2(cx.tr.t("models.diff.title"), cx.pal));
        if lines.is_empty() {
            ui.label(Text::small(cx.tr.t("models.diff.empty"), cx.pal));
        }
        egui::ScrollArea::vertical()
            .id_salt("models-diff-list")
            .max_height(360.0)
            .show(ui, |ui| {
                for l in &lines {
                    let who = if l.slug.is_empty() {
                        cx.tr.t("models.all")
                    } else {
                        l.slug.clone()
                    };
                    let value =
                        |v: &Option<String>| v.clone().unwrap_or_else(|| cx.tr.t("models.default"));
                    ui.horizontal_wrapped(|ui| {
                        ui.label(Text::mono(who, cx.pal));
                        ui.label(Text::small(field_label(cx, l.field), cx.pal));
                        ui.label(Text::mono_muted(
                            format!("{} → {}", value(&l.before), value(&l.after)),
                            cx.pal,
                        ));
                    });
                }
            });
        ui.add_space(8.0);
        if ui
            .add(Button::new(cx.tr.t("models.diff.close")).kbd("Esc"))
            .clicked()
        {
            close = true;
        }
    });
    if close || r.should_close() {
        state.diff_open = false;
    }
}

fn price_dialog(ctx: &egui::Context, cx: &Cx<'_>, state: &mut State, saved: &Saved<'_>) {
    let Some(d) = state.price.as_mut() else {
        return;
    };
    let table = model::shown_price(
        &model::Pending::default(),
        &Saved {
            prices: &BTreeMap::new(),
            ..*saved
        },
        state.prices.as_ref(),
        &d.slug,
    );
    let mut close = false;
    let mut result: Option<Option<PriceEntry>> = None;
    let r = dialog(ctx, egui::Id::unique("models-price"), 440.0, |ui| {
        ui.label(Text::h2(
            cx.tr.f("models.price.title", &[("model", &d.slug)]),
            cx.pal,
        ));
        ui.add(egui::Label::new(Text::small(cx.tr.t("models.price.body"), cx.pal)).wrap());
        if let Some((p, _)) = &table {
            ui.label(Text::caption(
                cx.tr.f("models.price.table", &[("price", &price_text(p))]),
                cx.pal,
            ));
        }
        for (label, value) in [
            ("models.price.input", &mut d.input),
            ("models.price.cached", &mut d.cached),
            ("models.price.output", &mut d.output),
        ] {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(150.0, 24.0), Sense::hover());
                ui.painter().text(
                    rect.left_center(),
                    egui::Align2::LEFT_CENTER,
                    cx.tr.t(label),
                    font(text::SMALL, Weight::Regular),
                    cx.pal.text_muted,
                );
                ui.add(
                    TextEdit::singleline(value)
                        .id(egui::Id::unique(("models-price-field", label)))
                        .font(egui::TextStyle::Monospace)
                        .desired_width(120.0)
                        .margin(vec2(8.0, 4.0)),
                )
                .accessible_name(cx.tr.t(label));
            });
        }
        if let Some(e) = &d.error {
            ui.label(Text::small(e, cx.pal).color(cx.pal.red));
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .add(Button::primary(cx.tr.t("models.price.save")).small())
                .clicked()
            {
                match parse_price(d) {
                    Some(p) => result = Some(Some(p)),
                    None => d.error = Some(cx.tr.t("models.price.invalid")),
                }
            }
            if ui
                .add(Button::new(cx.tr.t("models.price.remove")).small())
                .clicked()
            {
                result = Some(None);
            }
            if ui
                .add(Button::ghost(cx.tr.t("action.cancel")).small().kbd("Esc"))
                .clicked()
            {
                close = true;
            }
        });
    });
    if let Some(price) = result {
        let slug = d.slug.clone();
        state.pending.set_price(saved, &slug, price);
        state.price = None;
    } else if close || r.should_close() {
        state.price = None;
    }
}

/// The dialog's prices (input and output required; cached defaults to input).
pub fn parse_price(d: &PriceDialog) -> Option<PriceEntry> {
    let input = Usd::parse_dollars(&d.input).filter(|v| v.micros() >= 0)?;
    let output = Usd::parse_dollars(&d.output).filter(|v| v.micros() >= 0)?;
    let cached = if d.cached.trim().is_empty() {
        None
    } else {
        Some(Usd::parse_dollars(&d.cached).filter(|v| v.micros() >= 0)?)
    };
    Some(PriceEntry {
        input,
        cached_input: cached,
        cache_write: None,
        output,
        long_context: Vec::new(),
        tiers: BTreeMap::new(),
        tier_prices: BTreeMap::new(),
        source: None,
        rev: None,
    })
}
