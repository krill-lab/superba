//! Controls and evidence for the live router catalog.

use egui::Ui;
use superba_core::{CatalogLayer, CatalogReadiness, ModelsView, RouterCatalogModel};

use super::{Cx, State};
use crate::design::widgets::Dropdown;

pub(super) fn layer_key(layer: CatalogLayer) -> &'static str {
    match layer {
        CatalogLayer::Builtin => "models.catalog.builtin",
        CatalogLayer::Codex => "models.catalog.codex",
        CatalogLayer::Standard => "models.catalog.standard",
        CatalogLayer::Synthesized => "models.catalog.synthesized",
    }
}

pub(super) fn row<'a>(view: &'a ModelsView, id: &str) -> Option<&'a RouterCatalogModel> {
    view.router_catalog
        .as_ref()?
        .models
        .iter()
        .find(|m| m.entry["slug"].as_str() == Some(id))
}

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

pub(super) fn badge(cx: &Cx<'_>, row: &RouterCatalogModel) -> String {
    let layer = if row
        .sources
        .iter()
        .any(|s| s.layer == CatalogLayer::Synthesized)
    {
        CatalogLayer::Synthesized
    } else if row.sources.iter().any(|s| s.layer == CatalogLayer::Codex) {
        CatalogLayer::Codex
    } else if row
        .sources
        .iter()
        .any(|s| s.layer == CatalogLayer::Standard)
    {
        CatalogLayer::Standard
    } else {
        CatalogLayer::Builtin
    };
    let state = match row.readiness {
        CatalogReadiness::Ready => None,
        CatalogReadiness::Pending => Some("models.catalog.pending"),
        CatalogReadiness::Failed => Some("models.catalog.failed"),
        CatalogReadiness::NeedsTranslation => Some("models.catalog.translation"),
    };
    let label = cx.tr.t(layer_key(layer));
    state.map_or_else(
        || label.clone(),
        |key| format!("{label} · {}", cx.tr.t(key)),
    )
}

pub(super) fn evidence(row: &RouterCatalogModel) -> String {
    row.sources
        .iter()
        .map(|s| {
            format!(
                "{} · {}\n{}\n{} · {} · {}\n{}",
                s.provider_name,
                s.upstream_id,
                s.endpoint,
                s.protocol,
                s.key_scope,
                s.observed_at,
                s.evidence
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub(super) fn test(state: &mut State, cx: &Cx<'_>, model: String) {
    if state.catalog_test.is_some() {
        return;
    }
    state.catalog_result = None;
    state.catalog_error = None;
    let service = cx.service.clone();
    state.catalog_test = Some(crate::runtime::task(async move {
        service.test_router_model(model).await
    }));
}

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

fn save_settings(state: &mut State, cx: &Cx<'_>, settings: superba_core::RouterCatalogSettings) {
    let service = cx.service.clone();
    state.catalog_error = None;
    state.catalog_save = Some(crate::runtime::task(async move {
        service.save_router_catalog(settings).await
    }));
}

pub(super) fn controls(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, view: &ModelsView) {
    let Some(catalog) = &view.router_catalog else {
        return;
    };
    let mut settings = catalog.settings.clone();
    let mut save = false;
    ui.horizontal_wrapped(|ui| {
        save |= ui
            .add_enabled(
                state.catalog_save.is_none() && !cx.busy,
                egui::Checkbox::new(
                    &mut settings.show_prefixed,
                    cx.tr.t("models.catalog.prefixed"),
                ),
            )
            .changed();
        Dropdown::new("catalog-source")
            .width(150.0)
            .selected_text(state.catalog_source.map_or_else(
                || cx.tr.t("models.catalog.all_sources"),
                |s| cx.tr.t(layer_key(s)),
            ))
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut state.catalog_source,
                    None,
                    cx.tr.t("models.catalog.all_sources"),
                );
                for source in [
                    CatalogLayer::Builtin,
                    CatalogLayer::Codex,
                    CatalogLayer::Standard,
                    CatalogLayer::Synthesized,
                ] {
                    ui.selectable_value(
                        &mut state.catalog_source,
                        Some(source),
                        cx.tr.t(layer_key(source)),
                    );
                }
            });
        let providers: std::collections::BTreeMap<_, _> = catalog
            .models
            .iter()
            .flat_map(|m| &m.sources)
            .filter(|s| s.provider != "builtin")
            .map(|s| (s.provider.clone(), s.provider_name.clone()))
            .collect();
        Dropdown::new("catalog-provider")
            .width(150.0)
            .selected_text(
                state
                    .catalog_provider
                    .as_ref()
                    .and_then(|p| providers.get(p))
                    .cloned()
                    .unwrap_or_else(|| cx.tr.t("models.catalog.all_providers")),
            )
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut state.catalog_provider,
                    None,
                    cx.tr.t("models.catalog.all_providers"),
                );
                for (id, name) in &providers {
                    ui.selectable_value(&mut state.catalog_provider, Some(id.clone()), name);
                }
            });
        ui.menu_button(cx.tr.t("models.catalog.namespaces"), |ui| {
            for (provider, slug) in &catalog.settings.provider_slugs {
                ui.horizontal(|ui| {
                    ui.label(providers.get(provider).unwrap_or(provider));
                    let draft = state
                        .catalog_slugs
                        .entry(provider.clone())
                        .or_insert_with(|| slug.clone());
                    ui.add(egui::TextEdit::singleline(draft).desired_width(150.0));
                });
            }
            if ui
                .add_enabled(
                    state.catalog_save.is_none(),
                    egui::Button::new(cx.tr.t("models.catalog.save_namespaces")),
                )
                .clicked()
            {
                settings.provider_slugs.extend(state.catalog_slugs.clone());
                save = true;
                ui.close();
            }
        });
        if state.catalog_test.is_some() {
            ui.spinner();
            ui.label(cx.tr.t("models.catalog.testing"));
        }
    });
    if save {
        save_settings(state, cx, settings);
    }
    let failures: Vec<_> = catalog
        .sources
        .iter()
        .filter(|s| s.error.is_some())
        .collect();
    if !failures.is_empty() {
        ui.collapsing(
            cx.tr
                .f("models.catalog.source_errors", &[("n", &failures.len())]),
            |ui| {
                for source in failures {
                    ui.label(format!(
                        "{} · {}: {}{}",
                        source.provider,
                        cx.tr.t(layer_key(source.layer)),
                        source.error.as_deref().unwrap_or_default(),
                        if source.stale {
                            format!(" · {}", cx.tr.t("models.catalog.stale"))
                        } else {
                            String::new()
                        }
                    ))
                    .on_hover_text(&source.endpoint);
                }
            },
        );
    }
    if let Some(result) = &state.catalog_result {
        ui.label(format!(
            "{} · {} · {}",
            result.model,
            cx.tr.t(if result.passed {
                "models.catalog.passed"
            } else {
                "models.catalog.failed"
            }),
            result.detail
        ));
    }
    if let Some(error) = &state.catalog_error {
        ui.colored_label(cx.pal.red, error);
    }
}
