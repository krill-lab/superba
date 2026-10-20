//! A minimal table: weighted columns, caption header, 1 px row dividers and row
//! hover. Each cell gets its own child `Ui` laid out left-to-right, centered;
//! the cells of [`Table::right_aligned`] columns (numbers) right-to-left, under
//! a right-aligned header label that ends where they end
//! ([`Table::right_inset`]).

use egui::{Align, Layout, Rect, Sense, Stroke, Ui, UiBuilder, WidgetInfo, pos2, vec2};

use crate::design::fonts::{Weight, font};
use crate::design::tokens::{Palette, size, text};

/// Column layout of a table.
#[derive(Clone, Debug)]
pub struct Table {
    weights: Vec<f32>,
    min_widths: Vec<f32>,
    right: Vec<usize>,
}

impl Table {
    /// Columns with relative widths.
    pub fn new(weights: &[f32]) -> Self {
        Self {
            weights: weights.to_vec(),
            min_widths: vec![0.0; weights.len()],
            right: Vec::new(),
        }
    }

    /// Space a right-aligned column keeps clear at its right edge when a
    /// left-aligned column follows it, so a number does not run into the text
    /// after it. Its header label and its cells' content both end there (see
    /// [`Table::right_inset`]); a caller sizing such a column adds it to the
    /// column's minimum width.
    pub const RIGHT_INSET: f32 = 12.0;

    /// Right-align these columns (numbers): the header label and the cell's
    /// content end at the column's right edge, or [`Table::RIGHT_INSET`]
    /// before it where a left-aligned column follows.
    pub fn right_aligned(mut self, columns: &[usize]) -> Self {
        self.right = columns.to_vec();
        self
    }

    /// The column `i` is right-aligned.
    pub fn is_right(&self, i: usize) -> bool {
        self.right.contains(&i)
    }

    /// Space column `i`'s header label and cell content keep clear at its
    /// right edge: [`Table::RIGHT_INSET`] for a right-aligned column followed
    /// by a left-aligned one, else 0 (the 12 px column gap, or the row's 12 px
    /// side padding after the last column, is room enough).
    pub fn right_inset(&self, i: usize) -> f32 {
        if self.is_right(i) && i + 1 < self.weights.len() && !self.is_right(i + 1) {
            Self::RIGHT_INSET
        } else {
            0.0
        }
    }

    /// Minimum width per column.
    pub fn min_widths(mut self, widths: &[f32]) -> Self {
        self.min_widths = widths.to_vec();
        self
    }

    fn columns(&self, rect: Rect, pad: f32) -> Vec<Rect> {
        let inner = rect.shrink2(vec2(pad, 0.0));
        let fixed: f32 = self.min_widths.iter().sum();
        let total_w: f32 = self.weights.iter().sum();
        let flexible = (inner.width() - fixed).max(0.0);
        let mut x = inner.min.x;
        let gap = 12.0;
        let n = self.weights.len();
        self.weights
            .iter()
            .zip(&self.min_widths)
            .enumerate()
            .map(|(i, (w, min))| {
                let width = min + flexible * w / total_w;
                let r = Rect::from_min_max(
                    pos2(x, rect.min.y),
                    pos2(x + width - if i + 1 < n { gap } else { 0.0 }, rect.max.y),
                );
                x += width;
                r
            })
            .collect()
    }

    /// Cell rectangles of a row occupying `rect` (12 px side padding, 12 px gaps).
    pub fn cells(&self, rect: Rect) -> Vec<Rect> {
        self.columns(rect, 12.0)
    }

    /// Header row with caption labels ([`size::TABLE_HEADER`] high). A label
    /// wider than its column is shortened with "…" (full text on hover), so it
    /// never runs into the next column. Screen readers see each non-empty label
    /// as a column header.
    pub fn header(&self, ui: &mut Ui, labels: &[String]) {
        let pal = Palette::of(ui.ctx());
        let (rect, response) = ui.allocate_exact_size(
            vec2(ui.available_width(), size::TABLE_HEADER),
            Sense::hover(),
        );
        let a11y = crate::design::a11y::enabled(ui);
        for (i, (col, label)) in self.columns(rect, 12.0).iter().zip(labels).enumerate() {
            if label.is_empty() {
                continue;
            }
            let inset = self.right_inset(i);
            let galley = super::log_table::elided(
                ui,
                label,
                font(text::CAPTION, Weight::Medium),
                pal.text_subtle,
                // A column sized to its label must not shorten it by rounding.
                col.width() - inset + 1.0,
            );
            let size = galley.size();
            let x = if self.is_right(i) {
                col.max.x - inset - size.x
            } else {
                col.min.x
            };
            let elided = galley.elided;
            ui.painter().galley(
                pos2(x, col.center().y - size.y / 2.0),
                galley,
                pal.text_subtle,
            );
            if a11y || elided {
                let cell = ui.interact(*col, response.id.with(("header", i)), Sense::hover());
                cell.widget_info(|| WidgetInfo::labeled(egui::Role::ColumnHeader, true, label));
                if elided {
                    cell.on_hover_text(label);
                }
            }
        }
        ui.painter().hline(
            rect.x_range(),
            rect.max.y - 0.5,
            Stroke::new(1.0, pal.border),
        );
    }

    /// One row; `cell(ui, column_index)` fills each cell. Returns the row response
    /// (hover / click on the whole row).
    pub fn row(
        &self,
        ui: &mut Ui,
        height: f32,
        last: bool,
        mut cell: impl FnMut(&mut Ui, usize),
    ) -> egui::Response {
        let pal = Palette::of(ui.ctx());
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
        if response.hovered() {
            ui.painter()
                .rect_filled(rect, 6, pal.hover.gamma_multiply(0.6));
        }
        for (i, col) in self.columns(rect, 12.0).into_iter().enumerate() {
            let layout = if self.is_right(i) {
                Layout::right_to_left(Align::Center)
            } else {
                Layout::left_to_right(Align::Center)
            };
            let mut content = col;
            content.max.x -= self.right_inset(i);
            let mut child = ui.new_child(UiBuilder::new().max_rect(content).layout(layout));
            child.spacing_mut().item_spacing.x = 6.0;
            child.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            child.set_clip_rect(col.intersect(ui.clip_rect()));
            cell(&mut child, i);
        }
        if !last {
            ui.painter().hline(
                rect.x_range(),
                rect.max.y - 0.5,
                Stroke::new(1.0, pal.border),
            );
        }
        response
    }
}
