//! Pager (UX §4): `1–11 / 340   ‹ 1 2 … 31 ›` in tabular figures. Pages are
//! zero-based in code and one-based on screen.

use egui::{
    Color32, CursorIcon, Id, Rect, Response, Sense, Stroke, StrokeKind, Ui, WidgetInfo, pos2, vec2,
};

use super::{focus_ring, hover_t, show_focus};
use crate::design::fonts::{Weight, font};
use crate::design::tokens::{Palette, radius, text};

/// Number of pages for `total` items at `per_page` (at least 1).
pub fn page_count(total: usize, per_page: usize) -> usize {
    total.div_ceil(per_page.max(1)).max(1)
}

/// One-based first and last item shown on `page` (`(0, 0)` without items).
pub fn item_range(page: usize, per_page: usize, total: usize) -> (usize, usize) {
    if total == 0 {
        return (0, 0);
    }
    let per_page = per_page.max(1);
    let page = page.min(page_count(total, per_page) - 1);
    let from = page * per_page + 1;
    (from, (from + per_page - 1).min(total))
}

/// The range label, `1–11 / 340`.
pub fn range_label(page: usize, per_page: usize, total: usize) -> String {
    let (from, to) = item_range(page, per_page, total);
    format!("{from}–{to} / {total}")
}

/// Page buttons to show: the first and last page, the current page and its
/// neighbours; `None` is an ellipsis.
pub fn page_buttons(current: usize, pages: usize) -> Vec<Option<usize>> {
    if pages <= 7 {
        return (0..pages).map(Some).collect();
    }
    let current = current.min(pages - 1);
    let mut wanted = vec![0, pages - 1, current];
    if current > 0 {
        wanted.push(current - 1);
    }
    if current + 1 < pages {
        wanted.push(current + 1);
    }
    // Near either end, show three pages there so the count of buttons is stable.
    if current <= 2 {
        wanted.extend([1, 2, 3]);
    }
    if current + 3 >= pages {
        wanted.extend([pages - 4, pages - 3, pages - 2]);
    }
    wanted.sort_unstable();
    wanted.dedup();
    let mut out = Vec::with_capacity(wanted.len() + 2);
    let mut last: Option<usize> = None;
    for p in wanted {
        if let Some(l) = last
            && p > l + 1
        {
            out.push(None);
        }
        out.push(Some(p));
        last = Some(p);
    }
    out
}

/// A pager over `total` items, `per_page` per page, bound to the zero-based `page`.
#[must_use = "call .show(ui)"]
pub struct Pager<'a> {
    id: Id,
    page: &'a mut usize,
    total: usize,
    per_page: usize,
    prev_label: String,
    next_label: String,
    buttons: bool,
}

impl<'a> Pager<'a> {
    /// New pager.
    pub fn new(
        id_salt: impl std::hash::Hash + std::fmt::Debug,
        page: &'a mut usize,
        total: usize,
        per_page: usize,
    ) -> Self {
        Self {
            id: Id::unique(("superba-pager", id_salt)),
            page,
            total,
            per_page: per_page.max(1),
            prev_label: "‹".into(),
            next_label: "›".into(),
            buttons: true,
        }
    }

    /// Accessible names of the previous / next buttons ("Previous page").
    pub fn labels(mut self, prev: impl Into<String>, next: impl Into<String>) -> Self {
        self.prev_label = prev.into();
        self.next_label = next.into();
        self
    }

    /// Show only the range label and the arrows (narrow footers).
    pub fn compact(mut self) -> Self {
        self.buttons = false;
        self
    }

