//! macOS: while the window hides in the menu bar Superba leaves the Dock and
//! the app switcher (activation policy "accessory", like menu-bar apps); when
//! the window comes back so does the Dock icon. Not run in CI (needs a Mac);
//! checked by hand with the bundled app.

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

/// Show (`true`) or remove (`false`) Superba's Dock icon. Main thread only
/// (the UI thread); a no-op elsewhere.
pub fn set_in_dock(visible: bool) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let policy = if visible {
        NSApplicationActivationPolicy::Regular
    } else {
        NSApplicationActivationPolicy::Accessory
    };
    if !app.setActivationPolicy(policy) {
        tracing::debug!(visible, "activation policy unchanged");
    }
    if visible {
        // Back in front of other apps (`activate` exists only from macOS 14).
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
    }
}
