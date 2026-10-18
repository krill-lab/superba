//! The left rail of the settings page (UX §3.6): Common, Changed, a divider,
//! then the catalog categories with counts; while searching, "Search results N"
//! on top and per-category hit counts (a click narrows the results).

use egui::{Sense, Ui, vec2};
use superba_core::Locale;

use super::State;
use super::model::{Category, Model, Rail};
use crate::design::fonts::{Weight, font, mono_font};
use crate::pages::Cx;

/// Height of one rail item.
const ITEM_H: f32 = 28.0;
/// Space between rail items.
const ITEM_GAP: f32 = 4.0;

/// Draw the rail.
pub(super) fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, model: &Model) {
    let counts = model.counts(&state.filter, &state.pending);
    let width = ui.available_width();
    // Items closer together than the page's default spacing: two-line labels
    // still leave room for every category.
    ui.spacing_mut().item_spacing.y = ITEM_GAP;
    let searching = state.filter.searching();
    let mut pick: Option<Rail> = None;
    if searching {
        let narrowed = matches!(state.filter.rail, Rail::Category(_));
        if item(
            ui,
            cx,
            width,
            &cx.tr.t("codex_settings.search_results"),
            counts.results,
            !narrowed,
        ) {
            pick = Some(Rail::All);
        }
    } else {
        if item(
            ui,
            cx,
            width,
            &cx.tr.t("codex_settings.common"),
            counts.common,
            state.filter.rail == Rail::Common,
        ) {
            pick = Some(Rail::Common);
        }
        if item(
            ui,
            cx,
            width,
            &cx.tr.t("codex_settings.changed"),
            counts.changed,
            state.filter.rail == Rail::Changed,
        ) {
            pick = Some(Rail::Changed);
        }
    }
    let (r, _) = ui.allocate_exact_size(vec2(width, 9.0), Sense::hover());
    ui.painter().hline(
        r.x_range().shrink(4.0),
        r.center().y,
        egui::Stroke::new(1.0, cx.pal.border),
    );
    // Categories that fit the rail; with an unusually long list the rest are
    // reached through search.
    for (i, c) in model.categories.iter().enumerate() {
        let n = counts.categories.get(&c.id).copied().unwrap_or(0);
        let label = category_label(cx, c);
        let on = state.filter.rail == Rail::Category(c.id.clone());
        if i > 0 && item_height(ui, &label, n, width, on) > ui.available_height() {
            break;
        }
        if item(ui, cx, width, &label, n, on) {
            pick = Some(if on && searching {
                Rail::All
            } else {
                Rail::Category(c.id.clone())
            });
        }
    }
    if let Some(rail) = pick {
        state.filter.rail = rail;
    }
}

/// Label of a category: the Chinese catalog label in Chinese, else the
/// translated `catalog.category.<id>`, else the catalog's English label.
pub(super) fn category_label(cx: &Cx<'_>, c: &Category) -> String {
    if let (Some(zh), Locale::ZhCn) = (&c.label_zh, cx.tr.locale()) {
        return zh.clone();
    }
    let key = format!("catalog.category.{}", c.id);
    let text = cx.tr.t(&key);
    if text == key { c.label.clone() } else { text }
}

/// Font of a rail label.
fn label_font(selected: bool) -> egui::FontId {
    font(
        13.0,
        if selected {
            Weight::Medium
        } else {
            Weight::Regular
        },
    )
}

/// Room of an item's label next to its count `n`.
fn label_room(ui: &Ui, n: usize, width: f32) -> f32 {
    let count = ui.painter().layout_no_wrap(
        n.to_string(),
        mono_font(12.0, Weight::Regular),
        egui::Color32::GRAY,
    );
    (width - 20.0 - count.size().x - 8.0).max(20.0)
}

/// Width the rail needs to show every label of `model` on one line (review
/// ux-visual #12: long English categories were cut).
pub(super) fn needed_width(ui: &Ui, cx: &Cx<'_>, model: &Model) -> f32 {
    let widest = model
        .categories
        .iter()
        .map(|c| category_label(cx, c))
        .chain([
            cx.tr.t("codex_settings.common"),
            cx.tr.t("codex_settings.changed"),
            cx.tr.t("codex_settings.search_results"),
        ])
        .map(|l| {
            ui.painter()
                .layout_no_wrap(l, label_font(true), egui::Color32::GRAY)
                .size()
                .x
        })
        .fold(0.0, f32::max);
    // Padding, the gap and a three-digit count.
    widest + 20.0 + 8.0 + 24.0
}

/// The label as laid out in an item: one line, or two when it does not fit
/// (the rail is at its narrowest).
fn label_job(
    label: &str,
    selected: bool,
    color: egui::Color32,
    room: f32,
) -> egui::text::LayoutJob {
    let mut job =
        egui::text::LayoutJob::simple(label.to_owned(), label_font(selected), color, room);
    job.wrap.max_rows = 2;
    job.wrap.overflow_character = Some('…');
    job
}

/// Height of an item: two-line labels make it taller.
fn item_height(ui: &Ui, label: &str, n: usize, width: f32, selected: bool) -> f32 {
    let room = label_room(ui, n, width);
    let g = ui
        .painter()
        .layout_job(label_job(label, selected, egui::Color32::GRAY, room));
    ITEM_H.max(g.size().y + 6.0)
}

/// One rail item; true when clicked (also the Claude Code settings page's).
pub(crate) fn item(
    ui: &mut Ui,
    cx: &Cx<'_>,
    width: f32,
    label: &str,
    n: usize,
    selected: bool,
) -> bool {
    let height = item_height(ui, label, n, width, selected);
    let (rect, r) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    let r = r.on_hover_cursor(egui::CursorIcon::PointingHand);
    if selected || r.hovered() {
        ui.painter().rect_filled(
            rect,
            6,
            if selected {
                cx.pal.active
            } else {
                cx.pal.hover
            },
        );
    }
    let color = if n == 0 {
        cx.pal.text_disabled
    } else if selected {
        cx.pal.text
    } else {
        cx.pal.text_muted
    };
    let count = n.to_string();
    let count_galley =
        ui.painter()
            .layout_no_wrap(count, mono_font(12.0, Weight::Regular), cx.pal.text_subtle);
    let text_w = (rect.width() - 20.0 - count_galley.size().x - 8.0).max(20.0);
    let galley = ui
        .painter()
        .layout_job(label_job(label, selected, color, text_w));
    ui.painter().galley(
        egui::pos2(rect.min.x + 10.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    ui.painter().galley(
        egui::pos2(
            rect.max.x - 10.0 - count_galley.size().x,
            rect.center().y - count_galley.size().y / 2.0,
        ),
        count_galley,
        cx.pal.text_subtle,
    );
    let label = label.to_owned();
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, &label));
    r.clicked()
}
