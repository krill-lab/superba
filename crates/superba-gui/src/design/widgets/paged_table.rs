//! Paged table (UX §4): a [`Table`] with fixed row height whose rows per page
//! come from the height it is given, a selected row that survives resizes (the
//! page re-anchors on it), keyboard navigation (↑ ↓ PgUp PgDn Home End Enter) and
//! a [`Pager`] footer. Used by every catalog-style list of a Fit page instead of a
//! scroll area.

use egui::{Id, Key, Rect, Response, Sense, Stroke, Ui, UiBuilder, vec2};

use super::{Pager, Table};
use crate::design::fonts::{Weight, font};
use crate::design::tokens::{Palette, radius, size, text};

/// Rows that fit `height` at `row_height` (at least 1).
pub fn rows_per_page(height: f32, row_height: f32) -> usize {
    if row_height <= 0.0 {
        return 1;
    }
    ((height / row_height).floor() as usize).max(1)
}

/// Paging and selection of a [`PagedTable`], kept in the page's state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PagedState {
    /// Zero-based page.
    pub page: usize,
    /// Selected row (index into the whole list).
    pub selected: Option<usize>,
    /// Rows per page of the last frame (0 before the first frame).
    pub per_page: usize,
}

impl PagedState {
    /// Rows of `total` shown on the current page.
    pub fn visible(&self, total: usize) -> std::ops::Range<usize> {
        let per = self.per_page.max(1);
        let start = (self.page * per).min(total);
        start..(start + per).min(total)
    }

    /// Apply a new page size: keep the selected row on screen, else keep the
    /// first visible row on screen, and clamp to the last page.
    pub fn resize(&mut self, per_page: usize, total: usize) {
        let per_page = per_page.max(1);
        if per_page != self.per_page {
            let anchor = self.selected.unwrap_or(self.page * self.per_page.max(1));
            self.per_page = per_page;
            self.page = anchor / per_page;
        }
        self.clamp(total);
    }

    /// Clamp page and selection to `total` rows (a shorter list after a filter).
    pub fn clamp(&mut self, total: usize) {
        let pages = super::pager::page_count(total, self.per_page.max(1));
        self.page = self.page.min(pages - 1);
        if let Some(s) = self.selected
            && s >= total
        {
            self.selected = total.checked_sub(1);
        }
    }

    /// Select `row` (clamped) and turn to its page.
    pub fn select(&mut self, row: usize, total: usize) {
        if total == 0 {
            self.selected = None;
            return;
        }
        let row = row.min(total - 1);
        self.selected = Some(row);
        self.page = row / self.per_page.max(1);
    }

    /// Apply one navigation key; true when it was used.
    pub fn key(&mut self, key: Key, total: usize) -> bool {
        if total == 0 {
            return false;
        }
        let per = self.per_page.max(1);
        let cur = self.selected;
        let next = match key {
            Key::ArrowDown => cur.map_or(self.page * per, |s| s + 1),
            Key::ArrowUp => cur.map_or(self.page * per, |s| s.saturating_sub(1)),
            Key::PageDown => {
                let pages = super::pager::page_count(total, per);
                let page = (self.page + 1).min(pages - 1);
                cur.map_or(page * per, |s| (s + per).min(total - 1).max(page * per))
            }
            Key::PageUp => {
                let page = self.page.saturating_sub(1);
                cur.map_or(page * per, |s| s.saturating_sub(per))
            }
            Key::Home => 0,
            Key::End => total - 1,
            _ => return false,
        };
        self.select(next, total);
        true
    }
}

/// What happened in a [`PagedTable`] this frame.
#[derive(Debug)]
pub struct PagedResponse {
    /// The rows area (keyboard focus; header and footer excluded).
    pub response: Response,
    /// The selection changed (click or key).
    pub selection_changed: bool,
    /// A row was activated (Enter or double click).
    pub activated: Option<usize>,
    /// The page changed (pager or key).
    pub page_changed: bool,
    /// Rows per page this frame.
    pub per_page: usize,
    /// The rows drawn this frame (index into the whole list) with their
    /// responses: context menus, and telling a double click from Enter.
    pub rows: Vec<(usize, Response)>,
}

