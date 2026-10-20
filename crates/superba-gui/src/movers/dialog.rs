//! The session migration dialog: the scene on its own paper card, then the real
//! progress of the switch's session migration (or of putting sessions back):
//! "Migrated N of M sessions", a bar, the stage, the session being worked on,
//! the elapsed time, and "Stop and undo" only while the migration can stop
//! safely (otherwise a note that it finishes on its own).

use std::time::Duration;

use egui::{Align, Color32, Layout, Rect, Sense, Stroke, StrokeKind, Ui, vec2};
use superba_core::{SessionDirection, SessionProgress, SessionStage};
use superba_switch::session_events::{session_count, session_percent, session_stage};

use super::pose::Pose;
use super::render::{self, SceneCache, View};
use crate::design::tokens::Palette;
use crate::design::widgets::{Button, Text, dialog, dialog_body};
use crate::flow::{Flow, SessionsRun};
use crate::pages::Cx;

/// Widest dialog.
const MAX_WIDTH: f32 = 720.0;
/// Narrowest scene.
const MIN_SCENE_WIDTH: f32 = 320.0;
/// The artwork's progress colour (`.progress` fill).
const CORAL: Color32 = Color32::from_rgb(0xE9, 0x90, 0x85);
/// Height of everything below the scene (progress, notes, button) and the
/// dialog's own padding, for fitting the scene into short windows.
const CHROME_HEIGHT: f32 = 260.0;

/// State of the dialog between frames: the scene's textures and clock.
#[derive(Debug, Default)]
pub struct MoversDialog {
    cache: SceneCache,
    /// egui time when the scene appeared.
    shown_at: Option<f64>,
}

impl MoversDialog {
    /// Rasterizations so far (tests).
    pub fn renders(&self) -> u32 {
        self.cache.renders
    }
}

/// What the dialog shows, taken from the flow before drawing.
struct Shown {
    sessions: SessionsRun,
    elapsed: Duration,
}

/// Draw the dialog while the running switch moves sessions; nothing otherwise.
/// Returns whether it was shown.
pub fn show(
    ctx: &egui::Context,
    cx: &Cx<'_>,
    flow: &mut Flow,
    state: &mut MoversDialog,
    now: Duration,
    reduce_motion: bool,
) -> bool {
    let Some(view) = flow.run().and_then(|run| {
        run.sessions_active().map(|s| Shown {
            sessions: s.clone(),
            elapsed: now.saturating_sub(s.started),
        })
    }) else {
        if state.shown_at.take().is_some() {
            state.cache.clear();
        }
        return false;
    };
    let pal = cx.pal;
    let tr = cx.tr;
    let screen = ctx.content_rect();
    let width = (screen.width() - 96.0).clamp(MIN_SCENE_WIDTH, MAX_WIDTH);
    let time = ctx.input(|i| i.time);
    let started = *state.shown_at.get_or_insert(time);
    let pose = if reduce_motion {
        Pose::still()
    } else if let Some(t) = frozen_time() {
        Pose::at(t)
    } else {
        Pose::at(time - started)
    };
    let mut stop = false;
    let p = &view.sessions.progress;
    let _ = dialog(
        ctx,
        egui::Id::unique("superba-session-movers"),
        width,
        |ui| {
            let title = if view.sessions.stop_requested {
                "movers.title.stopping"
            } else if p.direction == SessionDirection::Restore {
                "movers.title.restore"
            } else {
                "movers.title.migrate"
            };
            ui.label(Text::h2(tr.t(title), pal));
            dialog_body(ui, |ui| {
                let scene_w = width
                    .min((screen.height() - CHROME_HEIGHT) * render::aspect())
                    .max(MIN_SCENE_WIDTH.min(width));
                scene_card(ui, pal, state, &pose, scene_w, &|key| tr.t(key));
                ui.add_space(4.0);
                progress(ui, cx, p, view.elapsed);
            });
            ui.add_space(4.0);
            stop = controls(ui, cx, &view.sessions);
        },
    );
    if stop {
        flow.stop_sessions();
    }
    if let Some(after) = super::repaint_after(ctx.input(|i| i.viewport().visible()), reduce_motion)
    {
        ctx.request_repaint_after(after);
    }
    true
}

/// `SUPERBA_SCENE_TIME=<seconds>` holds the scene at that moment of its
/// cycle (screenshots compared with the SVG played by a browser).
fn frozen_time() -> Option<f64> {
    static FROZEN: std::sync::LazyLock<Option<f64>> = std::sync::LazyLock::new(|| {
        std::env::var("SUPERBA_SCENE_TIME")
            .ok()
            .and_then(|v| v.trim().parse().ok())
    });
    *FROZEN
}

