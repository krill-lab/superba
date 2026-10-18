//! Form row: a label column and the row's controls on one center line (user
//! bug 19). Every page draws its label + control rows with it.
//!
//! `ui.horizontal` centers each widget on the row height it has *so far*: the
//! row starts `interact_size.y` (22 px) high, so a 20 px label is centered on
//! 11 px, while a 28 px dropdown or button added after it does not fit, starts
//! at the row's top and is centered on 14 px — the label sits 3 px higher than
//! the control next to it.
//!
//! A [`FormRow`] knows its line height before it draws anything: at least
//! [`FormRow::height`] (default [`size::CONTROL_SM`], the height of a small
//! button or a [`super::Dropdown`]), the label's height, and the tallest one-line
//! control it held in the previous pass (a taller control or a label whose height
//! changed redoes the pass, so nothing is drawn off-center). The label — a text in
//! a fixed-width column, any widget such as a radio button, or a block such as a
//! title over a description — and every control are centered on that line; text
//! labels are centered like the text inside the controls, so same-size text
//! shares a baseline. Content taller than
//! [`size::CONTROL_LG`] (a multi-line editor, a wrapped list) is not a line: it
//! starts at the line's top and grows the row downward while the label stays on
//! the first line. An optional hint is drawn under the controls, aligned with them.
//!
//! The row draws in the parent's clip rectangle, so focus rings around its
//! controls are never cut.
//!
//! A text label names the row's controls for screen readers (user bug 56):
//! every control without a name of its own (a text field, a dropdown, a
//! segmented control) is labelled by it ([`crate::design::a11y`]).

use egui::{
    Align, AsIdSalt, FontSelection, IdSalt, Label, Layout, Rect, TextWrapMode, Ui, UiBuilder,
    WidgetText, pos2, vec2,
};

use super::Text;
use crate::design::fonts::{Weight, font};
use crate::design::tokens::{Palette, size, text};

/// Default width of the label column.
pub const LABEL_WIDTH: f32 = 112.0;
/// Default space between the label column and the controls.
pub const LABEL_GAP: f32 = 12.0;
/// Space between the controls' line and the hint below them.
pub const HINT_GAP: f32 = 2.0;
/// The tallest content that still counts as one line of controls.
const MAX_LINE: f32 = size::CONTROL_LG;

/// One label + controls row. Build it, then call [`FormRow::show`] (text label)
/// or [`FormRow::show_with`] (label drawn by the caller, e.g. a radio button;
/// build that row with [`FormRow::default`]).
#[must_use = "call .show(..) or .show_with(..)"]
pub struct FormRow {
    label: Option<WidgetText>,
    label_width: f32,
    gap: f32,
    height: f32,
    hint: Option<WidgetText>,
    right: bool,
    wrap: bool,
    id_salt: Option<IdSalt>,
}

impl Default for FormRow {
    /// A row with an empty label column: for [`FormRow::show_with`], or a
    /// continuation row whose controls line up with the rows above.
    fn default() -> Self {
        Self::new("")
    }
}

/// What [`FormRow::show`] drew.
#[derive(Debug)]
pub struct FormRowResponse<R> {
    /// The value returned by the closures.
    pub inner: R,
    /// The label: the text itself, or the area of the label widgets.
    pub label_rect: Rect,
    /// The controls' line: right of the label column, one line high; label and
    /// controls are centered on its center.
    pub line: Rect,
    /// The area the controls used (at least one line high).
    pub controls_rect: Rect,
    /// The whole row (label, controls and hint).
    pub rect: Rect,
}

impl FormRow {
    /// A row with a text label in the label column. Plain text is drawn 13 px in
    /// the muted color; a `RichText` keeps its font and color.
    pub fn new(label: impl Into<WidgetText>) -> Self {
        Self {
            label: Some(label.into()),
            label_width: LABEL_WIDTH,
            gap: LABEL_GAP,
            height: size::CONTROL_SM,
            hint: None,
            right: false,
            wrap: false,
            id_salt: None,
        }
    }

    /// A row of controls only (no label column, no gap), still on one center line.
    pub fn unlabeled() -> Self {
        Self {
            label: None,
            label_width: 0.0,
            gap: 0.0,
            ..Self::new("")
        }
    }

