//! A blank startup screen until the first state snapshot and recovery settle.
//! Its SVG reuses the migration shrimp and cached, GPU-transformed scene layers.

use std::f32::consts::TAU;
use std::sync::LazyLock;

use egui::{Rect, Sense, Ui, pos2, vec2};

use crate::design::fonts::{Weight, font};
use crate::design::{
    a11y,
    tokens::{Palette, text},
};
use crate::i18n::Tr;
use crate::movers::{self, pose::Pose, render, svg};

/// Standalone animated SVG, using the same shrimp symbol as session migration.
pub const SCENE_SVG: &str = include_str!("../../../assets/scenes/superba-startup-inspection.svg");

/// Parsed artwork, shared between windows; textures belong to each screen.
pub fn scene() -> Option<&'static svg::Scene> {
    static SCENE: LazyLock<Option<svg::Scene>> = LazyLock::new(|| {
        svg::analyse(SCENE_SVG)
            .inspect_err(|e| tracing::warn!(error = %e, "startup scene could not be read"))
            .ok()
    });
    SCENE.as_ref()
}

/// The inspection SVG's three loops, using the shared layer renderer's poses.
/// All loops join without a jump; reduced motion keeps the neutral frame.
pub fn pose(time: f64, reduced: bool) -> Pose {
    let mut pose = Pose::still();
    if !reduced {
        let phase = |period: f64| (time.rem_euclid(period) / period) as f32 * TAU;
        pose.breathe_y = (phase(4.8).cos() - 1.0) * 1.5;
        pose.breathe_deg = -1.2 * phase(4.8).cos();
        pose.sip.2 = -8.0 * phase(3.4).cos();
        pose.dot = 0.69 - 0.31 * phase(2.0).cos();
    }
    pose
}

/// One launch's readiness and animation cache. Later refreshes never reopen it.
#[derive(Debug, Default)]
pub struct StartupScreen {
    snapshot_received: bool,
    recovery_received: bool,
    started: Option<f64>,
    cache: render::SceneCache,
}

impl StartupScreen {
    /// The initial state and crash recovery must both finish before revealing UI.
    pub fn pending(&self) -> bool {
        !self.snapshot_received || !self.recovery_received
    }

    /// Refresh sends profiles and sites before its overview, including errors.
    pub fn snapshot_received(&mut self) {
        self.snapshot_received = true;
        self.release_if_ready();
    }

    /// An unsuccessful recovery is still a completed attempt, reported by the UI.
    pub fn recovery_received(&mut self) {
        self.recovery_received = true;
        self.release_if_ready();
    }

    fn release_if_ready(&mut self) {
        if !self.pending() {
            self.cache.clear();
        }
    }

    /// The SVG and one status line appear on the plain background.
    pub fn show(&mut self, ui: &mut Ui, tr: &Tr, reduced: bool) {
        let ctx = ui.ctx().clone();
        let screen = ui.max_rect();
        let pal = Palette::of(&ctx);
        ui.painter().rect_filled(screen, 0, pal.bg);
        let width = (screen.width() - 64.0).clamp(0.0, 420.0);
        let height = (screen.height() - 96.0).clamp(0.0, width * 380.0 / 480.0);
        let canvas = Rect::from_center_size(screen.center() - vec2(0.0, 20.0), vec2(width, height));
        let response = ui.interact(canvas, egui::Id::unique("startup-scene"), Sense::hover());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::accesskit::Role::Image, true, tr.t("overview.loading"))
        });
        let now = ctx.input(|i| i.time);
        let elapsed = now - *self.started.get_or_insert(now);
        if let Some(scene) = scene() {
            let view = render::View::fit(canvas, scene.size, ctx.pixels_per_point());
            render::paint_scene(
                ui.painter(),
                &mut self.cache,
                scene,
                view,
                &pose(elapsed, reduced),
                &str::to_owned,
            );
        }
        let status = tr.t("overview.startup_loading");
        let galley = ui.painter().layout_no_wrap(
            status.clone(),
            font(text::BODY, Weight::Regular),
            pal.text_muted,
        );
        let label = Rect::from_min_size(
            pos2(
                canvas.center().x - galley.size().x / 2.0,
                canvas.bottom() + 16.0,
            ),
            galley.size(),
        );
        ui.painter().galley(label.min, galley, pal.text_muted);
        let response = ui.interact(label, egui::Id::unique("startup-status"), Sense::hover());
        a11y::text(&response, &status);
        if !reduced
            && let Some(wait) = movers::repaint_after(ctx.input(|i| i.viewport().visible()), false)
        {
            ctx.request_repaint_after(wait);
        }
    }
}
