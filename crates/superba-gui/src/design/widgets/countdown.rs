//! The countdown bar of the armed hot switch (plan 7.3): fills linearly over the
//! countdown; amber while paused (WaitingTurn).

use egui::{Response, Sense, Ui, Widget, vec2};

use crate::design::tokens::Palette;

/// A thin progress bar.
#[must_use = "add it with ui.add(..)"]
pub struct CountdownBar {
    progress: f32,
    paused: bool,
    height: f32,
}

impl CountdownBar {
    /// `progress` is the elapsed fraction (0..=1).
    pub fn new(progress: f32) -> Self {
        Self {
            progress: progress.clamp(0.0, 1.0),
            paused: false,
            height: 3.0,
        }
    }

    /// Paused (amber).
    pub fn paused(mut self, paused: bool) -> Self {
        self.paused = paused;
        self
    }

    /// Bar height.
    pub fn height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }
}

impl Widget for CountdownBar {
    fn ui(self, ui: &mut Ui) -> Response {
        let pal = Palette::of(ui.ctx());
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), self.height), Sense::hover());
        let r = (self.height / 2.0).ceil() as u8;
        ui.painter().rect_filled(rect, r, pal.active);
        let mut fill = rect;
        fill.set_width(rect.width() * self.progress);
        let color = if self.paused { pal.amber } else { pal.text };
        if fill.width() > 0.5 {
            ui.painter().rect_filled(fill, r, color);
        }
        response.widget_info(|| {
            let mut info = egui::WidgetInfo::new(egui::Role::ProgressIndicator);
            info.value = Some(f64::from(self.progress));
            info
        });
        response
    }
}
