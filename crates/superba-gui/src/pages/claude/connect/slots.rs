//! The 模型槽位 section: one table row per slot — Opus, Sonnet, Haiku, Fable
//! and the custom row of `/model`, then the default, subagent and background
//! models — with a dropdown of the fetched models and, for the `/model`
//! slots, the capabilities written for the chosen model as chips; their row
//! text and capabilities are edited in a dialog.

use std::collections::BTreeSet;

use egui::{Id, RichText, TextEdit, Ui, vec2};
use superba_core::claude::capabilities_text;
use superba_core::{ClaudeCapability, ClaudeModelRow, ClaudeModels, ClaudeSlot, SlotModel};

use super::{Models, State, section_head};
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::icons::Icon;
use crate::design::tokens::{size, space, text};
use crate::design::widgets::dialog as modal;
use crate::design::widgets::{Badge, Button, Dropdown, FormRow, Status, Table, Text, dialog_body};
use crate::pages::shared::loading;
use crate::pages::{Cx, error_text};

/// A row of the slot table: a `/model` slot or one of the models Claude Code
/// uses by role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SlotKey {
    /// A `/model` slot (`ANTHROPIC_DEFAULT_*_MODEL`, the custom row).
    Slot(ClaudeSlot),
    /// The default model (`model`).
    Main,
    /// The subagent model (`CLAUDE_CODE_SUBAGENT_MODEL`).
    Subagent,
    /// The background model (`ANTHROPIC_SMALL_FAST_MODEL`).
    Background,
}

impl SlotKey {
    /// Every row, in table order.
    pub const ALL: [SlotKey; 8] = [
        SlotKey::Slot(ClaudeSlot::Opus),
        SlotKey::Slot(ClaudeSlot::Sonnet),
        SlotKey::Slot(ClaudeSlot::Haiku),
        SlotKey::Slot(ClaudeSlot::Fable),
        SlotKey::Slot(ClaudeSlot::Custom),
        SlotKey::Main,
        SlotKey::Subagent,
        SlotKey::Background,
    ];

    /// i18n key of the row's name.
    pub fn label_key(self) -> &'static str {
        match self {
            SlotKey::Slot(ClaudeSlot::Opus) => "claude.slot.opus",
            SlotKey::Slot(ClaudeSlot::Sonnet) => "claude.slot.sonnet",
            SlotKey::Slot(ClaudeSlot::Haiku) => "claude.slot.haiku",
            SlotKey::Slot(ClaudeSlot::Fable) => "claude.slot.fable",
            SlotKey::Slot(ClaudeSlot::Custom) => "claude.slot.custom",
            SlotKey::Main => "claude.role.main",
            SlotKey::Subagent => "claude.role.subagent",
            SlotKey::Background => "claude.role.background",
        }
    }

    /// i18n key of the row's explanation (shown on hover).
    pub fn tip_key(self) -> &'static str {
        match self {
            SlotKey::Slot(_) => "claude.slots.slot_tip",
            SlotKey::Main => "claude.role.main_tip",
            SlotKey::Subagent => "claude.role.subagent_tip",
            SlotKey::Background => "claude.role.background_tip",
        }
    }

    /// The setting the row writes (an environment variable, or `model`).
    pub fn setting(self) -> &'static str {
        match self {
            SlotKey::Slot(s) => s.env_model(),
            SlotKey::Main => "model",
            SlotKey::Subagent => "CLAUDE_CODE_SUBAGENT_MODEL",
            SlotKey::Background => "ANTHROPIC_SMALL_FAST_MODEL",
        }
    }

    /// Capabilities are written for the row's model (`/model` slots only).
    pub fn has_capabilities(self) -> bool {
        matches!(self, SlotKey::Slot(_))
    }

    /// The model of the row in `m`.
    pub fn model(self, m: &ClaudeModels) -> Option<&str> {
        match self {
            SlotKey::Slot(s) => m.slots.get(&s).map(|s| s.model.as_str()),
            SlotKey::Main => m.main.as_deref(),
            SlotKey::Subagent => m.subagent.as_deref(),
            SlotKey::Background => m.small_fast.as_deref(),
        }
    }

    /// Put `model` into the row (`None` clears it). A slot that gets another
    /// model loses the row text written for the one before.
    pub fn set(self, m: &mut ClaudeModels, model: Option<String>) {
        let model = model.filter(|id| !id.trim().is_empty());
        match self {
            SlotKey::Slot(s) => match model {
                Some(id) if m.slots.get(&s).is_some_and(|x| x.model == id) => {}
                Some(id) => {
                    m.slots.insert(s, SlotModel::new(id));
                }
                None => {
                    m.slots.remove(&s);
                }
            },
            SlotKey::Main => m.main = model,
            SlotKey::Subagent => m.subagent = model,
            SlotKey::Background => m.small_fast = model,
        }
    }
}

