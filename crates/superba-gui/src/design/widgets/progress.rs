//! Compact transfer progress with a readable value.
use crate::design::{a11y, tokens::Palette};
use egui::{Response, Sense, Ui, Widget, vec2};
/// A three point progress track, optionally indeterminate.
pub struct InlineProgress {
    fraction: Option<f32>,
    label: String,
    total: Option<String>,
}
impl InlineProgress {
    /// Fraction and a localized accessible description, including total size.
    pub fn new(fraction: Option<f32>, label: impl Into<String>) -> Self {
        Self {
            fraction,
            label: label.into(),
            total: None,
        }
    }
    /// Optional visible amount below the three-point track.
    pub fn total(mut self, total: impl Into<String>) -> Self {
        self.total = Some(total.into());
        self
    }
}
impl Widget for InlineProgress {
    fn ui(self, ui: &mut Ui) -> Response {
        let (r, response) = ui.allocate_exact_size(
            vec2(
                ui.available_width(),
                if self.total.is_some() { 25.0 } else { 3.0 },
            ),
            Sense::hover(),
        );
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::ProgressIndicator, true, &self.label)
        });
        a11y::describe(&response, egui::Role::ProgressIndicator, &self.label);
        let p = Palette::of(ui.ctx());
        let mut track = r;
        track.max.y = track.min.y + 3.0;
        ui.painter().rect_filled(track, 2, p.hover);
        let mut fill = track;
        if let Some(f) = self.fraction.filter(|f| f.is_finite()) {
            let f = f.clamp(0.0, 1.0);
            fill.max.x = fill.min.x + r.width() * f;
            ui.ctx().accesskit_node_builder(response.id, |n| {
                n.set_numeric_value(f as f64);
                n.set_min_numeric_value(0.0);
                n.set_max_numeric_value(1.0);
            });
        } else {
            let t = ui.input(|i| i.time) as f32;
            fill.min.x += (t.sin() + 1.0) * 0.35 * r.width();
            fill.max.x = fill.min.x + r.width() * 0.3;
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(80));
        }
        ui.painter().rect_filled(fill, 2, p.text_muted);
        if let Some(total) = self.total {
            ui.painter().text(
                r.left_bottom(),
                egui::Align2::LEFT_BOTTOM,
                &total,
                crate::design::fonts::font(
                    crate::design::tokens::text::CAPTION,
                    crate::design::fonts::Weight::Regular,
                ),
                p.text_muted,
            );
            ui.ctx()
                .accesskit_node_builder(response.id, |n| n.set_description(total));
        }
        response.on_hover_text(self.label)
    }
}
