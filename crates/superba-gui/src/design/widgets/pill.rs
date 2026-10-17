//! Fully rounded pills after Surge (user request 24): a status pill (dot +
//! label, like 系统代理 / 增强模式) and a notice pill (a red or amber count, then
//! the latest problem in gray, elided).

use egui::{Align2, Color32, CursorIcon, Response, Sense, Stroke, Ui, Widget, pos2, vec2};

use super::{Status, focus_ring, hover_t, show_focus};
use crate::design::backdrop::Backdrop;
use crate::design::fonts::{Weight, font};
use crate::design::tokens::{Palette, radius, text};

/// Height of a pill.
pub const PILL_HEIGHT: f32 = 30.0;

/// A clickable status pill: a dot in the status colour and a label.
#[must_use = "add it with ui.add(..)"]
pub struct StatusPill {
    label: String,
    status: Status,
    min_width: f32,
}

impl StatusPill {
    /// New pill.
    pub fn new(label: impl Into<String>, status: Status) -> Self {
        Self {
            label: label.into(),
            status,
            min_width: 0.0,
        }
    }

    /// Minimum width (pills in a row look even).
    pub fn min_width(mut self, w: f32) -> Self {
        self.min_width = w;
        self
    }
}

/// Fill of a pill on the window background.
fn pill_fill(ui: &Ui, t: f32) -> Color32 {
    let pal = Palette::of(ui.ctx());
    let b = Backdrop::of(ui.ctx());
    let base = if pal.dark {
        Color32::from_white_alpha(16)
    } else {
        Color32::from_black_alpha(10)
    };
    base.lerp_to_gamma(b.selected, t)
}

impl Widget for StatusPill {
    fn ui(self, ui: &mut Ui) -> Response {
        let pal = Palette::of(ui.ctx());
        let galley = ui.painter().layout_no_wrap(
            self.label.clone(),
            font(text::BODY, Weight::Medium),
            pal.text,
        );
        let w = (galley.size().x + 16.0 + 14.0 + 20.0).max(self.min_width);
        let (rect, response) = ui.allocate_exact_size(vec2(w, PILL_HEIGHT), Sense::click());
        let response = response.on_hover_cursor(CursorIcon::PointingHand);
        let label = self.label.clone();
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
        let t = hover_t(ui, &response);
        let p = ui.painter();
        p.rect_filled(rect, radius::PILL, pill_fill(ui, t));
        let dot = pos2(rect.min.x + 16.0, rect.center().y);
        p.circle_filled(dot, 4.5, pal.status(self.status));
        // The label is centred in the space right of the dot.
        let text_x = (rect.min.x + 30.0).max(rect.center().x + 7.0 - galley.size().x / 2.0);
        p.galley(
            pos2(text_x, rect.center().y - galley.size().y / 2.0),
            galley,
            pal.text,
        );
        if show_focus(&response) {
            focus_ring(p, rect, radius::PILL, pal);
        }
        response
    }
}

/// The problems pill at the top of the Overview: "● 2 warnings  <latest>"; with
/// no problems a calm "● <ok text>".
#[must_use = "add it with ui.add(..)"]
pub struct NoticePill {
    count: String,
    message: String,
    status: Status,
    max_width: f32,
}

impl NoticePill {
    /// `count` in the status colour ("2 警告"), then `message` (elided).
    pub fn new(count: impl Into<String>, message: impl Into<String>, status: Status) -> Self {
        Self {
            count: count.into(),
            message: message.into(),
            status,
            max_width: 520.0,
        }
    }

    /// Widest the pill may grow.
    pub fn max_width(mut self, w: f32) -> Self {
        self.max_width = w;
        self
    }
}

impl Widget for NoticePill {
    fn ui(self, ui: &mut Ui) -> Response {
        let pal = Palette::of(ui.ctx());
        let b = Backdrop::of(ui.ctx());
        let color = match self.status {
            Status::Ok | Status::Idle | Status::Busy => pal.text,
            s => pal.status(s),
        };
        let count = ui.painter().layout_no_wrap(
            self.count.clone(),
            font(text::BODY, Weight::Semibold),
            color,
        );
        let pad = 14.0;
        let lead = pad + 9.0 + 6.0;
        let room = (self.max_width - lead - count.size().x - 10.0 - pad).max(40.0);
        let mut job = egui::text::LayoutJob::single_section(
            self.message.clone(),
            egui::TextFormat {
                font_id: font(text::BODY, Weight::Regular),
                color: pal.text_muted,
                ..Default::default()
            },
        );
        job.wrap.max_width = room;
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        let message = ui.painter().layout_job(job);
        let gap = if self.message.is_empty() { 0.0 } else { 10.0 };
        let w = lead + count.size().x + gap + message.size().x + pad;
        let (rect, response) = ui.allocate_exact_size(vec2(w, PILL_HEIGHT), Sense::click());
        let response = response.on_hover_cursor(CursorIcon::PointingHand);
        let label = format!("{} {}", self.count, self.message);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
        let t = hover_t(ui, &response);
        let p = ui.painter();
        let fill = b.card.gamma_multiply(if pal.dark { 0.85 } else { 0.92 });
        p.rect(
            rect,
            radius::PILL,
            fill.lerp_to_gamma(b.card, t),
            Stroke::new(1.0, b.card_border),
            egui::StrokeKind::Inside,
        );
        p.circle_filled(
            pos2(rect.min.x + pad + 4.0, rect.center().y),
            4.0,
            pal.status(self.status),
        );
        let mut x = rect.min.x + lead;
        let cw = count.size().x;
        p.galley(
            pos2(x, rect.center().y - count.size().y / 2.0),
            count,
            color,
        );
        x += cw + gap;
        p.galley(
            pos2(x, rect.center().y - message.size().y / 2.0),
            message,
            pal.text_muted,
        );
        if show_focus(&response) {
            focus_ring(p, rect, radius::PILL, pal);
        }
        response
    }
}

/// A small gray caption with an optional value on the right, as a card header
/// row (used by dashboard cards): returns the caption's rectangle.
pub fn card_caption(ui: &mut Ui, caption: &str) -> egui::Rect {
    let pal = Palette::of(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    ui.painter().text(
        pos2(rect.min.x, rect.center().y),
        Align2::LEFT_CENTER,
        caption,
        font(text::SMALL, Weight::Medium),
        pal.text_muted,
    );
    rect
}
