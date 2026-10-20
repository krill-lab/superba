//! The router's model list on the Models page (alpha.8): the router model list
//! row (mode and provider prefixes), the source / provider filters, the
//! providers column (one chip per provider, its source and upstream id on
//! hover, a per-provider test on click), the model test and the notes about
//! the providers' lists.
//!
//! The page shows one row per model. What Codex is offered depends on the
//! mode: Codex's own models only (the default), plus the providers' models
//! (published once their tool test passed), or both plus one alias per
//! provider (`krill/gpt-5.5`, "GPT-5.5 · Krill") that pins the model to it.

use std::collections::BTreeMap;

use egui::{Layout, Ui};
use superba_core::{
    CatalogLayer, CatalogMode, CatalogProvenance, CatalogReadiness, ModelsView, RouterCatalogModel,
    RouterCatalogSettings, RouterCatalogView,
};

use super::{Cx, State};
use crate::design::icons::Icon;
use crate::design::tokens::{size, space};
use crate::design::widgets::{Badge, Button, Dropdown, FormRow, Status, Text};
use crate::i18n::Tr;

/// Width of the mode dropdown in the router model list row.
const MODE_W: f32 = 220.0;
/// Width of a slug field in the provider prefixes editor.
const SLUG_W: f32 = 150.0;
/// Width of one table filter.
const FILTER_W: f32 = 132.0;
/// Width of the two table filters side by side.
pub(super) const FILTERS_W: f32 = 2.0 * FILTER_W + space::S;

/// The name of a mode ("Codex models only").
pub fn mode_label(tr: &Tr, mode: CatalogMode) -> String {
    tr.t(match mode {
        CatalogMode::Codex => "models.router.mode.codex",
        CatalogMode::Extended => "models.router.mode.extended",
        CatalogMode::Prefixed => "models.router.mode.prefixed",
    })
}

/// What a mode offers Codex, in one sentence.
pub fn mode_help(tr: &Tr, mode: CatalogMode) -> String {
    tr.t(match mode {
        CatalogMode::Codex => "models.router.mode.codex_help",
        CatalogMode::Extended => "models.router.mode.extended_help",
        CatalogMode::Prefixed => "models.router.mode.prefixed_help",
    })
}

/// A mode in its menu: the name over what it offers Codex.
fn mode_option(cx: &Cx<'_>, mode: CatalogMode) -> egui::text::LayoutJob {
    use crate::design::fonts::{Weight, font};
    use crate::design::tokens::text;
    let mut job = egui::text::LayoutJob::default();
    job.append(
        &mode_label(cx.tr, mode),
        0.0,
        egui::TextFormat::simple(font(text::BODY, Weight::Medium), cx.pal.text),
    );
    job.append(
        &format!("\n{}", mode_help(cx.tr, mode)),
        0.0,
        egui::TextFormat::simple(font(text::CAPTION, Weight::Regular), cx.pal.text_muted),
    );
    job
}

/// Where a provider's metadata for a model came from.
pub(super) fn layer_key(layer: CatalogLayer) -> &'static str {
    match layer {
        CatalogLayer::Builtin => "models.catalog.builtin",
        CatalogLayer::Codex => "models.catalog.codex",
        CatalogLayer::Standard => "models.catalog.standard",
        CatalogLayer::StandardKnown => "models.catalog.standard_known",
        CatalogLayer::Synthesized => "models.catalog.synthesized",
    }
}

/// The row of a canonical model id.
pub(super) fn row<'a>(view: &'a ModelsView, id: &str) -> Option<&'a RouterCatalogModel> {
    view.router_catalog.as_ref()?.model(id)
}

/// The model passes the source and provider filters.
pub(super) fn matches(state: &State, view: &ModelsView, id: &str) -> bool {
    let Some(row) = row(view, id) else {
        return true;
    };
    row.sources.iter().any(|s| {
        state.catalog_source.is_none_or(|layer| s.layer == layer)
            && state
                .catalog_provider
                .as_ref()
                .is_none_or(|provider| *provider == s.provider)
    })
}

/// Some provider reaches the model through a protocol conversion.
pub(super) fn translated(row: &RouterCatalogModel) -> bool {
    row.sources.iter().any(CatalogProvenance::translated)
}