/// Extra footer content drawn right-aligned next to the pager.
type FooterFn<'a> = Box<dyn FnOnce(&mut Ui) + 'a>;

/// A paged, selectable table filling the height it is given.
#[must_use = "call .show(ui, ..)"]
pub struct PagedTable<'a> {
    id: Id,
    table: Table,
    header: Option<Vec<String>>,
    row_height: f32,
    total: usize,
    state: &'a mut PagedState,
    pager: bool,
    keys_when_idle: bool,
    footer_extra: Option<FooterFn<'a>>,
    tile_gap: Option<f32>,
    right_aligned: Vec<usize>,
}

impl<'a> PagedTable<'a> {
    /// A table over `total` rows with column `table`, bound to `state`.
    pub fn new(
        id_salt: impl std::hash::Hash + std::fmt::Debug,
        table: Table,
        total: usize,
        state: &'a mut PagedState,
    ) -> Self {
        Self {
            id: Id::unique(("superba-paged-table", id_salt)),
            table,
            header: None,
            row_height: size::ROW,
            total,
            state,
            pager: true,
            keys_when_idle: false,
            footer_extra: None,
            tile_gap: None,
            right_aligned: Vec::new(),
        }
    }

    /// Right-align the header labels of these columns (numbers, whose cells
    /// are right-aligned too).
    pub fn right_aligned(mut self, columns: &[usize]) -> Self {
        self.right_aligned = columns.to_vec();
        self
    }

    /// Caption header row (28 px).
    pub fn header(mut self, labels: Vec<String>) -> Self {
        self.header = Some(labels);
        self
    }

    /// Row height (default 32 px; 44 px two-line tables, 52 px two-line lists).
    pub fn row_height(mut self, height: f32) -> Self {
        self.row_height = height;
        self
    }

    /// Rows as separate rounded tiles (a card list such as the provider list):
    /// `gap` px of space between rows and no divider lines, so a line never
    /// runs between the rounded selection and hover fills.
    pub fn tiles(mut self, gap: f32) -> Self {
        self.tile_gap = Some(gap.max(0.0));
        self
    }

    /// No pager footer (the page draws its own).
    pub fn without_pager(mut self) -> Self {
        self.pager = false;
        self
    }

    /// Also handle the navigation keys while nothing has keyboard focus (the
    /// page's main list).
    pub fn keys_when_idle(mut self, on: bool) -> Self {
        self.keys_when_idle = on;
        self
    }

    /// Extra content on the right of the pager footer (a pending bar).
    pub fn footer(mut self, add: impl FnOnce(&mut Ui) + 'a) -> Self {
        self.footer_extra = Some(Box::new(add));
        self
    }

