//! Screenshots of the Models page in router mode, from the real page code on
//! the in-memory service the layout tests use (`tests/support/models_catalog.rs`:
//! Krill and an intranet relay named 「内网」, as in the alpha.7 bug report).
//! Run with HOME and SUPERBA_DATA_DIR pointed at a disposable directory:
//!
//!     models_frames --mode codex|extended|prefixed --width 1280|960 [--dark] [--zh]
//!         [--click X,Y] [--hover X,Y] [--wizard] --output FILE.png
//!
//! `--wizard` shows the setup wizard's "Line & models" step instead, where the
//! router model list is asked.
#![allow(dead_code)]
#[path = "../tests/support/mod.rs"]
mod support;
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use superba_core::{CatalogMode, Locale, SwitchService};
use superba_gui::{AppOptions, SuperbaApp, pages::Page};

struct Capture {
    app: SuperbaApp,
    output: Option<PathBuf>,
    started: Instant,
    click: Option<egui::Pos2>,
    hover: Option<egui::Pos2>,
    clicked: u8,
    frames: u32,
    done_at: Option<u32>,
}

impl eframe::App for Capture {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.app.logic(ctx, frame);
    }
    fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        self.app.raw_input_hook(ctx, input);
        self.frames += 1;
        let ready = self.started.elapsed() > Duration::from_millis(1200) && self.frames >= 10;
        match (self.click, self.hover) {
            (Some(pos), _) if ready && self.clicked < 2 => {
                input.events.push(egui::Event::PointerMoved(pos));
                input.events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: self.clicked == 0,
                    modifiers: Default::default(),
                });
                self.clicked += 1;
                if self.clicked == 2 {
                    self.done_at = Some(self.frames);
                    // Park the pointer away from the popup.
                    input
                        .events
                        .push(egui::Event::PointerMoved(egui::pos2(2.0, 2.0)));
                }
            }
            (None, Some(pos)) if ready && self.done_at.is_none() => {
                // Once: a tooltip waits for the pointer to rest.
                input.events.push(egui::Event::PointerMoved(pos));
                self.done_at = Some(self.frames);
            }
            (None, None) if ready => {
                input.events.push(egui::Event::PointerGone);
                self.done_at.get_or_insert(self.frames);
            }
            _ => {}
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.app.ui(ui, frame);
        // Tooltips need the pointer to rest for a moment.
        let settle = if self.hover.is_some() { 40 } else { 8 };
        if self.done_at.is_some_and(|f| self.frames > f + settle)
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

/// The setup wizard alone, at its "Line & models" step.
struct WizardShot {
    wizard: superba_gui::pages::wizard::Wizard,
    data: superba_gui::pages::Data,
    service: Arc<dyn SwitchService>,
    tr: superba_gui::i18n::Tr,
    fonts: superba_gui::design::fonts::FontReport,
    output: Option<PathBuf>,
    frames: u32,
}

impl eframe::App for WizardShot {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frames += 1;
        let ctx = ui.ctx().clone();
        let cx = superba_gui::pages::Cx {
            tr: &self.tr,
            pal: superba_gui::design::Palette::of(&ctx),
            data: &self.data,
            busy: false,
            now: superba_core::Timestamp::now(),
            fonts: &self.fonts,
            service: &self.service,
            log: &[],
        };
        superba_gui::pages::wizard::show(&ctx, &cx, &mut self.wizard, &mut Vec::new());
        if self.frames > 20
            && let Some(path) = self.output.take()
        {
            let close = ctx.clone();
            ctx.request_screenshot(move |image| {
                let size =
                    resvg::tiny_skia::IntSize::from_wh(image.width() as u32, image.height() as u32)
                        .unwrap();
                let pixels = image.pixels.iter().flat_map(|p| p.to_array()).collect();
                resvg::tiny_skia::Pixmap::from_vec(pixels, size)
                    .unwrap()
                    .save_png(path)
                    .unwrap();
                close.send_viewport_cmd(egui::ViewportCommand::Close);
            });
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

fn point(text: Option<String>) -> Option<egui::Pos2> {
    let text = text?;
    let (x, y) = text.split_once(',')?;
    Some(egui::pos2(x.parse().ok()?, y.parse().ok()?))
}

fn main() -> eframe::Result {
    let args: Vec<_> = std::env::args().collect();
    let arg = |name: &str| args.windows(2).find(|a| a[0] == name).map(|a| a[1].clone());
    let width: f32 = arg("--width").unwrap_or("1280".into()).parse().unwrap();
    let output = PathBuf::from(arg("--output").expect("--output FILE"));
    let mode = arg("--mode")
        .and_then(|m| CatalogMode::parse(&m))
        .unwrap_or_default();
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
        page: Some(Page::Models),
        ..Default::default()
    };
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([width, if width < 1000.0 { 640.0 } else { 800.0 }])
            .with_decorations(false),
        ..Default::default()
    };
    let service = Arc::new(support::fake::FakeService::new(
        support::fake::FakeOptions {
            fast: true,
            ..Default::default()
        },
    ));
    let rt = tokio::runtime::Runtime::new().unwrap();
    let base = rt
        .block_on(service.models_view(Default::default()))
        .unwrap();
    service.set_models_view(support::models_catalog::view(base, mode));
    let click = point(arg("--click"));
    let hover = point(arg("--hover"));
    if args.iter().any(|a| a == "--wizard") {
        let data = rt.block_on(async {
            let mut data = superba_gui::pages::Data {
                sites: service.sites().await.unwrap(),
                ..Default::default()
            };
            data.overview.finish(service.overview().await);
            data.profiles.finish(service.profile_set().await);
            data
        });
        let mut wizard = superba_gui::pages::wizard::Wizard::start(&data);
        wizard.step = superba_gui::pages::wizard::Step::LineModels;
        let theme = opts.theme;
        let locale = opts.locale.unwrap_or(Locale::En);
        return eframe::run_native(
            "Superba",
            native,
            Box::new(move |cc| {
                cc.egui_ctx.set_pixels_per_point(1.0);
                // The system CJK face, as the app finds it.
                let fonts = superba_gui::design::fonts::discover_cjk();
                cc.egui_ctx
                    .set_fonts(superba_gui::design::fonts::definitions(&fonts));
                superba_gui::design::style::install(&cc.egui_ctx, theme);
                Ok(Box::new(WizardShot {
                    wizard,
                    data,
                    service,
                    tr: superba_gui::i18n::Tr::new(locale),
                    fonts,
                    output: Some(output),
                    frames: 0,
                }))
            }),
        );
    }
    eframe::run_native(
        "Superba Models",
        native,
        Box::new(move |cc| {
            cc.egui_ctx.set_pixels_per_point(1.0);
            let app = SuperbaApp::new(cc, service, opts)?;
            Ok(Box::new(Capture {
                app,
                output: Some(output),
                started: Instant::now(),
                click,
                hover,
                clicked: 0,
                frames: 0,
                done_at: None,
            }))
        }),
    )
}
