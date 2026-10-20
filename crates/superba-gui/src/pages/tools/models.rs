//! The models a tool is given: the default model and how many models are
//! written, on the page; which ones, in a dialog with search, the provider's
//! list source and an explicit fetch (no paged inline list).
use super::form::{self, Rows};
use super::{State, provider};
use crate::design::icons::Icon;
use crate::design::tokens::{size, space, text};
use crate::design::widgets::{Badge, Button, Dropdown, Text, dialog};
use crate::pages::{Action, Cx};
use egui::{Align, Layout, RichText, Ui};
use superba_core::harness::{HarnessId, HarnessSettings};
use superba_core::{ModelList, ModelSource, ModelVisibility};

/// Widest default-model field.
const FIELD_W: f32 = 260.0;
/// Tallest default-model menu (it scrolls beyond).
const MENU_H: f32 = 280.0;
/// Width of the chooser dialog.
const DIALOG_W: f32 = 520.0;
/// Height of the dialog around its list: title, caption, toolbar, footer and
/// padding (the list gets the rest of the window).
const DIALOG_CHROME: f32 = 236.0;
/// Shortest and tallest model list in the dialog.
const LIST_H: (f32, f32) = (120.0, 420.0);

/// A model a tool can be given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// Model id.
    pub id: String,
    /// Context window in tokens.
    pub context: Option<u64>,
}

/// The models of the provider's list a tool can be given, in list order, then
/// chosen ids the list no longer has (so they can still be unchecked).
pub fn candidates(list: Option<&ModelList>, chosen: &[String]) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = list
        .map(|l| {
            l.models
                .iter()
                .filter(|m| m.supported_in_api && m.visibility != ModelVisibility::None)
                .map(|m| Candidate {
                    id: m.slug.clone(),
                    context: m.context_window,
                })
                .collect()
        })
        .unwrap_or_default();
    for id in chosen {
        if !out.iter().any(|c| &c.id == id) {
            out.push(Candidate {
                id: id.clone(),
                context: None,
            });
        }
    }
    out
}

/// Whether `id` is written (no explicit choice = every model).
pub fn written(s: &HarnessSettings, id: &str) -> bool {
    s.models.is_empty() || s.models.iter().any(|m| m == id)
}

/// How many of `all` are written.
pub fn written_count(s: &HarnessSettings, all: &[Candidate]) -> usize {
    all.iter().filter(|c| written(s, &c.id)).count()
}

/// Check or uncheck `id`. The last written model cannot be unchecked, and a
/// default model that is unchecked moves to the first one still written.
/// Returns whether anything changed.
pub fn set_written(s: &mut HarnessSettings, all: &[Candidate], id: &str, on: bool) -> bool {
    if written(s, id) == on {
        return false;
    }
    if s.models.is_empty() {
        s.models = all.iter().map(|c| c.id.clone()).collect();
    }
    if on {
        s.models.push(id.to_owned());
        return true;
    }
    if s.models.len() <= 1 {
        return false;
    }
    s.models.retain(|m| m != id);
    if s.default_model.as_deref() == Some(id) {
        s.default_model = s.models.first().cloned();
    }
    true
}

/// The candidates whose id contains `query` (any case).
pub fn filter<'a>(all: &'a [Candidate], query: &str) -> Vec<&'a Candidate> {
    let q = query.trim().to_lowercase();
    all.iter()
        .filter(|c| q.is_empty() || c.id.to_lowercase().contains(&q))
        .collect()
}

/// The default model shown: the chosen one, else the first written.
pub fn default_model(s: &HarnessSettings, all: &[Candidate]) -> Option<String> {
    s.default_model
        .clone()
        .or_else(|| all.iter().find(|c| written(s, &c.id)).map(|c| c.id.clone()))
}

