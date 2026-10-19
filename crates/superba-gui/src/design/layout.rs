//! Page layout (UX §2.3): the content rectangle of a page, the "Fit" area that
//! gives a page exactly that rectangle without a page-level `ScrollArea`, fixed and
//! filling tracks for panes and rows, and a side pane that docks on wide windows
//! and turns into a sheet on narrow ones.
//!
//! A Fit page divides its rectangle with fixed rows (strip, footer) and columns
//! (rail, list, detail) and pages its lists ([`super::widgets::PagedTable`])
//! instead of scrolling. Below [`FIT_MIN_HEIGHT`] of content the whole page scrolls
//! so nothing is clipped.
//!
//! Every pane is clipped to its rectangle plus [`focus::BLEED`] on each side
//! ([`pane_clip`]): the gaps between panes and the page padding have room for
//! the focus ring of a control that sits flush with a pane edge.

use egui::{
    Align, Align2, Area, Frame, Id, Layout, Margin, Order, Rect, ScrollArea, Sense, Stroke, Ui,
    UiBuilder, pos2, vec2,
};

use super::tokens::{Palette, chrome, focus, radius, size};

/// Smallest page height (header included) a Fit page is drawn in without the
/// scroll fallback.
pub const FIT_MIN_HEIGHT: f32 = size::PAGE_HEADER + size::FIT_MIN_BODY;

/// Central-area width below which the card's side padding shrinks from
/// [`chrome::CARD_PAD`] to [`chrome::CARD_PAD_NARROW`].
const NARROW: f32 = 760.0;

/// The page rectangle inside the central panel `full` (right of the sidebar):
/// inside the content card ([`super::backdrop::panel_rect`]), centered, at most
/// [`size::CONTENT_WIDE`] (Fit pages) or [`size::CONTENT_MAX`] (Scroll pages)
/// wide with at least the card's padding on both sides, from
/// [`size::PAGE_TOP`] to [`size::PAGE_BOTTOM`] above the window's bottom edge.
pub fn page_rect(full: Rect, wide: bool) -> Rect {
    let pad = if full.width() < NARROW {
        chrome::CARD_PAD_NARROW
    } else {
        chrome::CARD_PAD
    };
    let max = if wide {
        size::CONTENT_WIDE
    } else {
        size::CONTENT_MAX
    };
    let card = full.x_range();
    let card = (card.min, card.max - chrome::CARD_INSET);
    let card_w = (card.1 - card.0).max(0.0);
    let width = (card_w - pad * 2.0).clamp(0.0, max);
    let left = card.0 + ((card_w - width) / 2.0).max(pad.min(card_w / 2.0));
    let top = full.min.y + size::PAGE_TOP;
    let bottom = (full.max.y - size::PAGE_BOTTOM).max(top);
    Rect::from_min_max(pos2(left, top), pos2(left + width, bottom))
}

/// What [`fit_area`] did.
#[derive(Debug)]
pub struct FitResponse<R> {
    /// The value returned by the page.
    pub inner: R,
    /// The rectangle the page was given.
    pub rect: Rect,
    /// The page was shorter than its minimum and drawn inside a scroll area.
    pub scrolled: bool,
}

/// Give `add` exactly the available rectangle of `ui` (clipped, no `ScrollArea`).
/// When that rectangle is lower than `min_h`, `add` gets a `min_h` tall rectangle
/// inside a vertical `ScrollArea` instead, so a very short window clips nothing.
pub fn fit_area<R>(ui: &mut Ui, min_h: f32, add: impl FnOnce(&mut Ui) -> R) -> FitResponse<R> {
    let rect = ui.available_rect_before_wrap();
    if rect.height() >= min_h {
        let inner = fixed(ui, rect, add);
        return FitResponse {
            inner,
            rect,
            scrolled: false,
        };
    }
    let out = ScrollArea::vertical()
        .id_salt(("superba-fit-fallback", ui.scope_id()))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let r = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), min_h));
            fixed(ui, r, add)
        });
    FitResponse {
        inner: out.inner,
        rect: out.inner_rect,
        scrolled: true,
    }
}

/// The clip rectangle of a pane at `rect` inside a parent clipped to `parent`:
/// `rect` plus [`focus::BLEED`] on each side, so the outside focus ring of a
/// control flush with the pane edge is not cut.
pub fn pane_clip(rect: Rect, parent: Rect) -> Rect {
    rect.expand(focus::BLEED).intersect(parent)
}

/// Draw `add` in a child `Ui` that owns exactly `rect` (top-down, clipped with
/// [`pane_clip`]) and allocate `rect` in `ui`.
pub fn fixed<R>(ui: &mut Ui, rect: Rect, add: impl FnOnce(&mut Ui) -> R) -> R {
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::top_down(Align::Min)),
    );
    child.set_clip_rect(pane_clip(rect, ui.clip_rect()));
    let inner = add(&mut child);
    ui.allocate_rect(rect, Sense::hover());
    inner
}

