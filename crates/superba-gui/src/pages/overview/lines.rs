//! The Overview's 线路 card (user request 27): every line of the applied
//! provider in its failover order, with the last probe's latency, a tiny
//! 30-minute trace of the time to response headers of the requests the local
//! router sent through it, and the router's health of the line. A click on a
//! line plans the switch to it; 测速 probes the lines again.

use egui::{Align2, Color32, CursorIcon, Pos2, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};
use superba_core::{Line, RouterLineHealth, Site};

use super::cards::{calm, header};
use super::usage::{Snapshot, TRACE_MINUTES, Trace};
use super::{Action, Cx, caps_latency, latency_text};
use crate::design::backdrop::Backdrop;
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::icons::Icon;
use crate::design::tokens::{radius, text};
use crate::design::widgets::{Button, Latency, focus_ring, hover_t, show_focus};

/// One line as the card shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct LineRow {
    /// The line.
    pub line: Line,
    /// Its place in the failover order (1 = tried first).
    pub order: usize,
    /// Codex uses it now.
    pub active: bool,
    /// Last probe.
    pub latency: Latency,
    /// The router skips it after failures.
    pub skipped: bool,
    /// The router's requests through it in the last 30 minutes.
    pub trace: Option<Trace>,
}

/// The applied provider's lines in failover order (the profile's line
/// priority, the router's order while it runs), with what the card shows; the
/// line in use is the applied one, or in router mode the latest observed
/// upstream. A preference alone does not imply a request used it.
pub fn rows(cx: &Cx<'_>, site: &Site, snap: Option<&Snapshot>) -> Vec<LineRow> {
    let profile = cx.data.active_profile();
    let routed = cx.data.router_enabled();
    let active = cx
        .data
        .active_state()
        .and_then(|a| a.line_id.clone())
        .filter(|_| !routed);
    let probing = cx.data.probing_site(&site.id);
    let member = profile.and_then(|p| {
        snap.and_then(|s| s.router.as_ref())
            .and_then(|r| r.members.iter().find(|m| m.profile_id == p.id))
    });
    let mut lines = crate::pages::shared::ordered_lines(site, profile);
    // Disabled lines (user bug 108) are never used: the card leaves them out.
    lines.retain(|l| !profile.is_some_and(|p| p.is_line_disabled(&l.id)));
    lines
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let skipped = member.is_some_and(|m| {
                m.lines
                    .iter()
                    .any(|r| r.id == line.id.as_str() && r.health == RouterLineHealth::Open)
            });
            let trace = profile.and_then(|p| {
                snap.and_then(|s| {
                    s.traces
                        .get(&(p.id.to_string(), line.id.to_string()))
                        .cloned()
                })
            });
            LineRow {
                order: i + 1,
                active: if routed {
                    member
                        .and_then(|m| m.last_route.as_ref())
                        .is_some_and(|r| r.line_id == line.id.as_str())
                } else {
                    active.as_ref() == Some(&line.id)
                },
                latency: caps_latency(cx.data.site_caps(&site.id, &line.id), probing),
                skipped,
                trace,
                line,
            }
        })
        .collect::<Vec<_>>()
}

/// Actual latest upstream of this member; never infer it from breaker health.
pub fn current_text(cx: &Cx<'_>, snap: Option<&Snapshot>) -> Option<String> {
    if !cx.data.router_enabled() {
        return None;
    }
    let profile = cx.data.active_profile()?;
    let site = cx.data.active_site()?;
    let route = snap?
        .router
        .as_ref()?
        .members
        .iter()
        .find(|m| m.profile_id == profile.id)?
        .last_route
        .as_ref()?;
    Some(crate::pages::shared::route_text(cx.tr, route, |id| {
        site.line(&superba_core::LineId::new(id))
            .map(|l| cx.tr.text(&l.name))
            .unwrap_or_else(|| id.to_owned())
    }))
}

/// The applied direct line or the router's latest observed upstream.
pub fn effective(cx: &Cx<'_>, snap: Option<&Snapshot>) -> Option<LineRow> {
    let site = cx.data.active_site()?;
    rows(cx, &site, snap).into_iter().find(|r| r.active)
}

