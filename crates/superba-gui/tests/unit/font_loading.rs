use super::*;
use crate::design::fonts::{CjkFace, bundled_definitions};
use std::{sync::mpsc, time::Duration};

fn report(family: &str) -> FontReport {
    FontReport {
        cjk_regular: Some(CjkFace {
            family: family.into(),
            path: "test.ttf".into(),
            index: 0,
            weight: 400,
        }),
        ..Default::default()
    }
}

#[test]
fn first_frame_renders_while_worker_waits_then_repaint_installs_all_weights_once() {
    let ctx = Context::default();
    let fallback = report("fallback");
    let mut fonts = InstalledFonts::new(&ctx, fallback.clone(), bundled_definitions());
    let (release, waiting) = mpsc::channel();
    let (repaint, repaints) = mpsc::channel();
    let mut preferred = report("converted");
    preferred.cjk_medium = preferred.cjk_regular.clone();
    preferred.cjk_bold = preferred.cjk_regular.clone();
    let expected = preferred.clone();
    fonts.defer(&ctx, move || {
        waiting.recv_timeout(Duration::from_secs(10)).unwrap();
        Some((preferred, bundled_definitions()))
    });
    ctx.run_ui(egui::RawInput::default(), |ui| {
        fonts.poll(ui.ctx());
        ui.label("Superba 123");
        assert_eq!(fonts.report(), &fallback);
    })
    .drop_without_applying_deltas();
    ctx.set_repaint_observer(move |_| {
        let _ = repaint.send(());
    });
    release.send(()).unwrap();
    repaints.recv_timeout(Duration::from_secs(10)).unwrap();
    ctx.run_ui(egui::RawInput::default(), |ui| {
        fonts.poll(ui.ctx());
        assert_eq!(fonts.report(), &expected);
        ui.label("Superba 123");
    })
    .drop_without_applying_deltas();
    assert!(fonts.pending.is_none());
    fonts.poll(&ctx);
    assert_eq!(fonts.report(), &expected);
}

#[test]
fn conversion_failure_keeps_the_fallback() {
    let ctx = Context::default();
    let fallback = report("fallback");
    let mut fonts = InstalledFonts::new(&ctx, fallback.clone(), bundled_definitions());
    let (tx, rx) = mpsc::channel();
    fonts.defer(&ctx, move || {
        tx.send(()).unwrap();
        None
    });
    rx.recv_timeout(Duration::from_secs(10)).unwrap();
    // Receiving the completion signal can precede dropping the worker's sender.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while fonts.pending.is_some() {
        fonts.poll(&ctx);
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(fonts.report(), &fallback);
}
