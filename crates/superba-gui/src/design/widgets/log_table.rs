//! Request log table (UX §3.5): a dense, virtualised table for 最近请求 /
//! 活动请求. Columns have preferred widths in points and fill the width they
//! get without horizontal scrolling ([`column_widths`]): extra room goes to the
//! growing columns, a narrow table shrinks columns towards their minimum and
//! then hides low-value columns instead of eliding numbers;
//! 26 px one-line rows; sortable headers; a selected row with the `active` fill
//! and a 2 px bar on the left; an optional totals row pinned under the rows; an
//! "n new requests" pill while the table is scrolled down.
//!
//! Only the rows in view are built (`ScrollArea::show_viewport`), and cells are
//! painted directly, so ten thousand rows cost the same per frame as twenty.
//! The table scrolls inside its own area; the page around it never scrolls.

use egui::text::{LayoutJob, TextWrapping};
use egui::{
    Color32, CursorIcon, FontId, Id, Key, Rect, Response, ScrollArea, Sense, Stroke, Ui, UiBuilder,
    pos2, vec2,
};

use super::Status;
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::tokens::{Palette, radius, size, text};

/// Default row height (UX §3.5).
pub const ROW_HEIGHT: f32 = 26.0;
/// Horizontal padding inside each cell.
pub const CELL_PAD: f32 = 5.0;

/// Text alignment of a column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellAlign {
    /// Left (text).
    Left,
    /// Right (numbers).
    Right,
}

/// One column.
#[derive(Clone, Debug)]
pub struct LogColumn {
    /// Header text.
    pub label: String,
    /// Preferred width in points (scaled to the available width).
    pub width: f32,
    /// Alignment of the cells and the header.
    pub align: CellAlign,
    /// Clicking the header sorts by this column.
    pub sortable: bool,
    /// Header tooltip.
    pub tooltip: Option<String>,
    /// Narrowest useful width (0 = shrinks freely, proportionally).
    pub min: f32,
    /// Share of the room beyond the preferred widths (0 = keeps its width).
    pub grow: f32,
    /// Hidden when even the minimum widths do not fit, lowest rank first
    /// (`None` = always shown).
    pub drop_rank: Option<u8>,
}

impl LogColumn {
    /// A left-aligned sortable column.
    pub fn left(label: impl Into<String>, width: f32) -> Self {
        Self {
            label: label.into(),
            width,
            align: CellAlign::Left,
            sortable: true,
            tooltip: None,
            min: 0.0,
            grow: 0.0,
            drop_rank: None,
        }
    }

    /// The narrowest useful width.
    pub fn min(mut self, width: f32) -> Self {
        self.min = width.max(0.0);
        self
    }

    /// Take this share of the room beyond the preferred widths.
    pub fn grow(mut self, weight: f32) -> Self {
        self.grow = weight.max(0.0);
        self
    }

    /// Hide the column on narrow tables (rank 0 first).
    pub fn drop_rank(mut self, rank: u8) -> Self {
        self.drop_rank = Some(rank);
        self
    }

    /// A right-aligned (numeric) sortable column.
    pub fn right(label: impl Into<String>, width: f32) -> Self {
        Self {
            align: CellAlign::Right,
            ..Self::left(label, width)
        }
    }

    /// Not sortable.
    pub fn fixed_order(mut self) -> Self {
        self.sortable = false;
        self
    }

    /// Header tooltip.
    pub fn tooltip(mut self, tip: impl Into<String>) -> Self {
        self.tooltip = Some(tip.into());
        self
    }
}

/// What one cell shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LogCell {
    /// Text (elided with `…` when it does not fit).
    pub text: String,
    /// Text colour (default: primary text).
    pub color: Option<Color32>,
    /// Monospace (keys, ids).
    pub mono: bool,
    /// Medium weight.
    pub strong: bool,
    /// A status dot before the text.
    pub dot: Option<Status>,
    /// Hover text.
    pub tooltip: Option<String>,
    /// Shorter text shown instead when `text` does not fit.
    pub short: Option<String>,
}

impl LogCell {
    /// Plain text.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    /// Monospace text.
    pub fn mono(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            mono: true,
            ..Self::default()
        }
    }

    /// Text in another colour.
    pub fn color(mut self, color: Color32) -> Self {
        self.color = Some(color);
        self
    }

    /// Medium weight.
    pub fn strong(mut self) -> Self {
        self.strong = true;
        self
    }

    /// A status dot before the text.
    pub fn dot(mut self, status: Status) -> Self {
        self.dot = Some(status);
        self
    }

    /// Hover text.
    pub fn tooltip(mut self, tip: impl Into<String>) -> Self {
        self.tooltip = Some(tip.into());
        self
    }

    /// Show `short` when the text does not fit (`失败 · 流中断` → `失败`).
    pub fn short(mut self, short: impl Into<String>) -> Self {
        self.short = Some(short.into());
        self
    }
}

