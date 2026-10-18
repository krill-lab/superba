//! The tray icon (the menu-bar item on macOS) and what closing the window
//! does (item 70, GH#10).
//!
//! - [`menu`]: the tray menu as plain data, built from the app's [`Data`]
//!   (unit tested): Open Superba, the provider/line in use with a switch
//!   submenu, open or reopen ChatGPT through Superba, the router's state with
//!   its on/off, Quit Superba.
//! - [`close`]: the window's life as a pure state machine: a close request
//!   hides the window into the tray, asks once (Windows, Linux), or quits;
//!   Quit from the tray always quits.
//! - [`Tray`]: the platform icon. Windows: `Shell_NotifyIcon` through
//!   tray-icon (a left click opens the window, a right click the menu);
//!   macOS: an `NSStatusItem` with a template image through tray-icon (a click
//!   opens the menu); Linux: a StatusNotifierItem on the session bus through
//!   ksni (no GTK). A Linux desktop without a StatusNotifierItem host (GNOME
//!   without the AppIndicator extension) has no tray: the setting is shown
//!   disabled with the reason and closing the window quits.
//! - [`prompt`]: the first-close question.
//!
//! Events of the platform icon reach the app through a [`Sink`] (a queue and
//! a repaint request), so a hidden window is woken (eframe runs
//! [`eframe::App::logic`] for a hidden window) and handles them like any
//! other input.

pub mod close;
pub mod icon;
pub mod menu;
pub mod prompt;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(windows, target_os = "macos"))]
mod native;
#[cfg(target_os = "linux")]
mod sni;

use std::sync::{Arc, Mutex};

pub use close::{CloseOutcome, HideSupport, WindowCommand, WindowLife};
pub use menu::{TrayAction, TrayItem, TrayMenu};

#[cfg(doc)]
use crate::pages::Data;

/// What the tray reports to the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    /// A menu item was chosen.
    Action(TrayAction),
    /// The icon itself was clicked (Windows: left click; Linux: the host's
    /// "activate", usually a left click): open the window.
    Activate,
    /// The tray host went away (Linux: the StatusNotifierWatcher stopped,
    /// e.g. the AppIndicator extension was turned off).
    HostLost,
    /// The tray host is back and shows the icon again.
    HostBack,
}

/// Why there is no tray icon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// Turned off for this run (tests, `SUPERBA_TRAY=off`).
    Off,
    /// Linux: no StatusNotifierItem host on the session bus (GNOME without
    /// the AppIndicator extension, a bare window manager).
    NoHost,
    /// Linux: a Wayland session without XWayland, where a window cannot be
    /// hidden.
    CannotHide,
    /// The platform refused the icon (the message is for the log).
    Failed(String),
}

impl Unavailable {
    /// i18n key of the explanation shown under the disabled setting.
    pub fn reason_key(&self) -> &'static str {
        match self {
            Unavailable::Off => "tray.unavailable.off",
            Unavailable::NoHost => "tray.unavailable.no_host",
            Unavailable::CannotHide => "tray.unavailable.cannot_hide",
            Unavailable::Failed(_) => "tray.unavailable.failed",
        }
    }
}

/// Whether the tray icon is shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayStatus {
    /// The icon is shown; how the window hides into it.
    Shown(HideSupport),
    /// No icon (and closing the window quits).
    Unavailable(Unavailable),
}

impl Default for TrayStatus {
    fn default() -> Self {
        TrayStatus::Unavailable(Unavailable::Off)
    }
}

impl TrayStatus {
    /// The window can hide into the tray.
    pub fn can_hide(&self) -> bool {
        matches!(self, TrayStatus::Shown(_))
    }

    /// How the window hides, when it can.
    pub fn hide_support(&self) -> Option<HideSupport> {
        match self {
            TrayStatus::Shown(h) => Some(*h),
            TrayStatus::Unavailable(_) => None,
        }
    }
}

/// Where platform callbacks put [`TrayEvent`]s for the app: a queue plus a
/// repaint request to wake the (possibly hidden) window. Cheap to clone.
#[derive(Clone, Default)]
pub struct Sink {
    queue: Arc<Mutex<Vec<TrayEvent>>>,
    waker: Arc<Mutex<Option<egui::Context>>>,
}

impl std::fmt::Debug for Sink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sink").finish_non_exhaustive()
    }
}

