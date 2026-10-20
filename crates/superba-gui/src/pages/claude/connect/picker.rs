//! The /model 列表 section — every model of the provider in one table, ticked
//! when Claude Code's `/model` lists it (`modelPicker.options`), with ids
//! added by hand and the text of each row edited in a dialog — and the
//! closed 高级 group (hiding Claude Code's own rows, the vendor's recommended
//! variables, the configuration folder).

use egui::{Align, Id, Layout, RichText, TextEdit, Ui, vec2};
use superba_core::claude::anthropic_preset;
use superba_core::{ClaudeModelRow, ClaudeModels, ClaudeSlot, ModelListKind};

use super::{Models, State, endpoint_of, help_line, section_head, setting_row};
use crate::design::elide::Shape;
use crate::design::fonts::{Weight, mono_font};
use crate::design::icons::Icon;
use crate::design::tokens::{size, space, text};
use crate::design::widgets::dialog as modal;
use crate::design::widgets::value_chip::ValueChip;
use crate::design::widgets::{
    Badge, Button, Dropdown, FormRow, IconButton, Table, Text, Toggle, dialog_body,
};
use crate::pages::Cx;

/// The name of a model list ("Codex 列表").
pub fn list_name(cx: &Cx<'_>, kind: ModelListKind) -> String {
    cx.tr.t(match kind {
        ModelListKind::Codex => "claude.models.list_codex",
        ModelListKind::Standard => "claude.models.list_standard",
    })
}

/// A row of the `/model` table.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// The model.
    pub row: ClaudeModelRow,
    /// Its place in `/model` (0-based), when it is listed there.
    pub position: Option<usize>,
    /// Added by hand (can be removed).
    pub removable: bool,
}

/// The rows of the table for the search text `query` (id or display name),
/// in list order.
pub fn lines(rows: &[ClaudeModelRow], models: &ClaudeModels, query: &str) -> Vec<Line> {
    let q = query.trim().to_lowercase();
    rows.iter()
        .filter(|r| {
            q.is_empty()
                || r.id.to_lowercase().contains(&q)
                || r.display_name
                    .as_deref()
                    .is_some_and(|n| n.to_lowercase().contains(&q))
        })
        .map(|r| Line {
            position: models.picker.iter().position(|p| p.model == r.id),
            removable: models.extra_models.contains(&r.id),
            row: r.clone(),
        })
        .collect()
}

/// List `id` in `/model` (at the end) or take it out.
pub fn set_shown(models: &mut ClaudeModels, id: &str, on: bool) {
    if on {
        models.add_to_picker(id);
    } else {
        models.picker.retain(|p| p.model != id);
    }
}

/// Add a model id by hand (a provider without a list, an id the lists miss)
/// and list it in `/model`; false when `id` is empty.
pub fn add_manual(models: &mut ClaudeModels, rows: &[ClaudeModelRow], id: &str) -> bool {
    let id = id.trim();
    if id.is_empty() {
        return false;
    }
    if !rows.iter().any(|r| r.id == id) && !models.extra_models.iter().any(|m| m == id) {
        models.extra_models.push(id.to_owned());
    }
    models.add_to_picker(id);
    true
}

/// Remove an id added by hand (and its `/model` row).
pub fn remove_manual(models: &mut ClaudeModels, id: &str) {
    models.extra_models.retain(|m| m != id);
    models.picker.retain(|p| p.model != id);
}

/// Move the `/model` row of `id` one place up (`up`) or down; false when it
/// cannot move.
pub fn move_row(models: &mut ClaudeModels, id: &str, up: bool) -> bool {
    let Some(i) = models.picker.iter().position(|p| p.model == id) else {
        return false;
    };
    let j = if up {
        match i.checked_sub(1) {
            Some(j) => j,
            None => return false,
        }
    } else if i + 1 < models.picker.len() {
        i + 1
    } else {
        return false;
    };
    models.picker.swap(i, j);
    true
}

