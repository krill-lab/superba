//! The Overview's layout (user request 27): the route hero across the top and
//! an asymmetric bento grid below it — a wide left column (2/3: 今日, 最近会话)
//! and a narrow right one (1/3: 线路, 待办) whose rows split at different
//! heights — plus the card frame, card headers and the 待办 card.

use egui::{Align, Align2, Color32, CursorIcon, Layout, Rect, Sense, Ui, UiBuilder, pos2, vec2};

use super::todo::Todo;
use super::{Action, Cx, Page};
use crate::design::backdrop::{self, Backdrop};
use crate::design::fonts::{Weight, font};
use crate::design::icons::{self, Icon};
use crate::design::tokens::{radius, text};
use crate::design::widgets::{Button, Status, focus_ring, hover_t, show_focus};

/// Gap between cards.
pub const GAP: f32 = 16.0;
/// Inner padding of a card.
pub const PAD_X: f32 = 20.0;
/// Inner padding of a card (top and bottom).
pub const PAD_Y: f32 = 14.0;
/// Height of a card's header row.
pub const HEADER: f32 = 26.0;

/// The rectangles of the hero and the four cards.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    /// The route hero (full width).
    pub hero: Rect,
    /// 今日 (left, top).
    pub today: Rect,
    /// 最近会话 (left, bottom).
    pub sessions: Rect,
    /// 线路 (right, top).
    pub lines: Rect,
    /// 待办 (right, bottom).
    pub todo: Rect,
}

/// Lay the hero and the cards out in `area`. The left column takes two thirds
/// of the width; its rows split lower than the right column's, so the four
/// cards never line up into a plain table.
pub fn layout(area: Rect) -> Grid {
    // Inside a scroll area the height is unbounded: keep the natural size.
    let h = if area.height().is_finite() {
        area.height().max(600.0)
    } else {
        712.0
    };
    let hero_h = (h * 0.19).clamp(super::hero::HEIGHT, 152.0).floor();
    let w = area.width();
    let left_w = ((w - GAP) * 2.0 / 3.0).floor();
    let right_w = w - GAP - left_w;
    let g = h - hero_h - GAP;
    let today_h = (g * 0.54).clamp(270.0, 390.0).floor();
    let lines_h = (g * 0.48).clamp(236.0, 350.0).floor();
    let top = area.min.y + hero_h + GAP;
    let x0 = area.min.x;
    let x1 = x0 + left_w + GAP;
    let r = |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(pos2(x, y), vec2(w, h));
    Grid {
        hero: r(x0, area.min.y, w, hero_h),
        today: r(x0, top, left_w, today_h),
        sessions: r(x0, top + today_h + GAP, left_w, g - today_h - GAP),
        lines: r(x1, top, right_w, lines_h),
        todo: r(x1, top + lines_h + GAP, right_w, g - lines_h - GAP),
    }
}

/// Paint a floating card at `rect` and draw `add` inside its padding.
pub fn card(ui: &mut Ui, rect: Rect, add: impl FnOnce(&mut Ui)) {
    let b = Backdrop::of(ui.ctx());
    backdrop::panel(ui.painter(), rect, &b);
    let inner = rect.shrink2(vec2(PAD_X, PAD_Y));
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::top_down(Align::Min)),
    );
    // Room for focus rings of controls flush with the padding.
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    child.spacing_mut().item_spacing.y = 6.0;
    add(&mut child);
}

/// A card's header row: the gray caption, an optional gray note after it and
/// `right` (laid out right to left) on the right.
pub fn header(
    ui: &mut Ui,
    cx: &Cx<'_>,
    title: &str,
    note: Option<&str>,
    right: impl FnOnce(&mut Ui),
) {
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), HEADER),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_min_height(HEADER);
            ui.label(
                egui::RichText::new(title)
                    .font(font(text::SMALL + 1.0, Weight::Medium))
                    .color(cx.pal.text_muted),
            );
            if let Some(note) = note {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(note)
                            .font(font(text::CAPTION, Weight::Regular))
                            .color(cx.pal.text_subtle),
                    )
                    .truncate(),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), right);
        },
    );
}