/// The scene on its paper card, `width` wide (centred), aspect kept.
fn scene_card(
    ui: &mut Ui,
    pal: &Palette,
    state: &mut MoversDialog,
    pose: &Pose,
    width: f32,
    translate: &dyn Fn(&str) -> String,
) {
    let size = render::size();
    let height = width * size.1 / size.0;
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let rect = Rect::from_center_size(row.center(), vec2(width, height));
    if !ui.is_rect_visible(rect) {
        return;
    }
    let view = View::fit(rect, size, ui.ctx().pixels_per_point());
    let painter = ui.painter_at(rect.expand(1.0));
    if render::scene().is_some() {
        render::paint(&painter, &mut state.cache, view, pose, translate);
    } else {
        render::paint_placeholder(&painter, rect);
    }
    // The paper's own rounded corners (rx 38), outlined like a card.
    let radius = (38.0 * view.scale).round().clamp(0.0, 255.0) as u8;
    painter.rect_stroke(
        view.rect(size),
        radius,
        Stroke::new(1.0, pal.border),
        StrokeKind::Outside,
    );
}

/// The stage with its counter ("Reading 200 of 600 sessions", "Migrated 120
/// of 600 sessions") and the overall percentage of the same weighting, the
/// elapsed time, the bar, and the session being worked on.
fn progress(ui: &mut Ui, cx: &Cx<'_>, p: &SessionProgress, elapsed: Duration) {
    let pal = cx.pal;
    let tr = cx.tr;
    let fraction = p.fraction();
    ui.horizontal(|ui| {
        let count = if p.counter().1 == 0 && p.stage == SessionStage::Reading {
            tr.t("movers.preparing")
        } else {
            tr.text(&session_count(p))
        };
        ui.label(Text::strong(count, pal));
        ui.label(Text::muted(format!("· {}%", session_percent(p)), pal));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(Text::mono_muted(
                tr.f("movers.elapsed", &[("time", &clock(elapsed))]),
                pal,
            ));
        });
    });
    bar(ui, pal, fraction);
    // The counter names the stage already.
    let line = match current(p) {
        Some(name) => tr.f("movers.now", &[("name", &name)]),
        None => tr.text(&session_stage(p)),
    };
    ui.add(egui::Label::new(Text::small(line, pal)).truncate());
}

/// The session being worked on: "title — file name", either alone, or `None`.
fn current(p: &SessionProgress) -> Option<String> {
    let c = p.current.as_ref()?;
    let title = c
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "));
    let file = c
        .file
        .as_deref()
        .and_then(std::path::Path::file_name)
        .map(|n| n.to_string_lossy().into_owned());
    match (title, file) {
        (Some(t), Some(f)) => Some(format!("{t} — {f}")),
        (Some(t), None) => Some(t),
        (None, Some(f)) => Some(f),
        (None, None) => None,
    }
}

/// `m:ss`.
fn clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

/// A thin progress bar in the artwork's coral.
fn bar(ui: &mut Ui, pal: &Palette, fraction: f32) {
    let h = 6.0;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let r = (h / 2.0) as u8;
    ui.painter().rect_filled(rect, r, pal.active);
    let mut fill = rect;
    fill.set_width((rect.width() * fraction.clamp(0.0, 1.0)).max(h));
    ui.painter().rect_filled(fill, r, CORAL);
}

/// "Stop and undo" (returns whether it was clicked) or why there is none.
fn controls(ui: &mut Ui, cx: &Cx<'_>, s: &SessionsRun) -> bool {
    let pal = cx.pal;
    let tr = cx.tr;
    let p = &s.progress;
    let mut clicked = false;
    if s.stop_requested {
        ui.add(egui::Label::new(Text::small(tr.t("movers.stopping"), pal)).wrap());
    } else if s.can_stop() {
        ui.horizontal_wrapped(|ui| {
            clicked = ui.add(Button::new(tr.t("movers.stop")).small()).clicked();
            ui.add(egui::Label::new(Text::small(tr.t("movers.stop_hint"), pal)).wrap());
        });
    } else {
        let key = if p.direction == SessionDirection::Restore {
            "movers.no_stop.restore"
        } else {
            "movers.no_stop.finishing"
        };
        ui.add(egui::Label::new(Text::small(tr.t(key), pal)).wrap());
    }
    ui.label(Text::caption(tr.t("movers.keep_open"), pal));
    clicked
}
