//! API key / PAT input: masked monospace text with a reveal toggle.

use egui::{
    Align, CursorIcon, Frame, Id, Layout, Rect, Response, Sense, Stroke, StrokeKind, TextEdit, Ui,
    UiBuilder, vec2,
};

use super::{focus_ring, hover_t};
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::icons::{self, Icon};
use crate::design::tokens::{Palette, radius, size, text};

/// A key input bound to a string and a "revealed" flag.
#[must_use = "call .show(ui)"]
pub struct KeyInput<'a> {
    value: &'a mut String,
    revealed: &'a mut bool,
    id: Id,
    hint: String,
    reveal_label: String,
    invalid: bool,
    width: Option<f32>,
}

impl<'a> KeyInput<'a> {
    /// New input; `reveal_label` is the tooltip / accessible name of the eye button.
    pub fn new(
        id_salt: impl std::hash::Hash + std::fmt::Debug,
        value: &'a mut String,
        revealed: &'a mut bool,
        hint: impl Into<String>,
        reveal_label: impl Into<String>,
    ) -> Self {
        Self {
            value,
            revealed,
            id: Id::unique(id_salt),
            hint: hint.into(),
            reveal_label: reveal_label.into(),
            invalid: false,
            width: None,
        }
    }

    /// Red border (validation failed).
    pub fn invalid(mut self, invalid: bool) -> Self {
        self.invalid = invalid;
        self
    }

    /// Fixed width (default: available width).
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// Show; the response is the text edit's (`changed()`, `lost_focus()`).
    pub fn show(self, ui: &mut Ui) -> Response {
        let pal = Palette::of(ui.ctx());
        let width = self.width.unwrap_or_else(|| ui.available_width());
        let height = size::CONTROL_LG - 4.0;
        let (rect, frame_resp) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        let eye = Rect::from_min_size(
            egui::pos2(rect.right() - height, rect.top()),
            vec2(height, height),
        );
        let edit_rect = Rect::from_min_max(
            egui::pos2(rect.left() + 12.0, rect.top()),
            egui::pos2(eye.left(), rect.bottom()),
        );
        // Paint the frame first (the text edit draws on top of it). Focus is taken
        // from memory so the border reacts in the same frame.
        let focused = ui.memory(|m| m.has_focus(self.id));
        let t = hover_t(ui, &frame_resp);
        let border = if self.invalid {
            pal.red
        } else if focused {
            pal.text_subtle
        } else {
            pal.border.lerp_to_gamma(pal.border_strong, t)
        };
        ui.painter().rect(
            rect,
            radius::CONTROL,
            pal.bg,
            Stroke::new(1.0, border),
            StrokeKind::Inside,
        );
        if focused {
            focus_ring(ui.painter(), rect, radius::CONTROL, pal);
        }

        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(edit_rect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        let hint = egui::RichText::new(self.hint.clone())
            .font(font(text::BODY, Weight::Regular))
            .color(pal.text_subtle);
        let edit = TextEdit::singleline(self.value)
            .id(self.id)
            .password(!*self.revealed)
            .font(mono_font(text::SMALL, Weight::Regular))
            .text_color(pal.text)
            .hint_text(hint)
            .frame(Frame::NONE)
            .align(egui::Align2::LEFT_CENTER)
            .min_size(edit_rect.size())
            .desired_width(edit_rect.width());
        let response = child.add(edit);

        let eye_resp = ui
            .interact(eye, self.id.with("eye"), Sense::click())
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text(self.reveal_label.clone());
        if eye_resp.clicked() {
            *self.revealed = !*self.revealed;
        }
        let label = self.reveal_label.clone();
        eye_resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
        let et = hover_t(ui, &eye_resp);
        let icon = if *self.revealed {
            Icon::EyeOff
        } else {
            Icon::Eye
        };
        let icon_rect = Rect::from_center_size(eye.center(), vec2(size::ICON, size::ICON));
        icons::paint(
            ui.painter(),
            icon_rect,
            icon,
            pal.text_subtle.lerp_to_gamma(pal.text, et),
        );
        response
    }
}