impl Sink {
    /// Queue `event` and wake the window.
    pub fn push(&self, event: TrayEvent) {
        lock(&self.queue).push(event);
        if let Some(ctx) = lock(&self.waker).as_ref() {
            ctx.request_repaint();
        }
    }

    /// Events queued since the last call.
    pub fn take(&self) -> Vec<TrayEvent> {
        std::mem::take(&mut *lock(&self.queue))
    }

    /// Wake `ctx` on every later event.
    pub fn set_waker(&self, ctx: &egui::Context) {
        *lock(&self.waker) = Some(ctx.clone());
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The environment variable that turns the tray off for one run
/// (`SUPERBA_TRAY=off`): no icon, closing quits.
pub const ENV_SUPERBA_TRAY: &str = "SUPERBA_TRAY";

/// Which tray the app shows ([`crate::AppOptions::tray`]).
#[derive(Clone, Debug, Default)]
pub enum TrayOption {
    /// None: closing quits (tests that do not look at the tray).
    #[default]
    Off,
    /// The platform's icon.
    System,
    /// No platform icon, but the app behaves as if one were shown (the
    /// window hides in place); events come through the given sink. Tests.
    Simulated(Sink),
}

/// The tray icon of this run. Owned by the app on the UI thread (the
/// Windows and macOS icons must be created and changed there).
pub struct Tray {
    backend: Option<Backend>,
    sink: Sink,
    status: TrayStatus,
    menu: Option<TrayMenu>,
    tooltip: String,
    simulated: bool,
}

impl std::fmt::Debug for Tray {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tray")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl Tray {
    /// No tray (tests, `SUPERBA_TRAY=off`).
    pub fn disabled() -> Self {
        Self {
            backend: None,
            sink: Sink::default(),
            status: TrayStatus::Unavailable(Unavailable::Off),
            menu: None,
            tooltip: String::new(),
            simulated: false,
        }
    }

    /// A tray without a platform icon that reports [`TrayStatus::Shown`]
    /// ([`TrayOption::Simulated`]).
    pub fn simulated(ctx: &egui::Context, sink: Sink, menu: TrayMenu, tooltip: String) -> Self {
        sink.set_waker(ctx);
        Self {
            backend: None,
            sink,
            status: TrayStatus::Shown(HideSupport::InPlace),
            menu: Some(menu),
            tooltip,
            simulated: true,
        }
    }

    /// Show the icon with `menu`; events wake `ctx`. Must run on the UI
    /// thread inside the event loop (macOS needs the running application).
    pub fn start(ctx: &egui::Context, menu: TrayMenu, tooltip: String) -> Self {
        let mut tray = Self::disabled();
        if std::env::var(ENV_SUPERBA_TRAY).is_ok_and(|v| v.eq_ignore_ascii_case("off")) {
            return tray;
        }
        tray.sink.set_waker(ctx);
        match Backend::start(tray.sink.clone(), &menu, &tooltip) {
            Ok((backend, hide)) => {
                tray.backend = Some(backend);
                tray.status = TrayStatus::Shown(hide);
                tray.menu = Some(menu);
                tray.tooltip = tooltip;
            }
            Err(why) => {
                if let Unavailable::Failed(e) = &why {
                    tracing::warn!(error = %e, "no tray icon");
                } else {
                    tracing::info!(reason = ?why, "no tray icon");
                }
                tray.status = TrayStatus::Unavailable(why);
            }
        }
        tray
    }

    /// Whether the icon is shown.
    pub fn status(&self) -> &TrayStatus {
        &self.status
    }

    /// The events of the icon and its menu since the last call. A lost or
    /// regained tray host also changes [`Tray::status`].
    pub fn take_events(&mut self) -> Vec<TrayEvent> {
        let events = self.sink.take();
        for e in &events {
            match e {
                TrayEvent::HostLost if self.backend.is_some() || self.simulated => {
                    self.status = TrayStatus::Unavailable(Unavailable::NoHost);
                }
                TrayEvent::HostBack => {
                    if let Some(b) = &self.backend {
                        self.status = TrayStatus::Shown(b.hide_support());
                    } else if self.simulated {
                        self.status = TrayStatus::Shown(HideSupport::InPlace);
                    }
                }
                _ => {}
            }
        }
        events
    }

    /// Show `menu` and `tooltip` (no-op when unchanged).
    pub fn update(&mut self, menu: TrayMenu, tooltip: String) {
        if self.backend.is_none() && !self.simulated {
            return;
        }
        if self.menu.as_ref() != Some(&menu) {
            if let Some(backend) = self.backend.as_mut() {
                backend.set_menu(&menu);
            }
            self.menu = Some(menu);
        }
        if self.tooltip != tooltip {
            if let Some(backend) = self.backend.as_mut() {
                backend.set_tooltip(&tooltip);
            }
            self.tooltip = tooltip;
        }
    }

    /// The icon's tooltip (tests).
    pub fn tooltip(&self) -> &str {
        &self.tooltip
    }

    /// The menu the icon shows (tests, screenshots).
    pub fn menu(&self) -> Option<&TrayMenu> {
        self.menu.as_ref()
    }
}

#[cfg(any(windows, target_os = "macos"))]
use native::Backend;
#[cfg(target_os = "linux")]
use sni::Backend;

/// Other platforms: no tray.
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
struct Backend;

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
impl Backend {
    fn start(_: Sink, _: &TrayMenu, _: &str) -> Result<(Self, HideSupport), Unavailable> {
        Err(Unavailable::Failed("no tray on this platform".into()))
    }
    fn hide_support(&self) -> HideSupport {
        HideSupport::InPlace
    }
    fn set_menu(&mut self, _: &TrayMenu) {}
    fn set_tooltip(&mut self, _: &str) {}
}

/// Make the window leave the Dock while it hides in the menu bar (macOS), and
/// come back with it. A no-op elsewhere.
pub fn set_in_dock(visible: bool) {
    #[cfg(target_os = "macos")]
    macos::set_in_dock(visible);
    #[cfg(not(target_os = "macos"))]
    let _ = visible;
}

// ---------------------------------------------------------------------------
// Linux: the window system decides whether a window can hide.

/// Linux: whether this start should open its window through XWayland: a
/// Wayland session cannot hide a native window, so a user who keeps Superba in
/// the tray (`close_action = tray` or "start in the tray") gets an X11 window
/// when XWayland is there. `wayland` and `display` are `WAYLAND_DISPLAY` and
/// `DISPLAY`.
pub fn wants_x11(wayland: Option<&str>, display: Option<&str>, keeps_in_tray: bool) -> bool {
    let set = |v: Option<&str>| v.is_some_and(|v| !v.trim().is_empty());
    keeps_in_tray && set(wayland) && set(display)
}

/// Linux: how a window of this process hides, given the session's
/// `WAYLAND_DISPLAY` and `DISPLAY` and whether this start forced X11
/// ([`wants_x11`]). `None`: it cannot (Wayland without XWayland).
pub fn linux_hide_support(
    wayland: Option<&str>,
    display: Option<&str>,
    forced_x11: bool,
) -> Option<HideSupport> {
    let set = |v: Option<&str>| v.is_some_and(|v| !v.trim().is_empty());
    if forced_x11 || !set(wayland) {
        Some(HideSupport::InPlace)
    } else if set(display) {
        // A native Wayland window: start again through XWayland, hidden.
        Some(HideSupport::Relaunch)
    } else {
        None
    }
}

/// This start's window was forced onto X11 ([`wants_x11`]); set by
/// [`crate::run`] before the window opens.
static FORCED_X11: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Remember that this start forced X11.
pub fn set_forced_x11(forced: bool) {
    FORCED_X11.store(forced, std::sync::atomic::Ordering::SeqCst);
}

/// Whether this start forced X11.
pub fn forced_x11() -> bool {
    FORCED_X11.load(std::sync::atomic::Ordering::SeqCst)
}

/// Set by the app when the window closes to be started again hidden in the
/// tray ([`HideSupport::Relaunch`]); [`crate::run`] does it once the window
/// is gone.
static RELAUNCH_HIDDEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Ask [`crate::run`] to start Superba again hidden in the tray.
pub fn request_relaunch_hidden() {
    RELAUNCH_HIDDEN.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Whether a hidden restart was asked for (and clear it).
pub fn take_relaunch_hidden() -> bool {
    RELAUNCH_HIDDEN.swap(false, std::sync::atomic::Ordering::SeqCst)
}
