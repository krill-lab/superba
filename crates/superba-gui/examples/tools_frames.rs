//! Isolated M4 screenshots on the same in-memory service used by accessibility tests.
//! Run with HOME and SUPERBA_DATA_DIR pointed at a disposable directory.
#![allow(dead_code)]
#[path = "../tests/support/mod.rs"]
mod support;
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use superba_core::Locale;
use superba_gui::{AppOptions, SuperbaApp, pages::Page};
use support::fake;
struct Capture {
    app: SuperbaApp,
    output: Option<PathBuf>,
    started: Instant,
    row: usize,
    clicked: u8,
    frames: u32,
    selected_at: Option<u32>,
}
impl eframe::App for Capture {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.app.logic(ctx, frame);
    }
    fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        self.app.raw_input_hook(ctx, input);
        self.frames += 1;
        if self.started.elapsed() > Duration::from_millis(750)
            && self.frames >= 10
            && self.clicked < 2
        {
            let row = self.row as f32;
            let pos = egui::pos2(
                330.0,
                173.0 + row * 54.0 + if self.row >= 2 { 28.0 } else { 0.0 },
            );
            input.events.push(egui::Event::PointerMoved(pos));
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: self.clicked == 0,
                modifiers: Default::default(),
            });
            self.clicked += 1;
            if self.clicked == 2 {
                self.selected_at = Some(self.frames);
            }
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.app.ui(ui, frame);
        if self.started.elapsed() > Duration::from_secs(2)
            && self.selected_at.is_some_and(|f| self.frames > f + 6)
            && let Some(path) = self.output.take()
        {
            let ctx = ui.ctx().clone();
            ui.ctx().request_screenshot(move |image| {
                let size =
                    resvg::tiny_skia::IntSize::from_wh(image.width() as u32, image.height() as u32)
                        .unwrap();
                let pixels = image.pixels.iter().flat_map(|p| p.to_array()).collect();
                resvg::tiny_skia::Pixmap::from_vec(pixels, size)
                    .unwrap()
                    .save_png(path)
                    .unwrap();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            });
        }
        if self.output.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
    }
}
fn main() -> eframe::Result {
    let args: Vec<_> = std::env::args().collect();
    let arg = |name: &str| args.windows(2).find(|a| a[0] == name).map(|a| a[1].clone());
    let width: f32 = arg("--width").unwrap_or("1280".into()).parse().unwrap();
    let output = PathBuf::from(arg("--output").expect("--output FILE"));
    let row = arg("--row").unwrap_or("3".into()).parse().unwrap();
    assert!(
        std::env::var_os("SUPERBA_DATA_DIR").is_some(),
        "set an isolated data directory"
    );
    let opts = AppOptions {
        theme: Some(if args.iter().any(|a| a == "--dark") {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        }),
        locale: Some(if args.iter().any(|a| a == "--zh") {
            Locale::ZhCn
        } else {
            Locale::En
        }),
        page: Some(Page::Tools),
        ..Default::default()
    };
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([width, if width < 1000.0 { 640.0 } else { 800.0 }])
            .with_decorations(false),
        ..Default::default()
    };
    eframe::run_native(
        "Superba tools",
        native,
        Box::new(move |cc| {
            cc.egui_ctx.set_pixels_per_point(1.0);
            let app = SuperbaApp::new(
                cc,
                Arc::new(fake::FakeService::new(fake::FakeOptions {
                    fast: true,
                    ..Default::default()
                })),
                opts,
            )?;
            Ok(Box::new(Capture {
                app,
                output: Some(output),
                started: Instant::now(),
                row,
                clicked: 0,
                frames: 0,
                selected_at: None,
            }))
        }),
    )
}
