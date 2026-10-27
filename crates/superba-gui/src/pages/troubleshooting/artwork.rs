//! The migration shrimp, with SVG layers animated by the shared scene renderer.
use crate::movers::{pose::Pose, render, svg};
use egui::{Color32, Rect, Ui, vec2};
use std::sync::LazyLock;

pub(super) fn advice(ui: &Ui, rect: Rect, cache: &mut render::SceneCache) {
    static ADVICE: LazyLock<Option<svg::Scene>> = LazyLock::new(|| {
        svg::analyse(include_str!(
            "../../../../../assets/scenes/superba-diagnostic-apology.svg"
        ))
        .ok()
    });
    if let Some(scene) = ADVICE.as_ref() {
        render::paint_scene(
            ui.painter(),
            cache,
            scene,
            render::View::fit(rect, scene.size, ui.ctx().pixels_per_point()),
            &Pose::still(),
            &str::to_owned,
        );
    }
}

pub(super) fn paint(
    ui: &Ui,
    rect: Rect,
    cache: &mut render::SceneCache,
    happy: bool,
    reduced: bool,
) {
    static WORK: LazyLock<Option<svg::Scene>> = LazyLock::new(|| {
        svg::analyse(include_str!(
            "../../../../../assets/scenes/superba-diagnostic-worker.svg"
        ))
        .ok()
    });
    static HAPPY: LazyLock<Option<svg::Scene>> = LazyLock::new(|| {
        svg::analyse(include_str!(
            "../../../../../assets/scenes/superba-diagnostic-celebration.svg"
        ))
        .ok()
    });
    let scene = if happy { &HAPPY } else { &WORK };
    let time = ui.input(|i| i.time);
    let mut pose = Pose::still();
    if !reduced {
        let phase = (time * std::f64::consts::TAU / if happy { 0.9 } else { 1.6 }) as f32;
        pose.breathe_deg = phase.sin() * if happy { 5.0 } else { 4.0 };
        if happy {
            pose.bob_y = -34.0 * (phase.sin() * 0.5 + 0.5);
        } else {
            pose.breathe_y = -2.5 * (phase.sin() + 1.0);
            let tap = (time * std::f64::consts::TAU / 0.4).sin() as f32;
            pose.sip = (7.0 * tap, -9.0 * tap, -14.0 * tap);
        }
    }
    if let Some(scene) = scene.as_ref() {
        render::paint_scene(
            ui.painter(),
            cache,
            scene,
            render::View::fit(rect, scene.size, ui.ctx().pixels_per_point()),
            &pose,
            &str::to_owned,
        );
    }
    if happy {
        let scale = rect.width().min(rect.height() * 4.0 / 3.0) / 480.0;
        let origin = rect.center() - vec2(240.0, 180.0) * scale;
        let colors = [
            Color32::from_rgb(255, 105, 126),
            Color32::from_rgb(245, 197, 74),
            Color32::from_rgb(91, 195, 168),
            Color32::from_rgb(130, 143, 227),
        ];
        for side in [-1.0, 1.0] {
            for i in 0..24 {
                let age = if reduced {
                    (i as f32 + 1.0) / 25.0
                } else {
                    ((time as f32 / 1.8) + i as f32 / 24.0).fract()
                };
                let speed = 90.0 + (i % 7) as f32 * 19.0;
                let x = 240.0 + side * (160.0 - age * speed);
                let y = 270.0 - age * (330.0 + (i % 5) as f32 * 22.0) + 310.0 * age * age;
                let at = origin + vec2(x, y) * scale;
                let angle = age * 12.0 + i as f32;
                ui.painter().line_segment(
                    [at, at + vec2(angle.cos(), angle.sin()) * (9.0 * scale)],
                    egui::Stroke::new(3.0 * scale, colors[i % colors.len()]),
                );
            }
        }
    }
    if !reduced
        && let Some(wait) =
            crate::movers::repaint_after(ui.input(|i| i.viewport().visible()), false)
    {
        ui.ctx().request_repaint_after(wait);
    }
}