/// Draw the card.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, snap: Option<&Snapshot>, out: &mut Vec<Action>) {
    let tr = cx.tr;
    let site = cx.data.active_site().filter(|_| !cx.data.official_active());
    let profile = cx.data.active_profile();
    let name = match (profile, &site) {
        (Some(p), Some(s)) => Some(crate::app::provider_name(tr, p, s)),
        _ => None,
    };
    let probing = site.as_ref().is_some_and(|s| cx.data.probing_site(&s.id));
    header(
        ui,
        cx,
        &tr.t("overview.card.lines"),
        name.as_deref(),
        |ui| {
            if let Some(s) = site.as_ref().filter(|s| !s.lines.is_empty())
                && ui
                    .add(
                        Button::new(tr.t("overview.lines.probe"))
                            .small()
                            .icon(Icon::Gauge)
                            .loading(probing)
                            .enabled(!probing),
                    )
                    .on_hover_text(tr.t("overview.lines.probe_tip"))
                    .clicked()
            {
                out.push(Action::Probe(s.id.clone()));
            }
        },
    );
    let full = ui.max_rect();
    let body = Rect::from_min_max(pos2(full.min.x, ui.cursor().min.y + 4.0), full.max);
    let b = Backdrop::of(ui.ctx());
    let (Some(site), Some(profile)) = (site, profile) else {
        let (title, text) = if cx.data.official_active() {
            ("overview.lines.official", "overview.lines.official_body")
        } else {
            ("overview.lines.none", "overview.lines.none_body")
        };
        calm(
            ui,
            cx,
            body,
            Icon::Cloud,
            b.chart[0],
            &tr.t(title),
            &tr.t(text),
        );
        return;
    };
    let rows = rows(cx, &site, snap);
    let routed = super::routed_addr(cx).is_some();
    // Footer: how the order is used.
    let note = if routed {
        let priority =
            crate::pages::shared::priority_text(tr, rows.iter().map(|r| tr.text(&r.line.name)));
        let current = current_text(cx, snap).unwrap_or_else(|| tr.t("router.preference.waiting"));
        format!("{priority}\n{current}")
    } else if rows.len() <= 1 {
        tr.t("overview.lines.single")
    } else {
        tr.t("overview.lines.failover_off")
    };
    let mut job = egui::text::LayoutJob::single_section(
        note.clone(),
        egui::TextFormat {
            font_id: font(text::CAPTION, Weight::Regular),
            color: cx.pal.text_subtle,
            ..Default::default()
        },
    );
    job.wrap.max_width = body.width();
    job.wrap.max_rows = if routed { 4 } else { 2 };
    let g = ui.painter().layout_job(job);
    let gh = g.size().y;
    let note_h = (gh + 8.0).max(34.0);
    let list = Rect::from_min_max(body.min, pos2(body.max.x, body.max.y - note_h));
    let row_h = (list.height() / rows.len().max(1) as f32).clamp(40.0, 60.0);
    let shown = ((list.height() / row_h).floor() as usize).min(rows.len());
    for (i, row) in rows.iter().take(shown).enumerate() {
        let r = Rect::from_min_size(
            pos2(list.min.x - 8.0, list.min.y + i as f32 * row_h),
            vec2(list.width() + 16.0, row_h - 4.0),
        );
        let selected = if routed { row.order == 1 } else { row.active };
        if line_row(ui, cx, row, r, routed) && !selected && !cx.busy {
            out.push(crate::pages::shared::switch_action(
                cx.data,
                profile,
                &row.line.id,
            ));
        }
    }
    ui.painter()
        .galley(pos2(body.min.x, body.max.y - gh), g, cx.pal.text_subtle);
    if routed {
        ui.interact(
            Rect::from_min_max(pos2(body.min.x, body.max.y - gh), body.max),
            ui.scope_id().with("line-order-help"),
            Sense::hover(),
        )
        .on_hover_text(format!(
            "{note}\n{}\n{}",
            crate::pages::router::recovery_text(tr, cx.data.router_failover.as_ref()),
            tr.t("router.preference.observed")
        ));
    }
}

