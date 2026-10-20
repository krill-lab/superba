//! Neutral pill badges with an optional status dot and line icon. A badge is
//! a text label to screen readers and UI Automation (user bug 77: "Running" /
//! "Stopped" on Remote hosts were painted only, so nothing could read them).

use egui::{Color32, Rect, Response, Sense, Stroke, StrokeKind, Ui, Widget, pos2, vec2};

use super::Status;
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::icons::{self, Icon};
use crate::design::tokens::{Palette, radius, text};

/// Side length of a badge icon (points).
const ICON: f32 = 12.0;

/// A small neutral pill: `● Available`, `PAT`, `0.162.0`.
#[must_use = "add it with ui.add(..)"]
pub struct Badge {
    text: String,
    status: Option<Status>,
    icon: Option<Icon>,
    mono: bool,
    outline: bool,
    hollow: bool,
    clickable: bool,
}

impl Badge {
    /// New badge.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            status: None,
            icon: None,
            mono: false,
            outline: false,
            hollow: false,
            clickable: false,
        }
    }

    /// Prefix a status dot, or a rotating arc for `Busy`.
    pub fn status(mut self, status: Status) -> Self {
        self.status = Some(status);
        self.hollow = false;
        self
    }

    /// Use an empty circle for an absent installation or connection.
    pub fn hollow(mut self) -> Self {
        self.hollow = true;
        self.status = Some(Status::Idle);
        self
    }

    /// Prefix a line icon (after the status dot), in the text colour. Symbols
    /// the bundled fonts lack (check marks, two-way arrows) are drawn this way.
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Monospace text (versions, ports).
    pub fn mono(mut self) -> Self {
        self.mono = true;
        self
    }

    /// Border instead of fill.
    pub fn outline(mut self) -> Self {
        self.outline = true;
        self
    }

    /// A chip that opens something (details, a menu): it takes clicks, shows
    /// the pointing hand and a hover fill, and is a button to screen readers.
    pub fn clickable(mut self) -> Self {
        self.clickable = true;
        self
    }
}

impl Widget for Badge {
    fn ui(self, ui: &mut Ui) -> Response {
        let pal = Palette::of(ui.ctx());
        let font_id = if self.mono {
            mono_font(text::CAPTION, Weight::Regular)
        } else {
            font(text::CAPTION, Weight::Medium)
        };
        let galley = ui
            .painter()
            .layout_no_wrap(self.text.clone(), font_id, Color32::PLACEHOLDER);
        let dot_w = if self.status.is_some() { 12.0 } else { 0.0 };
        let icon_w = if self.icon.is_some() { ICON + 4.0 } else { 0.0 };
        let size = vec2(galley.size().x + 16.0 + dot_w + icon_w, 20.0);
        let sense = if self.clickable {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, response) = ui.allocate_exact_size(size, sense);
        let response = if self.clickable {
            let enabled = ui.is_enabled();
            let text = self.text.clone();
            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, &text));
            if enabled {
                response.on_hover_cursor(egui::CursorIcon::PointingHand)
            } else {
                response
            }
        } else {
            crate::design::a11y::text(&response, &self.text);
            response
        };
        if self.status == Some(Status::Busy) {
            ui.ctx()
                .accesskit_node_builder(response.id, |n| n.set_busy());
        }
        if ui.is_rect_visible(rect) {
            let t = if self.clickable {
                super::hover_t(ui, &response)
            } else {
                0.0
            };
            let painter = ui.painter();
            if self.outline {
                painter.rect_stroke(
                    rect,
                    radius::PILL,
                    Stroke::new(1.0, pal.border.lerp_to_gamma(pal.border_strong, t)),
                    StrokeKind::Inside,
                );
            } else {
                painter.rect_filled(rect, radius::PILL, pal.hover.lerp_to_gamma(pal.active, t));
            }
            if self.clickable && super::show_focus(&response) {
                super::focus_ring(painter, rect, radius::PILL, pal);
            }
            let mut x = rect.min.x + 8.0;
            if let Some(status) = self.status {
                let center = pos2(x + 3.0, rect.center().y);
                if status == Status::Busy {
                    icons::spinner(
                        painter,
                        Rect::from_center_size(center, vec2(10.0, 10.0)),
                        pal.text_muted,
                        ui.input(|i| i.time),
                    );
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(80));
                } else if self.hollow {
                    painter.circle_stroke(center, 3.0, Stroke::new(1.0, pal.text_muted));
                } else {
                    painter.circle_filled(center, 3.0, pal.status(status));
                }
                x += dot_w;
            }
            if let Some(icon) = self.icon {
                let icon_rect =
                    Rect::from_center_size(pos2(x + ICON / 2.0, rect.center().y), vec2(ICON, ICON));
                icons::paint(painter, icon_rect, icon, pal.text_muted);
                x += icon_w;
            }
            let text_rect = Rect::from_min_size(
                pos2(x, rect.center().y - galley.size().y / 2.0),
                galley.size(),
            );
            painter.galley(text_rect.min, galley, pal.text_muted);
        }
        response
    }
}
