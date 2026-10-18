//! The window buttons Superba draws where the system draws none (user bug 85):
//! Windows 11 caption buttons flush with the window's top right corner, round
//! GNOME / libadwaita buttons on Linux. macOS keeps its traffic lights.
//!
//! Windows: the maximize button is also known to Windows ([`super::windows`]
//! answers `HTMAXBUTTON` for it), which shows Snap Layouts while the pointer
//! rests on it. The pointer is then in the non-client area and egui does not
//! see it; its hover and press come from the window messages instead.

use egui::{Area, Color32, Context, Id, Order, Painter, Pos2, Rect, Sense, Stroke, pos2, vec2};

use super::{Buttons, WindowState};
use crate::design::Palette;
use crate::design::tokens::chrome;
use crate::i18n::Tr;

/// One window button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WindowButton {
    /// Minimize the window.
    Minimize,
    /// Maximize it, or restore it when it is maximized.
    Maximize,
    /// Close it (what closing does, quit or hide in the tray, is the app's).
    Close,
}

impl WindowButton {
    /// Left to right.
    pub const ALL: [WindowButton; 3] = [
        WindowButton::Minimize,
        WindowButton::Maximize,
        WindowButton::Close,
    ];

    /// The accessible name (the maximize button says "Restore window" while the
    /// window is maximized).
    pub fn name(self, tr: &Tr, maximized: bool) -> String {
        tr.t(match self {
            WindowButton::Minimize => "shell.window_minimize",
            WindowButton::Maximize if maximized => "shell.window_restore",
            WindowButton::Maximize => "shell.window_maximize",
            WindowButton::Close => "shell.window_close",
        })
    }
}

/// The buttons' rectangles, left to right, in a window covering `screen`
/// (none with the traffic lights). Windows: hit areas flush with the top and
/// right edges (a maximized window closes from the screen's corner pixel).
/// Linux: circles of [`chrome::ROUND_BUTTON`], vertically centered in the
/// title band.
pub fn rects(screen: Rect) -> Vec<(WindowButton, Rect)> {
    match chrome::BUTTONS {
        Buttons::TrafficLights => Vec::new(),
        Buttons::Caption => {
            let [w, h] = chrome::CAPTION_BUTTON;
            WindowButton::ALL
                .iter()
                .enumerate()
                .map(|(i, b)| {
                    let x = screen.max.x - w * (3 - i) as f32;
                    (*b, Rect::from_min_size(pos2(x, screen.min.y), vec2(w, h)))
                })
                .collect()
        }
        Buttons::Round => {
            let d = chrome::ROUND_BUTTON;
            let gap = chrome::ROUND_GAP;
            let cy = screen.min.y + chrome::BAND / 2.0;
            WindowButton::ALL
                .iter()
                .enumerate()
                .map(|(i, b)| {
                    let right = screen.max.x - gap - (d + gap) * (2 - i) as f32;
                    (
                        *b,
                        Rect::from_center_size(pos2(right - d / 2.0, cy), vec2(d, d)),
                    )
                })
                .collect()
        }
    }
}

/// Show the buttons above everything; the button clicked this frame. Keyboard
/// focus never lands on them (as on native caption buttons); screen readers and
/// UI Automation reach them by name.
pub fn show(ctx: &Context, tr: &Tr, screen: Rect, state: WindowState) -> Option<WindowButton> {
    let rects = rects(screen);
    let bounds = rects
        .iter()
        .fold(Rect::NOTHING, |acc, (_, r)| acc.union(*r));
    if !bounds.is_positive() {
        return None;
    }
    let pal = Palette::of(ctx);
    let mut clicked = None;
    Area::new(Id::unique("superba-window-buttons"))
        .order(Order::Tooltip)
        .fixed_pos(bounds.min)
        .constrain(false)
        .movable(false)
        .sense(Sense::hover())
        .show(ctx, |ui| {
            ui.set_min_size(bounds.size());
            ui.set_max_size(bounds.size());
            for (button, rect) in &rects {
                let id = Id::unique(("superba-window-button", *button));
                // CLICK without FOCUSABLE: invokable (AccessKit), not tabbable.
                let response = ui.interact(*rect, id, Sense::CLICK);
                let name = button.name(tr, state.maximized);
                response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &name));
                let hovered = response.hovered();
                let pressed = response.is_pointer_button_down_on();
                #[cfg(windows)]
                let (hovered, pressed) = if *button == WindowButton::Maximize {
                    super::windows::set_max_button(*rect, ctx.pixels_per_point());
                    let (nc_hover, nc_pressed) = super::windows::max_button_state();
                    (hovered || nc_hover, pressed || nc_pressed)
                } else {
                    (hovered, pressed)
                };
                if response.clicked() {
                    clicked = Some(*button);
                }
                let painter = ui.painter();
                match chrome::BUTTONS {
                    Buttons::Caption => {
                        paint_caption(painter, *rect, *button, hovered, pressed, state, pal);
                    }
                    Buttons::Round => {
                        paint_round(painter, *rect, *button, hovered, pressed, state, pal);
                    }
                    Buttons::TrafficLights => {}
                }
            }
        });
    clicked
}