/// An entry of a slot's dropdown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    /// Model id.
    pub id: String,
    /// The endpoint serves it (an id only the Codex list has is shown
    /// disabled).
    pub usable: bool,
    /// Display name from a list.
    pub display: Option<String>,
}

/// The entries of a slot's dropdown: the current model first when no list
/// has it (set by hand or before the list loaded), then the models the
/// endpoint serves in list order, then the "Codex only" ones.
pub fn choices(rows: &[ClaudeModelRow], current: Option<&str>) -> Vec<Choice> {
    let mut out = Vec::new();
    if let Some(id) = current
        && !rows.iter().any(|r| r.id == id)
    {
        out.push(Choice {
            id: id.to_owned(),
            usable: true,
            display: None,
        });
    }
    let choice = |r: &ClaudeModelRow| Choice {
        id: r.id.clone(),
        usable: r.usable,
        display: r.display_name.clone(),
    };
    out.extend(rows.iter().filter(|r| r.usable).map(choice));
    out.extend(rows.iter().filter(|r| !r.usable).map(choice));
    out
}

/// The capabilities written for `model`: the user's choice (`true`), else
/// the suggestion of its row (from its Codex metadata or its family).
pub fn capabilities(
    models: &ClaudeModels,
    rows: &[ClaudeModelRow],
    model: &str,
) -> (BTreeSet<ClaudeCapability>, bool) {
    if let Some((_, c)) = models
        .capabilities
        .iter()
        .find(|(m, _)| m.eq_ignore_ascii_case(model))
    {
        return (c.clone(), true);
    }
    match rows.iter().find(|r| r.id == model) {
        Some(r) => (r.suggested.clone(), false),
        None => (models.capabilities_of(model, None), false),
    }
}

/// How many chips of `widths` fit in `room` with `gap` between them: all of
/// them, or as many as leave room for a "+N" chip `more(n)` wide after them.
pub fn chips_that_fit(widths: &[f32], room: f32, gap: f32, more: impl Fn(usize) -> f32) -> usize {
    let total: f32 = widths.iter().sum::<f32>() + gap * widths.len().saturating_sub(1) as f32;
    if total <= room {
        return widths.len();
    }
    let mut used = 0.0;
    let mut n = 0;
    for (i, w) in widths.iter().enumerate() {
        let next = used + if i > 0 { gap } else { 0.0 } + w;
        let rest = widths.len() - (i + 1);
        if next + gap + more(rest) > room {
            break;
        }
        used = next;
        n = i + 1;
    }
    n
}

/// Column weights and minimum widths of the slot table: name, model,
/// capabilities, actions (each minimum includes the 12 point gap after it).
pub const COLUMNS: ([f32; 4], [f32; 4]) = ([0.3, 0.6, 1.0, 0.0], [128.0, 200.0, 116.0, 76.0]);

/// Height of a dropdown's open menu.
const MENU_H: f32 = 320.0;

pub(super) fn section(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, m: &Models<'_>) {
    let list = state.connect.models.get(&m.provider);
    let sources = list.map(|v| v.sources.clone()).unwrap_or_default();
    let has_list = list.is_some();
    let mut pull = false;
    section_head(
        ui,
        cx,
        &cx.tr.t("claude.slots.title"),
        Some(&cx.tr.t("claude.slots.caption")),
        |ui| {
            ui.spacing_mut().item_spacing.x = space::S;
            pull = ui
                .add(
                    Button::new(cx.tr.t("claude.models.pull"))
                        .icon(Icon::Download)
                        .small()
                        .loading(m.loading)
                        .enabled(!m.loading),
                )
                .on_hover_text(cx.tr.t("claude.models.pull_tip"))
                .clicked();
            for s in sources.iter().rev() {
                let name = super::picker::list_name(cx, s.kind);
                match &s.error {
                    None => {
                        ui.add(Badge::new(format!("{name} {}", s.count)).outline())
                            .on_hover_text(s.path.clone().unwrap_or_default());
                    }
                    Some(e) => {
                        ui.add(Badge::new(name).status(Status::Warn))
                            .on_hover_text(error_text(cx.tr, e));
                    }
                }
            }
        },
    );
    if pull {
        state.connect.load_models(cx, &m.provider, true);
    }
    load_state(ui, cx, state, m, has_list);
    table(ui, cx, state, m);
}

