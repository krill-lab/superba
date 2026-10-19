//! List rows after Surge's process list (user request 24): an icon in a soft
//! rounded tile, the name (and an optional gray line below it), and a value
//! right-aligned with a small gray secondary line under it; plus the share row
//! of the Overview's usage list (name and value over a proportional bar).

use egui::{Color32, CursorIcon, Rect, Response, Sense, Ui, Widget, pos2, vec2};

use super::{focus_ring, hover_t, show_focus};
use crate::design::fonts::{Weight, font};
use crate::design::icons::{self, Icon};
use crate::design::tokens::{Palette, radius, text};

/// Paint `icon` centred in a rounded tile of side `side` at `center`.
pub fn icon_tile(ui: &Ui, center: egui::Pos2, side: f32, icon: Icon, tint: Option<Color32>) {
    let pal = Palette::of(ui.ctx());
    let rect = Rect::from_center_size(center, vec2(side, side));
    let fill = match tint {
        Some(c) => {
            Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), if pal.dark { 46 } else { 30 })
        }
        None => pal.hover,
    };
    ui.painter()
        .rect_filled(rect, (side * 0.28).round() as u8, fill);
    let inner = side * 0.58;
    icons::paint(
        ui.painter(),
        Rect::from_center_size(center, vec2(inner, inner)),
        icon,
        tint.unwrap_or(pal.text_muted),
    );
}

/// Elide `s` to `width` in `font` (one line, "…" at the end).
fn elided(
    ui: &Ui,
    s: &str,
    font_id: egui::FontId,
    color: Color32,
    width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        s.to_owned(),
        egui::TextFormat {
            font_id,
            color,
            ..Default::default()
        },
    );
    job.wrap.max_width = width.max(8.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    ui.painter().layout_job(job)
}

/// What a Surge-style row shows: icon tile (with an optional status dot on its
/// corner), name with an optional gray line, and a right-aligned value with a
/// small gray secondary line.
#[derive(Clone, Debug)]
pub struct RowContent {
    /// Icon in the tile.
    pub icon: Icon,
    /// Colour of the icon and its tile (default: neutral gray).
    pub tint: Option<Color32>,
    /// Status dot on the tile's corner.
    pub dot: Option<Color32>,
    /// Name.
    pub title: String,
    /// Gray line under the name.
    pub subtitle: Option<String>,
    /// Right-aligned value.
    pub value: Option<String>,
    /// Small gray line under the value.
    pub secondary: Option<String>,
}

impl RowContent {
    /// A row with an icon and a name.
    pub fn new(icon: Icon, title: impl Into<String>) -> Self {
        Self {
            icon,
            tint: None,
            dot: None,
            title: title.into(),
            subtitle: None,
            value: None,
            secondary: None,
        }
    }

