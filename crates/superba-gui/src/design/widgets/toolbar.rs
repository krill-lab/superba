//! Bottom toolbar of a log (UX §3.5, like Surge): a hairline, then small ghost
//! buttons on the left and the right. [`tool_toggle`] is a button with a state
//! dot (`○` off, `●` on, amber while a sensitive option is on).

use egui::{CursorIcon, Rect, Response, Sense, Stroke, Ui, UiBuilder, pos2, vec2};

use super::Status;
use crate::design::fonts::{Weight, font};
use crate::design::icons::{self, Icon};
use crate::design::tokens::{Palette, radius, text};

/// Height of the toolbar.
pub const HEIGHT: f32 = 34.0;

/// The toolbar.
#[derive(Clone, Copy, Debug, Default)]
pub struct Toolbar;

impl Toolbar {
    /// Draw a toolbar of [`HEIGHT`] at the top of the available rectangle: `left`
    /// and `right` add their buttons (laid out left to right / right to left).
    pub fn show<R, S>(
        ui: &mut Ui,
        left: impl FnOnce(&mut Ui) -> R,
        right: impl FnOnce(&mut Ui) -> S,
    ) -> (R, S) {
        let pal = Palette::of(ui.ctx());
        let avail = ui.available_rect_before_wrap();
        let rect = Rect::from_min_size(avail.min, vec2(avail.width(), HEIGHT));
        ui.painter().hline(
            rect.x_range(),
            rect.min.y + 0.5,
            Stroke::new(1.0, pal.border),
        );
        let inner = rect.shrink2(vec2(0.0, 3.0));
        let mut l = ui.new_child(
            UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        l.spacing_mut().item_spacing.x = 2.0;
        let out = left(&mut l);
        let mut r = ui.new_child(
            UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        r.spacing_mut().item_spacing.x = 2.0;
        let out_right = right(&mut r);
        ui.allocate_rect(rect, Sense::hover());
        (out, out_right)
    }
}

fn paint_button(
    ui: &mut Ui,
    icon: Option<Icon>,
    dot: Option<Status>,
    label: &str,
    enabled: bool,
) -> Response {
    let pal = Palette::of(ui.ctx());
    let color = if enabled {
        pal.text_muted
    } else {
        pal.text_disabled
    };
    let g =
        ui.painter()
            .layout_no_wrap(label.to_owned(), font(text::CAPTION, Weight::Medium), color);
    let lead = if icon.is_some() || dot.is_some() {
        18.0
    } else {
        0.0
    };
    let size = vec2(g.size().x + 16.0 + lead, 26.0);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (r, resp) = ui.allocate_exact_size(size, sense);
    let resp = if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    };
    let hovered = enabled && resp.hovered();
    if hovered {
        ui.painter().rect_filled(r, radius::CONTROL, pal.hover);
    }
    let text_color = if hovered { pal.text } else { color };
    let mut x = r.min.x + 8.0;
    if let Some(icon) = icon {
        let ir = Rect::from_center_size(pos2(x + 6.0, r.center().y), vec2(13.0, 13.0));
        icons::paint(ui.painter(), ir, icon, text_color);
        x += lead;
    } else if let Some(status) = dot {
        let c = pos2(x + 5.0, r.center().y);
        if status == Status::Idle {
            ui.painter()
                .circle_stroke(c, 3.5, Stroke::new(1.2, pal.text_subtle));
        } else {
            ui.painter().circle_filled(c, 3.5, pal.status(status));
        }
        x += lead;
    }
    ui.painter()
        .galley(pos2(x, r.center().y - g.size().y / 2.0), g, text_color);
    let label = label.to_owned();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, &label));
    resp
}

/// A small toolbar button with an optional icon.
pub fn tool_button(ui: &mut Ui, icon: Option<Icon>, label: &str, enabled: bool) -> Response {
    paint_button(ui, icon, None, label, enabled)
}

/// A toolbar button with a state dot: hollow while off, `on_status` while on.
pub fn tool_toggle(
    ui: &mut Ui,
    label: &str,
    on: bool,
    on_status: Status,
    enabled: bool,
) -> Response {
    let dot = if on { on_status } else { Status::Idle };
    let r = paint_button(ui, None, Some(dot), label, enabled);
    let label = label.to_owned();
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, enabled, on, &label));
    r
}