/// A context window in tokens, short: `400K`, `1M`, `1.5M`.
pub fn context_text(n: u64) -> String {
    if n >= 1_000_000 {
        let m = n as f64 / 1e6;
        if (m - m.round()).abs() < 0.05 {
            format!("{}M", m.round())
        } else {
            format!("{m:.1}M")
        }
    } else if n >= 1_000 {
        format!("{}K", (n as f64 / 1e3).round())
    } else {
        n.to_string()
    }
}

/// Column weights and minimum widths of the `/model` table: shown, model,
/// source, context, actions (each minimum includes the 12 point gap after
/// it).
pub const COLUMNS: ([f32; 5], [f32; 5]) =
    ([0.0, 1.0, 0.5, 0.0, 0.0], [56.0, 168.0, 132.0, 88.0, 104.0]);

/// Width of the search and the "add model id" fields.
const FIELD_W: f32 = 200.0;

pub(super) fn section(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, m: &Models<'_>) {
    let shown = m.models.picker.len();
    section_head(
        ui,
        cx,
        &cx.tr.t("claude.picker.title"),
        Some(&cx.tr.f("claude.picker.caption", &[("n", &shown)])),
        |_| {},
    );
    toolbar(ui, cx, state, m);
    let lines = lines(&m.rows, &m.models, &state.connect.search);
    if lines.is_empty() {
        ui.add_space(space::S);
        let text = if m.rows.is_empty() {
            format!(
                "{} {}",
                cx.tr.t("claude.models.empty_title"),
                cx.tr.t("claude.models.empty_body")
            )
        } else {
            cx.tr.t("claude.picker.no_match")
        };
        ui.add(egui::Label::new(Text::small(text, cx.pal)).wrap());
        return;
    }
    table(ui, cx, state, m, &lines);
}

/// Search on the left, "add model id" on the right.
fn toolbar(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, m: &Models<'_>) {
    let mut add = false;
    FormRow::unlabeled()
        .id_salt("claude-picker-toolbar")
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = space::S;
            let w = FIELD_W.min((ui.available_width() - space::S) / 2.0 - 60.0);
            ui.add(
                TextEdit::singleline(&mut state.connect.search)
                    .id(egui::Id::unique("claude-models-search"))
                    .hint_text(cx.tr.t("claude.models.search"))
                    .desired_width(w)
                    .margin(vec2(10.0, 6.0)),
            )
            .accessible_name(cx.tr.t("claude.models.search"));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let id = state.connect.manual.trim().to_owned();
                let button = ui.add(
                    Button::new(cx.tr.t("claude.models.add"))
                        .icon(Icon::Plus)
                        .small()
                        .enabled(!id.is_empty()),
                );
                let field = ui
                    .add(
                        TextEdit::singleline(&mut state.connect.manual)
                            .hint_text(cx.tr.t("claude.models.add_hint"))
                            .desired_width(w)
                            .font(mono_font(text::SMALL, Weight::Regular))
                            .margin(vec2(10.0, 6.0)),
                    )
                    .accessible_name(cx.tr.t("claude.models.add_field"));
                let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                add = (button.clicked() || enter) && !id.is_empty();
            });
        });
    if add {
        let id = state.connect.manual.trim().to_owned();
        let rows = m.rows.clone();
        state.connect.edit_models(m.view, &m.provider, |x| {
            add_manual(x, &rows, &id);
        });
        state.connect.manual.clear();
    }
}