    /// Draw; `changed()` when the page changed.
    pub fn show(self, ui: &mut Ui) -> Response {
        let pal = Palette::of(ui.ctx());
        let pages = page_count(self.total, self.per_page);
        if *self.page >= pages {
            *self.page = pages - 1;
        }
        let current = *self.page;
        let mut next_page = None;
        let r = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let label = range_label(current, self.per_page, self.total);
            ui.label(
                egui::RichText::new(label)
                    .font(font(text::CAPTION, Weight::Regular))
                    .color(pal.text_muted),
            );
            ui.add_space(8.0);
            if arrow(
                ui,
                pal,
                self.id.with("prev"),
                false,
                current > 0,
                &self.prev_label,
            )
            .clicked()
            {
                next_page = Some(current - 1);
            }
            if self.buttons {
                for b in page_buttons(current, pages) {
                    match b {
                        Some(p) => {
                            if number(ui, pal, self.id.with(p), p, p == current).clicked() {
                                next_page = Some(p);
                            }
                        }
                        None => {
                            ui.label(
                                egui::RichText::new("…")
                                    .font(font(text::CAPTION, Weight::Regular))
                                    .color(pal.text_subtle),
                            );
                        }
                    }
                }
            }
            if arrow(
                ui,
                pal,
                self.id.with("next"),
                true,
                current + 1 < pages,
                &self.next_label,
            )
            .clicked()
            {
                next_page = Some(current + 1);
            }
        });
        let mut response = r.response;
        if let Some(p) = next_page
            && p != current
            && p < pages
        {
            *self.page = p;
            response.mark_changed();
        }
        response
    }
}

fn number(ui: &mut Ui, pal: &Palette, id: Id, page: usize, selected: bool) -> Response {
    let label = (page + 1).to_string();
    let galley = ui.painter().layout_no_wrap(
        label.clone(),
        font(text::CAPTION, Weight::Medium),
        Color32::PLACEHOLDER,
    );
    let w = (galley.size().x + 12.0).max(24.0);
    let (rect, _) = ui.allocate_exact_size(vec2(w, 24.0), Sense::hover());
    let response = ui
        .interact(rect, id, Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand);
    response.widget_info(|| WidgetInfo::selected(egui::Role::Button, true, selected, &label));
    let t = hover_t(ui, &response);
    let fill = if selected {
        pal.active
    } else {
        Color32::TRANSPARENT.lerp_to_gamma(pal.hover, t)
    };
    ui.painter().rect(
        rect,
        radius::INNER,
        fill,
        Stroke::new(1.0, if selected { pal.border } else { fill }),
        StrokeKind::Inside,
    );
    ui.painter().galley(
        pos2(
            rect.center().x - galley.size().x / 2.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        if selected { pal.text } else { pal.text_muted },
    );
    if show_focus(&response) {
        focus_ring(ui.painter(), rect, radius::INNER, pal);
    }
    response
}

fn arrow(ui: &mut Ui, pal: &Palette, id: Id, right: bool, enabled: bool, label: &str) -> Response {
    let (rect, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let mut response = ui.interact(rect, id, sense);
    if enabled {
        response = response.on_hover_cursor(CursorIcon::PointingHand);
    }
    response.widget_info(|| WidgetInfo::labeled(egui::Role::Button, enabled, label));
    let t = hover_t(ui, &response);
    ui.painter().rect_filled(
        rect,
        radius::INNER,
        Color32::TRANSPARENT.lerp_to_gamma(pal.hover, t),
    );
    let color = if enabled {
        pal.text_muted.lerp_to_gamma(pal.text, t)
    } else {
        pal.text_disabled
    };
    paint_chevron(ui, rect, right, color);
    if show_focus(&response) {
        focus_ring(ui.painter(), rect, radius::INNER, pal);
    }
    response
}

fn paint_chevron(ui: &Ui, rect: Rect, right: bool, color: Color32) {
    let c = rect.center();
    let (dx, dy) = (2.5, 5.0);
    let (tip, back) = if right {
        (c.x + dx, c.x - dx)
    } else {
        (c.x - dx, c.x + dx)
    };
    ui.painter().line(
        vec![pos2(back, c.y - dy), pos2(tip, c.y), pos2(back, c.y + dy)],
        Stroke::new(1.5, color),
    );
}
