//! The session migration scene (user bug 33): a coral krill carries session
//! crates from the left stack to the shelf on the right, sets each down and
//! walks back, while a second krill lounges in a chair with a coffee. Shown
//! with the real progress while a switch migrates sessions or puts them back
//! ([`dialog`]).
//!
//! The artwork is `assets/scenes/superba-session-movers.svg` (1440 x 900, CSS
//! keyframe animations on an 18 s carry cycle). egui cannot play SVG
//! animations, so the scene is ported:
//! - [`svg`] cuts the source into static layers, moving parts and texts, each
//!   placed by a chain of fixed and animated transforms;
//! - [`render`] rasterizes every layer once with resvg at the display's pixel
//!   density and paints them as textured quads, the texts with egui;
//! - [`pose`] / [`keyframes`] evaluate the SVG's own keyframes (values,
//!   offsets, timing functions, transform origins, delays) on the egui clock.
//!
//! Repaints are requested only while the dialog is on screen and the window
//! visible ([`repaint_after`]); "Reduce motion" (Preferences) shows the SVG's
//! own reduced-motion frame and repaints only for the elapsed-time clock.

pub mod dialog;
pub mod keyframes;
pub mod pose;
pub mod render;
pub mod svg;

use std::time::Duration;

/// Frame interval while animating (about 60 frames a second, also on faster
/// displays).
pub const FRAME: Duration = Duration::from_millis(15);

/// Interval of the elapsed-time clock when nothing moves.
pub const CLOCK_TICK: Duration = Duration::from_secs(1);

/// When to repaint next while the scene is shown: `None` when the window is
/// not visible (minimised or covered: nothing is drawn, so nothing is asked),
/// every second with reduced motion (the elapsed time), else every frame.
pub fn repaint_after(window_visible: Option<bool>, reduce_motion: bool) -> Option<Duration> {
    if window_visible == Some(false) {
        return None;
    }
    Some(if reduce_motion { CLOCK_TICK } else { FRAME })
}
