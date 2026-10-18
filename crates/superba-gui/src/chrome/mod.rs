//! The window frame (user bug 85): no title bar. The sidebar and the window
//! background are one surface, the pages sit in a card floating on it
//! ([`crate::design::backdrop::content_card`]), and the empty top of the window
//! moves it. Geometry: [`crate::design::tokens::chrome`].
//!
//! Per platform:
//! - **macOS**: a full-size content view under a transparent, hidden title bar.
//!   The system keeps drawing the traffic lights (over the top left of the
//!   sidebar) and keeps resizing, full screen, Mission Control and the
//!   accessibility of its buttons. Superba moves the window from the empty top
//!   and follows the system setting for a double click on a title bar
//!   ([`macos`]).
//! - **Windows**: the native frame without its caption ([`windows`]): Windows
//!   still resizes from every edge (outside the window on the left, right and
//!   bottom, as for every Windows 11 window; the top edge inside), snaps, and
//!   draws the shadow, the rounded corners and the border. Superba draws the
//!   Windows 11 caption buttons ([`buttons`]) and reports the maximize button to
//!   Windows, which then shows Snap Layouts on it. The empty top moves the
//!   window through the system's move loop (Aero Snap, Win+arrows), a double
//!   click maximizes or restores, a right click opens the system menu.
//! - **Linux**: an undecorated window (GNOME on Wayland has no server-side
//!   decorations; a header drawn by the app is how GTK 4 / libadwaita apps
//!   look) with round window buttons. Moving and resizing go through the
//!   window manager or compositor (`_NET_WM_MOVERESIZE`, `xdg_toplevel.move` /
//!   `resize`), so snapping and tiling behave as for other client-side
//!   decorated windows; Superba gives the window thin resize edges and a
//!   hairline border (there is no system shadow without decorations).
//!
//! Every window button is an AccessKit button with its own name ("Close
//! window", not the dialogs' "Close"), so UI Automation finds it by name; none
//! is in the Tab order (native caption buttons are not either).

pub mod buttons;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

use egui::{
    Area, Context, CursorIcon, Id, Order, PointerButton, Rect, ResizeDirection, Sense, Ui,
    ViewportCommand, pos2, vec2,
};

use crate::design::tokens::{chrome, size};
use crate::i18n::Tr;
pub use chrome::Buttons;

/// The window of this platform: no title bar ([`Buttons`] says which window
/// buttons it gets).
pub fn viewport(builder: egui::ViewportBuilder) -> egui::ViewportBuilder {
    match chrome::BUTTONS {
        // The traffic lights stay; the title and the title bar's background go.
        Buttons::TrafficLights => builder
            .with_fullsize_content_view(true)
            .with_title_shown(false)
            .with_titlebar_shown(false),
        // The frame stays (resize borders, snapping, shadow); `install` takes
        // only its caption away.
        Buttons::Caption => builder,
        Buttons::Round => builder.with_decorations(false),
    }
}

/// Platform hooks once the window exists (Windows: the frame without its
/// caption). Without a native window (tests) this does nothing.
pub fn install(cc: &eframe::CreationContext<'_>) {
    #[cfg(windows)]
    windows::install(cc);
    #[cfg(not(windows))]
    let _ = cc;
}

/// The parts of the window's top that move it, in a window covering `screen`:
/// the sidebar above its first row and the strip across the content above the
/// page header (the window margin and the card's padding above the title), left
/// of Superba's own window buttons.
pub fn drag_rects(screen: Rect) -> [Rect; 2] {
    let sidebar = screen.min.x + size::SIDEBAR;
    [
        Rect::from_min_max(
            screen.min,
            pos2(sidebar, screen.min.y + chrome::NAV_TOP - size::NAV_GAP),
        ),
        Rect::from_min_max(
            pos2(sidebar, screen.min.y),
            pos2(
                screen.max.x - buttons_size().x,
                screen.min.y + size::PAGE_TOP,
            ),
        ),
    ]
}

/// The window's drag areas ([`drag_rects`]). Call first in the frame, on the
/// root `Ui`: every widget registered later at the same place (pills, menus,
/// a page scrolled under the strip) takes the pointer before them. While a
/// modal dialog blocks the window (the first-run wizard, a confirmation), they
/// sit above it instead, so the window still moves as it would by a title bar;
/// nothing else is clickable there then.
pub fn drag_areas(ui: &mut Ui) {
    let screen = ui.max_rect();
    let modal = ui.ctx().memory(|m| m.top_modal_layer()).is_some();
    // Windows: a right click there opens the window menu (`windows.rs`).
    #[cfg(windows)]
    windows::set_drag_rects(drag_rects(screen), ui.ctx().pixels_per_point());
    for (i, rect) in drag_rects(screen).into_iter().enumerate() {
        let id = Id::unique(("superba-window-drag", i));
        // Neither focusable nor named: not a control, the window's background.
        let sense = Sense::CLICK | Sense::DRAG;
        if modal {
            Area::new(id.with("above-modal"))
                .order(Order::Tooltip)
                .fixed_pos(rect.min)
                .constrain(false)
                .movable(false)
                .sense(Sense::hover())
                .show(ui.ctx(), |ui| {
                    ui.set_min_size(rect.size());
                    ui.set_max_size(rect.size());
                    let response = ui.interact(rect, id, sense);
                    drag_response(ui.ctx(), &response);
                });
        } else {
            let response = ui.interact(rect, id, sense);
            drag_response(ui.ctx(), &response);
        }
    }
}

