//! On/off switch (settings, remote hosts, router).

use egui::{CursorIcon, Response, Sense, Stroke, StrokeKind, Ui, Widget, WidgetInfo, pos2, vec2};

use super::{focus_ring, show_focus};
use crate::design::tokens::{ANIM_SECS, Palette, radius};

/// A 36 x 20 toggle bound to a bool.
#[must_use = "add it with ui.add(..)"]
pub struct Toggle<'a> {
    on: &'a mut bool,
    enabled: bool,
    label: String,
}

impl<'a> Toggle<'a> {
    /// New toggle; `label` is the accessible name.
    pub fn new(on: &'a mut bool, label: impl Into<String>) -> Self {
        Self {
            on,
            enabled: true,
            label: label.into(),
        }
    }

    /// Enable or disable.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

impl Widget for Toggle<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let pal = Palette::of(ui.ctx());
        let size = vec2(36.0, 20.0);
        let sense = if self.enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, mut response) = ui.allocate_exact_size(size, sense);
        if response.clicked() && self.enabled {
            *self.on = !*self.on;
            response.mark_changed();
        }
        let on = *self.on;
        let label = self.label.clone();
        let enabled = self.enabled;
        response.widget_info(|| WidgetInfo::selected(egui::Role::Switch, enabled, on, &label));
        // Before the visibility check: Tab to a toggle below the fold scrolls it in.
        let focused = show_focus(&response);
        if ui.is_rect_visible(rect) {
            let t = ui
                .ctx()
                .animate_bool_with_time(response.id.with("on"), on, ANIM_SECS);
            let track = pal.text_muted.lerp_to_gamma(pal.primary, t);
            let track = if self.enabled {
                track
            } else {
                track.gamma_multiply(0.5)
            };
            let painter = ui.painter();
            painter.rect(
                rect,
                radius::PILL,
                track,
                Stroke::new(1.0, track),
                StrokeKind::Inside,
            );
            let knob_r = 7.0;
            let x = egui::lerp(rect.left() + 10.0..=rect.right() - 10.0, t);
            let knob = pal.surface.lerp_to_gamma(pal.on_primary, t);
            painter.circle_filled(pos2(x, rect.center().y), knob_r, knob);
            if focused {
                focus_ring(painter, rect, radius::PILL, pal);
            }
        }
        if self.enabled {
            response.on_hover_cursor(CursorIcon::PointingHand)
        } else {
            response
        }
    }
}