/// The section: default model, how many models are written (and the chooser),
/// and a failed model list with its retry.
pub fn section(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, h: HarnessId, s: &mut HarnessSettings) {
    form::section(ui, cx, &cx.tr.t("tools.models"), None);
    let Some(id) = provider(cx, s) else {
        ui.add_space(space::S);
        ui.add(egui::Label::new(Text::small(cx.tr.t("tools.choose_provider"), cx.pal)).wrap());
        return;
    };
    let all = candidates(state.models.get(&id), &s.models);
    let loading = state.model_load.loading();
    let mut rows = Rows::new();
    rows.row(ui, cx, &cx.tr.t("tools.default_model"), None, |ui| {
        let current = default_model(s, &all);
        let w = ui.available_width().min(FIELD_W);
        Dropdown::new(("tool-default-model", h))
            .label(cx.tr.t("tools.default_model"))
            .selected_text(RichText::new(current.as_deref().unwrap_or("—")).monospace())
            .fixed_width(w)
            .menu_height(MENU_H)
            .loading(loading && all.is_empty())
            .show_ui(ui, |ui| {
                let chosen: Vec<&Candidate> = all.iter().filter(|c| written(s, &c.id)).collect();
                for c in chosen {
                    if ui
                        .selectable_label(current.as_deref() == Some(c.id.as_str()), &c.id)
                        .clicked()
                    {
                        s.default_model = Some(c.id.clone());
                    }
                }
            });
    });
    rows.row(
        ui,
        cx,
        &cx.tr.t("tools.written_models"),
        Some(&cx.tr.t("tools.models_hint")),
        |ui| {
            if ui
                .add(Button::new(cx.tr.t("tools.choose_models")).small())
                .clicked()
            {
                state.search.clear();
                state.models_open = true;
            }
            let value = if all.is_empty() && loading {
                cx.tr.t("tools.loading")
            } else if s.models.is_empty() {
                cx.tr.f("tools.models_all", &[("n", &all.len())])
            } else {
                cx.tr.f(
                    "tools.models_some",
                    &[("n", &written_count(s, &all)), ("total", &all.len())],
                )
            };
            ui.label(Text::muted(value, cx.pal));
        },
    );
    let mut errors: Vec<String> = state
        .model_load
        .failure()
        .map(|f| f.text(cx))
        .into_iter()
        .collect();
    if let Some(list) = state.models.get(&id) {
        errors.extend(
            list.sources
                .iter()
                .filter_map(|s| s.error.as_ref())
                .chain(list.fallback_reason.as_ref())
                .map(|e| crate::pages::error_text(cx.tr, e)),
        );
    }
    if !errors.is_empty() {
        let mut pull = false;
        rows.row_with(
            ui,
            "tool-model-errors",
            |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(Text::body(cx.tr.t("tools.model_list"), cx.pal));
                for e in &errors {
                    ui.add(egui::Label::new(Text::small(e, cx.pal).color(cx.pal.red)).wrap());
                }
            },
            |ui| {
                pull = ui
                    .add(
                        Button::new(cx.tr.t("tools.pull"))
                            .icon(Icon::Download)
                            .small()
                            .loading(loading),
                    )
                    .clicked();
            },
        );
        if pull {
            state.pull_models(cx, id, true);
        }
    }
}