/// Transitional: draw a page whose content does not fit a fixed rectangle yet in
/// its own vertical `ScrollArea`. The page builder removes the call when the page
/// gets its fixed panes; Fit-page tests count such scroll areas.
pub fn scroll_body<R>(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    ScrollArea::vertical()
        .id_salt(id_salt)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            let r = add(ui);
            ui.add_space(40.0);
            r
        })
        .inner
}

/// Size of one track (column or row) of a pane layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Track {
    /// A fixed size in points (shrunk only when even the fixed tracks do not fit).
    Fixed(f32),
    /// An equal share of what the fixed tracks and gaps leave.
    Fill,
}

/// Sizes of `tracks` along an axis of length `total` with `gap` between tracks.
/// Fill tracks share the rest equally; when the fixed tracks alone are larger than
/// the room, they shrink proportionally and Fill tracks get nothing.
pub fn split(total: f32, tracks: &[Track], gap: f32) -> Vec<f32> {
    if tracks.is_empty() {
        return Vec::new();
    }
    let room = (total - gap * (tracks.len() - 1) as f32).max(0.0);
    let fixed: f32 = tracks
        .iter()
        .map(|t| match t {
            Track::Fixed(w) => w.max(0.0),
            Track::Fill => 0.0,
        })
        .sum();
    let fills = tracks.iter().filter(|t| matches!(t, Track::Fill)).count();
    let scale = if fixed > room && fixed > 0.0 {
        room / fixed
    } else {
        1.0
    };
    let share = if fills == 0 {
        0.0
    } else {
        ((room - fixed * scale) / fills as f32).max(0.0)
    };
    tracks
        .iter()
        .map(|t| match t {
            Track::Fixed(w) => w.max(0.0) * scale,
            Track::Fill => share,
        })
        .collect()
}

/// Column rectangles of `rect` (full height).
pub fn column_rects(rect: Rect, tracks: &[Track], gap: f32) -> Vec<Rect> {
    let mut x = rect.min.x;
    split(rect.width(), tracks, gap)
        .into_iter()
        .map(|w| {
            let r = Rect::from_min_max(pos2(x, rect.min.y), pos2(x + w, rect.max.y));
            x += w + gap;
            r
        })
        .collect()
}

/// Row rectangles of `rect` (full width).
pub fn row_rects(rect: Rect, tracks: &[Track], gap: f32) -> Vec<Rect> {
    let mut y = rect.min.y;
    split(rect.height(), tracks, gap)
        .into_iter()
        .map(|h| {
            let r = Rect::from_min_max(pos2(rect.min.x, y), pos2(rect.max.x, y + h));
            y += h + gap;
            r
        })
        .collect()
}

/// Lay the available rectangle of `ui` out in columns and draw column `i` with
/// `add(ui, i)` in its own child `Ui` (clipped with [`pane_clip`]). Returns the
/// column values.
pub fn columns<R>(
    ui: &mut Ui,
    tracks: &[Track],
    gap: f32,
    mut add: impl FnMut(&mut Ui, usize) -> R,
) -> Vec<R> {
    let rect = ui.available_rect_before_wrap();
    let rects = column_rects(rect, tracks, gap);
    let out = rects
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let mut child = ui.new_child(
                UiBuilder::new()
                    .max_rect(*r)
                    .layout(Layout::top_down(Align::Min)),
            );
            child.set_clip_rect(pane_clip(*r, ui.clip_rect()));
            add(&mut child, i)
        })
        .collect();
    ui.allocate_rect(rect, Sense::hover());
    out
}

/// Lay the available rectangle of `ui` out in rows (see [`columns`]).
pub fn rows<R>(
    ui: &mut Ui,
    tracks: &[Track],
    gap: f32,
    mut add: impl FnMut(&mut Ui, usize) -> R,
) -> Vec<R> {
    let rect = ui.available_rect_before_wrap();
    let rects = row_rects(rect, tracks, gap);
    let out = rects
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let mut child = ui.new_child(
                UiBuilder::new()
                    .max_rect(*r)
                    .layout(Layout::top_down(Align::Min)),
            );
            child.set_clip_rect(pane_clip(*r, ui.clip_rect()));
            add(&mut child, i)
        })
        .collect();
    ui.allocate_rect(rect, Sense::hover());
    out
}

/// A detail pane on the right of a page's main area: docked as a fixed column on
/// wide content, a sheet over the right edge below [`SidePane::dock_below`].
#[must_use = "call .show(..)"]
#[derive(Clone, Debug)]
pub struct SidePane {
    id: Id,
    width: f32,
    dock_min: f32,
    gap: f32,
}

impl SidePane {
    /// A pane `width` points wide (docked from 1120 px of content by default).
    pub fn new(id_salt: impl std::hash::Hash + std::fmt::Debug, width: f32) -> Self {
        Self {
            id: Id::unique(("superba-side-pane", id_salt)),
            width,
            dock_min: 1120.0,
            gap: 16.0,
        }
    }