/// Widths of `columns` in a table `total` wide; 0 for a hidden column. The
/// widths of the shown columns add up to `total`:
/// - room beyond the preferred widths goes to the growing columns (all columns
///   in proportion when none grows);
/// - less room shrinks every column towards its minimum, in proportion to how
///   far it can shrink;
/// - when even the minimums do not fit, columns with a drop rank are hidden
///   (lowest rank first) until they do; past that everything shrinks in
///   proportion.
pub fn column_widths(columns: &[LogColumn], total: f32) -> Vec<f32> {
    let total = total.max(0.0);
    let mut shown = vec![true; columns.len()];
    let sum = |shown: &[bool], f: &dyn Fn(&LogColumn) -> f32| -> f32 {
        columns
            .iter()
            .zip(shown)
            .filter(|(_, s)| **s)
            .map(|(c, _)| f(c))
            .sum()
    };
    let min_of = |c: &LogColumn| c.min.min(c.width.max(0.0));
    let mut ranked: Vec<usize> = (0..columns.len())
        .filter(|i| columns[*i].drop_rank.is_some())
        .collect();
    ranked.sort_by_key(|i| columns[*i].drop_rank);
    for i in ranked {
        if sum(&shown, &min_of) <= total {
            break;
        }
        shown[i] = false;
    }
    let pref = sum(&shown, &|c| c.width.max(0.0));
    let min = sum(&shown, &min_of);
    let grow = sum(&shown, &|c| c.grow);
    columns
        .iter()
        .zip(&shown)
        .map(|(c, s)| {
            if !s || pref <= 0.0 {
                return 0.0;
            }
            let w = c.width.max(0.0);
            if pref <= total {
                if grow > 0.0 {
                    w + (total - pref) * c.grow / grow
                } else {
                    w * total / pref
                }
            } else if min < total {
                let m = min_of(c);
                m + (w - m) * (total - min) / (pref - min)
            } else if min > 0.0 {
                min_of(c) * total / min
            } else {
                0.0
            }
        })
        .collect()
}

/// Widths of `columns` scaled to `total` (the sum equals `total`, so the table
/// never scrolls horizontally). Columns keep their proportions.
pub fn fit_widths(columns: &[f32], total: f32) -> Vec<f32> {
    let preferred: f32 = columns.iter().map(|w| w.max(0.0)).sum();
    if preferred <= 0.0 || columns.is_empty() {
        return vec![0.0; columns.len()];
    }
    let scale = total.max(0.0) / preferred;
    columns.iter().map(|w| w.max(0.0) * scale).collect()
}

/// A one-line galley of `text` elided to `max_width`.
pub fn elided(
    ui: &Ui,
    text: &str,
    font_id: FontId,
    color: Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font_id, color);
    job.wrap = TextWrapping {
        max_width: max_width.max(1.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.painter().layout_job(job)
}

/// What happened in a [`LogTable`] this frame.
#[derive(Debug)]
pub struct LogTableResponse {
    /// A row was clicked (selection).
    pub clicked: Option<usize>,
    /// A row was double-clicked or Enter was pressed on the selection.
    pub activated: Option<usize>,
    /// ↑ / ↓ moved the selection to this row.
    pub key_selected: Option<usize>,
    /// A sortable header was clicked.
    pub sort_clicked: Option<usize>,
    /// The "new rows" pill was clicked.
    pub pill_clicked: bool,
    /// The rows are scrolled to the top.
    pub at_top: bool,
    /// Where the rows are drawn (empty states go here).
    pub body_rect: Rect,
    /// The rows area (keyboard focus; header and totals excluded).
    pub response: Response,
}

/// The table. Build it, then call [`LogTable::show`] with a cell function.
#[must_use = "call .show(ui, ..)"]
pub struct LogTable<'a> {
    id: Id,
    columns: &'a [LogColumn],
    rows: usize,
    row_height: f32,
    selected: Option<usize>,
    sort: Option<(usize, bool)>,
    totals: Option<Vec<LogCell>>,
    pill: Option<String>,
    scroll_to_top: bool,
    reveal: Option<usize>,
}