/// One line; returns true when it was clicked.
fn line_row(ui: &mut Ui, cx: &Cx<'_>, row: &LineRow, r: Rect, routed: bool) -> bool {
    let pal = cx.pal;
    let b = Backdrop::of(ui.ctx());
    // Geometry first: latency on the right, the trace left of it.
    let lat_right = r.max.x - 10.0;
    let lat_g = ui.painter().layout_no_wrap(
        latency_text(cx, row.latency),
        mono_font(text::SMALL, Weight::Medium),
        pal.text,
    );
    let lw = lat_g.size().x;
    let dot = pos2(lat_right - lw - 9.0, r.center().y);
    let trace_w = (r.width() * 0.22).clamp(48.0, 72.0);
    let trace_rect = Rect::from_min_size(
        pos2(dot.x - 14.0 - trace_w, r.center().y - 10.0),
        vec2(trace_w, 20.0),
    );
    let id = ui.scope_id().with(("overview-line", row.line.id.as_str()));
    let resp = ui.interact(r, id, Sense::click());
    let resp = if row.active {
        resp
    } else {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    };
    let host = row.line.origin.host().to_owned();
    let over_trace = resp
        .hover_pos()
        .is_some_and(|p| trace_rect.expand(4.0).contains(p));
    let resp = resp.on_hover_text(if routed && !over_trace {
        cx.tr.t("router.preference.choose")
    } else if over_trace {
        trace_tip(cx, row.trace.as_ref(), routed)
    } else if row.active {
        cx.tr.f("overview.line.active_tip", &[("host", &host)])
    } else {
        cx.tr.f("overview.line.switch_tip", &[("host", &host)])
    });
    let name = if routed && row.order == 1 {
        cx.tr.f(
            "router.preference.first",
            &[("line", &cx.tr.text(&row.line.name))],
        )
    } else {
        cx.tr.text(&row.line.name)
    };
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, row.active, &name));
    let t = hover_t(ui, &resp);
    let p = ui.painter();
    if row.active {
        p.rect_filled(r, radius::ROW, b.selected);
    } else if t > 0.0 {
        p.rect_filled(
            r,
            radius::ROW,
            Color32::TRANSPARENT.lerp_to_gamma(b.hover, t),
        );
    }
    // Order disc.
    let oc = pos2(r.min.x + 19.0, r.center().y);
    let (fill, fg) = if row.active {
        (b.chart[0], Color32::WHITE)
    } else {
        (pal.active, pal.text_muted)
    };
    p.circle_filled(oc, 10.0, fill);
    p.text(
        oc,
        Align2::CENTER_CENTER,
        row.order.to_string(),
        font(text::CAPTION - 1.0, Weight::Semibold),
        fg,
    );
    if row.latency == Latency::Probing {
        crate::design::icons::spinner(
            p,
            Rect::from_center_size(dot, vec2(9.0, 9.0)),
            pal.text_muted,
            ui.input(|i| i.time),
        );
        ui.ctx().request_repaint();
    } else {
        p.circle_filled(dot, 3.5, pal.status(row.latency.status()));
    }
    let lh = lat_g.size().y;
    p.galley(
        pos2(lat_right - lw, r.center().y - lh / 2.0),
        lat_g,
        pal.text,
    );
    trace(ui, cx, trace_rect, row.trace.as_ref(), row.skipped);
    // Name and host.
    let text_x = oc.x + 18.0;
    let max_w = (trace_rect.min.x - 10.0 - text_x).max(30.0);
    let one_line = |s: String, size: f32, weight: Weight, color: Color32| {
        let mut job = egui::text::LayoutJob::single_section(
            s,
            egui::TextFormat {
                font_id: font(size, weight),
                color,
                ..Default::default()
            },
        );
        job.wrap.max_width = max_w;
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        ui.painter().layout_job(job)
    };
    let title = one_line(name, text::SMALL + 1.0, Weight::Medium, pal.text);
    let sub = if row.skipped {
        format!("{host} · {}", cx.tr.t("overview.lines.skipped"))
    } else if row.active {
        format!("{host} · {}", cx.tr.t("overview.lines.in_use"))
    } else {
        host
    };
    let sub_color = if row.skipped {
        pal.red
    } else {
        pal.text_subtle
    };
    let detail = one_line(sub, text::CAPTION, Weight::Regular, sub_color);
    let h = title.size().y + detail.size().y;
    let y = r.center().y - h / 2.0;
    let th = title.size().y;
    ui.painter().galley(pos2(text_x, y), title, pal.text);
    ui.painter().galley(pos2(text_x, y + th), detail, sub_color);
    if show_focus(&resp) {
        focus_ring(ui.painter(), r, radius::ROW, pal);
    }
    resp.clicked()
}