/// A readiness that keeps a model (or one provider of it) from Codex: its
/// pill text, colour and explanation. `None` when it is published.
pub(super) fn readiness_pill(
    cx: &Cx<'_>,
    readiness: CatalogReadiness,
) -> Option<(String, Status, String)> {
    let (key, status, tip) = match readiness {
        CatalogReadiness::Ready => return None,
        CatalogReadiness::Pending => (
            "models.catalog.pending",
            Status::Warn,
            "models.router.pending_tip",
        ),
        CatalogReadiness::Failed => (
            "models.catalog.failed",
            Status::Error,
            "models.router.failed_tip",
        ),
        CatalogReadiness::NeedsTranslation => (
            "models.catalog.translation",
            Status::Idle,
            "models.router.translation_tip",
        ),
    };
    Some((cx.tr.t(key), status, cx.tr.t(tip)))
}

/// Everything about one provider of a model: its source, upstream id,
/// pinned alias and state (the chip's tooltip and menu).
pub fn chip_lines(cx: &Cx<'_>, source: &CatalogProvenance) -> Vec<String> {
    let mut lines = vec![format!(
        "{} · {}",
        source.provider_name,
        cx.tr.t(layer_key(source.layer))
    )];
    lines.push(
        cx.tr
            .f("models.router.upstream_id", &[("id", &source.upstream_id)]),
    );
    if let Some(alias) = &source.alias {
        lines.push(cx.tr.f("models.router.alias", &[("alias", alias)]));
    }
    let status = match source.readiness {
        Some(CatalogReadiness::Ready) | None => cx.tr.t("models.router.ready"),
        Some(other) => readiness_pill(cx, other).map(|p| p.2).unwrap_or_default(),
    };
    lines.push(status);
    if source.layer == CatalogLayer::Builtin {
        lines.push(cx.tr.t("models.router.builtin_tip"));
    }
    if source.translated() {
        lines.push(cx.tr.f(
            "models.router.converted",
            &[("protocol", &protocol_name(&source.protocol))],
        ));
    }
    if !source.endpoint.is_empty() && source.endpoint != "bundled" {
        lines.push(source.endpoint.clone());
    }
    lines
}

fn protocol_name(protocol: &str) -> String {
    match protocol {
        "chat_completions" => "Chat Completions".into(),
        "anthropic_messages" => "Anthropic Messages".into(),
        "gemini" => "Gemini".into(),
        "responses" => "Responses".into(),
        other => other.to_owned(),
    }
}

/// Start a test of `model` (a canonical id: each provider; an alias: that
/// provider only). The result arrives as a toast.
pub(super) fn test(state: &mut State, cx: &Cx<'_>, model: String) {
    if state.catalog_test.is_some() {
        return;
    }
    let service = cx.service.clone();
    let requested = model.clone();
    state.catalog_test = Some((
        requested,
        crate::runtime::task(async move { service.test_router_model(model).await }),
    ));
}

/// Show or hide a model in Codex's picker (the bare id and its aliases).
pub(super) fn visibility(
    state: &mut State,
    cx: &Cx<'_>,
    view: &ModelsView,
    slug: String,
    visible: bool,
) {
    let Some(catalog) = &view.router_catalog else {
        return;
    };
    if state.catalog_save.is_some() {
        return;
    }
    let mut settings = catalog.settings.clone();
    settings.visibility.insert(slug.clone(), visible);
    if let Some(value) = settings.overrides.get_mut(&slug) {
        value.hidden = !visible;
    }
    save_settings(state, cx, settings);
}

fn save_settings(state: &mut State, cx: &Cx<'_>, settings: RouterCatalogSettings) {
    let service = cx.service.clone();
    state.catalog_save = Some(crate::runtime::task(async move {
        service.save_router_catalog(settings).await
    }));
}

