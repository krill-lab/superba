//! # superba-gui
//!
//! The Superba desktop GUI: eframe/egui (wgpu with Direct3D 12 on Windows, glow
//! elsewhere; [`startup`]) with a Vercel Geist look, en + zh-CN, dark-first with
//! a light theme (plan 11).
//!
//! Architecture (plan 5.4): egui runs on the main thread; [`runtime::Backend`]
//! runs a tokio runtime on its own thread that owns an
//! `Arc<dyn superba_core::SwitchService>`. The UI sends commands; results,
//! `SwitchEvent`s and `SwitchDecider` questions come back through a channel
//! followed by `request_repaint`. The UI-side states of plan 7.2 (Idle, Planning,
//! Blocked, Armed with a `Countdown`, ConfirmFull) live in [`flow::Flow`];
//! `SwitchService::execute` drives the rest.
//!
//! Modules:
//! - [`design`]: tokens, bundled Geist fonts + CJK fallback, icons, egui style,
//!   custom widgets (pill buttons, segmented control, status dots, cards, toasts,
//!   countdown bar, key input, latency badge, ...);
//! - [`flow`]: the pure UI state machine (unit tested);
//! - [`runtime`]: the background runtime bridge;
//! - [`i18n`]: layered catalogs (`locales/*.toml`);
//! - `pages` / `shell` / [`app`]: the views; [`palette`]: the Cmd/Ctrl+K palette.
//!
//! There is no demo mode: screenshots come from this binary running against an
//! isolated home and a local mock relay (`tools/screenshots.sh`).

#![warn(missing_docs)]

pub mod app;
pub mod args;
pub mod cadence;
pub mod design;
pub mod external;
pub mod flow;
pub mod i18n;
pub mod in_effect;
pub mod instance;
pub mod launch;
pub mod movers;
pub mod notices;
pub mod pages;
pub mod palette;
pub mod runtime;
pub mod shell;
pub mod startup;

use std::sync::Arc;

pub use app::{AppOptions, SuperbaApp};
pub use args::GuiArgs;
use superba_core::SwitchService;

/// Window title.
pub const APP_TITLE: &str = "Superba";

/// The production switching service for this machine.
pub fn service() -> anyhow::Result<Arc<dyn SwitchService>> {
    let ctx = superba_switch::detect_context().map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(superba_switch::default_service(ctx))
}

/// Run the GUI until the window closes. `report` gets this run's renderer and
/// any retry in [its GUI log](startup::Reporter::gui_log).
pub fn run(args: GuiArgs, report: &startup::Reporter) -> anyhow::Result<()> {
    let service = service()?;
    let size = args.size.unwrap_or(design::tokens::size::DEFAULT_WINDOW);
    let viewport = egui::ViewportBuilder::default()
        .with_title(APP_TITLE)
        .with_app_id("superba")
        .with_inner_size(size)
        .with_min_inner_size(design::tokens::size::MIN_WINDOW)
        .with_icon(Arc::new(app::window_icon()));
    let opts = AppOptions {
        theme: args.theme,
        locale: args.locale,
        page: args.page,
        launch_chatgpt: args.launch_chatgpt,
        single_instance: true,
    };
    let gpu = startup::Gpu::from_env_value(std::env::var(startup::ENV_SUPERBA_GPU).ok().as_deref());
    let mut result = run_window(&viewport, gpu, &service, &opts, report);
    if let Err(e) = &result
        && startup::retry_in_software(gpu, e)
    {
        // A GPU that is listed but cannot draw into the window (its driver refused
        // the device or the surface): once more on the software rasterizer.
        report.note(&format!("{e}; trying the software renderer"));
        result = run_window(&viewport, startup::Gpu::Software, &service, &opts, report);
    }
    result.map_err(|e| {
        anyhow::Error::new(startup::WindowError {
            reason: e.to_string(),
        })
    })
}

/// Open the window once, with the renderer options for `gpu`.
fn run_window(
    viewport: &egui::ViewportBuilder,
    gpu: startup::Gpu,
    service: &Arc<dyn SwitchService>,
    opts: &AppOptions,
    report: &startup::Reporter,
) -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: viewport.clone(),
        ..startup::native_options(gpu)
    };
    let (service, opts, report) = (Arc::clone(service), opts.clone(), report.clone());
    eframe::run_native(
        APP_TITLE,
        options,
        Box::new(move |cc| {
            report.note(&format!("renderer: {}", startup::describe_renderer(cc)));
            let app = SuperbaApp::new(cc, service, opts)?;
            Ok(Box::new(app))
        }),
    )
}
