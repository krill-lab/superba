//! macOS: what a double click on the window's top does, as for a title bar:
//! System Settings → Desktop & Dock → "Double-click a window's title bar to"
//! (`AppleActionOnDoubleClick`: Maximize = zoom, the default; Minimize; or Do
//! Nothing; macOS before 11 stored a boolean `AppleMiniaturizeOnDoubleClick`).
//! Also the window's own zoom and miniaturize for it. Not run in CI's unit
//! tests; the window itself is checked on macOS runners.

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSWindow};
use objc2_foundation::{NSString, NSUserDefaults};

use super::DoubleClick;

/// The action the user chose for a double click on a title bar.
pub fn double_click_action() -> DoubleClick {
    let defaults = NSUserDefaults::standardUserDefaults();
    let action = defaults
        .stringForKey(&NSString::from_str("AppleActionOnDoubleClick"))
        .map(|s| s.to_string());
    match action.as_deref() {
        Some("Minimize") => DoubleClick::Minimize,
        Some("None") => DoubleClick::Nothing,
        Some(_) => DoubleClick::Zoom,
        None if defaults.boolForKey(&NSString::from_str("AppleMiniaturizeOnDoubleClick")) => {
            DoubleClick::Minimize
        }
        None => DoubleClick::Zoom,
    }
}

/// Superba's window: the key window (the one just double-clicked), else the
/// main one. Main thread only (the UI thread).
fn window() -> Option<objc2::rc::Retained<NSWindow>> {
    let mtm = MainThreadMarker::new()?;
    let app = NSApplication::sharedApplication(mtm);
    app.keyWindow().or_else(|| app.mainWindow())
}

/// Zoom the window, or back to the user's frame (`-[NSWindow zoom:]`).
pub fn zoom() {
    if let Some(w) = window() {
        w.zoom(None);
    }
}

/// Minimize the window into the Dock (`-[NSWindow miniaturize:]`).
pub fn miniaturize() {
    if let Some(w) = window() {
        w.miniaturize(None);
    }
}
