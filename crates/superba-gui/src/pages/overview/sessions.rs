//! The Overview's 最近会话 card (user request 27): the five Codex sessions
//! with the newest requests — name, where they ran, model, tokens, when they
//! were last active and their estimated cost over the seven days. A click
//! opens the Usage page filtered to that session.
//!
//! The request log knows this computer's requests (the local router and Codex's
//! session files here), so the host column names this computer and the client
//! (desktop app, terminal, `codex exec`).

use egui::{Align2, Color32, CursorIcon, Rect, Sense, Ui, pos2, vec2};
use superba_core::RequestClient;

use super::cards::{calm, header};
use super::usage::{SessionSummary, Snapshot};
use super::{Action, Cx, Page};
use crate::design::backdrop::Backdrop;
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::icons::Icon;
use crate::design::tokens::{radius, text};
use crate::design::widgets::kpi;
use crate::design::widgets::{Button, focus_ring, hover_t, show_focus};

/// Usage page command that shows one session's requests.
pub fn usage_command(key: &str) -> String {
    format!("session:{key}")
}

/// Column widths (title takes the rest): host, model, tokens, last active, cost.
fn columns(width: f32) -> [f32; 6] {
    let fixed = [96.0, 124.0, 62.0, 84.0, 64.0];
    let title = (width - fixed.iter().sum::<f32>()).max(120.0);
    [title, fixed[0], fixed[1], fixed[2], fixed[3], fixed[4]]
}

/// "本机 · App", "本机 · 终端": this computer and the client that sent the
/// newest request.
pub fn host_label(cx: &Cx<'_>, s: &SessionSummary) -> String {
    let client = match s.client {
        RequestClient::Desktop => "overview.sessions.client.desktop",
        RequestClient::Tui => "overview.sessions.client.tui",
        RequestClient::Exec => "overview.sessions.client.exec",
        _ => "overview.sessions.client.other",
    };
    format!(
        "{} · {}",
        cx.tr.t("overview.sessions.this_computer"),
        cx.tr.t(client)
    )
}

/// Draw the card.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, snap: Option<&Snapshot>, out: &mut Vec<Action>) {
    let tr = cx.tr;
    header(
        ui,
        cx,
        &tr.t("overview.card.sessions"),
        Some(&tr.t("overview.sessions.window")),
        |ui| {
            if ui
                .add(Button::ghost(tr.t("overview.sessions.all")).small())
                .clicked()
            {
                out.push(Action::Go(Page::Usage));
            }
        },
    );
    let full = ui.max_rect();
    let body = Rect::from_min_max(pos2(full.min.x, ui.cursor().min.y + 2.0), full.max);
    let b = Backdrop::of(ui.ctx());
    let sessions = snap.map(|s| s.sessions.as_slice()).unwrap_or_default();
    if sessions.is_empty() {
        let (title, body_text) = if snap.is_none() {
            ("overview.sessions.loading", "")
        } else {
            ("overview.sessions.empty", "overview.sessions.empty_body")
        };
        let body_text = if body_text.is_empty() {
            String::new()
        } else {
            tr.t(body_text)
        };
        calm(
            ui,
            cx,
            body,
            Icon::Chat,
            b.chart[1],
            &tr.t(title),
            &body_text,
        );
        return;
    }
    let pal = cx.pal;
    let cols = columns(body.width());
    // Column captions.
    let caps = [
        tr.t("overview.sessions.col.session"),
        tr.t("overview.sessions.col.host"),
        tr.t("overview.sessions.col.model"),
        tr.t("overview.sessions.col.tokens"),
        tr.t("overview.sessions.col.last"),
        tr.t("overview.sessions.col.cost"),
    ];
    let cap_y = body.min.y + 8.0;
    let mut x = body.min.x;
    for (i, c) in caps.iter().enumerate() {
        let right = i >= 3;
        let (pos, align) = if right {
            (pos2(x + cols[i] - 8.0, cap_y), Align2::RIGHT_CENTER)
        } else {
            (pos2(x, cap_y), Align2::LEFT_CENTER)
        };
        ui.painter().text(
            pos,
            align,
            c,
            font(text::CAPTION - 1.0, Weight::Medium),
            pal.text_subtle,
        );
        x += cols[i];
    }
    let top = body.min.y + 18.0;
    let row_h = ((body.max.y - top) / super::usage::RECENT_SESSIONS as f32).clamp(28.0, 40.0);
    let fits = ((body.max.y - top) / row_h).floor() as usize;
    for (i, s) in sessions.iter().take(fits).enumerate() {
        let r = Rect::from_min_size(
            pos2(body.min.x - 8.0, top + i as f32 * row_h),
            vec2(body.width() + 16.0, row_h - 2.0),
        );
        if session_row(ui, cx, s, r, &cols) {
            out.push(Action::PageCommand {
                page: Page::Usage,
                command: usage_command(&s.key),
            });
        }
    }
}