/// Windows 11's close button colours (pointer over, pressed).
const CLOSE_HOVER: Color32 = Color32::from_rgb(0xC4, 0x2B, 0x1C);
const CLOSE_PRESSED: Color32 = Color32::from_rgb(0xC8, 0x40, 0x31);

/// A Windows 11 caption button: no fill at rest, a subtle fill on hover (red
/// for close), the Segoe Fluent glyph drawn with 1 px lines in a 10 px box.
fn paint_caption(
    painter: &Painter,
    rect: Rect,
    button: WindowButton,
    hovered: bool,
    pressed: bool,
    state: WindowState,
    pal: &Palette,
) {
    let fill = match (button, pressed, hovered) {
        (WindowButton::Close, true, _) => CLOSE_PRESSED,
        (WindowButton::Close, false, true) => CLOSE_HOVER,
        (_, true, _) if pal.dark => Color32::from_white_alpha(11),
        (_, true, _) => Color32::from_black_alpha(6),
        (_, false, true) if pal.dark => Color32::from_white_alpha(15),
        (_, false, true) => Color32::from_black_alpha(10),
        _ => Color32::TRANSPARENT,
    };
    painter.rect_filled(rect, 0, fill);
    let color = if button == WindowButton::Close && (hovered || pressed) {
        Color32::WHITE
    } else if state.focused {
        pal.text
    } else {
        pal.text_disabled
    };
    glyph(
        painter,
        rect.center(),
        10.0,
        button,
        state.maximized,
        color,
        1.0,
    );
}

/// A round GNOME / libadwaita window button: a faint circle that darkens on
/// hover and press, a symbolic glyph.
fn paint_round(
    painter: &Painter,
    rect: Rect,
    button: WindowButton,
    hovered: bool,
    pressed: bool,
    state: WindowState,
    pal: &Palette,
) {
    let alpha = match (pressed, hovered) {
        (true, _) => 46,
        (false, true) => 30,
        _ => 17,
    };
    let fill = if pal.dark {
        Color32::from_white_alpha(alpha + 4)
    } else {
        Color32::from_black_alpha(alpha)
    };
    painter.circle_filled(rect.center(), rect.width() / 2.0, fill);
    let color = if state.focused {
        pal.text
    } else {
        pal.text_subtle
    };
    glyph(
        painter,
        rect.center(),
        8.0,
        button,
        state.maximized,
        color,
        1.5,
    );
}

/// A window button glyph centered at `c` in a `box_size` square, lines of
/// `width` physical pixels (whole pixels, on the pixel grid where they are
/// 1 px wide).
fn glyph(
    painter: &Painter,
    c: Pos2,
    box_size: f32,
    button: WindowButton,
    maximized: bool,
    color: Color32,
    width: f32,
) {
    let ppp = painter.pixels_per_point();
    let px = |v: f32| (v * ppp).round() / ppp;
    let w = (width * ppp).round().max(1.0) / ppp;
    let stroke = Stroke::new(w, color);
    // Pixel-aligned centre; odd-pixel lines sit on pixel centres.
    let odd = ((w * ppp).round() as i32) % 2 == 1;
    let half = if odd { 0.5 / ppp } else { 0.0 };
    let c = pos2(px(c.x) + half, px(c.y) + half);
    let h = px(box_size / 2.0);
    match button {
        WindowButton::Minimize => {
            painter.line_segment([pos2(c.x - h, c.y), pos2(c.x + h, c.y)], stroke);
        }
        WindowButton::Maximize if maximized => {
            // Restore: a front square and the corner of the one behind it.
            let o = px(box_size * 0.2);
            let front = Rect::from_min_max(pos2(c.x - h, c.y - h + o), pos2(c.x + h - o, c.y + h));
            painter.rect_stroke(front, px(1.0), stroke, egui::StrokeKind::Middle);
            painter.line_segment([pos2(c.x - h + o, c.y - h), pos2(c.x + h, c.y - h)], stroke);
            painter.line_segment([pos2(c.x + h, c.y - h), pos2(c.x + h, c.y + h - o)], stroke);
        }
        WindowButton::Maximize => {
            let r = Rect::from_min_max(pos2(c.x - h, c.y - h), pos2(c.x + h, c.y + h));
            painter.rect_stroke(r, px(1.0), stroke, egui::StrokeKind::Middle);
        }
        WindowButton::Close => {
            painter.line_segment([pos2(c.x - h, c.y - h), pos2(c.x + h, c.y + h)], stroke);
            painter.line_segment([pos2(c.x + h, c.y - h), pos2(c.x - h, c.y + h)], stroke);
        }
    }
}
