//! The blocking migration cover. Scene time never drives reported work progress.

mod art;
mod scene;

use std::time::Duration;

use egui::{Align, Area, Id, Layout, Order, Rect, ScrollArea, Sense, UiBuilder, vec2};
use superba_core::{StepStatus, SwitchIntent, SwitchPhase};

use crate::design::widgets::Text;
use crate::flow::{Flow, Run};
use crate::pages::Cx;
use crate::shell::{self, BannerState};

pub(crate) fn show(
    ctx: &egui::Context,
    cx: &Cx<'_>,
    flow: &mut Flow,
    banner: &mut BannerState,
    now: Duration,
) {
    let Some(run) = flow.run() else {
        return;
    };
    let elapsed = now.saturating_sub(run.started);
    let waiting = run.waiting_for_turns();
    let rollback = matches!(
        run.plan.request.intent,
        SwitchIntent::RollbackMigration { .. }
    ) || matches!(
        run.phase,
        SwitchPhase::RollingBack | SwitchPhase::RollbackRestart
    );
    let screen = ctx.content_rect();
    let area = Area::new(Id::unique("migration-cover"))
        .order(Order::Foreground)
        .fixed_pos(screen.min)
        .constrain(false)
        .interactable(true);
    // The app also skips drawing pages and palettes while this cover owns input.
    // Decision modals are drawn after this area and can take the modal layer.
    ctx.memory_mut(|memory| memory.set_modal_layer(area.layer()));
    area.show(ctx, |ui| {
        ui.set_min_size(screen.size());
        ui.painter()
            .rect_filled(screen, 0, cx.pal.bg.gamma_multiply(0.97));
        ui.allocate_rect(screen, Sense::CLICK);

        let width = (screen.width() - 48.0).clamp(240.0, 720.0);
        let height = (if waiting { 600.0_f32 } else { 452.0 }).min(screen.height() - 40.0);
        let column = Rect::from_center_size(screen.center(), vec2(width, height));
        let mut body = ui.new_child(
            UiBuilder::new()
                .max_rect(column)
                .layout(Layout::top_down(Align::Center)),
        );
        ScrollArea::vertical()
            .id_salt("migration-cover-content")
            .max_height(height)
            .auto_shrink([false, true])
            .show(&mut body, |ui| {
                ui.set_width(width);
                ui.vertical_centered(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.label(Text::h2(
                        cx.tr.t(if rollback {
                            "migration.rollback"
                        } else {
                            "migration.title"
                        }),
                        cx.pal,
                    ));
                    ui.add_space(5.0);
                    ui.label(Text::muted(cx.tr.t("migration.subtitle"), cx.pal));
                    ui.add_space(12.0);
                    let scene_height =
                        (width * 280.0 / 720.0).min(if waiting { 190.0 } else { 280.0 });
                    let (rect, _) =
                        ui.allocate_exact_size(vec2(width, scene_height), Sense::hover());
                    scene::paint(ui.painter(), rect, elapsed, cx.pal, cx.tr);
                    ui.add_space(18.0);
                    if let Some(run) = flow.run() {
                        progress(ui, cx, run, elapsed);
                    }
                });
                if waiting {
                    ui.add_space(18.0);
                    // "Wait / force / cancel" normally lives in the page banner.
                    // Keep it inside the cover so waiting never locks the user out.
                    shell::banner(ui, cx, flow, banner, now, (height - 300.0).max(160.0));
                }
            });
    });
    ctx.request_repaint_after(Duration::from_millis(16));
}

fn progress(ui: &mut egui::Ui, cx: &Cx<'_>, run: &Run, elapsed: Duration) {
    let recovering = matches!(
        run.phase,
        SwitchPhase::Recovery | SwitchPhase::RollingBack | SwitchPhase::RollbackRestart
    );
    let stage = if !recovering && !run.waiting_for_turns() {
        run.steps
            .iter()
            .rev()
            .find(|(_, status)| *status == StepStatus::Running)
            .map(|(step, _)| superba_switch::messages::step(step))
            .unwrap_or_else(|| superba_switch::messages::phase(run.phase))
    } else {
        superba_switch::messages::phase(run.phase)
    };
    ui.label(Text::strong(cx.tr.text(&stage), cx.pal));
    ui.add_space(10.0);

    // The backend reports steps, not item-level percentages. An indeterminate
    // activity rail accompanies the real stage and completed-step count.
    let width = ui.available_width().min(360.0);
    let (track, _) = ui.allocate_exact_size(vec2(width, 4.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(track, 2, cx.pal.border);
    let color = if recovering || run.waiting_for_turns() {
        cx.pal.amber
    } else {
        art::CORAL
    };
    let fraction = (elapsed.as_secs_f64() * 1.6).sin() as f32 * 0.5 + 0.5;
    let segment = Rect::from_min_size(
        track.min + vec2((track.width() - width * 0.24) * fraction, 0.0),
        vec2(width * 0.24, track.height()),
    );
    painter.rect_filled(segment, 2, color);

    let done = run
        .steps
        .iter()
        .filter(|(_, status)| *status == StepStatus::Done)
        .count();
    let secs = elapsed.as_secs();
    let time = format!("{}:{:02}", secs / 60, secs % 60);
    ui.add_space(9.0);
    ui.label(Text::small(
        cx.tr
            .f("migration.progress", &[("n", &done), ("time", &time)]),
        cx.pal,
    ));
}