impl<'a> LogTable<'a> {
    /// A table over `rows` rows.
    pub fn new(
        id_salt: impl std::hash::Hash + std::fmt::Debug,
        columns: &'a [LogColumn],
        rows: usize,
    ) -> Self {
        Self {
            id: Id::unique(("superba-log-table", id_salt)),
            columns,
            rows,
            row_height: ROW_HEIGHT,
            selected: None,
            sort: None,
            totals: None,
            pill: None,
            scroll_to_top: false,
            reveal: None,
        }
    }

    /// Row height (default 26 px).
    pub fn row_height(mut self, height: f32) -> Self {
        self.row_height = height.max(16.0);
        self
    }

    /// The selected row.
    pub fn selected(mut self, row: Option<usize>) -> Self {
        self.selected = row;
        self
    }

    /// The sorted column and whether it sorts descending (arrow in the header).
    pub fn sort(mut self, column: usize, descending: bool) -> Self {
        self.sort = Some((column, descending));
        self
    }

    /// A totals row pinned under the rows.
    pub fn totals(mut self, cells: Vec<LogCell>) -> Self {
        self.totals = Some(cells);
        self
    }

    /// Show the "new rows" pill with this text.
    pub fn pill(mut self, text: Option<String>) -> Self {
        self.pill = text;
        self
    }

    /// Scroll back to the first row this frame.
    pub fn scroll_to_top(mut self, yes: bool) -> Self {
        self.scroll_to_top = yes;
        self
    }

    /// Scroll so that `row` is visible this frame.
    pub fn reveal(mut self, row: Option<usize>) -> Self {
        self.reveal = row;
        self
    }

