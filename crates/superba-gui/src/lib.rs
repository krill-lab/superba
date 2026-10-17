//! # superba-gui
//!
//! The Superba desktop GUI: eframe/egui (glow) with a Vercel Geist look, en +
//! zh-CN, dark-first with a light theme (plan 11).
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
pub mod design;
pub mod flow;
pub mod i18n;
pub mod movers;
pub mod pages;
pub mod palette;
pub mod runtime;
pub mod shell;

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

/// Run the GUI until the window closes.
pub fn run(args: GuiArgs) -> anyhow::Result<()> {
    let service = service()?;
    let size = args.size.unwrap_or(design::tokens::size::DEFAULT_WINDOW);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_TITLE)
            .with_app_id("superba")
            .with_inner_size(size)
            .with_min_inner_size(design::tokens::size::MIN_WINDOW)
            .with_icon(Arc::new(app::window_icon())),
        ..eframe::NativeOptions::default()
    };
    let opts = AppOptions {
        theme: args.theme,
        locale: args.locale,
        page: args.page,
    };
    eframe::run_native(
        APP_TITLE,
        options,
        Box::new(move |cc| {
            let app = SuperbaApp::new(cc, service, opts)?;
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("GUI failed: {e}"))
}