/// The list's loading or failure, above the table.
fn load_state(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, m: &Models<'_>, has_list: bool) {
    let name = cx
        .data
        .profile(&m.provider)
        .map(|p| super::provider_label(cx, p))
        .unwrap_or_else(|| m.provider.to_string());
    let failure = if state.connect.load.is_chosen(&m.provider) {
        state.connect.load.failure().map(|f| f.text(cx))
    } else {
        None
    };
    match (&failure, has_list) {
        (Some(f), false) => {
            // What failed and why on the left, Retry on the right.
            let mut retry = false;
            super::setting_row(
                ui,
                cx,
                &cx.tr.f("claude.models.load_failed", &[("provider", &name)]),
                |ui| super::help_line(ui, cx, Some(Status::Warn), f),
                true,
                size::CONTROL_LG * 3.0,
                |ui| {
                    retry = ui
                        .add(
                            Button::new(cx.tr.t("action.retry"))
                                .icon(Icon::Refresh)
                                .small(),
                        )
                        .clicked();
                },
            );
            if retry {
                state.connect.load_models(cx, &m.provider, true);
            }
        }
        (Some(f), true) => {
            super::help_line(
                ui,
                cx,
                Some(Status::Warn),
                &cx.tr.f("claude.models.refresh_failed", &[("error", f)]),
            );
        }
        (None, false) if m.loading => {
            loading::spinner_small(
                ui,
                cx,
                &cx.tr.f("claude.models.loading", &[("provider", &name)]),
            );
        }
        _ => {}
    }
}

/// Widths of capability chips named `names` (a [`Badge`]: text and padding).
fn chip_widths(ui: &Ui, names: &[String]) -> Vec<f32> {
    let f = font(text::CAPTION, Weight::Medium);
    names
        .iter()
        .map(|n| {
            ui.painter()
                .layout_no_wrap(n.clone(), f.clone(), egui::Color32::PLACEHOLDER)
                .size()
                .x
                + 16.0
        })
        .collect()
}

fn table(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, m: &Models<'_>) {
    let (weights, mins) = COLUMNS;
    let table = Table::new(&weights).min_widths(&mins);
    table.header(
        ui,
        &[
            cx.tr.t("claude.slots.col_slot"),
            cx.tr.t("claude.slots.col_model"),
            cx.tr.t("claude.slots.col_caps"),
            cx.tr.t("claude.slots.col_actions"),
        ],
    );
    let mut set: Option<(SlotKey, Option<String>)> = None;
    let mut edit: Option<SlotKey> = None;
    let unset = cx.tr.t("claude.summary.unset");
    for (i, key) in SlotKey::ALL.into_iter().enumerate() {
        let label = cx.tr.t(key.label_key());
        let current = key.model(&m.models).map(str::to_owned);
        let last = i + 1 == SlotKey::ALL.len();
        table.row(ui, size::ROW_TABLE2, last, |ui, col| match col {
            0 => {
                // The setting it writes is in the tooltip: in the cell it
                // would be cut at every width.
                let tip = match key {
                    SlotKey::Slot(_) => format!("{}\n{}", cx.tr.t(key.tip_key()), key.setting()),
                    // The roles' explanations start with their setting.
                    _ => cx.tr.t(key.tip_key()),
                };
                ui.add(egui::Label::new(Text::body(&label, cx.pal)).truncate())
                    .on_hover_text(tip);
            }
            1 => {
                if let Some(v) = model_dropdown(ui, cx, m, key, &label, current.as_deref(), &unset)
                {
                    set = Some((key, v));
                }
            }
            2 => caps_cell(ui, cx, m, key, current.as_deref()),
            _ => {
                // Row text and capabilities belong to a slot's model.
                if key.has_capabilities() && current.is_some() {
                    let r = ui
                        .add(Button::new(cx.tr.t("claude.slots.edit")).small())
                        .accessible_name(cx.tr.f("claude.slots.edit_named", &[("slot", &label)]));
                    if r.clicked() {
                        edit = Some(key);
                    }
                }
            }
        });
    }
    if let Some((key, model)) = set {
        state
            .connect
            .edit_models(m.view, &m.provider, |x| key.set(x, model));
    }
    if edit.is_some() {
        state.connect.slot_edit = edit;
    }
}

