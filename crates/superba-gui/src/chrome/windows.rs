//! Windows: the native frame without its caption (user bug 85). Not run in
//! CI's unit tests (it needs a real window); checked in the Windows test VMs.
//!
//! winit keeps its decorated window styles (`WS_CAPTION`, `WS_THICKFRAME`,
//! the maximize box), so Windows keeps snapping, the window menu, the shadow,
//! the rounded corners and the border. A window subclass (comctl32) in front of
//! winit's window procedure then:
//! - `WM_NCCALCSIZE`: takes the caption and the top frame into the client area
//!   (left, right and bottom stay non-client: the invisible resize borders of
//!   every Windows 11 window); a maximized window keeps its top below the
//!   screen edge;
//! - `WM_NCHITTEST`: resizes from the top `SM_CYFRAME + SM_CXPADDEDBORDER`
//!   pixels (the top border is inside the window now) and answers
//!   `HTMAXBUTTON` over Superba's maximize button, which makes Windows 11 show
//!   Snap Layouts there. The button's hover and press then arrive as
//!   non-client messages, which this module tracks ([`max_button_state`]) and
//!   handles itself (the default handling would draw a classic button);
//! - `WM_GETMINMAXINFO`: the minimum size winit sets counts the caption; the
//!   client area keeps the requested minimum without it.
//!
//! - `WM_RBUTTONUP` over the window's empty top ([`set_drag_rects`]): the
//!   window menu, as on a title bar.
//!
//! Moving and the double click come from egui ([`super::drag_areas`]):
//! `ViewportCommand::StartDrag` enters the system's move loop (Aero Snap,
//! dragging a maximized window restores it).

use std::sync::Mutex;

use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{ClientToScreen, ScreenToClient};
use windows_sys::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForWindow, GetSystemMetricsForDpi,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnableMenuItem, GWL_EXSTYLE, GWL_STYLE, GetClientRect, GetSystemMenu, GetWindowLongW,
    GetWindowRect, HTCLIENT, HTLEFT, HTMAXBUTTON, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, IsZoomed,
    MF_BYCOMMAND, MF_ENABLED, MF_GRAYED, MINMAXINFO, NCCALCSIZE_PARAMS, PostMessageW, SC_CLOSE,
    SC_MAXIMIZE, SC_MINIMIZE, SC_MOVE, SC_RESTORE, SC_SIZE, SM_CXPADDEDBORDER, SM_CYFRAME,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER,
    SetMenuDefaultItem, SetWindowPos, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
    WM_GETMINMAXINFO, WM_NCCALCSIZE, WM_NCDESTROY, WM_NCHITTEST, WM_NCLBUTTONDBLCLK,
    WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCMOUSELEAVE, WM_NCMOUSEMOVE, WM_RBUTTONUP, WM_SYSCOMMAND,
};

/// Our subclass id (any constant unique for this window procedure).
const SUBCLASS_ID: usize = 0x5355_5042; // "SUPB"

/// What the frame and the egui side share (one window per process).
struct State {
    /// The window (0 before [`install`]).
    hwnd: isize,
    /// Superba's maximize button in client pixels (empty: none yet).
    max_button: RECT,
    /// The window's empty top (its drag areas) in client pixels.
    drag: [RECT; 2],
    /// The pointer rests on the maximize button (non-client).
    hover: bool,
    /// The maximize button is pressed (non-client).
    pressed: bool,
    /// `TrackMouseEvent` for the non-client leave is armed.
    tracking: bool,
    /// For repaints when the button's state changes.
    ctx: Option<egui::Context>,
}

const EMPTY: RECT = RECT {
    left: 0,
    top: 0,
    right: 0,
    bottom: 0,
};

