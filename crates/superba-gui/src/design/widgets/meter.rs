//! Usage meters and stacked distribution bars.

use egui::{Color32, Rect, Response, Sense, Ui, Widget, pos2, vec2};

use crate::design::tokens::Palette;

/// A thin horizontal meter (0..=1); turns amber above 80 % and red above 95 %.
#[must_use = "add it with ui.add(..)"]
pub struct Meter {
    value: f32,
    height: f32,
}

impl Meter {
    /// New meter.
    pub fn new(value: f32) -> Self {
        Self {
            value: value.clamp(0.0, 1.0),
            height: 6.0,
        }
    }
}

impl Widget for Meter {
    fn ui(self, ui: &mut Ui) -> Response {
        let pal = Palette::of(ui.ctx());
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), self.height), Sense::hover());
        let r = (self.height / 2.0) as u8;
        ui.painter().rect_filled(rect, r, pal.active);
        let color = if self.value > 0.95 {
            pal.red
        } else if self.value > 0.8 {
            pal.amber
        } else {
            pal.text
        };
        let mut fill = rect;
        fill.set_width((rect.width() * self.value).max(if self.value > 0.0 {
            self.height
        } else {
            0.0
        }));
        if fill.width() > 0.0 {
            ui.painter().rect_filled(fill, r, color);
        }
        response
    }
}

/// A stacked bar of shares; the first share uses the primary text color, the rest
/// progressively lighter grays (no hue: categories are not statuses).
#[must_use = "add it with ui.add(..)"]
pub struct SplitBar {
    shares: Vec<f32>,
    height: f32,
}

impl SplitBar {
    /// New bar from raw counts.
    pub fn new(counts: &[u32]) -> Self {
        let total: u32 = counts.iter().sum();
        let shares = if total == 0 {
            Vec::new()
        } else {
            counts.iter().map(|c| *c as f32 / total as f32).collect()
        };
        Self {
            shares,
            height: 8.0,
        }
    }

    /// Gray for share `index` (shared with legends).
    pub fn color(pal: &Palette, index: usize) -> Color32 {
        let steps = [pal.text, pal.text_subtle, pal.border_strong, pal.active];
        steps[index.min(steps.len() - 1)]
    }
}

impl Widget for SplitBar {
    fn ui(self, ui: &mut Ui) -> Response {
        let pal = Palette::of(ui.ctx());
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), self.height), Sense::hover());
        let r = (self.height / 2.0) as u8;
        ui.painter().rect_filled(rect, r, pal.active);
        let mut x = rect.min.x;
        let gap = 2.0;
        let n = self.shares.len();
        for (i, share) in self.shares.iter().enumerate() {
            let w = rect.width() * share;
            if w < 0.5 {
                continue;
            }
            let seg = Rect::from_min_max(
                pos2(x, rect.min.y),
                pos2(
                    (x + w - if i + 1 < n { gap } else { 0.0 }).max(x + 1.0),
                    rect.max.y,
                ),
            );
            ui.painter().rect_filled(seg, r, Self::color(pal, i));
            x += w;
        }
        response
    }
}

/// A rounded bar of the share of a subscription quota that is left (PR #20):
/// green above 20 %, amber above 10 %, red at 10 % or less.
#[must_use = "add it with ui.add(..)"]
pub struct QuotaBar {
    remaining: f64,
    width: Option<f32>,
    height: f32,
}

impl QuotaBar {
    /// A bar for `remaining` percent (0..=100).
    pub fn new(remaining: f64) -> Self {
        Self {
            remaining: if remaining.is_finite() {
                remaining.clamp(0.0, 100.0)
            } else {
                0.0
            },
            width: None,
            height: 6.0,
        }
    }

    /// A fixed width (default: the available width).
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    /// The fill colour for `remaining` percent.
    pub fn color(pal: &Palette, remaining: f64) -> Color32 {
        if remaining <= 10.0 {
            pal.red
        } else if remaining <= 20.0 {
            pal.amber
        } else {
            pal.green
        }
    }
}

impl Widget for QuotaBar {
    fn ui(self, ui: &mut Ui) -> Response {
        let pal = Palette::of(ui.ctx());
        let width = self.width.unwrap_or_else(|| ui.available_width());
        let (rect, response) = ui.allocate_exact_size(vec2(width, self.height), Sense::hover());
        let r = (self.height / 2.0) as u8;
        ui.painter().rect_filled(rect, r, pal.active);
        let share = (self.remaining / 100.0) as f32;
        if share > 0.0 {
            let mut fill = rect;
            fill.set_width((rect.width() * share).max(self.height));
            ui.painter()
                .rect_filled(fill, r, Self::color(pal, self.remaining));
        }
        response
    }
}