    /// What the row shows as one line, for screen readers (user bug 56): the
    /// name, the line under it, the value and the line under the value.
    pub fn accessible_name(&self) -> String {
        std::iter::once(&self.title)
            .chain(&self.subtitle)
            .chain(&self.value)
            .chain(&self.secondary)
            .filter(|s| !s.trim().is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Side of the icon tile in a row of height `h`.
pub fn tile_side(h: f32) -> f32 {
    (h - 20.0).clamp(24.0, 34.0)
}

/// Paint `row` into `rect` (no allocation, no selection fill): for custom lists
/// and table cells. Returns the rectangle the name was drawn in.
pub fn paint_row(ui: &Ui, rect: Rect, row: &RowContent) -> Rect {
    let pal = Palette::of(ui.ctx());
    let tile = tile_side(rect.height());
    let tile_center = pos2(rect.min.x + 10.0 + tile / 2.0, rect.center().y);
    icon_tile(ui, tile_center, tile, row.icon, row.tint);
    if let Some(dot) = row.dot {
        let c = tile_center + vec2(tile / 2.0 - 2.0, tile / 2.0 - 2.0);
        let ring = if pal.dark {
            pal.surface
        } else {
            Color32::WHITE
        };
        ui.painter().circle_filled(c, 5.5, ring);
        ui.painter().circle_filled(c, 4.0, dot);
    }
    let left = rect.min.x + 10.0 + tile + 12.0;
    let right = rect.max.x - 12.0;
    let value = row.value.as_ref().map(|v| {
        ui.painter()
            .layout_no_wrap(v.clone(), font(text::BODY, Weight::Medium), pal.text)
    });
    let secondary = row.secondary.as_ref().map(|v| {
        ui.painter().layout_no_wrap(
            v.clone(),
            font(text::CAPTION, Weight::Regular),
            pal.text_subtle,
        )
    });
    // The name shares its line with the value, the gray line with the
    // secondary value: each gets the room its neighbour leaves (a long value
    // never cuts the status line under the name, review ux-visual #12).
    let room = |g: Option<f32>| (right - g.map_or(0.0, |w| w + 14.0) - left).max(40.0);
    let (value_w, secondary_w) = (
        value.as_ref().map(|g| g.size().x),
        secondary.as_ref().map(|g| g.size().x),
    );
    let name_w = if value.is_some() {
        room(value_w)
    } else {
        // Without a value the secondary text is centred beside both lines.
        room(secondary_w)
    };
    let sub_w = if value.is_some() {
        room(secondary_w)
    } else {
        name_w
    };
    let title = elided(
        ui,
        &row.title,
        font(text::BODY + 1.0, Weight::Medium),
        pal.text,
        name_w,
    );
    let subtitle = row.subtitle.as_ref().map(|s| {
        elided(
            ui,
            s,
            font(text::CAPTION, Weight::Regular),
            pal.text_subtle,
            sub_w,
        )
    });
    let p = ui.painter();
    let two = |a: f32, b: Option<f32>| match b {
        Some(b) => {
            let total = a + 2.0 + b;
            (
                rect.center().y - total / 2.0,
                Some(rect.center().y - total / 2.0 + a + 2.0),
            )
        }
        None => (rect.center().y - a / 2.0, None),
    };
    let (ty, sy) = two(title.size().y, subtitle.as_ref().map(|g| g.size().y));
    let name_rect = Rect::from_min_size(pos2(left, ty), title.size());
    p.galley(pos2(left, ty), title, pal.text);
    if let (Some(g), Some(y)) = (subtitle, sy) {
        p.galley(pos2(left, y), g, pal.text_subtle);
    }
    if let Some(v) = value {
        let (mut vy, sy) = two(v.size().y, secondary.as_ref().map(|g| g.size().y));
        if secondary.is_none() && row.subtitle.is_some() {
            // On the name's line: the gray line under the name takes the
            // row's whole width below it.
            vy = name_rect.center().y - v.size().y / 2.0;
        }
        p.galley(pos2(right - v.size().x, vy), v, pal.text);
        if let (Some(g), Some(y)) = (secondary, sy) {
            p.galley(pos2(right - g.size().x, y), g, pal.text_subtle);
        }
    } else if let Some(g) = secondary {
        p.galley(
            pos2(right - g.size().x, rect.center().y - g.size().y / 2.0),
            g,
            pal.text_subtle,
        );
    }
    name_rect
}

/// A Surge-style list row widget ([`RowContent`] with a selection and hover
/// fill).
#[must_use = "add it with ui.add(..)"]
pub struct ListRow {
    content: RowContent,
    selected: bool,
    height: f32,
    sense: Sense,
    status: Option<(super::Status, String)>,
}

impl ListRow {
    /// New row (56 px, clickable).
    pub fn new(icon: Icon, title: impl Into<String>) -> Self {
        Self {
            content: RowContent::new(icon, title),
            selected: false,
            height: 56.0,
            sense: Sense::click(),
            status: None,
        }
    }

    /// Colour of the icon (and its tile).
    pub fn tint(mut self, c: Color32) -> Self {
        self.content.tint = Some(c);
        self
    }

    /// Status dot on the icon tile.
    pub fn dot(mut self, c: Color32) -> Self {
        self.content.dot = Some(c);
        self
    }

    /// Gray line under the name.
    pub fn subtitle(mut self, s: impl Into<String>) -> Self {
        self.content.subtitle = Some(s.into());
        self
    }

    /// Right-aligned value.
    pub fn value(mut self, s: impl Into<String>) -> Self {
        self.content.value = Some(s.into());
        self
    }

    /// Small gray line under the value.
    pub fn secondary(mut self, s: impl Into<String>) -> Self {
        self.content.secondary = Some(s.into());
        self
    }

    /// State dot at the far right; its meaning is included in the accessible name.
    pub fn status(mut self, status: super::Status, label: impl Into<String>) -> Self {
        self.status = Some((status, label.into()));
        self
    }

    /// Selected (soft gray fill).
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Row height.
    pub fn height(mut self, h: f32) -> Self {
        self.height = h;
        self
    }

    /// Display only (no click).
    pub fn static_row(mut self) -> Self {
        self.sense = Sense::hover();
        self
    }
}

impl Widget for ListRow {
    fn ui(self, ui: &mut Ui) -> Response {
        let pal = Palette::of(ui.ctx());
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), self.height), self.sense);
        let clickable = self.sense.senses_click();
        let response = if clickable {
            response.on_hover_cursor(CursorIcon::PointingHand)
        } else {
            response
        };
        let mut title = self.content.accessible_name();
        if let Some((_, label)) = &self.status {
            title.push_str(&format!(", {label}"));
        }
        let selected = self.selected;
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::ListItem, ui.is_enabled(), selected, &title)
        });
        crate::design::a11y::selectable(&response, egui::Role::ListItem, &title, selected);
        if !ui.is_rect_visible(rect) {
            return response;
        }
        let t = if clickable {
            hover_t(ui, &response)
        } else {
            0.0
        };
        let fill = if self.selected {
            pal.active
        } else {
            Color32::TRANSPARENT.lerp_to_gamma(pal.hover, t)
        };
        ui.painter().rect_filled(rect, radius::ROW, fill);
        let mut content_rect = rect;
        if let Some((status, _)) = &self.status {
            content_rect.max.x -= 20.0;
            super::StatusDot::new(*status)
                .size(6.0)
                .paint_at(ui, pos2(rect.right() - 12.0, rect.center().y));
        }
        paint_row(ui, content_rect, &self.content);
        if clickable && show_focus(&response) {
            focus_ring(ui.painter(), rect, radius::ROW, pal);
        }
        response
    }
}