/// "Router model list": what the router offers Codex, as a settings row
/// (title and the mode's explanation on the left, the mode and the provider
/// prefixes on the right), with a hairline below.
pub(super) fn mode_row(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, view: &ModelsView) {
    let Some(catalog) = &view.router_catalog else {
        return;
    };
    let mode = catalog.settings.mode;
    let mut picked = None;
    let enabled = state.catalog_save.is_none() && !cx.busy && !view.read_only;
    // The controls keep the width they used last frame (the prefixes button
    // is wider in English); the explanation takes the rest and wraps (prose
    // wraps, UI spec §7), so the row grows a line at 960 px instead of
    // eliding what the mode does.
    let controls_id = egui::Id::unique("superba-models-mode-controls");
    let controls_w: f32 = ui
        .data(|d| d.get_temp(controls_id))
        .unwrap_or(MODE_W + space::S + 160.0);
    let text_w = (ui.available_width() - controls_w - space::L).max(160.0);
    let row = FormRow::default()
        .label_width(text_w)
        .gap(space::L)
        .height(size::CONTROL + space::S)
        .right()
        .id_salt("models-router-mode")
        .show_with(
            ui,
            |ui| {
                ui.vertical(|ui| {
                    ui.set_max_width(text_w);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(Text::strong(cx.tr.t("models.router.title"), cx.pal));
                    ui.add(egui::Label::new(Text::small(mode_help(cx.tr, mode), cx.pal)).wrap());
                });
            },
            |ui| {
                ui.spacing_mut().item_spacing.x = space::S;
                ui.add_enabled_ui(enabled, |ui| {
                    Dropdown::new("models-router-mode")
                        .medium()
                        .label(cx.tr.t("models.router.title"))
                        .width(MODE_W)
                        .selected_text(mode_label(cx.tr, mode))
                        .show_ui(ui, |ui| {
                            for choice in CatalogMode::ALL {
                                if ui
                                    .selectable_label(choice == mode, mode_option(cx, choice))
                                    .clicked()
                                {
                                    picked = Some(choice);
                                }
                            }
                        });
                });
                prefixes(ui, cx, state, catalog, enabled);
            },
        );
    let used = row.controls_rect.width();
    if (used - controls_w).abs() > 0.5 {
        ui.data_mut(|d| d.insert_temp(controls_id, used));
        ui.ctx().request_repaint();
    }
    let (r, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().hline(
        r.x_range(),
        r.center().y,
        egui::Stroke::new(1.0, cx.pal.border),
    );
    if let Some(choice) = picked.filter(|m| *m != mode) {
        let mut settings = catalog.settings.clone();
        settings.mode = choice;
        save_settings(state, cx, settings);
    }
}

/// The display name of each member with a slug.
fn provider_names(catalog: &RouterCatalogView) -> BTreeMap<String, String> {
    let mut names: BTreeMap<String, String> = catalog
        .settings
        .provider_slugs
        .keys()
        .map(|id| (id.clone(), id.clone()))
        .collect();
    for source in catalog.models.iter().flat_map(|m| &m.sources) {
        if let Some(name) = names.get_mut(&source.provider) {
            name.clone_from(&source.provider_name);
        }
    }
    names
}

/// A slug the router would accept: lower-case letters, digits and single
/// hyphens, at most 48 characters.
pub fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 48
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && !slug.contains("--")
}

/// A generated number (`p2`): worth a hint to pick a memorable name.
fn numbered(slug: &str) -> bool {
    slug.strip_prefix('p')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// "Provider prefixes": every provider's slug, editable, with an example of
/// the alias it makes.
fn prefixes(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    catalog: &RouterCatalogView,
    enabled: bool,
) {
    let button = ui.add(
        Button::new(cx.tr.t("models.router.prefixes"))
            .trailing(Icon::ChevronDown)
            .enabled(enabled && !catalog.settings.provider_slugs.is_empty()),
    );
    let names = provider_names(catalog);
    let example = catalog
        .models
        .first()
        .map(|m| m.id().to_owned())
        .unwrap_or_else(|| "gpt-5.5".into());
    let mut save = false;
    // Opens leftwards from the button, inside the page card.
    let popup = egui::Popup::menu(&button)
        .align(egui::RectAlign::BOTTOM_END)
        .gap(crate::design::widgets::dropdown::MENU_GAP)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_width(SLUG_W + 260.0);
            ui.spacing_mut().item_spacing.y = space::S;
            ui.label(Text::strong(cx.tr.t("models.router.prefixes"), cx.pal));
            // A menu lays out justified; prose reads better ragged.
            ui.with_layout(Layout::top_down(egui::Align::Min), |ui| {
                ui.add(
                    egui::Label::new(Text::small(
                        cx.tr.f(
                            "models.router.prefixes_help",
                            &[("mode", &mode_label(cx.tr, CatalogMode::Prefixed))],
                        ),
                        cx.pal,
                    ))
                    .wrap(),
                );
            });
            // Text fields keep their frame inside the menu.
            let v = ui.visuals_mut();
            v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, cx.pal.border);
            v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, cx.pal.border_strong);
            v.extreme_bg_color = cx.pal.surface;
            let label_w = 110.0;
            for (id, slug) in &catalog.settings.provider_slugs {
                let name = names.get(id).cloned().unwrap_or_else(|| id.clone());
                let draft = state
                    .catalog_slugs
                    .entry(id.clone())
                    .or_insert_with(|| slug.clone());
                let ok = valid_slug(draft.trim());
                FormRow::new(name.clone())
                    .label_width(label_w)
                    .id_salt(("models-slug", id))
                    .hint(Text::caption(
                        if !ok {
                            cx.tr.t("models.router.slug_invalid")
                        } else if numbered(draft.trim()) {
                            cx.tr.t("models.router.slug_numbered")
                        } else {
                            cx.tr.f(
                                "models.router.slug_example",
                                &[("alias", &format!("{}/{example}", draft.trim()))],
                            )
                        },
                        cx.pal,
                    ))
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::singleline(draft)
                                .font(egui::TextStyle::Monospace)
                                .desired_width(SLUG_W)
                                .margin(egui::vec2(8.0, 4.0))
                                .text_color_opt((!ok).then_some(cx.pal.red)),
                        )
                        .accessible_name(cx.tr.f("models.router.slug_of", &[("provider", &name)]));
                    });
            }
            let drafts: Vec<String> = catalog
                .settings
                .provider_slugs
                .keys()
                .map(|id| {
                    state
                        .catalog_slugs
                        .get(id)
                        .map(|s| s.trim().to_owned())
                        .unwrap_or_default()
                })
                .collect();
            let mut unique = drafts.clone();
            unique.sort();
            unique.dedup();
            let valid = drafts.iter().all(|s| valid_slug(s)) && unique.len() == drafts.len();
            if !valid && unique.len() != drafts.len() {
                ui.label(
                    Text::caption(cx.tr.t("models.router.slug_duplicate"), cx.pal)
                        .color(cx.pal.red),
                );
            }
            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(
                        Button::new(cx.tr.t("models.catalog.save_namespaces"))
                            .small()
                            .enabled(valid && state.catalog_save.is_none()),
                    )
                    .clicked()
                {
                    save = true;
                }
            });
        });
    if popup.is_none() {
        // Closed: the next opening starts from the saved slugs.
        state.catalog_slugs.clear();
    }
    if save {
        let mut settings = catalog.settings.clone();
        for (id, slug) in &mut settings.provider_slugs {
            if let Some(draft) = state.catalog_slugs.get(id) {
                *slug = draft.trim().to_owned();
            }
        }
        state.catalog_slugs.clear();
        egui::Popup::close_all(ui.ctx());
        save_settings(state, cx, settings);
    }
}