    /// Below this content width the pane becomes a sheet.
    pub fn dock_below(mut self, width: f32) -> Self {
        self.dock_min = width;
        self
    }

    /// Gap between the main area and a docked pane.
    pub fn gap(mut self, gap: f32) -> Self {
        self.gap = gap;
        self
    }

    /// Whether the pane docks at `content_width`.
    pub fn docked(&self, content_width: f32) -> bool {
        content_width >= self.dock_min
    }

    /// Draw `main` and, while `open`, the pane. A sheet closes on Escape.
    pub fn show<R>(
        self,
        ui: &mut Ui,
        open: &mut bool,
        main: impl FnOnce(&mut Ui) -> R,
        pane: impl FnOnce(&mut Ui),
    ) -> R {
        let rect = ui.available_rect_before_wrap();
        let pal = Palette::of(ui.ctx());
        if !*open {
            return fixed(ui, rect, main);
        }
        if self.docked(rect.width()) {
            let cols = column_rects(rect, &[Track::Fill, Track::Fixed(self.width)], self.gap);
            let inner = {
                let mut child = ui.new_child(
                    UiBuilder::new()
                        .max_rect(cols[0])
                        .layout(Layout::top_down(Align::Min)),
                );
                child.set_clip_rect(pane_clip(cols[0], ui.clip_rect()));
                main(&mut child)
            };
            let x = cols[1].min.x - self.gap / 2.0;
            ui.painter()
                .vline(x, cols[1].y_range(), Stroke::new(1.0, pal.border));
            let mut child = ui.new_child(
                UiBuilder::new()
                    .max_rect(cols[1])
                    .layout(Layout::top_down(Align::Min)),
            );
            child.set_clip_rect(pane_clip(cols[1], ui.clip_rect()));
            pane(&mut child);
            ui.allocate_rect(rect, Sense::hover());
            return inner;
        }
        let inner = fixed(ui, rect, main);
        let w = self.width.min(rect.width());
        let sheet = Rect::from_min_max(pos2(rect.max.x - w, rect.min.y), rect.max);
        Area::new(self.id.with("sheet"))
            .order(Order::Foreground)
            .pivot(Align2::LEFT_TOP)
            .fixed_pos(sheet.min)
            .constrain(false)
            .show(ui.ctx(), |ui| {
                Frame::new()
                    .fill(pal.surface)
                    .stroke(Stroke::new(1.0, pal.border))
                    .corner_radius(radius::CARD)
                    .inner_margin(Margin::same(16))
                    .show(ui, |ui| {
                        ui.set_width(w - 32.0);
                        ui.set_height(sheet.height() - 32.0);
                        pane(ui);
                    });
            });
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            *open = false;
        }
        inner
    }
}

/// Fixed section rail and independently scrolling detail pane.
pub fn rail_split<L, R>(
    ui: &mut Ui,
    rail_w: f32,
    rail: impl FnOnce(&mut Ui) -> L,
    detail: impl FnOnce(&mut Ui) -> R,
) -> (L, R) {
    rail_split_with_id(ui, rail_w, "section-detail", rail, detail)
}

/// A section rail whose detail keeps independent scroll and widget state per id.
pub fn rail_split_with_id<L, R>(
    ui: &mut Ui,
    rail_w: f32,
    detail_id: impl egui::AsIdSalt,
    rail: impl FnOnce(&mut Ui) -> L,
    detail: impl FnOnce(&mut Ui) -> R,
) -> (L, R) {
    let rect = ui.available_rect_before_wrap();
    let width = rail_w.min((rect.width() - 100.0).max(0.0));
    let left = Rect::from_min_max(rect.min, pos2(rect.min.x + width, rect.max.y));
    let right = Rect::from_min_max(pos2(left.max.x + 24.0, rect.min.y), rect.max);
    let mut rail_ui = ui.new_child(
        UiBuilder::new()
            .id_salt("section-rail")
            .max_rect(left)
            .layout(Layout::top_down(Align::Min)),
    );
    rail_ui.set_clip_rect(pane_clip(left, ui.clip_rect()));
    let l = rail(&mut rail_ui);
    let pal = Palette::of(ui.ctx());
    ui.painter().vline(
        left.max.x + 12.0,
        rect.y_range(),
        Stroke::new(1.0, pal.border),
    );
    let mut detail_ui = ui.new_child(
        UiBuilder::new()
            .id_salt(detail_id)
            .max_rect(right)
            .layout(Layout::top_down(Align::Min)),
    );
    detail_ui.set_clip_rect(pane_clip(right, ui.clip_rect()));
    let r = egui::ScrollArea::vertical()
        .id_salt("section-detail-scroll")
        .auto_shrink([false, false])
        .show(&mut detail_ui, detail)
        .inner;
    ui.allocate_rect(rect, Sense::hover());
    (l, r)
}
