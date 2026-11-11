//! The window-wide motion settings widgets read: the frame interval of a
//! running animation and the "Reduce motion" preference (set by the app from
//! the settings, read by widgets that have no access to them).

use std::time::Duration;

use egui::{Context, Id};

/// Frame interval while something moves (about 60 frames a second, also on
/// faster displays).
pub const FRAME: Duration = Duration::from_millis(15);

fn key() -> Id {
    Id::unique("superba-reduce-motion")
}

/// Make "Reduce motion" `on` for the widgets drawn in `ctx`.
pub fn set_reduced(ctx: &Context, on: bool) {
    if reduced(ctx) != on {
        ctx.data_mut(|d| d.insert_temp(key(), on));
        ctx.request_repaint();
    }
}

/// "Reduce motion" is on (off until [`set_reduced`]).
pub fn reduced(ctx: &Context) -> bool {
    ctx.data(|d| d.get_temp(key())).unwrap_or(false)
}

/// Ask for the next animation frame: not with "Reduce motion" (nothing moves)
/// nor while the window is minimised or covered (nothing is drawn).
pub fn next_frame(ctx: &Context) {
    if reduced(ctx) || ctx.input(|i| i.viewport().visible()) == Some(false) {
        return;
    }
    ctx.request_repaint_after(FRAME);
}
