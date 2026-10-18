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
//! - `pages` / `shell` / [`app`]: the views; [`palette`]: the Cmd/Ctrl+K palette;
//! - [`chrome`]: the window frame without a title bar (traffic lights on macOS,
//!   Superba's own window buttons on Windows and Linux);
//! - [`tray`]: the tray icon / menu-bar item and what closing the window does
//!   (item 70); [`instance`]: one window per user (a second start shows it).
//!
//! There is no demo mode: screenshots come from this binary running against an
//! isolated home and a local mock relay (`tools/screenshots.sh`).

#![warn(missing_docs)]

pub mod app;
pub mod args;
pub mod cadence;
pub mod chrome;
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
pub mod tray;

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
    // Item 70: the window choices are needed before the window opens (start
    // hidden; on Wayland, a window that must hide opens through XWayland).
    let saved = startup_settings(&service);
    let start_in_tray = args.start_in_tray || saved.as_ref().is_some_and(|s| s.start_in_tray);
    let keeps_in_tray = start_in_tray
        || saved
            .as_ref()
            .is_some_and(|s| s.close_action == Some(superba_core::CloseAction::Tray));
    let x11 = cfg!(target_os = "linux")
        && tray::wants_x11(
            std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
            std::env::var("DISPLAY").ok().as_deref(),
            keeps_in_tray,
        );
    tray::set_forced_x11(x11);
    let size = args.size.unwrap_or(design::tokens::size::DEFAULT_WINDOW);
    // No title bar (user bug 85); the title still names the window for the
    // taskbar, the window switcher and UI Automation.
    let viewport = chrome::viewport(
        egui::ViewportBuilder::default()
            .with_title(APP_TITLE)
            .with_app_id("superba")
            .with_inner_size(size)
            .with_min_inner_size(design::tokens::size::MIN_WINDOW)
            .with_icon(Arc::new(app::window_icon())),
    );
    let opts = AppOptions {
        theme: args.theme,
        locale: args.locale,
        page: args.page,
        launch_chatgpt: args.launch_chatgpt,
        single_instance: true,
        tray: tray::TrayOption::System,
        start_in_tray,
    };
    let gpu = startup::Gpu::from_env_value(std::env::var(startup::ENV_SUPERBA_GPU).ok().as_deref());
    let mut result = run_window(&viewport, gpu, x11, &service, &opts, report);
    if let Err(e) = &result
        && startup::retry_in_software(gpu, e)
    {
        // A GPU that is listed but cannot draw into the window (its driver refused
        // the device or the surface): once more on the software rasterizer.
        report.note(&format!("{e}; trying the software renderer"));
        result = run_window(
            &viewport,
            startup::Gpu::Software,
            x11,
            &service,
            &opts,
            report,
        );
    }
    let result = result.map_err(|e| {
        anyhow::Error::new(startup::WindowError {
            reason: e.to_string(),
        })
    });
    if result.is_ok() && tray::take_relaunch_hidden() {
        relaunch_in_tray(&args);
    }
    result
}

/// The saved settings, read before the window opens (`None` when they cannot
/// be read within two seconds; the app reads them again anyway).
fn startup_settings(service: &Arc<dyn SwitchService>) -> Option<superba_core::Settings> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    runtime.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(2), service.profile_set())
            .await
            .ok()?
            .ok()
            .map(|set| set.settings)
    })
}

/// A native Wayland window cannot hide: Superba starts again hidden in the
/// tray, through XWayland ([`tray::HideSupport::Relaunch`]), once this
/// window is gone (and with it this process's single-instance registration).
fn relaunch_in_tray(args: &GuiArgs) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    // It outlives this process (which ends right after).
    let mut command = superba_core::process::detached_command(exe);
    command.arg("--start-in-tray");
    if let Some(locale) = args.locale {
        command.arg("--lang").arg(locale.code());
    }
    if let Some(theme) = args.theme {
        command.arg("--theme").arg(match theme {
            egui::Theme::Dark => "dark",
            egui::Theme::Light => "light",
        });
    }
    if let Err(e) = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        tracing::warn!(error = %e, "Superba could not start again in the tray");
    }
}

/// Open the window once, with the renderer options for `gpu`.
fn run_window(
    viewport: &egui::ViewportBuilder,
    gpu: startup::Gpu,
    x11: bool,
    service: &Arc<dyn SwitchService>,
    opts: &AppOptions,
    report: &startup::Reporter,
) -> Result<(), eframe::Error> {
    #[cfg_attr(not(target_os = "linux"), expect(unused_mut))]
    let mut options = eframe::NativeOptions {
        viewport: viewport.clone(),
        ..startup::native_options(gpu)
    };
    #[cfg(target_os = "linux")]
    if x11 {
        use winit::platform::x11::EventLoopBuilderExtX11 as _;
        options.event_loop_builder = Some(Box::new(|builder| {
            builder.with_x11();
        }));
    }
    #[cfg(not(target_os = "linux"))]
    let _ = x11;
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
