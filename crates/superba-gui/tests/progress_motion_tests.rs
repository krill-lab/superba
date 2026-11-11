#![allow(missing_docs)]
//! Update progress bars move smoothly (user bug 138): a determinate bar eases
//! between the backend's reports instead of jumping once per report, an
//! indeterminate bar (Superba's own update, verify/install stages) slides
//! every frame instead of at a 10–12 fps timer, and "Reduce motion" keeps
//! both still.

use std::time::Duration;

use egui::{Rect, ViewportId, pos2, vec2};
use superba_gui::design::widgets::InlineProgress;
use superba_gui::design::widgets::progress;
use superba_gui::design::{Palette, fonts, motion, style};

const WIDTH: f32 = 600.0;

fn context() -> egui::Context {
    let ctx = egui::Context::default();
    ctx.set_fonts(fonts::definitions(&fonts::FontReport::default()));
    style::install(&ctx, Some(egui::Theme::Light));
    // The first passes of a context ask for more (fonts, layout); settle them.
    for _ in 0..3 {
        let output = ctx.run_ui(egui::RawInput::default(), |_| {});
        output.drop_without_applying_deltas();
    }
    ctx
}

/// One frame at egui time `time` drawing a bar at `fraction`; `page_poll` asks
/// for the next frame like the Updates page does while something runs.
/// Returns the repaint delay the frame asked for and the fill's left and right
/// edges.
fn bar_frame(
    ctx: &egui::Context,
    time: f64,
    fraction: Option<f32>,
    page_poll: Option<Duration>,
) -> (Duration, Option<(f32, f32)>) {
    let output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(WIDTH, 100.0))),
            time: Some(time),
            ..Default::default()
        },
        |ui| {
            ui.spacing_mut().window_margin = egui::Margin::ZERO;
            egui::Frame::new().show(ui, |ui| {
                ui.set_width(WIDTH);
                ui.add(InlineProgress::new(fraction, "Download progress"));
            });
            if let Some(after) = page_poll {
                ui.ctx().request_repaint_after(after);
            }
        },
    );
    let pal = Palette::for_theme(egui::Theme::Light);
    let still = pal.text_muted.gamma_multiply(0.5);
    let fill = output.shapes.iter().find_map(|s| match &s.shape {
        egui::Shape::Rect(r) if r.fill == pal.text_muted || r.fill == still => {
            Some((r.rect.min.x, r.rect.max.x))
        }
        _ => None,
    });
    let delay = output.viewport_output[&ViewportId::ROOT].repaint_delay;
    output.drop_without_applying_deltas();
    (delay, fill)
}

/// What one simulated run of the event loop showed.
#[derive(Debug)]
struct Run {
    /// (time, fill left, fill right) of every drawn frame.
    frames: Vec<(f64, f32, f32)>,
}

impl Run {
    fn fps(&self) -> f64 {
        let span = self.frames.last().unwrap().0 - self.frames[0].0;
        (self.frames.len() - 1) as f64 / span
    }

    /// Largest jump of the fill's right (determinate) or left (shuttle) edge
    /// between two drawn frames, in pixels.
    fn max_jump(&self, left: bool) -> f32 {
        self.frames
            .windows(2)
            .map(|w| {
                if left {
                    (w[1].1 - w[0].1).abs()
                } else {
                    (w[1].2 - w[0].2).abs()
                }
            })
            .fold(0.0, f32::max)
    }

    fn csv(&self) -> String {
        let mut s = String::from("time_s,fill_left_px,fill_right_px\n");
        for (t, l, r) in &self.frames {
            s.push_str(&format!("{t:.4},{l:.2},{r:.2}\n"));
        }
        s
    }
}

/// Drive the bar the way eframe does in reactive mode: the next frame comes
/// when the last one asked for it, no sooner than one 60 Hz vsync. A backend
/// reports `progress(t)` every 150 ms (both downloaders' REPORT_EVERY) and the
/// Updates page asks for a frame every 100 ms while something runs.
fn simulate(secs: f64, progress: impl Fn(f64) -> Option<f32>) -> Run {
    const VSYNC: f64 = 1.0 / 60.0;
    const REPORT_EVERY: f64 = 0.150;
    let ctx = context();
    let mut t = 0.0;
    let mut frames = Vec::new();
    while t <= secs {
        let reported = (t / REPORT_EVERY).floor() * REPORT_EVERY;
        let (delay, fill) = bar_frame(
            &ctx,
            t,
            progress(reported),
            Some(Duration::from_millis(100)),
        );
        let (l, r) = fill.expect("a fill is drawn");
        frames.push((t, l, r));
        t += delay.as_secs_f64().max(VSYNC);
    }
    Run { frames }
}