/// Two presses on the window's top this close in time (seconds) and space
/// (points) are a double click (the usual system default is 0.5 s).
const DOUBLE_CLICK_SECS: f64 = 0.5;
const DOUBLE_CLICK_DIST: f32 = 6.0;

/// What a press on an empty top area does, as on a title bar.
///
/// Windows and Linux: the press hands the pointer to the system's move loop
/// right away (the Windows move loop with Aero Snap, `_NET_WM_MOVERESIZE` /
/// `xdg_toplevel.move`), so the window follows the pointer from the first
/// pixel; a second press within [`DOUBLE_CLICK_SECS`] maximizes or restores
/// the window. The move loop takes the button's release, so the double click is
/// told by the presses, not by egui's clicks. Should the system refuse the
/// first move (an inactive X11 window), the drag that follows starts it.
///
/// macOS: `performWindowDrag` anchors the move at the mouse-down, so the move
/// starts with the pointer's first motion and a plain click stays a click: a
/// double click does what the system setting says. (Windows: the right click's
/// window menu is the window procedure's, `windows.rs`.)
fn drag_response(ctx: &Context, response: &egui::Response) {
    if cfg!(target_os = "macos") {
        if response.drag_started_by(PointerButton::Primary) {
            ctx.send_viewport_cmd(ViewportCommand::StartDrag);
        } else if response.double_clicked() {
            double_click(ctx);
        }
        return;
    }
    let pressed = response.is_pointer_button_down_on()
        && ctx.input(|i| i.pointer.button_pressed(PointerButton::Primary));
    if pressed {
        let now = ctx.input(|i| i.time);
        // On the screen: after the window moved under the pointer, a press at
        // the same spot of the window is not a double click.
        let origin = ctx.input(|i| {
            i.viewport()
                .inner_rect
                .map(|r| r.min.to_vec2())
                .unwrap_or_default()
        });
        let pos = response.interact_pointer_pos().unwrap_or_default() + origin;
        let last_id = Id::unique("superba-window-drag-last-press");
        let last: Option<(f64, egui::Pos2)> = ctx.data(|d| d.get_temp(last_id));
        let double = last.is_some_and(|(t, p)| {
            now - t <= DOUBLE_CLICK_SECS && p.distance(pos) <= DOUBLE_CLICK_DIST
        });
        if double {
            ctx.data_mut(|d| d.remove::<(f64, egui::Pos2)>(last_id));
            double_click(ctx);
        } else {
            ctx.data_mut(|d| d.insert_temp(last_id, (now, pos)));
            ctx.send_viewport_cmd(ViewportCommand::StartDrag);
        }
    } else if response.drag_started_by(PointerButton::Primary) {
        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
    }
}

/// A double click on the window's top.
fn double_click(ctx: &Context) {
    // macOS: the window's own zoom / miniaturize, as its title bar does (zoom
    // toggles between the user's frame and the screen).
    #[cfg(target_os = "macos")]
    match macos::double_click_action() {
        DoubleClick::Zoom => macos::zoom(),
        DoubleClick::Minimize => macos::miniaturize(),
        DoubleClick::Nothing => {}
    }
    #[cfg(not(target_os = "macos"))]
    toggle_maximized(ctx);
    #[cfg(target_os = "macos")]
    let _ = ctx;
}

/// What a double click on a title bar does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoubleClick {
    /// Maximize (zoom) the window, or restore it.
    Zoom,
    /// Minimize the window.
    Minimize,
    /// Nothing.
    Nothing,
}

/// The state of the window the buttons and edges depend on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowState {
    /// The window is maximized.
    pub maximized: bool,
    /// The window is in full screen.
    pub fullscreen: bool,
    /// The window has the keyboard focus (inactive windows draw their
    /// buttons dimmed).
    pub focused: bool,
}

impl WindowState {
    /// The state egui reports for the current viewport.
    pub fn of(ctx: &Context) -> Self {
        ctx.input(|i| {
            let v = i.viewport();
            WindowState {
                maximized: v.maximized.unwrap_or(false),
                fullscreen: v.fullscreen.unwrap_or(false),
                focused: v.focused.unwrap_or(true),
            }
        })
    }
}

fn toggle_maximized(ctx: &Context) {
    let maximized = WindowState::of(ctx).maximized;
    ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
}

