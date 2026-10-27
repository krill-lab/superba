//! Title-bar mode toggle and a full-window ripple that covers the content swap.
use crate::design::{icons::Icon, tokens::Palette, widgets::Segmented};
use crate::pages::{Action, Cx};
use egui::{Context, Id, Order, Pos2, Rect, Sense, Stroke, pos2};
use std::time::Duration;

const COVER: f64 = 0.42;
const END: f64 = 0.88;

/// Active transition; changing modes is committed only after the ripple covers the window.
#[derive(Debug, Default)]
pub struct Transition {
    ripple: Option<Ripple>,
}
#[derive(Debug)]
struct Ripple {
    began: f64,
    origin: Pos2,
    target: bool,
    committed: bool,
    reduced: bool,
}

impl Transition {
    /// Whether a transition is still running.
    pub fn active(&self) -> bool {
        self.ripple.is_some()
    }

    /// The segment selected while the full-window transition is in progress.
    pub fn target(&self) -> Option<bool> {
        self.ripple.as_ref().map(|r| r.target)
    }

    /// Begin from the title-bar control. Repeated clicks cannot restart the effect.
    pub fn start(&mut self, ctx: &Context, target: bool, reduced: bool) {
        if self.active() {
            return;
        }
        self.ripple = Some(Ripple {
            began: ctx.input(|i| i.time),
            origin: ctx
                .data(|d| d.get_temp::<Pos2>(Id::unique("mode-toggle-origin")))
                .unwrap_or(pos2(70.0, 16.0)),
            target,
            committed: false,
            reduced,
        });
        ctx.request_repaint();
    }

    /// Poll before drawing content; return the mode once, at full coverage.
    pub fn advance(&mut self, ctx: &Context) -> Option<bool> {
        let ripple = self.ripple.as_mut()?;
        let elapsed = ctx.input(|i| i.time) - ripple.began;
        if !ripple.committed && (ripple.reduced || elapsed >= COVER) {
            ripple.committed = true;
            return Some(ripple.target);
        }
        if ripple.reduced || elapsed >= END {
            self.ripple = None;
        }
        None
    }

    /// Paint the water ripple above the content, blocking clicks until it fades.
    pub fn paint(&self, ctx: &Context, pal: &Palette) {
        let Some(ripple) = &self.ripple else {
            return;
        };
        if ripple.reduced {
            return;
        }
        let elapsed = (ctx.input(|i| i.time) - ripple.began).max(0.0);
        let progress = (elapsed / COVER).clamp(0.0, 1.0) as f32;
        let fade = if elapsed <= COVER {
            1.0
        } else {
            (1.0 - (elapsed - COVER) / (END - COVER)).clamp(0.0, 1.0) as f32
        };
        let screen = ctx.content_rect();
        let radius = cover_radius(screen, ripple.origin) * (1.0 - (1.0 - progress).powi(3));
        egui::Area::new(Id::unique("mode-ripple"))
            .order(Order::Tooltip)
            .fixed_pos(screen.min)
            .constrain(false)
            .movable(false)
            .show(ctx, |ui| {
                ui.allocate_exact_size(screen.size(), Sense::click_and_drag());
                let painter = ui.painter().with_clip_rect(screen);
                let tint = pal.surface.lerp_to_gamma(pal.link, 0.08);
                for ring in (1..=5).rev() {
                    painter.circle_filled(
                        ripple.origin,
                        radius + ring as f32 * 5.0,
                        pal.link.gamma_multiply(fade * 0.015),
                    );
                }
                painter.circle_filled(ripple.origin, radius, tint.gamma_multiply(fade));
                for ring in 0..3 {
                    let r = radius - ring as f32 * 16.0;
                    if r > 0.0 {
                        painter.circle_stroke(
                            ripple.origin,
                            r,
                            Stroke::new(
                                1.0 + ring as f32 * 0.5,
                                pal.link
                                    .gamma_multiply(fade * (1.0 - progress * 0.8) * 0.14),
                            ),
                        );
                    }
                }
            });
        ctx.request_repaint_after(Duration::from_millis(16));
    }
}

/// Radius that reaches every corner, including when the window is resized.
pub fn cover_radius(screen: Rect, origin: Pos2) -> f32 {
    [
        screen.left_top(),
        screen.right_top(),
        screen.left_bottom(),
        screen.right_bottom(),
    ]
    .into_iter()
    .map(|p| p.distance(origin))
    .fold(0.0_f32, f32::max)
        + 2.0
}

/// Sliding mode selector, simple on the left and full on the right.
pub fn title_button(
    ctx: &Context,
    cx: &Cx<'_>,
    simple: bool,
    enabled: bool,
    out: &mut Vec<Action>,
) {
    let screen = ctx.content_rect();
    let offset = if cfg!(target_os = "macos") {
        egui::vec2(88.0, 8.0)
    } else {
        egui::vec2(12.0, 2.0)
    };
    egui::Area::new(Id::unique("title-mode-switch"))
        .order(Order::Foreground)
        .fixed_pos(screen.min + offset)
        .constrain(false)
        .movable(false)
        .show(ctx, |ui| {
            let mut selected = simple;
            let response = ui
                .add_enabled_ui(enabled, |ui| {
                    Segmented::new("title-mode-segments", &mut selected)
                        .label(cx.tr.t("simple.mode_selector"))
                        .small()
                        .option(true, cx.tr.t("simple.short_simple"))
                        .icon(Icon::Overview)
                        .option(false, cx.tr.t("simple.short_full"))
                        .icon(Icon::Settings)
                        .show(ui)
                })
                .inner;
            ctx.data_mut(|d| {
                d.insert_temp(Id::unique("mode-toggle-origin"), response.rect.center())
            });
            if enabled && response.changed() && selected != simple {
                out.push(Action::SetSimpleMode(selected));
            }
        });
}