/// The source and provider filters of the table.
pub(super) fn filters(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, view: &ModelsView) {
    let Some(catalog) = &view.router_catalog else {
        return;
    };
    ui.spacing_mut().item_spacing.x = space::S;
    let layers: Vec<CatalogLayer> = [
        CatalogLayer::Codex,
        CatalogLayer::StandardKnown,
        CatalogLayer::Synthesized,
        CatalogLayer::Builtin,
    ]
    .into_iter()
    .filter(|l| {
        catalog
            .models
            .iter()
            .flat_map(|m| &m.sources)
            .any(|s| s.layer == *l)
    })
    .collect();
    let mut options = vec![(None, cx.tr.t("models.catalog.all_sources"))];
    options.extend(layers.into_iter().map(|l| (Some(l), cx.tr.t(layer_key(l)))));
    Dropdown::new("catalog-source")
        .label(cx.tr.t("models.router.source_filter"))
        .width(FILTER_W)
        .show_options(ui, &mut state.catalog_source, &options);
    let providers: BTreeMap<String, String> = catalog
        .models
        .iter()
        .flat_map(|m| &m.sources)
        .map(|s| (s.provider.clone(), s.provider_name.clone()))
        .collect();
    let mut options = vec![(None, cx.tr.t("models.catalog.all_providers"))];
    options.extend(providers.into_iter().map(|(id, name)| (Some(id), name)));
    Dropdown::new("catalog-provider")
        .label(cx.tr.t("models.router.provider_filter"))
        .width(FILTER_W)
        .show_options(ui, &mut state.catalog_provider, &options);
}

/// What a click in the providers column asks for.
pub(super) enum ChipAction {
    /// Test this provider only (its alias).
    Test(String),
}

