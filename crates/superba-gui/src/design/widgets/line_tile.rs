//! Line tile (moved from the overview page): a selectable 104 px card with the
//! line name, its host, and either a latency badge with a capability summary or
//! the pending switch countdown. Used by Overview, the wizard and Providers.

use egui::{
    Align, CursorIcon, Layout, Response, RichText, Sense, Stroke, StrokeKind, Ui, pos2, vec2,
};

use super::{Latency, LatencyBadge, Status, StatusDot, Text, focus_ring, show_focus};
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::icons::{self, Icon};
use crate::design::tokens::{ANIM_SECS, Palette, radius, text};

/// Height of a tile.
pub const TILE_HEIGHT: f32 = 104.0;

/// Tiles per row that keep them legible when a site has many lines or the window
/// is narrow (12 px gaps, at least 200 px per tile when all fit in one row).
pub fn tile_columns(width: f32, lines: usize) -> usize {
    let n = lines.max(1);
    if (width - 12.0 * (n - 1) as f32) / n as f32 >= 200.0 {
        n
    } else if width >= 412.0 {
        n.min(2)
    } else {
        1
    }
}

/// One line tile; `clicked()` on the response when chosen.
#[must_use = "add it with ui.add(..)"]
pub struct LineTile {
    name: String,
    host: String,
    hover: Option<String>,
    width: f32,
    selected: bool,
    pending: Option<String>,
    latency: Latency,
    caps: String,
}

impl LineTile {
    /// A tile `width` wide for the line `name` on `host`.
    pub fn new(name: impl Into<String>, host: impl Into<String>, width: f32) -> Self {
        Self {
            name: name.into(),
            host: host.into(),
            hover: None,
            width,
            selected: false,
            pending: None,
            latency: Latency::Unknown,
            caps: String::new(),
        }
    }

    /// Hover text (the line's full URL).
    pub fn hover(mut self, text: impl Into<String>) -> Self {
        self.hover = Some(text.into());
        self
    }

    /// The line Codex uses now (check mark, strong border).
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// A switch to this line is armed: its countdown label replaces the latency.
    pub fn pending(mut self, label: Option<String>) -> Self {
        self.pending = label;
        self
    }

    /// Latency badge.
    pub fn latency(mut self, latency: Latency) -> Self {
        self.latency = latency;
        self
    }

    /// Capability summary after the latency ("PAT and API key").
    pub fn caps(mut self, caps: impl Into<String>) -> Self {
        self.caps = caps.into();
        self
    }
}

fn paint_elided(
    painter: &egui::Painter,
    at: egui::Pos2,
    width: f32,
    label: &str,
    font_id: egui::FontId,
    color: egui::Color32,
) {
    let mut job = egui::text::LayoutJob::simple_singleline(label.to_owned(), font_id, color);
    job.wrap.max_width = width.max(0.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let galley = painter.layout_job(job);
    painter.galley(at, galley, color);
}

fn elided(ui: &mut Ui, text: RichText) {
    let full = text.text().to_owned();
    ui.add(egui::Label::new(text).truncate())
        .on_hover_text(full);
}

impl egui::Widget for LineTile {
    fn ui(self, ui: &mut Ui) -> Response {
        let pal = Palette::of(ui.ctx());
        let (rect, response) =
            ui.allocate_exact_size(vec2(self.width, TILE_HEIGHT), Sense::click());
        let response = response.on_hover_cursor(CursorIcon::PointingHand);
        let name = self.name.clone();
        let selected = self.selected;
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::RadioButton, true, selected, &name)
        });
        let t =
            ui.ctx()
                .animate_bool_with_time(response.id.with("h"), response.hovered(), ANIM_SECS);
        let pending = self.pending.is_some();
        let border = if pending {
            pal.status(Status::Warn)
        } else if self.selected {
            pal.text
        } else {
            pal.border.lerp_to_gamma(pal.border_strong, t)
        };
        let fill = if self.selected {
            pal.hover
        } else {
            pal.surface.lerp_to_gamma(pal.hover, t * 0.5)
        };
        let painter = ui.painter();
        painter.rect(
            rect,
            radius::CARD - 2,
            fill,
            Stroke::new(if pending { 2.0 } else { 1.0 }, border),
            StrokeKind::Inside,
        );
        let x = rect.min.x + 14.0;
        let mut y = rect.min.y + 13.0;
        paint_elided(
            painter,
            pos2(x, y),
            self.width - if self.selected { 54.0 } else { 28.0 },
            &self.name,
            font(text::BODY, Weight::Semibold),
            pal.text,
        );
        if self.selected && !pending {
            let r = egui::Rect::from_min_size(pos2(rect.max.x - 30.0, y), vec2(16.0, 16.0));
            painter.circle_filled(r.center(), 9.0, pal.primary);
            icons::paint(painter, r.shrink(3.5), Icon::Check, pal.on_primary);
        }
        y += 21.0;
        paint_elided(
            painter,
            pos2(x, y),
            self.width - 28.0,
            &self.host,
            mono_font(text::CAPTION, Weight::Regular),
            pal.text_subtle,
        );
        // Bottom row: latency badge + capability summary, or the pending switch.
        let badge_rect = egui::Rect::from_min_size(pos2(x, rect.max.y - 34.0), vec2(80.0, 22.0));
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(badge_rect.with_max_x(rect.max.x - 14.0))
                .layout(Layout::left_to_right(Align::Center)),
        );
        if let Some(label) = self.pending {
            child.add(StatusDot::new(Status::Warn).pulse(true));
            elided(
                &mut child,
                RichText::new(label)
                    .font(font(text::CAPTION, Weight::Medium))
                    .color(pal.status(Status::Warn)),
            );
        } else {
            child.add(LatencyBadge::new(self.latency));
            elided(&mut child, Text::small(self.caps, pal));
        }
        if show_focus(&response) {
            focus_ring(ui.painter(), rect, radius::CARD - 2, pal);
        }
        if response.gained_focus() {
            response.scroll_to_me(Some(Align::Center));
        }
        match self.hover {
            Some(h) => response.on_hover_text(h),
            None => response,
        }
    }
}