/// Before/after evidence for user bug 138: per-frame fill positions of a
/// 6 s download and of the indeterminate shuttle, written as CSV to
/// `SUPERBA_PROGRESS_EVIDENCE` (a directory) when set.
#[test]
fn progress_motion_evidence() {
    let download = simulate(6.0, |t| Some((t / 6.0).min(1.0) as f32));
    let shuttle = simulate(3.0, |_| None);
    let summary = format!(
        "determinate: {} frames, {:.1} fps, largest step {:.1} px\n\
         indeterminate: {} frames, {:.1} fps, largest step {:.1} px\n",
        download.frames.len(),
        download.fps(),
        download.max_jump(false),
        shuttle.frames.len(),
        shuttle.fps(),
        shuttle.max_jump(true),
    );
    eprintln!("{summary}");
    if let Some(dir) = std::env::var_os("SUPERBA_PROGRESS_EVIDENCE") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("determinate.csv"), download.csv()).unwrap();
        std::fs::write(dir.join("indeterminate.csv"), shuttle.csv()).unwrap();
        std::fs::write(dir.join("summary.txt"), &summary).unwrap();
    }
    // Every frame (60 Hz) instead of the page's 100 ms poll, and no step a
    // viewer reads as a jump.
    assert!(download.fps() > 55.0, "{summary}");
    assert!(download.max_jump(false) < 4.0, "{summary}");
    assert!(shuttle.fps() > 55.0, "{summary}");
    assert!(shuttle.max_jump(true) < 4.0, "{summary}");
    // The fill keeps up with the reports: at most a few hundred ms behind.
    let (t, _, right) = download.frames[download.frames.len() / 2];
    let behind = (t / 6.0) as f32 - right / WIDTH;
    assert!((0.0..0.1).contains(&behind), "{behind} behind at {t}");
}

#[test]
fn a_new_bar_starts_at_its_report_and_never_slides_back() {
    let first = progress::ease(None, 0.4, 1.0, WIDTH, false);
    assert_eq!(first.shown, 0.4);
    // A higher report is approached, not jumped to.
    let next = progress::ease(Some(first), 0.6, 1.016, WIDTH, false);
    assert!(next.shown > 0.4 && next.shown < 0.45, "{next:?}");
    // The next file (or a retry) starts lower: shown at once.
    let lower = progress::ease(Some(next), 0.1, 1.032, WIDTH, false);
    assert_eq!(lower.shown, 0.1);
    // Out-of-range reports are clamped.
    assert_eq!(progress::ease(None, 1.5, 0.0, WIDTH, false).shown, 1.0);
}

#[test]
fn the_fill_catches_up_and_then_stops_asking_for_frames() {
    let mut fill = progress::ease(None, 0.0, 0.0, WIDTH, false);
    let mut t = 0.0;
    let mut frames = 0;
    while fill.shown < 1.0 {
        t += 1.0 / 60.0;
        frames += 1;
        fill = progress::ease(Some(fill), 1.0, t, WIDTH, false);
        assert!(frames < 120, "not caught up after 2 s: {fill:?}");
    }
    // About seven time constants to come within a quarter pixel of 600 px.
    assert!(t < 2.0 && t > 1.0, "{t}");

    // A drawn bar asks for frames while it moves and none once it is there.
    let ctx = context();
    let (delay, _) = bar_frame(&ctx, 0.0, Some(0.2), None);
    assert_eq!(delay, Duration::MAX, "a bar at its report asks for nothing");
    let (delay, _) = bar_frame(&ctx, 0.016, Some(0.8), None);
    assert!(delay <= motion::FRAME, "{delay:?}");
    let mut t = 0.016;
    let mut last = (Duration::ZERO, None);
    for _ in 0..200 {
        t += 1.0 / 60.0;
        last = bar_frame(&ctx, t, Some(0.8), None);
    }
    assert_eq!(last.0, Duration::MAX);
    assert_eq!(last.1, Some((0.0, 0.8 * WIDTH)));
}

#[test]
fn the_shuttle_asks_for_every_frame() {
    let ctx = context();
    let (delay, a) = bar_frame(&ctx, 1.0, None, None);
    assert!(delay <= motion::FRAME, "{delay:?}");
    let (_, b) = bar_frame(&ctx, 1.0 + 1.0 / 60.0, None, None);
    assert_ne!(a, b, "the shuttle moves between frames");
    let (l, r) = b.unwrap();
    assert!(l >= 0.0 && r <= WIDTH + 0.01, "{l}..{r}");
    assert!((r - l - 0.3 * WIDTH).abs() < 0.01);
}

#[test]
fn reduce_motion_shows_reports_as_they_are_and_keeps_the_shuttle_still() {
    let ctx = context();
    motion::set_reduced(&ctx, true);
    let (_, fill) = bar_frame(&ctx, 0.0, Some(0.2), None);
    assert_eq!(fill, Some((0.0, 0.2 * WIDTH)));
    let (delay, fill) = bar_frame(&ctx, 0.016, Some(0.7), None);
    assert_eq!(fill, Some((0.0, 0.7 * WIDTH)), "no easing");
    assert_eq!(delay, Duration::MAX, "nothing moves, no frames asked");
    let (delay, a) = bar_frame(&ctx, 1.0, None, None);
    let (_, b) = bar_frame(&ctx, 2.0, None, None);
    assert_eq!(delay, Duration::MAX);
    assert_eq!(a, b, "the shuttle stands still");
    assert_eq!(a, Some((0.0, WIDTH)), "a dimmed full track reads as busy");
    motion::set_reduced(&ctx, false);
    assert!(!motion::reduced(&ctx));
}

#[test]
fn a_minimised_window_asks_for_no_animation_frames() {
    let ctx = context();
    let mut viewports = egui::ViewportIdMap::default();
    viewports.insert(
        ViewportId::ROOT,
        egui::ViewportInfo {
            minimized: Some(true),
            ..Default::default()
        },
    );
    let output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(WIDTH, 100.0))),
            time: Some(1.0),
            viewports,
            ..Default::default()
        },
        |ui| {
            ui.add(InlineProgress::new(None, "Download progress"));
        },
    );
    let delay = output.viewport_output[&ViewportId::ROOT].repaint_delay;
    output.drop_without_applying_deltas();
    assert_eq!(delay, Duration::MAX);
}