fn table(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, m: &Models<'_>, lines: &[Line]) {
    let (weights, mins) = COLUMNS;
    let table = Table::new(&weights).min_widths(&mins).right_aligned(&[3]);
    table.header(
        ui,
        &[
            cx.tr.t("claude.picker.col_show"),
            cx.tr.t("claude.picker.col_model"),
            cx.tr.t("claude.picker.col_source"),
            cx.tr.t("claude.picker.col_context"),
            cx.tr.t("claude.picker.col_actions"),
        ],
    );
    let edit_label = cx.tr.t("claude.picker.edit");
    let edit_w = Button::new(edit_label.clone()).small().desired_size(ui).x;
    let mut toggle: Option<(String, bool)> = None;
    let mut edit: Option<String> = None;
    let mut remove: Option<String> = None;
    for (i, line) in lines.iter().enumerate() {
        let r = &line.row;
        table.row(
            ui,
            size::ROW_TABLE2,
            i + 1 == lines.len(),
            |ui, col| match col {
                0 => {
                    // egui's box is drawn with the inactive border, too faint on
                    // white for a column of them: the strong border here.
                    let strong = egui::Stroke::new(1.0, cx.pal.border_strong);
                    ui.visuals_mut().widgets.inactive.bg_stroke = strong;
                    let mut on = line.position.is_some();
                    let resp = ui
                        .add_enabled(r.usable, egui::Checkbox::without_text(&mut on))
                        .accessible_name(cx.tr.f("claude.picker.show_named", &[("model", &r.id)]))
                        .on_disabled_hover_text(cx.tr.t("claude.models.codex_only_tip"));
                    if resp.changed() {
                        toggle = Some((r.id.clone(), on));
                    }
                }
                1 => {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let color = if r.usable {
                            cx.pal.text
                        } else {
                            cx.pal.text_disabled
                        };
                        ui.add(
                            egui::Label::new(
                                RichText::new(&r.id)
                                    .font(mono_font(text::SMALL, Weight::Medium))
                                    .color(color),
                            )
                            .truncate(),
                        )
                        .on_hover_text(model_tip(cx, r));
                        if let Some(n) = &r.display_name {
                            ui.add(egui::Label::new(Text::caption(n, cx.pal)).truncate());
                        }
                    });
                }
                2 => {
                    if !r.usable {
                        ui.add(Badge::new(cx.tr.t("claude.models.codex_only")).outline())
                            .on_hover_text(cx.tr.t("claude.models.codex_only_tip"));
                    } else {
                        let text = if r.manual {
                            cx.tr.t("claude.models.manual")
                        } else {
                            r.lists
                                .iter()
                                .map(|k| list_name(cx, *k))
                                .collect::<Vec<_>>()
                                .join(" · ")
                        };
                        ui.add(egui::Label::new(Text::small(text, cx.pal)).truncate());
                    }
                }
                3 => {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add_space(Table::RIGHT_INSET);
                        match r.context_window {
                            Some(n) => {
                                let all = crate::pages::shared::format::group_thousands(n);
                                ui.label(Text::small(context_text(n), cx.pal))
                                    .on_hover_text(
                                        cx.tr.f("claude.detail.context", &[("n", &all)]),
                                    );
                            }
                            None => {
                                ui.label(Text::caption("—", cx.pal));
                            }
                        }
                    });
                }
                _ => {
                    if line.position.is_some() {
                        let resp = ui
                            .add(Button::new(edit_label.clone()).small())
                            .accessible_name(
                                cx.tr.f("claude.picker.edit_named", &[("model", &r.id)]),
                            );
                        if resp.clicked() {
                            edit = Some(r.id.clone());
                        }
                    } else {
                        ui.add_space(edit_w);
                    }
                    if line.removable {
                        let name = cx.tr.f("claude.picker.remove_named", &[("model", &r.id)]);
                        if ui
                            .add(IconButton::new(Icon::Close, name).size(size::CONTROL_SM))
                            .clicked()
                        {
                            remove = Some(r.id.clone());
                        }
                    }
                }
            },
        );
    }
    if let Some((id, on)) = toggle {
        state
            .connect
            .edit_models(m.view, &m.provider, |x| set_shown(x, &id, on));
    }
    if let Some(id) = remove {
        state
            .connect
            .edit_models(m.view, &m.provider, |x| remove_manual(x, &id));
    }
    if edit.is_some() {
        state.connect.picker_edit = edit;
    }
}