/// One row of a usage share list: icon, name and value on one line, a
/// proportional bar under the name (Surge's 流量 list).
#[must_use = "add it with ui.add(..)"]
pub struct ShareRow {
    icon: Icon,
    name: String,
    value: String,
    share: f32,
    color: Color32,
}

impl ShareRow {
    /// `share` of the largest row (0..=1) sets the bar length.
    pub fn new(
        icon: Icon,
        name: impl Into<String>,
        value: impl Into<String>,
        share: f32,
        color: Color32,
    ) -> Self {
        Self {
            icon,
            name: name.into(),
            value: value.into(),
            share: share.clamp(0.0, 1.0),
            color,
        }
    }
}

/// Height of a [`ShareRow`].
pub const SHARE_ROW_HEIGHT: f32 = 36.0;

impl Widget for ShareRow {
    fn ui(self, ui: &mut Ui) -> Response {
        let pal = Palette::of(ui.ctx());
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), SHARE_ROW_HEIGHT), Sense::hover());
        if !ui.is_rect_visible(rect) {
            return response;
        }
        let tile = 22.0;
        let cy = rect.min.y + 13.0;
        icon_tile(ui, pos2(rect.min.x + tile / 2.0, cy), tile, self.icon, None);
        let left = rect.min.x + tile + 10.0;
        let value = ui.painter().layout_no_wrap(
            self.value.clone(),
            font(text::SMALL + 1.0, Weight::Medium),
            pal.text,
        );
        let name_w = (rect.max.x - value.size().x - 12.0 - left).max(30.0);
        let name = elided(
            ui,
            &self.name,
            font(text::SMALL + 1.0, Weight::Regular),
            pal.text,
            name_w,
        );
        let p = ui.painter();
        p.galley(pos2(left, cy - name.size().y / 2.0), name, pal.text);
        p.galley(
            pos2(rect.max.x - value.size().x, cy - value.size().y / 2.0),
            value,
            pal.text,
        );
        let track_w = rect.max.x - left;
        let bar = Rect::from_min_size(
            pos2(left, rect.min.y + 26.0),
            vec2((track_w * self.share).max(4.0), 4.0),
        );
        p.rect_filled(bar, 2, self.color);
        response
    }
}