/// A calm centred message (empty states): an icon in a soft disc, a line of
/// text and an optional gray line, in `rect`.
pub fn calm(ui: &Ui, cx: &Cx<'_>, rect: Rect, icon: Icon, tint: Color32, title: &str, body: &str) {
    let p = ui.painter();
    let c = pos2(rect.center().x, rect.center().y - 18.0);
    p.circle_filled(
        c,
        20.0,
        Backdrop::area(tint, if cx.pal.dark { 46 } else { 30 }),
    );
    icons::paint(p, Rect::from_center_size(c, vec2(20.0, 20.0)), icon, tint);
    p.text(
        pos2(c.x, c.y + 34.0),
        Align2::CENTER_CENTER,
        title,
        font(text::BODY, Weight::Medium),
        cx.pal.text,
    );
    // The state's words for screen readers (user bug 56).
    let text = if body.is_empty() {
        title.to_owned()
    } else {
        format!("{title}. {body}")
    };
    crate::design::a11y::text(
        &ui.interact(
            Rect::from_center_size(pos2(c.x, c.y + 40.0), vec2(rect.width(), 40.0)),
            ui.scope_id().with(("overview-calm", title)),
            Sense::hover(),
        ),
        &text,
    );
    if !body.is_empty() {
        let mut job = egui::text::LayoutJob::single_section(
            body.to_owned(),
            egui::TextFormat {
                font_id: font(text::CAPTION, Weight::Regular),
                color: cx.pal.text_subtle,
                ..Default::default()
            },
        );
        job.wrap.max_width = (rect.width() - 24.0).max(40.0);
        job.wrap.max_rows = 2;
        job.halign = egui::Align::Center;
        let g = p.layout_job(job);
        p.galley(pos2(c.x, c.y + 50.0), g, cx.pal.text_subtle);
    }
}

// ---------------------------------------------------------------------------
// 待办
// ---------------------------------------------------------------------------

/// Height of one to-do row.
const TODO_ROW: f32 = 46.0;

/// Icon and colour of a to-do.
fn todo_icon(ui: &Ui, cx: &Cx<'_>, t: &Todo) -> (Icon, Color32) {
    match t.status {
        Status::Error => (Icon::Warning, cx.pal.red),
        Status::Warn => (Icon::Warning, cx.pal.amber),
        _ => match t.kind {
            super::todo::TodoKind::AppUpdate | super::todo::TodoKind::CliUpdate => {
                (Icon::Download, Backdrop::of(ui.ctx()).chart[0])
            }
            _ => (Icon::Info, cx.pal.text_muted),
        },
    }
}

/// The 待办 card: actionable items with their fix buttons, or a calm "all
/// good" state.
pub fn todo_card(ui: &mut Ui, cx: &Cx<'_>, todos: &[Todo], out: &mut Vec<Action>) {
    let count = (!todos.is_empty()).then(|| todos.len().to_string());
    header(
        ui,
        cx,
        &cx.tr.t("overview.card.todo"),
        count.as_deref(),
        |_| {},
    );
    let full = ui.max_rect();
    let body = Rect::from_min_max(pos2(full.min.x, ui.cursor().min.y + 2.0), full.max);
    if todos.is_empty() {
        calm(
            ui,
            cx,
            body,
            Icon::Check,
            cx.pal.green,
            &cx.tr.t("overview.todo.all_clear"),
            &cx.tr.t("overview.todo.all_clear_body"),
        );
        return;
    }
    let fits = ((body.height() + 2.0) / TODO_ROW).floor().max(1.0) as usize;
    // When some do not fit, the last row says how many more there are.
    let shown = if todos.len() > fits {
        fits - 1
    } else {
        todos.len()
    };
    let mut y = body.min.y;
    for (i, t) in todos.iter().take(shown).enumerate() {
        let row = Rect::from_min_size(pos2(body.min.x, y), vec2(body.width(), TODO_ROW - 4.0));
        todo_row(ui, cx, i, t, row, out);
        y += TODO_ROW;
    }
    if shown < todos.len() {
        let row = Rect::from_min_size(pos2(body.min.x, y), vec2(body.width(), TODO_ROW - 4.0));
        let resp = ui
            .interact(
                row,
                ui.scope_id().with("overview-todo-more"),
                Sense::click(),
            )
            .on_hover_cursor(CursorIcon::PointingHand);
        let more = cx
            .tr
            .f("overview.todo.more", &[("n", &(todos.len() - shown))]);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &more));
        let t = hover_t(ui, &resp);
        let b = Backdrop::of(ui.ctx());
        ui.painter().rect_filled(
            row,
            radius::ROW,
            Color32::TRANSPARENT.lerp_to_gamma(b.hover, t),
        );
        ui.painter().text(
            pos2(row.min.x + 8.0, row.center().y),
            Align2::LEFT_CENTER,
            more,
            font(text::SMALL, Weight::Medium),
            cx.pal.text_muted,
        );
        if show_focus(&resp) {
            focus_ring(ui.painter(), row, radius::ROW, cx.pal);
        }
        if resp.clicked() {
            out.push(Action::Go(Page::Diagnostics));
        }
    }
}