static STATE: Mutex<State> = Mutex::new(State {
    hwnd: 0,
    max_button: RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    },
    drag: [EMPTY; 2],
    hover: false,
    pressed: false,
    tracking: false,
    ctx: None,
});

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Take the caption away once the window exists, keeping its client size.
pub fn install(cc: &eframe::CreationContext<'_>) {
    let Ok(handle) = cc.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(h) = handle.as_raw() else {
        return;
    };
    let hwnd = h.hwnd.get() as HWND;
    {
        let mut s = state();
        s.hwnd = hwnd as isize;
        s.ctx = Some(cc.egui_ctx.clone());
    }
    // SAFETY: `hwnd` is the live top-level window eframe just created on this
    // (the UI) thread; the subclass procedure only touches it and `STATE`.
    unsafe {
        let mut before = zero_rect();
        GetClientRect(hwnd, &mut before);
        if SetWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID, 0) == 0 {
            tracing::warn!("SetWindowSubclass failed; the window keeps its title bar");
            return;
        }
        // Recompute the frame: the caption becomes client area.
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED
                | SWP_NOMOVE
                | SWP_NOSIZE
                | SWP_NOZORDER
                | SWP_NOOWNERZORDER
                | SWP_NOACTIVATE,
        );
        let mut after = zero_rect();
        GetClientRect(hwnd, &mut after);
        let grown = (after.bottom - after.top) - (before.bottom - before.top);
        if grown > 0 && IsZoomed(hwnd) == 0 {
            // Back to the size the window was asked for (`with_inner_size`).
            let mut w = zero_rect();
            GetWindowRect(hwnd, &mut w);
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                0,
                0,
                w.right - w.left,
                w.bottom - w.top - grown,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOACTIVATE,
            );
        }
    }
}

fn px(rect: egui::Rect, ppp: f32) -> RECT {
    RECT {
        left: (rect.min.x * ppp).round() as i32,
        top: (rect.min.y * ppp).round() as i32,
        right: (rect.max.x * ppp).round() as i32,
        bottom: (rect.max.y * ppp).round() as i32,
    }
}

/// Superba's maximize button this frame, in points (`ppp` pixels per point).
pub fn set_max_button(rect: egui::Rect, ppp: f32) {
    state().max_button = px(rect, ppp);
}

/// The window's drag areas this frame, in points: a right click there opens
/// the window menu.
pub fn set_drag_rects(rects: [egui::Rect; 2], ppp: f32) {
    state().drag = rects.map(|r| px(r, ppp));
}

/// The maximize button's (hover, pressed) while the pointer is on it.
pub fn max_button_state() -> (bool, bool) {
    let s = state();
    (s.hover, s.pressed)
}

fn zero_rect() -> RECT {
    RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    }
}

fn repaint() {
    if let Some(ctx) = state().ctx.clone() {
        ctx.request_repaint();
    }
}

/// The resize border's thickness at the window's DPI (8 px at 100 %).
unsafe fn frame_thickness(hwnd: HWND) -> i32 {
    // SAFETY: plain Win32 queries.
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        GetSystemMetricsForDpi(SM_CYFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi)
    }
}

/// Height of the standard non-client area above the client area (caption
/// and top frame) for this window's styles and DPI.
unsafe fn caption_height(hwnd: HWND) -> i32 {
    // SAFETY: plain Win32 queries.
    unsafe {
        let mut r = zero_rect();
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        AdjustWindowRectExForDpi(&mut r, style, 0, ex, GetDpiForWindow(hwnd));
        -r.top
    }
}

fn lparam_point(lparam: LPARAM) -> POINT {
    POINT {
        x: (lparam & 0xFFFF) as u16 as i16 as i32,
        y: ((lparam >> 16) & 0xFFFF) as u16 as i16 as i32,
    }
}

fn contains(r: &RECT, p: &POINT) -> bool {
    p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
}

unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    // SAFETY (whole function): called by comctl32 for our window on its thread
    // with the message's own parameters; pointers in `lparam` are valid for the
    // messages that carry them (NCCALCSIZE_PARAMS, MINMAXINFO).
    unsafe {
        match msg {
            WM_NCCALCSIZE if wparam != 0 => {
                let params = lparam as *mut NCCALCSIZE_PARAMS;
                let top = (*params).rgrc[0].top;
                let result = DefSubclassProc(hwnd, msg, wparam, lparam);
                if result != 0 {
                    return result;
                }
                (*params).rgrc[0].top = top;
                if IsZoomed(hwnd) != 0 {
                    // A maximized window reaches past the screen by its frame.
                    (*params).rgrc[0].top += frame_thickness(hwnd);
                }
                0
            }
            WM_NCHITTEST => {
                let hit = DefSubclassProc(hwnd, msg, wparam, lparam) as u32;
                let mut p = lparam_point(lparam);
                ScreenToClient(hwnd, &mut p);
                let border = frame_thickness(hwnd);
                let top_edge = IsZoomed(hwnd) == 0 && p.y < border;
                let answer = match hit {
                    HTLEFT if top_edge => HTTOPLEFT,
                    HTRIGHT if top_edge => HTTOPRIGHT,
                    HTCLIENT if top_edge => {
                        let mut c = zero_rect();
                        GetClientRect(hwnd, &mut c);
                        if p.x < border * 2 {
                            HTTOPLEFT
                        } else if p.x >= c.right - border * 2 {
                            HTTOPRIGHT
                        } else {
                            HTTOP
                        }
                    }
                    HTCLIENT if contains(&state().max_button, &p) => HTMAXBUTTON,
                    other => other,
                };
                answer as LRESULT
            }
            WM_NCMOUSEMOVE => {
                let on_button = wparam as u32 == HTMAXBUTTON;
                let changed = {
                    let mut s = state();
                    let changed = s.hover != on_button || (!on_button && s.pressed);
                    s.hover = on_button;
                    if !on_button {
                        s.pressed = false;
                    }
                    if on_button && !s.tracking {
                        let mut t = TRACKMOUSEEVENT {
                            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE | TME_NONCLIENT,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        s.tracking = TrackMouseEvent(&mut t) != 0;
                    }
                    changed
                };
                if changed {
                    repaint();
                }
                DefSubclassProc(hwnd, msg, wparam, lparam)
            }
            WM_NCMOUSELEAVE => {
                {
                    let mut s = state();
                    s.hover = false;
                    s.pressed = false;
                    s.tracking = false;
                }
                repaint();
                DefSubclassProc(hwnd, msg, wparam, lparam)
            }
            WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if wparam as u32 == HTMAXBUTTON => {
                state().pressed = true;
                repaint();
                0
            }
            WM_NCLBUTTONUP if wparam as u32 == HTMAXBUTTON => {
                let was_pressed = std::mem::replace(&mut state().pressed, false);
                repaint();
                if was_pressed {
                    let command = if IsZoomed(hwnd) != 0 {
                        SC_RESTORE
                    } else {
                        SC_MAXIMIZE
                    };
                    PostMessageW(hwnd, WM_SYSCOMMAND, command as WPARAM, 0);
                }
                0
            }
            WM_GETMINMAXINFO => {
                let result = DefSubclassProc(hwnd, msg, wparam, lparam);
                let info = lparam as *mut MINMAXINFO;
                let min = &mut (*info).ptMinTrackSize.y;
                if *min > 0 {
                    *min = (*min - caption_height(hwnd)).max(1);
                }
                result
            }
            WM_RBUTTONUP => {
                // winit (egui) gets the release too; then the window menu
                // when it was the window's empty top.
                let result = DefSubclassProc(hwnd, msg, wparam, lparam);
                let mut p = lparam_point(lparam);
                let on_top = state().drag.iter().any(|r| contains(r, &p));
                if on_top {
                    ClientToScreen(hwnd, &mut p);
                    show_system_menu(hwnd, p);
                }
                result
            }
            WM_NCDESTROY => {
                RemoveWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID);
                {
                    let mut s = state();
                    s.hwnd = 0;
                    s.ctx = None;
                }
                DefSubclassProc(hwnd, msg, wparam, lparam)
            }
            _ => DefSubclassProc(hwnd, msg, wparam, lparam),
        }
    }
}

/// The window menu (Restore, Move, Size, Minimize, Maximize, Close) at the
/// screen point `p`, its items enabled as for a native title bar.
unsafe fn show_system_menu(hwnd: HWND, p: POINT) {
    // SAFETY: plain Win32 calls on our own window and its system menu.
    unsafe {
        let menu = GetSystemMenu(hwnd, 0);
        if menu.is_null() {
            return;
        }
        let maximized = IsZoomed(hwnd) != 0;
        let enable = |item: u32, on: bool| {
            EnableMenuItem(
                menu,
                item,
                MF_BYCOMMAND | if on { MF_ENABLED } else { MF_GRAYED },
            );
        };
        enable(SC_RESTORE, maximized);
        enable(SC_MOVE, !maximized);
        enable(SC_SIZE, !maximized);
        enable(SC_MINIMIZE, true);
        enable(SC_MAXIMIZE, !maximized);
        enable(SC_CLOSE, true);
        SetMenuDefaultItem(menu, SC_CLOSE, 0);
        let command = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            p.x,
            p.y,
            0,
            hwnd,
            std::ptr::null(),
        );
        if command != 0 {
            PostMessageW(hwnd, WM_SYSCOMMAND, command as WPARAM, 0);
        }
    }
}
