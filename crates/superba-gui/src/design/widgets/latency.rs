//! Latency badge: status dot + monospace milliseconds.

use egui::{Color32, Response, Sense, Ui, Widget, pos2, vec2};

use super::Status;
use crate::design::fonts::{Weight, mono_font};
use crate::design::icons;
use crate::design::tokens::{Palette, radius, text};

/// What to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Latency {
    /// A probe is running.
    Probing,
    /// Measured latency.
    Ms(u32),
    /// The line did not answer.
    Unreachable,
    /// Never probed.
    Unknown,
}

impl Latency {
    /// Status color bucket: < 250 ms ok, < 600 ms warn, else error.
    pub fn status(self) -> Status {
        match self {
            Latency::Ms(ms) if ms < 250 => Status::Ok,
            Latency::Ms(ms) if ms < 600 => Status::Warn,
            Latency::Ms(_) | Latency::Unreachable => Status::Error,
            Latency::Probing => Status::Busy,
            Latency::Unknown => Status::Idle,
        }
    }
}

/// A latency pill.
#[must_use = "add it with ui.add(..)"]
pub struct LatencyBadge {
    latency: Latency,
}

impl LatencyBadge {
    /// New badge.
    pub fn new(latency: Latency) -> Self {
        Self { latency }
    }
}

impl Widget for LatencyBadge {
    fn ui(self, ui: &mut Ui) -> Response {
        let pal = Palette::of(ui.ctx());
        let label = match self.latency {
            Latency::Ms(ms) => format!("{ms} ms"),
            Latency::Probing => "···".to_owned(),
            Latency::Unreachable | Latency::Unknown => "—".to_owned(),
        };
        let galley = ui.painter().layout_no_wrap(
            label,
            mono_font(text::CAPTION, Weight::Medium),
            Color32::PLACEHOLDER,
        );
        let w = (galley.size().x + 30.0).max(72.0);
        let (rect, response) = ui.allocate_exact_size(vec2(w, 22.0), Sense::hover());
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            painter.rect_filled(rect, radius::PILL, pal.hover);
            let c = pos2(rect.min.x + 12.0, rect.center().y);
            if self.latency == Latency::Probing {
                let r = egui::Rect::from_center_size(c, vec2(10.0, 10.0));
                icons::spinner(painter, r, pal.text_muted, ui.input(|i| i.time));
                ui.ctx().request_repaint();
            } else {
                painter.circle_filled(c, 3.5, pal.status(self.latency.status()));
            }
            painter.galley(
                pos2(rect.min.x + 22.0, rect.center().y - galley.size().y / 2.0),
                galley,
                pal.text,
            );
        }
        response
    }
}