/// The model dropdown of a row; `Some` when a new value was picked.
fn model_dropdown(
    ui: &mut Ui,
    cx: &Cx<'_>,
    m: &Models<'_>,
    key: SlotKey,
    label: &str,
    current: Option<&str>,
    unset: &str,
) -> Option<Option<String>> {
    let options = choices(&m.rows, current);
    let selected = match current {
        Some(id) => RichText::new(id)
            .font(mono_font(text::SMALL, Weight::Regular))
            .color(cx.pal.text),
        None => RichText::new(unset)
            .font(font(text::SMALL, Weight::Regular))
            .color(cx.pal.text_subtle),
    };
    let mut pick = None;
    Dropdown::new(("claude-slot", key.setting()))
        .label(cx.tr.f("claude.slots.model_of", &[("slot", &label)]))
        .selected_text(selected)
        .fixed_width(ui.available_width())
        .loading(m.loading && options.is_empty())
        .menu_height(MENU_H)
        .show_ui(ui, |ui| {
            if ui.selectable_label(current.is_none(), unset).clicked() && current.is_some() {
                pick = Some(None);
            }
            for c in &options {
                let on = current == Some(c.id.as_str());
                let text = RichText::new(&c.id).font(mono_font(text::SMALL, Weight::Regular));
                if c.usable {
                    let r = ui.selectable_label(on, text);
                    let r = match &c.display {
                        Some(d) => r.on_hover_text(d),
                        None => r,
                    };
                    if r.clicked() && !on {
                        pick = Some(Some(c.id.clone()));
                    }
                } else {
                    ui.add_enabled(false, egui::Button::selectable(false, text))
                        .on_disabled_hover_text(cx.tr.t("claude.models.codex_only_tip"));
                }
            }
            if options.is_empty() {
                ui.label(Text::caption(cx.tr.t("claude.models.empty_body"), cx.pal));
            }
        });
    pick
}

/// The capabilities of the row's model as chips (filled: chosen by the user;
/// outlined: suggested), "+N" when they do not all fit.
fn caps_cell(ui: &mut Ui, cx: &Cx<'_>, m: &Models<'_>, key: SlotKey, model: Option<&str>) {
    let dash = |ui: &mut Ui, tip: String| {
        ui.label(Text::caption("—", cx.pal)).on_hover_text(tip);
    };
    if !key.has_capabilities() {
        dash(ui, cx.tr.t("claude.slots.caps_na"));
        return;
    }
    let Some(model) = model else {
        dash(ui, cx.tr.t("claude.slots.caps_unset"));
        return;
    };
    let (caps, own) = capabilities(&m.models, &m.rows, model);
    let list = capabilities_text(&caps);
    let tip = if own {
        cx.tr.f("claude.caps.written", &[("list", &list)])
    } else {
        cx.tr.f("claude.slots.caps_suggested", &[("list", &list)])
    };
    if caps.is_empty() {
        ui.label(Text::caption(cx.tr.t("claude.slots.caps_none"), cx.pal))
            .on_hover_text(tip);
        return;
    }
    let names: Vec<String> = ClaudeCapability::ALL
        .into_iter()
        .filter(|c| caps.contains(c))
        .map(|c| c.as_str().to_owned())
        .collect();
    let widths = chip_widths(ui, &names);
    let gap = ui.spacing().item_spacing.x;
    let more_w = |n: usize| chip_widths(ui, &[format!("+{n}")])[0];
    let shown = chips_that_fit(&widths, ui.available_width(), gap, more_w);
    let r = ui
        .horizontal(|ui| {
            for n in &names[..shown] {
                let b = Badge::new(n.clone());
                ui.add(if own { b } else { b.outline() });
            }
            if shown < names.len() {
                ui.add(Badge::new(format!("+{}", names.len() - shown)).outline());
            }
        })
        .response;
    r.on_hover_text(tip);
}