/// Longest run of minutes without a request that a trace still bridges.
const MAX_GAP: usize = 4;

/// What the trace shows, for its hover text.
fn trace_tip(cx: &Cx<'_>, trace: Option<&Trace>, routed: bool) -> String {
    match trace {
        Some(t) if t.samples > 0 => {
            let median = t
                .median_ms
                .map_or_else(|| "—".to_owned(), |m| format!("{m} ms"));
            let mut s = cx.tr.f(
                "overview.lines.trace_tip",
                &[("n", &t.samples), ("ms", &median)],
            );
            if t.errors > 0 {
                s.push('\n');
                s.push_str(&cx.tr.f("overview.lines.trace_errors", &[("n", &t.errors)]));
            }
            s
        }
        Some(t) if t.errors > 0 => cx.tr.f("overview.lines.trace_errors", &[("n", &t.errors)]),
        _ if routed => cx.tr.t("overview.lines.trace_idle"),
        _ => cx.tr.t("overview.lines.trace_router_off"),
    }
}

/// The 30-minute trace: per-minute medians as a line (gaps where no request
/// was answered) over a faint base line; dashed when there is nothing to show.
fn trace(ui: &Ui, cx: &Cx<'_>, rect: Rect, trace: Option<&Trace>, skipped: bool) {
    let pal = cx.pal;
    let b = Backdrop::of(ui.ctx());
    let p = ui.painter();
    let base = if pal.dark {
        Color32::from_white_alpha(28)
    } else {
        Color32::from_black_alpha(28)
    };
    let Some(t) = trace.filter(|t| t.samples > 0) else {
        p.extend(Shape::dashed_line(
            &[
                pos2(rect.min.x, rect.max.y - 2.0),
                pos2(rect.max.x, rect.max.y - 2.0),
            ],
            Stroke::new(1.0, base),
            2.0,
            3.0,
        ));
        return;
    };
    p.line_segment(
        [
            pos2(rect.min.x, rect.max.y - 1.0),
            pos2(rect.max.x, rect.max.y - 1.0),
        ],
        Stroke::new(1.0, base),
    );
    let max = t.points.iter().flatten().copied().max().unwrap_or(1).max(1) as f32 * 1.15;
    let step = rect.width() / (TRACE_MINUTES - 1) as f32;
    let color = if skipped { pal.red } else { b.chart[1] };
    let flush = |seg: &mut Vec<Pos2>| {
        match seg.len() {
            0 => {}
            1 => {
                p.circle_filled(seg[0], 1.6, color);
            }
            _ => {
                p.add(Shape::line(seg.clone(), Stroke::new(1.5, color)));
            }
        }
        seg.clear();
    };
    // Minutes without an answered request are bridged when the gap is short
    // (a request every two minutes is a steady line, not a row of dots).
    let mut segment: Vec<Pos2> = Vec::new();
    let mut last: Option<usize> = None;
    for (i, v) in t.points.iter().enumerate() {
        let Some(ms) = v else {
            continue;
        };
        if last.is_some_and(|l| i - l > MAX_GAP) {
            flush(&mut segment);
        }
        segment.push(pos2(
            rect.min.x + step * i as f32,
            rect.max.y - 2.0 - (*ms as f32 / max) * (rect.height() - 3.0),
        ));
        last = Some(i);
    }
    flush(&mut segment);
}