/// The window buttons and (Linux) the resize edges and the hairline border,
/// above everything else (also above a modal dialog, like native window
/// buttons). Call last in the frame.
pub fn overlay(ctx: &Context, tr: &Tr) {
    if chrome::BUTTONS == Buttons::TrafficLights {
        return;
    }
    let screen = ctx.content_rect();
    let state = WindowState::of(ctx);
    if state.fullscreen {
        return;
    }
    if let Some(clicked) = buttons::show(ctx, tr, screen, state) {
        match clicked {
            buttons::WindowButton::Minimize => {
                ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
            }
            buttons::WindowButton::Maximize => toggle_maximized(ctx),
            buttons::WindowButton::Close => ctx.send_viewport_cmd(ViewportCommand::Close),
        }
    }
    if chrome::BUTTONS == Buttons::Round && !state.maximized {
        resize_edges(ctx, screen);
        let pal = crate::design::Palette::of(ctx);
        let painter = ctx.layer_painter(egui::LayerId::new(
            Order::Tooltip,
            Id::unique("superba-window-border"),
        ));
        painter.rect_stroke(
            screen,
            0,
            egui::Stroke::new(
                1.0,
                pal.border_strong
                    .gamma_multiply(if pal.dark { 0.9 } else { 0.7 }),
            ),
            egui::StrokeKind::Inside,
        );
    }
}

/// The resize edges of a window covering `screen` where Superba resizes it
/// itself (Linux): [`chrome::RESIZE_EDGE`] wide along each edge, corners
/// [`chrome::RESIZE_CORNER`] square.
pub fn resize_rects(screen: Rect) -> [(ResizeDirection, Rect); 8] {
    let e = chrome::RESIZE_EDGE;
    let c = chrome::RESIZE_CORNER;
    let (l, t, r, b) = (screen.min.x, screen.min.y, screen.max.x, screen.max.y);
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_min_max(pos2(x0, y0), pos2(x1, y1));
    [
        (ResizeDirection::NorthWest, rect(l, t, l + c, t + c)),
        (ResizeDirection::NorthEast, rect(r - c, t, r, t + c)),
        (ResizeDirection::SouthWest, rect(l, b - c, l + c, b)),
        (ResizeDirection::SouthEast, rect(r - c, b - c, r, b)),
        (ResizeDirection::North, rect(l + c, t, r - c, t + e)),
        (ResizeDirection::South, rect(l + c, b - e, r - c, b)),
        (ResizeDirection::West, rect(l, t + c, l + e, b - c)),
        (ResizeDirection::East, rect(r - e, t + c, r, b - c)),
    ]
}

fn resize_cursor(direction: ResizeDirection) -> CursorIcon {
    match direction {
        ResizeDirection::North => CursorIcon::ResizeNorth,
        ResizeDirection::South => CursorIcon::ResizeSouth,
        ResizeDirection::East => CursorIcon::ResizeEast,
        ResizeDirection::West => CursorIcon::ResizeWest,
        ResizeDirection::NorthEast => CursorIcon::ResizeNorthEast,
        ResizeDirection::SouthEast => CursorIcon::ResizeSouthEast,
        ResizeDirection::NorthWest => CursorIcon::ResizeNorthWest,
        ResizeDirection::SouthWest => CursorIcon::ResizeSouthWest,
    }
}

/// Linux: each edge in its own small area above everything, so a press there
/// starts the window manager's resize and the cursor shows the direction.
fn resize_edges(ctx: &Context, screen: Rect) {
    for (direction, rect) in resize_rects(screen) {
        let id = Id::unique(("superba-window-resize", direction as u8));
        Area::new(id)
            .order(Order::Tooltip)
            .fixed_pos(rect.min)
            .constrain(false)
            .movable(false)
            .sense(Sense::hover())
            .show(ctx, |ui| {
                ui.set_min_size(rect.size());
                ui.set_max_size(rect.size());
                // Click and drag, like the drag areas below, so the edge wins
                // where both are (the window's top); the press hands the
                // pointer to the window manager's resize right away.
                let response = ui
                    .interact(rect, id.with("edge"), Sense::CLICK | Sense::DRAG)
                    .on_hover_cursor(resize_cursor(direction));
                let pressed = response.is_pointer_button_down_on()
                    && ctx.input(|i| i.pointer.button_pressed(PointerButton::Primary));
                if pressed || response.drag_started_by(PointerButton::Primary) {
                    ctx.send_viewport_cmd(ViewportCommand::BeginResize(direction));
                }
            });
    }
}

/// The size of the window buttons' group (for tests and the page layout).
pub fn buttons_size() -> egui::Vec2 {
    match chrome::BUTTONS {
        Buttons::TrafficLights => vec2(0.0, 0.0),
        Buttons::Caption => vec2(chrome::CAPTION_BUTTON[0] * 3.0, chrome::CAPTION_BUTTON[1]),
        Buttons::Round => vec2(
            chrome::ROUND_BUTTON * 3.0 + chrome::ROUND_GAP * 3.0,
            chrome::BAND,
        ),
    }
}
