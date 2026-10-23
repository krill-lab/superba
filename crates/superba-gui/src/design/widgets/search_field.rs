//! A one-line search field with a clear button (×) inside its right edge and
//! Esc to clear (the Models page, the tools' model picker, Claude Code's
//! model list).

use egui::{Response, TextEdit, Ui, vec2};

use super::IconButton;
use crate::design::icons::Icon;

/// Side of the clear button.
const CLEAR: f32 = 18.0;

/// A search field; [`SearchField::show`] returns the text field's response.
pub struct SearchField<'a> {
    text: &'a mut String,
    id: egui::Id,
    hint: String,
    clear_tip: String,
    width: Option<f32>,
    height: Option<f32>,
    margin: egui::Vec2,
}

impl<'a> SearchField<'a> {
    /// A field over `text`, `id` naming it (a page-unique name), `hint` its placeholder and
    /// accessible name, `clear_tip` the clear button's name.
    pub fn new(
        text: &'a mut String,
        id: &'static str,
        hint: impl Into<String>,
        clear_tip: impl Into<String>,
    ) -> Self {
        Self {
            text,
            id: egui::Id::unique(id),
            hint: hint.into(),
            clear_tip: clear_tip.into(),
            width: None,
            height: None,
            margin: vec2(10.0, 5.0),
        }
    }

    /// The field's width (default: the available width).
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// A fixed height (default: the text's).
    pub fn height(mut self, height: f32) -> Self {
        self.height = Some(height);
        self
    }

    /// Inner margin.
    pub fn margin(mut self, margin: egui::Vec2) -> Self {
        self.margin = margin;
        self
    }

    /// Draw the field. A click on × or Esc clears the text (and keeps the
    /// focus in the field).
    pub fn show(self, ui: &mut Ui) -> Response {
        let Self {
            text,
            id,
            hint,
            clear_tip,
            width,
            height,
            margin,
        } = self;
        let width = width.unwrap_or_else(|| ui.available_width());
        // Room for × on the right, so the text never runs under it.
        let edit = TextEdit::singleline(text)
            .id(id)
            .hint_text(hint.clone())
            .desired_width(width - margin.x * 2.0 - CLEAR)
            .margin(egui::Margin {
                left: margin.x as i8,
                right: (margin.x + CLEAR) as i8,
                top: margin.y as i8,
                bottom: margin.y as i8,
            });
        let r = match height {
            Some(h) => ui.add_sized([width, h], edit),
            None => ui.add(edit),
        }
        .accessible_name(hint);
        let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
        // egui gives up the focus on Esc before the caller sees the response.
        if escape && (r.has_focus() || r.lost_focus()) && !text.is_empty() {
            text.clear();
            r.request_focus();
        }
        if !text.is_empty() {
            let rect = egui::Rect::from_center_size(
                egui::pos2(
                    r.rect.right() - margin.x / 2.0 - CLEAR / 2.0,
                    r.rect.center().y,
                ),
                vec2(CLEAR, CLEAR),
            );
            let clear = ui.put(rect, IconButton::new(Icon::Close, clear_tip).size(CLEAR));
            if clear.clicked() {
                text.clear();
                r.request_focus();
            }
        }
        r
    }
}