/// The hover text of a model: its id and the reasoning levels its Codex
/// metadata names.
fn model_tip(cx: &Cx<'_>, r: &ClaudeModelRow) -> String {
    if r.reasoning_levels.is_empty() {
        return r.id.clone();
    }
    let levels = cx.tr.f(
        "claude.detail.levels",
        &[("levels", &r.reasoning_levels.join(", "))],
    );
    format!("{}\n{levels}", r.id)
}

/// The dialog of a `/model` row: its title, description, the Claude model
/// it behaves as, and its place in the list.
pub(super) fn dialog(ctx: &egui::Context, cx: &Cx<'_>, state: &mut State, m: &Models<'_>) {
    let Some(id) = state.connect.picker_edit.clone() else {
        return;
    };
    let mut next = m.models.clone();
    let Some(pos) = next.picker.iter().position(|p| p.model == id) else {
        state.connect.picker_edit = None;
        return;
    };
    let n = next.picker.len();
    let in_slot = !next.slots_of(&id).is_empty();
    let mut close = false;
    let mut moved: Option<bool> = None;
    let r = modal(ctx, Id::unique("claude-picker-dialog"), 480.0, |ui| {
        ui.label(Text::title(cx.tr.t("claude.picker_dialog.title"), cx.pal));
        ui.add(egui::Label::new(Text::mono(&id, cx.pal)).truncate())
            .on_hover_text(&id);
        dialog_body(ui, |ui| {
            if in_slot {
                ui.add(
                    egui::Label::new(Text::caption(cx.tr.t("claude.picker.slot_row"), cx.pal))
                        .wrap(),
                );
            }
            let labels = [
                cx.tr.t("claude.picker_dialog.label"),
                cx.tr.t("claude.picker_dialog.description"),
                cx.tr.t("claude.picker.behaves_as"),
                cx.tr.t("claude.picker_dialog.position"),
            ];
            let label_w = FormRow::label_width_of(ui, labels.iter().map(String::as_str));
            let p = &mut next.picker[pos];
            let mut label = p.label.clone().unwrap_or_default();
            let mut desc = p.description.clone().unwrap_or_default();
            FormRow::new(labels[0].clone())
                .label_width(label_w)
                .show(ui, |ui| {
                    ui.add(
                        TextEdit::singleline(&mut label)
                            .hint_text(cx.tr.t("claude.picker.label_hint"))
                            .desired_width(f32::INFINITY),
                    );
                });
            FormRow::new(labels[1].clone())
                .label_width(label_w)
                .show(ui, |ui| {
                    ui.add(
                        TextEdit::singleline(&mut desc)
                            .hint_text(cx.tr.t("claude.picker.description_hint"))
                            .desired_width(f32::INFINITY),
                    );
                });
            p.label = (!label.trim().is_empty()).then_some(label);
            p.description = (!desc.trim().is_empty()).then_some(desc);
            FormRow::new(labels[2].clone())
                .label_width(label_w)
                .show(ui, |ui| {
                    let current = p.behaves_as.clone().unwrap_or_default();
                    let none = cx.tr.t("claude.picker.behaves_none");
                    let mut pick: Option<Option<String>> = None;
                    Dropdown::new("claude-picker-behaves")
                        .selected_text(if current.is_empty() {
                            none.clone()
                        } else {
                            current.clone()
                        })
                        .width(220.0)
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(current.is_empty(), &none).clicked() {
                                pick = Some(None);
                            }
                            for slot in ClaudeSlot::ALL {
                                if let Some(k) = slot.known_model()
                                    && ui.selectable_label(current == k, k).clicked()
                                {
                                    pick = Some(Some(k.to_owned()));
                                }
                            }
                        })
                        .response
                        .on_hover_text(cx.tr.t("claude.picker.behaves_as_tip"));
                    if let Some(v) = pick {
                        p.behaves_as = v;
                    }
                });
            FormRow::new(labels[3].clone())
                .label_width(label_w)
                .show(ui, |ui| {
                    ui.label(Text::small(
                        cx.tr.f(
                            "claude.picker_dialog.position_of",
                            &[("i", &(pos + 1)), ("n", &n)],
                        ),
                        cx.pal,
                    ));
                    if ui
                        .add(
                            IconButton::new(Icon::ArrowUp, cx.tr.t("claude.picker.up"))
                                .enabled(pos > 0),
                        )
                        .clicked()
                    {
                        moved = Some(true);
                    }
                    if ui
                        .add(
                            IconButton::new(Icon::ArrowDown, cx.tr.t("claude.picker.down"))
                                .enabled(pos + 1 < n),
                        )
                        .clicked()
                    {
                        moved = Some(false);
                    }
                });
        });
        ui.add_space(space::XS);
        ui.horizontal(|ui| {
            if ui
                .add(Button::primary(cx.tr.t("claude.slot_dialog.done")))
                .clicked()
            {
                close = true;
            }
        });
    });
    if let Some(up) = moved {
        move_row(&mut next, &id, up);
    }
    if r.should_close() {
        close = true;
    }
    if next != m.models {
        state
            .connect
            .edit_models(m.view, &m.provider, |x| *x = next);
    }
    if close {
        state.connect.picker_edit = None;
    }
}