    /// Draw the table in the available rectangle; `cell(row, column)` describes a
    /// cell (only called for rows in view and for the totals row).
    pub fn show(
        self,
        ui: &mut Ui,
        mut cell: impl FnMut(usize, usize) -> LogCell,
    ) -> LogTableResponse {
        let pal = Palette::of(ui.ctx());
        let rect = ui.available_rect_before_wrap();
        let header_h = size::TABLE_HEADER;
        let totals_h = if self.totals.is_some() {
            self.row_height
        } else {
            0.0
        };
        // Keyboard focus (and its ring) belongs to the rows, not the header and
        // the totals row.
        let rows_rect = Rect::from_min_max(
            pos2(rect.min.x, rect.min.y + header_h),
            pos2(
                rect.max.x,
                (rect.max.y - totals_h).max(rect.min.y + header_h),
            ),
        );
        let response = ui.interact(rows_rect, self.id, Sense::focusable_noninteractive());
        let widths = column_widths(self.columns, rect.width());
        let xs: Vec<(f32, f32)> = widths
            .iter()
            .scan(rect.min.x, |x, w| {
                let start = *x;
                *x += w;
                Some((start, *w))
            })
            .collect();

        // Header.
        let header = Rect::from_min_size(rect.min, vec2(rect.width(), header_h));
        let mut sort_clicked = None;
        let header_font = font(text::CAPTION, Weight::Medium);
        for (i, (col, (x, w))) in self.columns.iter().zip(&xs).enumerate() {
            if *w <= 0.0 {
                continue;
            }
            let cell_rect = Rect::from_min_size(pos2(*x, header.min.y), vec2(*w, header_h));
            let sense = if col.sortable {
                Sense::click()
            } else {
                Sense::hover()
            };
            let mut r = ui.interact(cell_rect, self.id.with(("h", i)), sense);
            if col.sortable {
                r = r.on_hover_cursor(CursorIcon::PointingHand);
            }
            let sorted = self.sort.filter(|(c, _)| *c == i);
            let arrow_w = if sorted.is_some() { 10.0 } else { 0.0 };
            let color = if sorted.is_some() || r.hovered() {
                pal.text
            } else {
                pal.text_subtle
            };
            let g = elided(
                ui,
                &col.label,
                header_font.clone(),
                color,
                w - CELL_PAD * 2.0 - arrow_w,
            );
            let gw = g.size().x;
            let tx = match col.align {
                CellAlign::Left => x + CELL_PAD,
                CellAlign::Right => x + w - CELL_PAD - gw - arrow_w,
            };
            let elided_header = g.elided;
            ui.painter()
                .galley(pos2(tx, cell_rect.center().y - g.size().y / 2.0), g, color);
            if let Some((_, desc)) = sorted {
                let ax = tx + gw + 3.0;
                let cy = cell_rect.center().y;
                let pts = if desc {
                    vec![
                        pos2(ax, cy - 2.0),
                        pos2(ax + 6.0, cy - 2.0),
                        pos2(ax + 3.0, cy + 2.0),
                    ]
                } else {
                    vec![
                        pos2(ax, cy + 2.0),
                        pos2(ax + 6.0, cy + 2.0),
                        pos2(ax + 3.0, cy - 2.0),
                    ]
                };
                ui.painter().add(egui::Shape::convex_polygon(
                    pts,
                    pal.text_muted,
                    Stroke::NONE,
                ));
            }
            let tip = match (&col.tooltip, elided_header) {
                (Some(t), true) => Some(format!("{} · {t}", col.label)),
                (Some(t), false) => Some(t.clone()),
                (None, true) => Some(col.label.clone()),
                (None, false) => None,
            };
            if let Some(t) = tip {
                r = r.on_hover_text(t);
            }
            if r.clicked() && col.sortable {
                sort_clicked = Some(i);
            }
        }
        ui.painter().hline(
            rect.x_range(),
            header.max.y - 0.5,
            Stroke::new(1.0, pal.border),
        );

        // Body.
        let body = Rect::from_min_max(
            pos2(rect.min.x, header.max.y),
            pos2(rect.max.x, rect.max.y - totals_h),
        );
        let focused = response.has_focus();
        let mut key_selected = None;
        let mut activated = None;
        if focused && self.rows > 0 {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    self.id,
                    egui::EventFilter {
                        vertical_arrows: true,
                        ..Default::default()
                    },
                );
            });
            let (down, up, enter) = ui.input(|i| {
                (
                    i.key_pressed(Key::ArrowDown),
                    i.key_pressed(Key::ArrowUp),
                    i.key_pressed(Key::Enter),
                )
            });
            if down {
                key_selected = Some(self.selected.map_or(0, |s| (s + 1).min(self.rows - 1)));
            } else if up {
                key_selected = Some(self.selected.map_or(0, |s| s.saturating_sub(1)));
            }
            if enter {
                activated = self.selected;
            }
        }
        let row_h = self.row_height;
        let reveal = key_selected.or(self.reveal);
        let mut area = ScrollArea::vertical()
            .id_salt(self.id.with("rows"))
            .auto_shrink([false, false]);
        let state_id = self.id.with("rows-state");
        if self.scroll_to_top {
            area = area.vertical_scroll_offset(0.0);
        } else if let Some(row) = reveal {
            let offset: f32 = ui.ctx().data(|d| d.get_temp(state_id)).unwrap_or(0.0);
            let top = row as f32 * row_h;
            let bottom = top + row_h;
            if top < offset {
                area = area.vertical_scroll_offset(top);
            } else if bottom > offset + body.height() {
                area = area.vertical_scroll_offset(bottom - body.height());
            }
        }
        let mut body_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(body)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        body_ui.set_clip_rect(body.intersect(ui.clip_rect()));
        let selected = self.selected;
        let columns = self.columns;
        let id = self.id;
        let cell_font = font(text::CAPTION, Weight::Regular);
        let strong_font = font(text::CAPTION, Weight::Medium);
        let mono = mono_font(text::CAPTION, Weight::Regular);
        let mut clicked = None;
        let out = area.show_viewport(&mut body_ui, |ui, viewport| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            ui.set_height(row_h * self.rows as f32);
            let first = (viewport.min.y / row_h).floor().max(0.0) as usize;
            let last = ((viewport.max.y / row_h).ceil() as usize + 1).min(self.rows);
            let origin = ui.max_rect().min;
            for row in first..last {
                let r = Rect::from_min_size(
                    pos2(body.min.x, origin.y + row as f32 * row_h),
                    vec2(body.width(), row_h),
                );
                let resp = ui.interact(r, id.with(("row", row)), Sense::click());
                let is_sel = selected == Some(row);
                if is_sel {
                    ui.painter().rect_filled(r, radius::INNER, pal.active);
                    ui.painter().rect_filled(
                        Rect::from_min_size(r.min, vec2(2.0, r.height())),
                        0,
                        pal.text,
                    );
                } else if resp.hovered() {
                    ui.painter()
                        .rect_filled(r, radius::INNER, pal.hover.gamma_multiply(0.6));
                }
                let mut tip = None;
                let pointer_x = resp.hover_pos().map(|p| p.x);
                for (c, (col, (x, w))) in columns.iter().zip(&xs).enumerate() {
                    if *w <= 0.0 {
                        continue;
                    }
                    let cell_rect = Rect::from_min_size(pos2(*x, r.min.y), vec2(*w, row_h));
                    let content = cell(row, c);
                    let f = if content.mono {
                        mono.clone()
                    } else if content.strong {
                        strong_font.clone()
                    } else {
                        cell_font.clone()
                    };
                    let elided_text = paint_cell(ui, &cell_rect, col.align, &content, f, pal);
                    if pointer_x.is_some_and(|px| px >= *x && px < x + w) {
                        tip = content
                            .tooltip
                            .clone()
                            .or_else(|| elided_text.then(|| content.text.clone()));
                    }
                }
                let resp = match tip {
                    Some(t) if !t.is_empty() => resp.on_hover_text(t),
                    _ => resp,
                };
                if resp.clicked() {
                    clicked = Some(row);
                }
                if resp.double_clicked() {
                    activated = Some(row);
                }
            }
        });
        ui.ctx()
            .data_mut(|d| d.insert_temp(state_id, out.state.clamped_offset().y));
        let at_top = out.state.clamped_offset().y < 1.0;
        if clicked.is_some() {
            response.request_focus();
        }

        // Totals.
        if let Some(cells) = &self.totals {
            let tr = Rect::from_min_max(pos2(rect.min.x, rect.max.y - totals_h), rect.max);
            ui.painter().rect_filled(tr, 0, pal.bg_subtle);
            ui.painter()
                .hline(tr.x_range(), tr.min.y + 0.5, Stroke::new(1.0, pal.border));
            for ((col, (x, w)), content) in columns.iter().zip(&xs).zip(cells) {
                if *w <= 0.0 {
                    continue;
                }
                let cell_rect = Rect::from_min_size(pos2(*x, tr.min.y), vec2(*w, totals_h));
                let f = if content.mono {
                    mono.clone()
                } else {
                    strong_font.clone()
                };
                let elided_text = paint_cell(ui, &cell_rect, col.align, content, f, pal);
                let tip = content
                    .tooltip
                    .clone()
                    .or_else(|| elided_text.then(|| content.text.clone()));
                if let Some(t) = tip.filter(|t| !t.is_empty()) {
                    ui.interact(
                        cell_rect,
                        id.with(("total", col.label.as_str())),
                        Sense::hover(),
                    )
                    .on_hover_text(t);
                }
            }
        }

        // "n new requests" pill.
        let mut pill_clicked = false;
        if let Some(label) = &self.pill {
            let g = ui.painter().layout_no_wrap(
                label.clone(),
                font(text::CAPTION, Weight::Medium),
                pal.on_primary,
            );
            let pr = Rect::from_center_size(
                pos2(body.center().x, body.min.y + 18.0),
                vec2(g.size().x + 20.0, 22.0),
            );
            let r = ui
                .interact(pr, id.with("pill"), Sense::click())
                .on_hover_cursor(CursorIcon::PointingHand);
            let fill = if r.hovered() {
                pal.primary_hover
            } else {
                pal.primary
            };
            ui.painter().rect_filled(pr, radius::PILL, fill);
            ui.painter().galley(
                pos2(
                    pr.center().x - g.size().x / 2.0,
                    pr.center().y - g.size().y / 2.0,
                ),
                g,
                pal.on_primary,
            );
            pill_clicked = r.clicked();
        }
        ui.allocate_rect(rect, Sense::hover());
        LogTableResponse {
            clicked,
            activated,
            key_selected,
            sort_clicked,
            pill_clicked,
            at_top,
            body_rect: body,
            response,
        }
    }
}

/// Paint one cell; true when its text was elided.
fn paint_cell(
    ui: &Ui,
    rect: &Rect,
    align: CellAlign,
    content: &LogCell,
    font_id: FontId,
    pal: &Palette,
) -> bool {
    let color = content.color.unwrap_or(pal.text);
    let dot_w = if content.dot.is_some() { 12.0 } else { 0.0 };
    let max = rect.width() - CELL_PAD * 2.0 - dot_w;
    let mut g = elided(ui, &content.text, font_id.clone(), color, max);
    if g.elided
        && let Some(short) = &content.short
    {
        g = elided(ui, short, font_id, color, max);
    }
    // The full text is in the tooltip whenever something else was shown.
    let elided_text = g.elided || g.text() != content.text;
    let gw = g.size().x;
    let x = match align {
        CellAlign::Left => rect.min.x + CELL_PAD + dot_w,
        CellAlign::Right => rect.max.x - CELL_PAD - gw,
    };
    if let Some(status) = content.dot {
        let dx = x - dot_w + 4.0;
        ui.painter()
            .circle_filled(pos2(dx, rect.center().y), 3.0, pal.status(status));
    }
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    painter.galley(pos2(x, rect.center().y - g.size().y / 2.0), g, color);
    elided_text
}
