//! Progress bars (user bug 138: they moved in visible jumps).
//!
//! The downloaders report every 150 ms and the Updates page polls them every
//! 100 ms, so a bar drawn straight from the latest report advanced in uneven
//! 100/200 ms steps of several pixels, and the indeterminate shuttle was drawn
//! on an 80 ms timer (about 12 frames a second). Every bar here is painted by
//! [`paint_bar`]: a determinate fill eases towards the latest report and asks
//! for the next frame until it has caught up, the shuttle asks for every
//! frame, and "Reduce motion" ([`motion::reduced`]) shows reports as they are
//! and keeps the shuttle still.
use crate::design::{a11y, motion, tokens::Palette};
use egui::{Color32, Id, Rect, Response, Sense, Ui, Widget, pos2, vec2};

/// How fast a fill catches up with a new report: it covers 1 - 1/e of the
/// distance in this time. Long enough to bridge the 150 ms between reports
/// without stopping, short enough that the bar never trails visibly.
pub const CATCH_UP_SECS: f32 = 0.25;

/// A fill within this many pixels of its report has caught up.
const SETTLE_PX: f32 = 0.25;

/// The longest time between two frames the easing accounts for: after a
/// stall (a window shown again) the fill catches up, it does not teleport.
const MAX_STEP_SECS: f64 = 0.25;

/// Share of the track the indeterminate shuttle covers.
const SHUTTLE: f32 = 0.3;

/// What a determinate bar showed last (kept per bar in egui's memory).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fill {
    /// Fraction drawn (0..=1).
    pub shown: f32,
    /// The report it was easing towards.
    pub target: f32,
    /// egui time it was drawn at.
    pub time: f64,
}

/// The fill to draw at `time` for the latest report `target` after `prev`, on
/// a bar `width` pixels wide. A new bar starts at its report; a report below
/// the fill (the next file, a retry) is shown at once, never slid back to;
/// with `reduce_motion` every report is shown as it is.
pub fn ease(prev: Option<Fill>, target: f32, time: f64, width: f32, reduce_motion: bool) -> Fill {
    let target = target.clamp(0.0, 1.0);
    let shown = match prev {
        Some(prev) if !reduce_motion && target > prev.shown => {
            let mut dt = (time - prev.time).clamp(0.0, MAX_STEP_SECS);
            if prev.shown >= prev.target {
                // The bar was at rest, asking for no frames: the time since
                // it was drawn was not spent moving. Start from one frame.
                dt = dt.min(motion::FRAME.as_secs_f64());
            }
            let dt = dt as f32;
            let shown = prev.shown + (target - prev.shown) * (1.0 - (-dt / CATCH_UP_SECS).exp());
            if (target - shown) * width.max(1.0) <= SETTLE_PX {
                target
            } else {
                shown
            }
        }
        _ => target,
    };
    Fill {
        shown,
        target,
        time,
    }
}

/// Left edge of the indeterminate shuttle at `time`, as a share of the track
/// (0..=1 - its own width): it swings from end to end and back.
pub fn shuttle(time: f64) -> f32 {
    ((time.sin() as f32) + 1.0) * 0.5 * (1.0 - SHUTTLE)
}

/// Colors and shape of a bar.
#[derive(Clone, Copy, Debug)]
pub struct BarLook {
    /// The empty track.
    pub track: Color32,
    /// The fill and the shuttle.
    pub fill: Color32,
    /// Corner radius.
    pub radius: u8,
    /// Narrowest visible fill (a round cap at 0 %), in pixels.
    pub min_fill: f32,
}

/// Paint a progress bar over `rect`: the fill eased towards `fraction` (its
/// state kept under `id`), or the indeterminate shuttle while the fraction is
/// unknown. Asks for the next frame while something moves.
pub fn paint_bar(ui: &Ui, id: Id, rect: Rect, fraction: Option<f32>, look: BarLook) {
    let ctx = ui.ctx();
    let reduce = motion::reduced(ctx);
    let time = ui.input(|i| i.time);
    let width = rect.width();
    ui.painter().rect_filled(rect, look.radius, look.track);
    let (fill, color) = if let Some(target) = fraction.filter(|f| f.is_finite()) {
        let prev = ctx.data(|d| d.get_temp::<Fill>(id));
        let next = ease(prev, target, time, width, reduce);
        ctx.data_mut(|d| d.insert_temp(id, next));
        if next.shown < target.clamp(0.0, 1.0) {
            motion::next_frame(ctx);
        }
        let mut fill = rect;
        fill.set_width((width * next.shown).max(look.min_fill).min(width));
        (fill, look.fill)
    } else {
        // A later determinate stage starts at its own report.
        ctx.data_mut(|d| d.remove::<Fill>(id));
        if reduce {
            (rect, look.fill.gamma_multiply(0.5))
        } else {
            motion::next_frame(ctx);
            let left = rect.min.x + shuttle(time) * width;
            let fill = Rect::from_min_max(
                pos2(left, rect.min.y),
                pos2(left + SHUTTLE * width, rect.max.y),
            );
            (fill, look.fill)
        }
    };
    ui.painter().rect_filled(fill, look.radius, color);
}

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
        let fraction = self.fraction.filter(|f| f.is_finite());
        if let Some(f) = fraction {
            // Assistive technology reads the report, not the eased fill.
            let f = f.clamp(0.0, 1.0);
            ui.ctx().accesskit_node_builder(response.id, |n| {
                n.set_numeric_value(f as f64);
                n.set_min_numeric_value(0.0);
                n.set_max_numeric_value(1.0);
            });
        }
        paint_bar(
            ui,
            response.id,
            track,
            fraction,
            BarLook {
                track: p.hover,
                fill: p.text_muted,
                radius: 2,
                min_fill: 0.0,
            },
        );
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