/// One session; returns true when it was clicked.
fn session_row(ui: &mut Ui, cx: &Cx<'_>, s: &SessionSummary, r: Rect, cols: &[f32; 6]) -> bool {
    let pal = cx.pal;
    let b = Backdrop::of(ui.ctx());
    let id = ui.scope_id().with(("overview-session", s.key.as_str()));
    let label = s.label();
    let mut tip = cx.tr.f(
        "overview.sessions.tip",
        &[("title", &label), ("n", &s.requests)],
    );
    if s.cost_micros.is_none_or(|c| c <= 0) || s.unpriced {
        // A missing or partial cost says why.
        tip.push('\n');
        tip.push_str(&cx.tr.t("overview.sessions.unpriced_tip"));
    }
    let resp = ui
        .interact(r, id, Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(tip);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
    let t = hover_t(ui, &resp);
    if t > 0.0 {
        ui.painter().rect_filled(
            r,
            radius::ROW,
            Color32::TRANSPARENT.lerp_to_gamma(b.hover, t),
        );
    }
    let y = r.center().y;
    let mut x = r.min.x + 8.0;
    let one = |s: String, w: f32, font_id: egui::FontId, color: Color32| {
        let mut job = egui::text::LayoutJob::single_section(
            s,
            egui::TextFormat {
                font_id,
                color,
                ..Default::default()
            },
        );
        job.wrap.max_width = (w - 10.0).max(10.0);
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        ui.painter().layout_job(job)
    };
    let cells: [(String, egui::FontId, Color32); 6] = [
        (
            label.clone(),
            font(text::SMALL + 1.0, Weight::Medium),
            pal.text,
        ),
        (
            host_label(cx, s),
            font(text::CAPTION, Weight::Regular),
            pal.text_muted,
        ),
        (
            s.model.clone().unwrap_or_else(|| "—".into()),
            mono_font(text::CAPTION, Weight::Regular),
            pal.text_muted,
        ),
        (
            kpi::si(s.tokens).text(),
            font(text::SMALL, Weight::Medium),
            pal.text,
        ),
        (
            cx.ago(s.last.min(cx.now)),
            font(text::CAPTION, Weight::Regular),
            pal.text_muted,
        ),
        (
            s.cost_micros
                .filter(|c| *c > 0)
                .map_or_else(|| "—".to_owned(), |c| kpi::usd(c).text()),
            font(text::SMALL, Weight::Semibold),
            pal.text,
        ),
    ];
    for (i, (s_text, f, color)) in cells.into_iter().enumerate() {
        let g = one(s_text, cols[i], f, color);
        let gy = y - g.size().y / 2.0;
        if i >= 3 {
            let gw = g.size().x;
            ui.painter()
                .galley(pos2(x + cols[i] - 8.0 - gw, gy), g, color);
        } else {
            ui.painter().galley(pos2(x, gy), g, color);
        }
        x += cols[i];
    }
    if show_focus(&resp) {
        focus_ring(ui.painter(), r, radius::ROW, pal);
    }
    resp.clicked()
}