/// The providers column: one chip per provider in member order, a dot when
/// that provider is not ready, an arrow when it goes through a protocol
/// conversion. Chips that do not fit become "+N" (the rest on hover and in
/// its menu); nothing is squeezed.
pub(super) fn providers_cell(
    ui: &mut Ui,
    cx: &Cx<'_>,
    row: &RouterCatalogModel,
    testing: bool,
) -> Option<ChipAction> {
    let mut action = None;
    let width = ui.available_width();
    let chip = |source: &CatalogProvenance| {
        let mut badge = Badge::new(source.provider_name.clone()).clickable();
        if let Some(status) = source
            .readiness
            .and_then(|r| readiness_pill(cx, r))
            .map(|p| p.1)
        {
            badge = badge.status(status);
        }
        if source.translated() {
            badge = badge.icon(Icon::ArrowRightLeft);
        }
        badge
    };
    let gap = ui.spacing().item_spacing.x;
    // Measure: a chip is its text plus padding, a dot and an icon.
    let widths: Vec<f32> = row
        .sources
        .iter()
        .map(|s| {
            let text = ui
                .painter()
                .layout_no_wrap(
                    s.provider_name.clone(),
                    crate::design::fonts::font(
                        crate::design::tokens::text::CAPTION,
                        crate::design::fonts::Weight::Medium,
                    ),
                    egui::Color32::PLACEHOLDER,
                )
                .size()
                .x;
            let dot = s.readiness.is_some_and(|r| r != CatalogReadiness::Ready);
            text + 16.0 + if dot { 12.0 } else { 0.0 } + if s.translated() { 16.0 } else { 0.0 }
        })
        .collect();
    let more_w = 34.0;
    let mut shown = 0;
    let mut used = 0.0;
    for (i, w) in widths.iter().enumerate() {
        let rest = i + 1 < widths.len();
        let need = used + w + if rest { gap + more_w } else { 0.0 };
        if need > width && shown > 0 {
            break;
        }
        used += w + gap;
        shown += 1;
    }
    for source in &row.sources[..shown] {
        let r = ui.add(chip(source));
        let tip = chip_lines(cx, source).join("\n");
        let r = r.on_hover_text(tip);
        egui::Popup::menu(&r)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
            .show(|ui| {
                ui.set_min_width(220.0);
                for (i, line) in chip_lines(cx, source).into_iter().enumerate() {
                    ui.label(if i == 0 {
                        Text::strong(line, cx.pal)
                    } else {
                        Text::small(line, cx.pal)
                    });
                }
                if let Some(alias) = &source.alias {
                    ui.add_space(space::XS);
                    if ui
                        .add(
                            Button::new(cx.tr.f(
                                "models.router.test_provider",
                                &[("provider", &source.provider_name)],
                            ))
                            .small()
                            .enabled(!testing),
                        )
                        .clicked()
                    {
                        action = Some(ChipAction::Test(alias.clone()));
                    }
                }
            });
    }
    let hidden = &row.sources[shown..];
    if !hidden.is_empty() {
        let r = ui.add(
            Badge::new(format!("+{}", hidden.len()))
                .outline()
                .clickable(),
        );
        let tip = hidden
            .iter()
            .map(|s| chip_lines(cx, s).join("\n"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let r = r.on_hover_text(tip);
        egui::Popup::menu(&r)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
            .show(|ui| {
                ui.set_min_width(220.0);
                for source in hidden {
                    for line in chip_lines(cx, source) {
                        ui.label(Text::small(line, cx.pal));
                    }
                    if let Some(alias) = &source.alias
                        && ui
                            .add(
                                Button::new(cx.tr.f(
                                    "models.router.test_provider",
                                    &[("provider", &source.provider_name)],
                                ))
                                .small()
                                .enabled(!testing),
                            )
                            .clicked()
                    {
                        action = Some(ChipAction::Test(alias.clone()));
                    }
                    ui.separator();
                }
            });
    }
    action
}

/// Problems with the providers' lists, worst first: a provider whose lists
/// all failed, then a stale list. One failed list next to a working one is
/// normal (a site without a Codex catalog) and only shows in the details.
pub(super) fn list_problems(cx: &Cx<'_>, catalog: &RouterCatalogView) -> (Status, Vec<String>) {
    let names = provider_names(catalog);
    let mut by_member: BTreeMap<&str, Vec<&superba_core::CatalogSourceStatus>> = BTreeMap::new();
    for s in &catalog.sources {
        by_member.entry(s.provider.as_str()).or_default().push(s);
    }
    let mut errors = Vec::new();
    let mut stale = Vec::new();
    let mut details = Vec::new();
    for (member, lists) in &by_member {
        let name = names
            .get(*member)
            .cloned()
            .unwrap_or_else(|| (*member).to_owned());
        let failed = lists
            .iter()
            .filter(|s| s.error.is_some() && !s.stale)
            .count();
        if failed == lists.len() {
            errors.push(
                cx.tr
                    .f("models.router.lists_failed", &[("provider", &name)]),
            );
        } else if lists.iter().any(|s| s.stale) {
            stale.push(cx.tr.f("models.router.lists_stale", &[("provider", &name)]));
        }
        for s in lists.iter().filter(|s| s.error.is_some()) {
            details.push(format!(
                "{name} · {}: {}",
                cx.tr.t(layer_key(s.layer)),
                s.error.as_deref().unwrap_or_default()
            ));
        }
    }
    let status = if !errors.is_empty() {
        Status::Warn
    } else {
        Status::Idle
    };
    let mut out = errors;
    out.extend(stale);
    out.extend(details);
    (status, out)
}