    /// Draw rows with `cell(ui, row, column)`.
    pub fn show(self, ui: &mut Ui, mut cell: impl FnMut(&mut Ui, usize, usize)) -> PagedResponse {
        let pal = Palette::of(ui.ctx());
        let rect = ui.available_rect_before_wrap();
        let header_h = if self.header.is_some() {
            size::TABLE_HEADER
        } else {
            0.0
        };
        let footer_h = if self.pager || self.footer_extra.is_some() {
            size::FOOTER
        } else {
            0.0
        };
        let list_h = (rect.height() - header_h - footer_h).max(self.row_height);
        // Tiles: n rows take n heights and n - 1 gaps.
        let gap = self.tile_gap.unwrap_or(0.0);
        let pitch = self.row_height + gap;
        let per_page = rows_per_page(list_h + gap, pitch);
        let before = (self.state.page, self.state.selected);
        self.state.resize(per_page, self.total);

        // The keyboard focus (and its ring) belongs to the rows: a ring around
        // the whole widget would frame the header and the pager footer too.
        let rows_rect = Rect::from_min_max(
            egui::pos2(rect.min.x, rect.min.y + header_h),
            egui::pos2(
                rect.max.x,
                (rect.max.y - footer_h).max(rect.min.y + header_h),
            ),
        );
        let response = ui.interact(rows_rect, self.id, Sense::focusable_noninteractive());
        let focused = response.has_focus();
        if focused {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    self.id,
                    egui::EventFilter {
                        vertical_arrows: true,
                        ..Default::default()
                    },
                );
            });
        }
        let idle = self.keys_when_idle && ui.memory(|m| m.focused().is_none());
        let mut activated = None;
        if focused || idle {
            for key in [
                Key::ArrowDown,
                Key::ArrowUp,
                Key::PageDown,
                Key::PageUp,
                Key::Home,
                Key::End,
            ] {
                if ui.input(|i| i.key_pressed(key)) {
                    self.state.key(key, self.total);
                }
            }
            if ui.input(|i| i.key_pressed(Key::Enter)) {
                activated = self.state.selected;
            }
        }

        let mut y = rect.min.y;
        if let Some(labels) = &self.header {
            let r = Rect::from_min_size(rect.min, vec2(rect.width(), header_h));
            for (i, (col, label)) in self.table.cells(r).iter().zip(labels).enumerate() {
                let (x, align) = if self.right_aligned.contains(&i) {
                    (col.max.x, egui::Align2::RIGHT_CENTER)
                } else {
                    (col.min.x, egui::Align2::LEFT_CENTER)
                };
                ui.painter().text(
                    egui::pos2(x, col.center().y),
                    align,
                    label,
                    font(text::CAPTION, Weight::Medium),
                    pal.text_subtle,
                );
            }
            ui.painter()
                .hline(r.x_range(), r.max.y - 0.5, Stroke::new(1.0, pal.border));
            y += header_h;
        }
        let rows = self.state.visible(self.total);
        let last = rows.end.saturating_sub(1);
        let mut clicked = None;
        let mut row_responses = Vec::with_capacity(rows.len());
        for row in rows {
            let r = egui::Rect::from_min_size(
                egui::pos2(rect.min.x, y),
                vec2(rect.width(), self.row_height),
            );
            let resp = ui
                .interact(r, self.id.with(("row", row)), Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            let selected = self.state.selected == Some(row);
            // Tiles get the softer list-row radius (Surge-like lists).
            let rr = if self.tile_gap.is_some() {
                radius::ROW
            } else {
                radius::CONTROL
            };
            if selected {
                ui.painter().rect_filled(r, rr, pal.active);
            } else if resp.hovered() {
                ui.painter()
                    .rect_filled(r, rr, pal.hover.gamma_multiply(0.6));
            }
            for (i, col) in self.table.cells(r).into_iter().enumerate() {
                let mut child = ui.new_child(
                    UiBuilder::new()
                        .max_rect(col)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                child.spacing_mut().item_spacing.x = 6.0;
                child.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                child.set_clip_rect(col.intersect(ui.clip_rect()));
                cell(&mut child, row, i);
            }
            if row != last && self.tile_gap.is_none() {
                ui.painter()
                    .hline(r.x_range(), r.max.y - 0.5, Stroke::new(1.0, pal.border));
            }
            if resp.clicked() {
                clicked = Some(row);
            }
            if resp.double_clicked() {
                activated = Some(row);
            }
            row_responses.push((row, resp));
            y += pitch;
        }
        if let Some(row) = clicked {
            self.state.select(row, self.total);
            response.request_focus();
        }

        let mut page_changed = false;
        if footer_h > 0.0 {
            let fr = Rect::from_min_max(egui::pos2(rect.min.x, rect.max.y - footer_h), rect.max);
            let mut child = ui.new_child(
                UiBuilder::new()
                    .max_rect(fr)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            child.set_clip_rect(fr.intersect(ui.clip_rect()));
            if self.pager {
                let mut page = self.state.page;
                if Pager::new(self.id, &mut page, self.total, per_page)
                    .show(&mut child)
                    .changed()
                {
                    self.state.page = page;
                    page_changed = true;
                    // The selection stays where it is; keys continue from the new page.
                    if self
                        .state
                        .selected
                        .is_some_and(|s| !self.state.visible(self.total).contains(&s))
                    {
                        self.state.selected = None;
                    }
                }
            }
            if let Some(extra) = self.footer_extra {
                child.with_layout(egui::Layout::right_to_left(egui::Align::Center), extra);
            }
        }
        ui.allocate_rect(rect, Sense::hover());
        PagedResponse {
            response,
            selection_changed: self.state.selected != before.1,
            activated,
            page_changed: page_changed || self.state.page != before.0,
            per_page,
            rows: row_responses,
        }
    }
}