/// The chooser dialog of the selected tool, while open. Changes are part of
/// the tool's draft like every other setting (nothing is written here).
pub fn chooser(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    if !state.models_open {
        return;
    }
    let Some(h) = state.selected.harness() else {
        state.models_open = false;
        return;
    };
    let mut s = state.settings(h);
    let before = s.clone();
    let Some(id) = provider(cx, &s) else {
        state.models_open = false;
        return;
    };
    let all = candidates(state.models.get(&id), &s.models);
    let loading = state.model_load.loading();
    let screen = ui.ctx().content_rect();
    let width = DIALOG_W.min(screen.width() - 2.0 * space::XXL);
    let list_h = (screen.height() - 2.0 * space::XXL - DIALOG_CHROME).clamp(LIST_H.0, LIST_H.1);
    let mut close = false;
    let mut pull = false;
    let provider_name = cx
        .data
        .profile(&id)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| id.to_string());
    let response = dialog(
        ui.ctx(),
        egui::Id::unique("tool-models-dialog"),
        width,
        |ui| {
            ui.label(Text::title(cx.tr.t("tools.models_dialog"), cx.pal));
            ui.label(Text::small(
                format!(
                    "{} · {} · {}",
                    state.selected.name(),
                    provider_name,
                    cx.tr.f(
                        "tools.models_some",
                        &[("n", &written_count(&s, &all)), ("total", &all.len())]
                    )
                ),
                cx.pal,
            ));
            ui.add_space(space::XS);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::S;
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    pull = ui
                        .add(
                            Button::new(cx.tr.t("tools.pull"))
                                .icon(Icon::Download)
                                .small()
                                .loading(loading),
                        )
                        .clicked();
                    if let Some(p) = cx.data.profile(&id) {
                        source(ui, cx, p, out);
                    }
                    let r = ui.add_sized(
                        [ui.available_width(), size::CONTROL_SM],
                        egui::TextEdit::singleline(&mut state.search)
                            .hint_text(cx.tr.t("tools.search"))
                            .align(egui::Align2::LEFT_CENTER),
                    );
                    r.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::Role::TextInput,
                            true,
                            cx.tr.t("tools.search"),
                        )
                    });
                });
            });
            if let Some(f) = state.model_load.failure() {
                ui.add(egui::Label::new(Text::small(f.text(cx), cx.pal).color(cx.pal.red)).wrap());
            }
            form::hairline(ui, cx.pal.border);
            let shown = filter(&all, &state.search);
            egui::ScrollArea::vertical()
                .id_salt("tool-models-list")
                .max_height(list_h)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    if shown.is_empty() {
                        ui.add_space(space::S);
                        ui.label(Text::small(
                            cx.tr.t(if loading {
                                "tools.loading"
                            } else {
                                "tools.no_models"
                            }),
                            cx.pal,
                        ));
                    }
                    let default = default_model(&s, &all);
                    for c in shown {
                        ui.allocate_ui_with_layout(
                            egui::vec2(ui.available_width(), size::ROW),
                            Layout::left_to_right(Align::Center),
                            |ui| {
                                let mut on = written(&s, &c.id);
                                if ui
                                    .checkbox(&mut on, RichText::new(&c.id).monospace())
                                    .changed()
                                {
                                    set_written(&mut s, &all, &c.id, on);
                                }
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if let Some(w) = c.context {
                                        ui.label(
                                            RichText::new(form::context_label(w))
                                                .font(crate::design::fonts::font(
                                                    text::SMALL,
                                                    crate::design::fonts::Weight::Regular,
                                                ))
                                                .color(cx.pal.text_muted),
                                        );
                                    }
                                    if default.as_deref() == Some(c.id.as_str()) {
                                        ui.add(Badge::new(cx.tr.t("tools.default_badge")));
                                    }
                                });
                            },
                        );
                    }
                });
            form::hairline(ui, cx.pal.border);
            ui.horizontal(|ui| {
                if ui
                    .add(
                        Button::ghost(cx.tr.t("tools.models_select_all"))
                            .small()
                            .enabled(!s.models.is_empty()),
                    )
                    .clicked()
                {
                    s.models.clear();
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    close = ui
                        .add(Button::primary(cx.tr.t("tools.done")).small())
                        .clicked();
                });
            });
        },
    );
    if pull {
        state.pull_models(cx, id, true);
    }
    if close || response.should_close() {
        state.models_open = false;
    }
    if s != before {
        state.edit(h, s);
    }
}

/// Which of the provider's model lists to read (a provider setting, saved at once).
fn source(ui: &mut Ui, cx: &Cx<'_>, p: &superba_core::Profile, out: &mut Vec<Action>) {
    let mut source = p.model_source.unwrap_or(ModelSource::Auto);
    let old = source;
    let label = |source| {
        cx.tr.t(match source {
            ModelSource::Auto => "models.list_source.auto",
            ModelSource::Codex => "models.list_source.codex",
            ModelSource::Standard => "models.list_source.standard",
        })
    };
    Dropdown::new("tool-source")
        .label(cx.tr.t("models.list_source.label"))
        .selected_text(label(source))
        .show_ui(ui, |ui| {
            for value in [ModelSource::Auto, ModelSource::Codex, ModelSource::Standard] {
                if ui.selectable_label(source == value, label(value)).clicked() {
                    source = value;
                }
            }
        });
    if source != old {
        let mut p = p.clone();
        p.model_source = Some(source);
        out.push(Action::SaveProfile {
            profile: Box::new(p),
            key: None,
        });
    }
}