/// The dialog of a `/model` slot: its row text and the capabilities of its
/// model.
pub(super) fn dialog(ctx: &egui::Context, cx: &Cx<'_>, state: &mut State, m: &Models<'_>) {
    let Some(SlotKey::Slot(slot)) = state.connect.slot_edit else {
        state.connect.slot_edit = None;
        return;
    };
    let Some(current) = m.models.slots.get(&slot).cloned() else {
        state.connect.slot_edit = None;
        return;
    };
    let key = SlotKey::Slot(slot);
    let label = cx.tr.t(key.label_key());
    let mut next = m.models.clone();
    let mut close = false;
    let r = modal(ctx, Id::unique("claude-slot-dialog"), 480.0, |ui| {
        ui.label(Text::title(
            cx.tr.f("claude.slot_dialog.title", &[("slot", &label)]),
            cx.pal,
        ));
        ui.add(egui::Label::new(Text::mono(&current.model, cx.pal)).truncate())
            .on_hover_text(&current.model);
        dialog_body(ui, |ui| {
            row_text(ui, cx, &label, &mut next, slot);
            ui.add_space(space::XS);
            caps_editor(ui, cx, m, &mut next, &current.model);
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
    if r.should_close() {
        close = true;
    }
    if next != m.models {
        state
            .connect
            .edit_models(m.view, &m.provider, |x| *x = next);
    }
    if close {
        state.connect.slot_edit = None;
    }
}

/// Name and description of the slot's `/model` row.
fn row_text(ui: &mut Ui, cx: &Cx<'_>, label: &str, m: &mut ClaudeModels, slot: ClaudeSlot) {
    let Some(s) = m.slots.get_mut(&slot) else {
        return;
    };
    ui.label(Text::caption(
        cx.tr.f("claude.detail.row_text", &[("slot", &label)]),
        cx.pal,
    ));
    let mut name = s.name.clone().unwrap_or_default();
    let mut desc = s.description.clone().unwrap_or_default();
    let label_w = FormRow::label_width_of(
        ui,
        [
            cx.tr.t("claude.slot_dialog.name").as_str(),
            cx.tr.t("claude.slot_dialog.description").as_str(),
        ],
    );
    FormRow::new(cx.tr.t("claude.slot_dialog.name"))
        .label_width(label_w)
        .show(ui, |ui| {
            ui.add(
                TextEdit::singleline(&mut name)
                    .hint_text(cx.tr.t("claude.detail.name_hint"))
                    .desired_width(f32::INFINITY),
            );
        });
    FormRow::new(cx.tr.t("claude.slot_dialog.description"))
        .label_width(label_w)
        .show(ui, |ui| {
            ui.add(
                TextEdit::singleline(&mut desc)
                    .hint_text(cx.tr.t("claude.detail.description_hint"))
                    .desired_width(f32::INFINITY),
            );
        });
    let name = (!name.trim().is_empty()).then_some(name);
    let desc = (!desc.trim().is_empty()).then_some(desc);
    if name != s.name || desc != s.description {
        s.name = name;
        s.description = desc;
    }
}

/// The eight capability checkboxes of `model` (two columns), "Use the
/// suggestion" when the user chose them, and the list written.
fn caps_editor(ui: &mut Ui, cx: &Cx<'_>, m: &Models<'_>, next: &mut ClaudeModels, model: &str) {
    let (caps, own) = capabilities(next, &m.rows, model);
    let mut reset = false;
    FormRow::new(Text::strong(cx.tr.t("claude.caps.title"), cx.pal))
        .label_width(0.0)
        .right()
        .id_salt("claude-slot-caps")
        .show(ui, |ui| {
            if own {
                reset = ui
                    .add(Button::ghost(cx.tr.t("claude.caps.suggested_reset")).small())
                    .clicked();
            } else {
                ui.label(Text::caption(cx.tr.t("claude.caps.suggested"), cx.pal));
            }
        });
    if reset {
        next.capabilities
            .retain(|k, _| !k.eq_ignore_ascii_case(model));
        return;
    }
    let mut chosen = caps.clone();
    egui::Grid::new("claude-slot-caps-grid")
        .num_columns(2)
        .spacing(vec2(space::L, space::XS))
        .show(ui, |ui| {
            for (i, c) in ClaudeCapability::ALL.into_iter().enumerate() {
                let mut on = chosen.contains(&c);
                ui.checkbox(&mut on, c.as_str())
                    .on_hover_text(cx.tr.t(&format!("claude.caps.{}", c.as_str())));
                if on {
                    chosen.insert(c);
                } else {
                    chosen.remove(&c);
                }
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
        });
    if chosen != caps {
        next.capabilities
            .retain(|k, _| !k.eq_ignore_ascii_case(model));
        next.capabilities.insert(model.to_owned(), chosen.clone());
    }
    ui.add(
        egui::Label::new(Text::caption(
            cx.tr.f(
                "claude.caps.written",
                &[("list", &capabilities_text(&chosen))],
            ),
            cx.pal,
        ))
        .wrap(),
    );
}