/// The closed 高级 group.
pub(super) fn advanced(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, m: &Models<'_>) {
    egui::CollapsingHeader::new(Text::section(cx.tr.t("claude.advanced.title"), cx.pal))
        .id_salt("claude-advanced")
        .default_open(false)
        .show(ui, |ui| {
            ui.add_space(space::XS);
            let hints = endpoint_of(cx, &m.provider)
                .and_then(|e| e.preset)
                .and_then(|p| anthropic_preset(&p))
                .filter(|p| !p.env_hints.is_empty());
            let mut replace = m.models.replace_builtin;
            setting_row(
                ui,
                cx,
                &cx.tr.t("claude.picker.replace"),
                |ui| help_line(ui, cx, None, &cx.tr.t("claude.picker.replace_tip")),
                false,
                size::CONTROL_LG,
                |ui| {
                    ui.add(Toggle::new(&mut replace, cx.tr.t("claude.picker.replace")));
                },
            );
            if replace != m.models.replace_builtin {
                state
                    .connect
                    .edit_models(m.view, &m.provider, |x| x.replace_builtin = replace);
            }
            if let Some(p) = &hints {
                setting_row(
                    ui,
                    cx,
                    &cx.tr.t("claude.hints.title"),
                    |ui| {
                        help_line(
                            ui,
                            cx,
                            None,
                            &cx.tr.f("claude.hints.body", &[("vendor", &p.vendor)]),
                        );
                        for (k, v) in p.env_hints {
                            ui.add(
                                egui::Label::new(Text::mono_muted(format!("{k}={v}"), cx.pal))
                                    .wrap(),
                            );
                        }
                    },
                    false,
                    0.0,
                    |_| {},
                );
            }
            let dir = superba_core::paths::display_path(&m.view.config_dir);
            setting_row(
                ui,
                cx,
                &cx.tr.t("claude.advanced.config_dir"),
                |ui| {
                    help_line(
                        ui,
                        cx,
                        None,
                        &cx.tr.t(if m.view.config_dir_from_env {
                            "claude.advanced.config_dir_env"
                        } else {
                            "claude.advanced.config_dir_default"
                        }),
                    )
                },
                true,
                super::connection::VALUE_W,
                |ui| {
                    let room = ui.available_width();
                    ui.add(
                        ValueChip::new(&dir, Shape::Path)
                            .plain()
                            .size(text::SMALL)
                            .max_width(room)
                            .tooltip(dir.clone()),
                    );
                },
            );
        });
}