    /// Width of a label column that holds `labels` (plain text labels: 13 px):
    /// the widest of them, so a group of rows lines its controls up right after
    /// its longest label in every language.
    pub fn label_width_of<'a>(ui: &Ui, labels: impl IntoIterator<Item = &'a str>) -> f32 {
        labels
            .into_iter()
            .map(|l| {
                WidgetText::from(l)
                    .into_galley(
                        ui,
                        Some(TextWrapMode::Extend),
                        f32::INFINITY,
                        FontSelection::FontId(font(text::SMALL, Weight::Regular)),
                    )
                    .size()
                    .x
            })
            .fold(0.0, f32::max)
    }

    /// Width of the label column. A longer label is not cut: the controls of its
    /// row start after it.
    pub fn label_width(mut self, width: f32) -> Self {
        self.label_width = width.max(0.0);
        self
    }

    /// Space between the label column and the controls.
    pub fn gap(mut self, gap: f32) -> Self {
        self.gap = gap.max(0.0);
        self
    }

    /// Minimum line height: the height of the row's tallest control (default
    /// [`size::CONTROL_SM`]).
    pub fn height(mut self, height: f32) -> Self {
        self.height = height.max(0.0);
        self
    }

    /// A hint under the controls (plain text is drawn as a caption).
    pub fn hint(mut self, hint: impl Into<WidgetText>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Right-align the controls (a settings row: text left, control right).
    pub fn right(mut self) -> Self {
        self.right = true;
        self
    }

    /// Let the controls wrap onto more lines; the first line stays centered with
    /// the label.
    pub fn wrap(mut self) -> Self {
        self.wrap = true;
        self
    }

    /// Salt of the id under which the measured line height is kept (default: the
    /// label text, or the next auto id for rows without a text label).
    pub fn id_salt(mut self, salt: impl AsIdSalt) -> Self {
        self.id_salt = Some(IdSalt::new(salt));
        self
    }

    /// Draw the row with its text label and the controls added by `add`.
    pub fn show<R>(self, ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> FormRowResponse<R> {
        let pal = Palette::of(ui.ctx());
        match self.label.clone().filter(|t| !t.is_empty()) {
            Some(text) => {
                let text = match text {
                    WidgetText::Text(s) => WidgetText::from(Text::small(s, pal)),
                    other => other,
                };
                // The label is drawn first; its node names the controls.
                let label_id = std::cell::Cell::new(None);
                let out = self.show_inner(
                    ui,
                    Some(|ui: &mut Ui| {
                        let r = ui.add(
                            Label::new(text)
                                .wrap_mode(TextWrapMode::Extend)
                                .selectable(false),
                        );
                        label_id.set(Some(r.id));
                        r.rect
                    }),
                    |ui: &mut Ui| match label_id.get() {
                        Some(id) => crate::design::a11y::label_unnamed_by(ui, id, add),
                        None => add(ui),
                    },
                );
                FormRowResponse {
                    inner: out.inner.1,
                    // The text itself, not its band.
                    label_rect: out.inner.0.unwrap_or(out.label_rect),
                    line: out.line,
                    controls_rect: out.controls_rect,
                    rect: out.rect,
                }
            }
            None => {
                let out = self.show_inner(ui, None::<fn(&mut Ui)>, add);
                FormRowResponse {
                    inner: out.inner.1,
                    label_rect: out.label_rect,
                    line: out.line,
                    controls_rect: out.controls_rect,
                    rect: out.rect,
                }
            }
        }
    }

    /// Draw the row with a label drawn by `add_label` (widgets in a left-to-right
    /// layout, centered on the line) and the controls added by `add`. The first
    /// label widget that names something (a text, a radio button) names the
    /// controls that have no name of their own.
    pub fn show_with<L, R>(
        self,
        ui: &mut Ui,
        add_label: impl FnOnce(&mut Ui) -> L,
        add: impl FnOnce(&mut Ui) -> R,
    ) -> FormRowResponse<(L, R)> {
        let label_id = std::cell::Cell::new(None);
        let out = self.show_inner(
            ui,
            Some(|ui: &mut Ui| {
                let (inner, id) = crate::design::a11y::first_named(ui, add_label);
                label_id.set(id);
                inner
            }),
            |ui: &mut Ui| match label_id.get() {
                Some(id) => crate::design::a11y::label_unnamed_by(ui, id, add),
                None => add(ui),
            },
        );
        FormRowResponse {
            inner: (
                out.inner.0.expect("the label closure always runs"),
                out.inner.1,
            ),
            label_rect: out.label_rect,
            line: out.line,
            controls_rect: out.controls_rect,
            rect: out.rect,
        }
    }

    fn show_inner<L, R>(
        self,
        ui: &mut Ui,
        add_label: Option<impl FnOnce(&mut Ui) -> L>,
        add: impl FnOnce(&mut Ui) -> R,
    ) -> FormRowResponse<(Option<L>, R)> {
        let pal = Palette::of(ui.ctx());
        let id = match (&self.id_salt, &self.label) {
            (Some(salt), _) => ui.scope_id().with(("superba-form-row", *salt)),
            (None, Some(t)) if !t.is_empty() => ui
                .scope_id()
                .with(("superba-form-row", t.text().to_owned())),
            _ => ui.next_auto_id().with("superba-form-row"),
        };
        let line_id = id.with("line");
        let label_id = id.with("label");
        let (measured, label_h): (f32, Option<f32>) = ui
            .ctx()
            .data(|d| (d.get_temp(line_id).unwrap_or(0.0), d.get_temp(label_id)));
        let line_h = self.height.max(measured).max(label_h.unwrap_or(0.0));
        let avail = ui.available_rect_before_wrap();
        let top = avail.min;

        // Label: in a band as high as the label was in the last pass, centered on
        // the line. Single widgets (a text, a radio button) are centered in it,
        // and a block (`ui.vertical`, which starts at the top of its parent) fills
        // it. A label taller than the line makes the line that tall.
        let has_column = self.label.is_some() || add_label.is_some();
        let mut label_w = self.label_width;
        let mut label_inner = None;
        let label_rect = if let Some(add_label) = add_label {
            let band = label_h.unwrap_or(line_h);
            let max = Rect::from_min_size(
                pos2(top.x, top.y + (line_h - band) / 2.0),
                vec2(self.label_width, band),
            );
            let mut child = ui.new_child(
                UiBuilder::new()
                    .max_rect(max)
                    .layout(Layout::left_to_right(Align::Center)),
            );
            label_inner = Some(add_label(&mut child));
            let used = child.min_rect();
            label_w = label_w.max(used.width());
            if (used.height() - band).abs() > 0.5 {
                ui.ctx()
                    .data_mut(|d| d.insert_temp(label_id, used.height()));
                ui.ctx()
                    .request_discard("FormRow: the label's height changed");
            }
            used
        } else {
            Rect::from_min_size(pos2(top.x, top.y + line_h / 2.0), vec2(0.0, 0.0))
        };

        // Controls, centered on the line.
        let x = if has_column {
            top.x + label_w + self.gap
        } else {
            top.x
        };
        let line = Rect::from_min_max(pos2(x, top.y), pos2(avail.max.x.max(x), top.y + line_h));
        let layout = if self.right {
            Layout::right_to_left(Align::Center)
        } else {
            Layout::left_to_right(Align::Center).with_main_wrap(self.wrap)
        };
        let mut child = ui.new_child(UiBuilder::new().max_rect(line).layout(layout));
        let inner = add(&mut child);
        let used = child.min_rect();

        // A one-line control taller than the line: redo the pass with a line that
        // holds it (the next pass reads the stored height).
        if !self.wrap && used.height() > line_h + 0.5 && used.height() <= MAX_LINE {
            ui.ctx().data_mut(|d| d.insert_temp(line_id, used.height()));
            ui.ctx()
                .request_discard("FormRow: a control is taller than the row's line");
        }

        let mut bottom = used.max.y.max(line.max.y).max(label_rect.max.y);
        if let Some(hint) = self.hint {
            let hint = match hint {
                WidgetText::Text(s) => WidgetText::from(Text::caption(s, pal)),
                other => other,
            };
            let max = Rect::from_min_max(
                pos2(x, bottom + HINT_GAP),
                pos2(avail.max.x.max(x), avail.max.y.max(bottom + HINT_GAP)),
            );
            let mut child = ui.new_child(
                UiBuilder::new()
                    .max_rect(max)
                    .layout(Layout::top_down(Align::Min)),
            );
            child.add(Label::new(hint).wrap());
            bottom = child.min_rect().max.y;
        }

        let rect = Rect::from_min_max(top, pos2(avail.max.x.max(used.max.x), bottom));
        ui.advance_cursor_after_rect(rect);
        FormRowResponse {
            inner: (label_inner, inner),
            label_rect,
            line,
            controls_rect: used,
            rect,
        }
    }
}