fn todo_row(ui: &mut Ui, cx: &Cx<'_>, i: usize, t: &Todo, row: Rect, out: &mut Vec<Action>) {
    let pal = cx.pal;
    let (icon, color) = todo_icon(ui, cx, t);
    let label = t.fix_label(cx);
    let button = Button::new(label.clone()).small();
    let bw = button.desired_size(ui).x;
    // The button on the right, centred vertically.
    let brect = Rect::from_min_size(pos2(row.max.x - bw, row.center().y - 14.0), vec2(bw, 28.0));
    // The to-do's text first (screen readers read it before its button; the
    // whole text is on hover too, rows are one line each).
    let text_rect = Rect::from_min_max(
        pos2(row.min.x, row.min.y),
        pos2(brect.min.x - 6.0, row.max.y),
    );
    let full = match &t.detail {
        Some(d) if !d.is_empty() => format!("{}\n{d}", t.title),
        _ => t.title.clone(),
    };
    let text_resp = ui.interact(
        text_rect,
        ui.scope_id().with(("overview-todo-text", i)),
        Sense::hover(),
    );
    crate::design::a11y::text(&text_resp, &full.replace('\n', ". "));
    text_resp.on_hover_text(full);
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(brect)
            .id_salt(("overview-todo-fix", i))
            .layout(Layout::left_to_right(Align::Center)),
    );
    let tip = t
        .fix_tip(cx)
        .or_else(|| t.detail.clone())
        .unwrap_or_else(|| t.title.clone());
    if child.add(button).on_hover_text(tip).clicked() {
        out.push(t.fix(cx));
    }
    let p = ui.painter();
    let ic = pos2(row.min.x + 10.0, row.center().y);
    icons::paint(p, Rect::from_center_size(ic, vec2(16.0, 16.0)), icon, color);
    let text_x = row.min.x + 28.0;
    let max_w = (brect.min.x - 10.0 - text_x).max(40.0);
    let galley = |s: &str, size: f32, weight: Weight, color: Color32| {
        let mut job = egui::text::LayoutJob::single_section(
            s.to_owned(),
            egui::TextFormat {
                font_id: font(size, weight),
                color,
                ..Default::default()
            },
        );
        job.wrap.max_width = max_w;
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        p.layout_job(job)
    };
    let title = galley(&t.title, text::SMALL, Weight::Medium, pal.text);
    match &t.detail {
        Some(d) if !d.is_empty() => {
            let detail = galley(d, text::CAPTION, Weight::Regular, pal.text_subtle);
            let h = title.size().y + detail.size().y + 1.0;
            let y = row.center().y - h / 2.0;
            let th = title.size().y;
            p.galley(pos2(text_x, y), title, pal.text);
            p.galley(pos2(text_x, y + th + 1.0), detail, pal.text_subtle);
        }
        _ => {
            let th = title.size().y;
            p.galley(pos2(text_x, row.center().y - th / 2.0), title, pal.text);
        }
    }
}
