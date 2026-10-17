//! Status dots: the only place (with the countdown bar and danger buttons) where
//! green, amber and red appear.

use egui::{Response, Sense, Ui, Widget, vec2};

use crate::design::tokens::Palette;

/// A status shown as a colored dot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Status {
    /// Healthy / available / done.
    Ok,
    /// Needs attention / waiting / paused.
    Warn,
    /// Failed / unavailable / blocked.
    Error,
    /// Idle / unknown / not applicable.
    Idle,
    /// In progress (neutral, pulsing).
    Busy,
}

/// An 8 px status dot; `pulse` adds a soft expanding halo (live / in progress).
#[must_use = "add it with ui.add(..)"]
pub struct StatusDot {
    status: Status,
    size: f32,
    pulse: bool,
}

impl StatusDot {
    /// New dot.
    pub fn new(status: Status) -> Self {
        Self {
            status,
            size: 8.0,
            pulse: matches!(status, Status::Busy),
        }
    }

    /// Diameter.
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    /// Enable the halo animation.
    pub fn pulse(mut self, pulse: bool) -> Self {
        self.pulse = pulse;
        self
    }

    /// Paint at a center point without allocating space.
    pub fn paint_at(&self, ui: &Ui, center: egui::Pos2) {
        let pal = Palette::of(ui.ctx());
        let color = pal.status(self.status);
        let r = self.size / 2.0;
        if self.pulse {
            let t = (ui.input(|i| i.time) % 1.6 / 1.6) as f32;
            let halo = color.gamma_multiply(0.45 * (1.0 - t));
            ui.painter().circle_filled(center, r + 5.0 * t, halo);
            ui.ctx().request_repaint();
        }
        ui.painter().circle_filled(center, r, color);
    }
}

impl Widget for StatusDot {
    fn ui(self, ui: &mut Ui) -> Response {
        let (rect, response) =
            ui.allocate_exact_size(vec2(self.size + 2.0, self.size + 8.0), Sense::hover());
        if ui.is_rect_visible(rect) {
            self.paint_at(ui, rect.center());
        }
        response
    }
}
