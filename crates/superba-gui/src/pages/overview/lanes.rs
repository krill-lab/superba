//! The request path with several coding tools (user request 118): one lane
//! per tool — tool → 本地路由 | 直连 → provider · line → model — with the
//! routed lanes first, sharing one router node, and the lanes that do not fit
//! folded into 「+N」 (design: `.private/v3/overview2/DESIGN.md`).
//!
//! With one tool the Overview keeps the single-tool hero ([`super::hero`]).

use egui::{
    Align2, Color32, CursorIcon, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2,
};

use super::hero::Hero;
use super::tools::{self, ToolRoute, Via};
use super::{Action, Cx, Page};
use crate::design::backdrop::Backdrop;
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::icons::{self, Icon};
use crate::design::tokens::{radius, text};
use crate::design::widgets::{MonogramTile, focus_ring, hover_t, show_focus};

/// Height of the caption row above the lanes.
const CAPTION: f32 = 24.0;
/// Smallest lane height.
pub const LANE_MIN: f32 = 30.0;
/// Largest lane height.
const LANE_MAX: f32 = 36.0;
/// Card padding (as the single-tool hero).
const PAD: egui::Vec2 = vec2(20.0, 14.0);
/// Side of a lane's tool tile.
const TILE: f32 = 22.0;
/// Space between the columns (the links run in it).
const GUTTER: f32 = 30.0;

/// Lanes that fit in a hero `height` points tall.
pub fn capacity(height: f32) -> usize {
    let room = height - 2.0 * PAD.y - CAPTION;
    ((room / LANE_MIN).floor() as usize).clamp(2, 6)
}

/// The hero's height for `lanes` lanes at most `cap` points (the 960×640
/// window keeps the single-tool height: three lanes).
pub fn height(lanes: usize, cap: f32) -> f32 {
    let want = 2.0 * PAD.y + CAPTION + lanes as f32 * LANE_MIN + 4.0;
    want.clamp(super::hero::HEIGHT, cap.max(super::hero::HEIGHT))
}

/// Where the columns and lanes of the diagram go in a hero `rect`.
#[derive(Clone, Debug, PartialEq)]
pub struct Geometry {
    /// Tool column (tile and name).
    pub tool: (f32, f32),
    /// Router column.
    pub router: (f32, f32),
    /// Provider column.
    pub provider: (f32, f32),
    /// Model column.
    pub model: (f32, f32),
    /// Lane centres, top to bottom.
    pub lanes: Vec<f32>,
    /// Lane height.
    pub lane_h: f32,
    /// Lanes drawn for tools.
    pub shown: usize,
    /// Tools folded into 「+N」.
    pub folded: usize,
}

/// Lay out `total` tool lanes in `rect`.
pub fn geometry(rect: Rect, total: usize) -> Geometry {
    let inner = rect.shrink2(PAD);
    let (shown, folded) = tools::visible_lanes(total, rect.height());
    let rows = shown;
    let top = inner.min.y + CAPTION;
    let lane_h = ((inner.max.y - top) / rows.max(1) as f32).clamp(LANE_MIN - 4.0, LANE_MAX);
    let w = inner.width();
    let x0 = inner.min.x;
    let tool_w = (w * 0.24).clamp(120.0, 200.0);
    let router_w = (w * 0.19).clamp(120.0, 156.0);
    let rest = w - tool_w - router_w - 2.0 * GUTTER - 16.0;
    let provider_w = (rest * 0.55).floor();
    let x1 = x0 + tool_w + GUTTER;
    let x2 = x1 + router_w + GUTTER;
    let x3 = x2 + provider_w + 16.0;
    Geometry {
        tool: (x0, x0 + tool_w),
        router: (x1, x1 + router_w),
        provider: (x2, x2 + provider_w),
        model: (x3, inner.max.x),
        lanes: (0..rows).map(|i| top + lane_h * (i as f32 + 0.5)).collect(),
        lane_h,
        shown,
        folded,
    }
}

/// A one-line galley no wider than `max` (elided with "…").
fn galley(
    ui: &Ui,
    s: &str,
    font_id: egui::FontId,
    color: Color32,
    max: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        s.to_owned(),
        egui::TextFormat {
            font_id,
            color,
            ..Default::default()
        },
    );
    job.wrap.max_width = max.max(10.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    ui.painter().layout_job(job)
}

/// A clickable, named cell; returns whether it was clicked.
fn cell(ui: &mut Ui, cx: &Cx<'_>, r: Rect, id: egui::Id, name: &str, tip: &str) -> bool {
    let b = Backdrop::of(ui.ctx());
    let resp = ui
        .interact(r, id, Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand);
    let label = name.to_owned();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
    let resp = if tip.is_empty() {
        resp
    } else {
        resp.on_hover_text(tip)
    };
    let t = hover_t(ui, &resp);
    if t > 0.0 {
        ui.painter().rect_filled(
            r,
            radius::INNER,
            Color32::TRANSPARENT.lerp_to_gamma(b.hover, t),
        );
    }
    if show_focus(&resp) {
        focus_ring(ui.painter(), r, radius::INNER, cx.pal);
    }
    resp.clicked()
}

/// A chevron button centred on `at` named by `label_key` (it opens a popover).
fn chevron(ui: &mut Ui, cx: &Cx<'_>, at: Pos2, label_key: &str) -> egui::Response {
    let b = Backdrop::of(ui.ctx());
    let r = Rect::from_center_size(at, vec2(20.0, 20.0));
    let label = cx.tr.t(label_key);
    let c = ui
        .interact(
            r,
            ui.scope_id().with(("overview-lane-menu", label_key)),
            Sense::click(),
        )
        .on_hover_cursor(CursorIcon::PointingHand);
    c.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
    let c = c.on_hover_text(&label);
    let t = hover_t(ui, &c);
    let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&c));
    let fill = if open {
        b.selected
    } else {
        Color32::TRANSPARENT.lerp_to_gamma(b.selected, t)
    };
    ui.painter().rect_filled(r, radius::INNER, fill);
    icons::paint(
        ui.painter(),
        r.shrink(4.0),
        Icon::ChevronDown,
        cx.pal.text_muted.lerp_to_gamma(cx.pal.text, t),
    );
    if show_focus(&c) {
        focus_ring(ui.painter(), r, radius::INNER, cx.pal);
    }
    c
}

/// A line with an arrowhead at `to`.
fn arrow(ui: &Ui, from: Pos2, to: Pos2, stroke: Stroke, dashed: bool) {
    if to.x - from.x < 8.0 {
        return;
    }
    let p = ui.painter();
    if dashed {
        p.extend(Shape::dashed_line(&[from, to], stroke, 4.0, 3.0));
    } else {
        p.line_segment([from, to], stroke);
    }
    p.line_segment([to + vec2(-5.0, -4.0), to], stroke);
    p.line_segment([to + vec2(-5.0, 4.0), to], stroke);
}

/// Draw the lanes of `routes` in the hero `rect` (the caption and the
/// desktop app's action come from the single-tool hero's model).
pub fn show(
    ui: &mut Ui,
    cx: &Cx<'_>,
    hero: &Hero,
    routes: &[ToolRoute],
    rect: Rect,
    out: &mut Vec<Action>,
) {
    let pal = cx.pal;
    let tr = cx.tr;
    let b = Backdrop::of(ui.ctx());
    crate::design::backdrop::panel(ui.painter(), rect, &b);
    let inner = rect.shrink2(PAD);
    ui.painter().text(
        pos2(inner.min.x, inner.min.y + 10.0),
        Align2::LEFT_CENTER,
        tr.t("overview.hero.caption"),
        font(text::SMALL + 1.0, Weight::Medium),
        pal.text_muted,
    );
    let launch = super::hero::launch_button(ui, cx, hero, pos2(inner.max.x, inner.min.y + 10.0));
    let g = geometry(rect, routes.len());
    let shown = &routes[..g.shown];
    let neutral = pal.border_strong;
    let warn = pal.status(crate::design::widgets::Status::Warn);

    // The router node: one pill over the routed lanes (they come first).
    let routed: Vec<usize> = (0..shown.len())
        .filter(|i| shown[*i].via != Via::Direct)
        .collect();
    let router_rect = routed.first().zip(routed.last()).map(|(a, z)| {
        Rect::from_min_max(
            pos2(g.router.0, g.lanes[*a] - g.lane_h / 2.0 + 3.0),
            pos2(g.router.1, g.lanes[*z] + g.lane_h / 2.0 - 3.0),
        )
    });

    // Links first, cells on top.
    for (i, r) in shown.iter().enumerate() {
        let y = g.lanes[i];
        let start = pos2(g.tool.1 + 6.0, y);
        let end = pos2(g.provider.0 - 4.0, y);
        match (r.via, router_rect) {
            (Via::Router, Some(rr)) => {
                arrow(
                    ui,
                    start,
                    pos2(rr.min.x - 3.0, y),
                    Stroke::new(1.6, neutral),
                    false,
                );
                arrow(
                    ui,
                    pos2(rr.max.x + 3.0, y),
                    end,
                    Stroke::new(1.6, neutral),
                    false,
                );
            }
            (Via::RouterOff, Some(rr)) => {
                arrow(
                    ui,
                    start,
                    pos2(rr.min.x - 3.0, y),
                    Stroke::new(1.4, warn),
                    true,
                );
                arrow(
                    ui,
                    pos2(rr.max.x + 3.0, y),
                    end,
                    Stroke::new(1.4, warn),
                    true,
                );
            }
            _ => {
                arrow(ui, start, end, Stroke::new(1.6, neutral), false);
                let label = if r.uncounted {
                    format!(
                        "{} · {}",
                        tr.t("overview.lanes.direct"),
                        tr.t("overview.lanes.uncounted")
                    )
                } else {
                    tr.t("overview.lanes.direct")
                };
                let gl = galley(
                    ui,
                    &label,
                    font(text::CAPTION - 1.0, Weight::Regular),
                    pal.text_subtle,
                    g.router.1 - g.router.0,
                );
                // Below the line: above it would touch a router pill in the
                // lane before.
                let at = pos2((g.router.0 + g.router.1) / 2.0 - gl.size().x / 2.0, y + 2.0);
                let lr = Rect::from_min_size(at, gl.size());
                ui.painter().rect_filled(lr.expand(2.0), 3, b.card);
                ui.painter().galley(at, gl, pal.text_subtle);
                if r.uncounted {
                    ui.interact(
                        lr,
                        ui.scope_id().with(("overview-lane-uncounted", r.harness)),
                        Sense::hover(),
                    )
                    .on_hover_text(tr.t("overview.lanes.uncounted_tip"));
                }
            }
        }
        // Provider → model.
        arrow(
            ui,
            pos2(g.model.0 - 14.0, y),
            pos2(g.model.0 - 3.0, y),
            Stroke::new(1.4, neutral),
            false,
        );
    }

    // The router.
    if let Some(rr) = router_rect {
        let on = cx.data.router_enabled();
        let p = ui.painter();
        p.rect_filled(rr, radius::CARD, b.card);
        if on {
            p.rect_stroke(
                rr,
                radius::CARD,
                Stroke::new(1.2, pal.border_strong),
                StrokeKind::Inside,
            );
        } else {
            super::hero::dashed_rrect(
                p,
                rr.shrink(0.5),
                radius::CARD as f32,
                Stroke::new(1.2, warn),
            );
        }
        let title = tr.t("overview.hero.router");
        let detail = if on {
            super::routed_addr(cx).unwrap_or_else(|| tr.t("overview.hero.router_not_routed"))
        } else {
            tr.t("overview.lanes.router_off")
        };
        let tall = rr.height() >= 44.0;
        let icon_c = pos2(
            rr.min.x + 18.0,
            if tall {
                rr.center().y - 8.0
            } else {
                rr.center().y
            },
        );
        icons::paint(
            ui.painter(),
            Rect::from_center_size(icon_c, vec2(16.0, 16.0)),
            Icon::Route,
            if on { b.chart[1] } else { warn },
        );
        let tg = galley(
            ui,
            &title,
            font(text::SMALL, Weight::Semibold),
            pal.text,
            rr.width() - 40.0,
        );
        ui.painter().galley(
            pos2(rr.min.x + 32.0, icon_c.y - tg.size().y / 2.0),
            tg,
            pal.text,
        );
        if tall {
            let dg = galley(
                ui,
                &detail,
                mono_font(text::CAPTION - 1.0, Weight::Regular),
                pal.text_subtle,
                rr.width() - 20.0,
            );
            ui.painter().galley(
                pos2(rr.min.x + 12.0, rr.center().y + 4.0),
                dg,
                pal.text_subtle,
            );
        }
        let tip = format!(
            "{}\n{}",
            tr.t(if on {
                "overview.hero.router_on_tip"
            } else {
                "overview.lanes.router_off_tip"
            }),
            tr.t("overview.hero.open_router")
        );
        if cell(
            ui,
            cx,
            rr,
            ui.scope_id().with("overview-lane-router"),
            &title,
            &tip,
        ) {
            out.push(Action::Go(Page::Router));
        }
    }

    // Tool, provider and model cells.
    for (i, r) in shown.iter().enumerate() {
        let y = g.lanes[i];
        let h = g.lane_h - 4.0;
        // Tool.
        let tr_rect = Rect::from_min_max(
            pos2(g.tool.0 - 4.0, y - h / 2.0),
            pos2(g.tool.1, y + h / 2.0),
        );
        let route_text = route_text(cx, r);
        if cell(
            ui,
            cx,
            tr_rect,
            ui.scope_id().with(("overview-lane-tool", r.harness)),
            &r.name,
            &route_text,
        ) {
            out.push(tools::tools_command(r.harness));
        }
        MonogramTile::new(tools::monogram(r.harness))
            .size(TILE)
            .paint_at(ui, pos2(g.tool.0 + TILE / 2.0, y));
        let ng = galley(
            ui,
            &r.name,
            font(text::BODY, Weight::Semibold),
            pal.text,
            g.tool.1 - g.tool.0 - TILE - 10.0,
        );
        ui.painter().galley(
            pos2(g.tool.0 + TILE + 8.0, y - ng.size().y / 2.0),
            ng,
            pal.text,
        );
        // Codex: the single-tool hero's problem marker on the tool.
        if r.harness == superba_core::RequestHarness::Codex
            && let Some(status) = hero.provider.marker.or(hero.this.marker)
        {
            ui.painter().circle_filled(
                pos2(g.tool.0 + TILE - 1.0, y - TILE / 2.0 + 1.0),
                4.0,
                pal.status(status),
            );
        }

        // Provider.
        let pr = Rect::from_min_max(
            pos2(g.provider.0, y - h / 2.0),
            pos2(g.provider.1, y + h / 2.0),
        );
        let (ptext, pcolor) = match &r.provider {
            Some(p) => (p.clone(), pal.text),
            None => (tr.t("overview.lanes.no_provider"), pal.text_subtle),
        };
        let pname = format!("{} · {ptext}", r.name);
        if cell(
            ui,
            cx,
            pr,
            ui.scope_id().with(("overview-lane-provider", r.harness)),
            &pname,
            &tr.t("overview.hero.open_providers"),
        ) {
            out.push(Action::Go(Page::Providers));
        }
        icons::paint(
            ui.painter(),
            Rect::from_center_size(pos2(pr.min.x + 10.0, y), vec2(14.0, 14.0)),
            Icon::Cloud,
            pal.text_subtle,
        );
        let codex = r.harness == superba_core::RequestHarness::Codex;
        let menu_w = if codex { 24.0 } else { 0.0 };
        let pg = galley(
            ui,
            &ptext,
            font(text::SMALL, Weight::Medium),
            pcolor,
            pr.width() - 26.0 - menu_w,
        );
        let p_end = pr.min.x + 22.0 + pg.size().x;
        ui.painter()
            .galley(pos2(pr.min.x + 22.0, y - pg.size().y / 2.0), pg, pcolor);
        // Codex keeps the single-tool hero's provider and line popover.
        if codex && hero.provider.menu {
            let c = chevron(ui, cx, pos2(p_end + 12.0, y), "overview.hero.switch_menu");
            super::hero::provider_popover(&c, cx, hero.current_line.as_deref(), out);
        }

        // Model.
        let mr = Rect::from_min_max(pos2(g.model.0, y - h / 2.0), pos2(g.model.1, y + h / 2.0));
        let (mtext, mcolor) = match &r.model {
            Some(m) => (m.clone(), pal.text),
            None => (tr.t("overview.hero.default_model"), pal.text_subtle),
        };
        let mname = format!("{} · {mtext}", r.name);
        if cell(
            ui,
            cx,
            mr,
            ui.scope_id().with(("overview-lane-model", r.harness)),
            &mname,
            &tr.t("overview.hero.open_models"),
        ) {
            out.push(Action::Go(Page::Models));
        }
        let mg = galley(
            ui,
            &mtext,
            mono_font(text::CAPTION, Weight::Regular),
            mcolor,
            mr.width() - 8.0 - menu_w,
        );
        let mut m_end = mr.min.x + 4.0 + mg.size().x;
        ui.painter()
            .galley(pos2(mr.min.x + 4.0, y - mg.size().y / 2.0), mg, mcolor);
        // Codex: its speed tier and the speed popover, as in the single-tool
        // hero.
        if codex {
            let room = mr.max.x - m_end - 8.0 - menu_w;
            if room > 60.0 && !hero.model.detail.is_empty() {
                let dg = galley(
                    ui,
                    &hero.model.detail,
                    font(text::CAPTION, Weight::Regular),
                    pal.text_subtle,
                    room,
                );
                ui.painter().galley(
                    pos2(m_end + 8.0, y - dg.size().y / 2.0),
                    dg.clone(),
                    pal.text_subtle,
                );
                m_end += 8.0 + dg.size().x;
            }
            if hero.model.menu {
                let c = chevron(ui, cx, pos2(m_end + 12.0, y), "overview.hero.speed_menu");
                super::hero::speed_popover(&c, cx, out);
            }
        }
    }

    // 「+N」, after the caption: the folded tools and their routes.
    if g.folded > 0 {
        let caption_w = ui
            .painter()
            .layout_no_wrap(
                tr.t("overview.hero.caption"),
                font(text::SMALL + 1.0, Weight::Medium),
                pal.text_muted,
            )
            .size()
            .x;
        let y = inner.min.y + 10.0;
        let rest = &routes[g.shown..];
        let label = tr.f("overview.tools.more", &[("n", &rest.len())]);
        let names = rest
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>()
            .join(" · ");
        let gl = galley(
            ui,
            &label,
            font(text::CAPTION, Weight::Semibold),
            pal.text_muted,
            120.0,
        );
        let x = inner.min.x + caption_w + 12.0;
        let pill = Rect::from_min_size(pos2(x, y - 10.0), vec2(gl.size().x + 16.0, 20.0));
        let right = launch
            .as_ref()
            .map_or(inner.max.x, |(r, ..)| r.min.x - 12.0);
        let ng = galley(
            ui,
            &names,
            font(text::CAPTION, Weight::Regular),
            pal.text_subtle,
            right - pill.max.x - 10.0,
        );
        let tip = rest
            .iter()
            .map(|r| format!("{} — {}", r.name, route_text(cx, r)))
            .collect::<Vec<_>>()
            .join("\n");
        let area = Rect::from_min_max(pill.min, pos2(pill.max.x + 8.0 + ng.size().x, pill.max.y));
        let a11y = format!("{label} {names}");
        if cell(
            ui,
            cx,
            area.expand(2.0),
            ui.scope_id().with("overview-lane-more"),
            &a11y,
            &tip,
        ) {
            out.push(Action::Go(Page::Tools));
        }
        ui.painter()
            .rect_filled(pill, pill.height() / 2.0, pal.hover);
        ui.painter().galley(
            pos2(pill.center().x - gl.size().x / 2.0, y - gl.size().y / 2.0),
            gl,
            pal.text_muted,
        );
        ui.painter().galley(
            pos2(pill.max.x + 8.0, y - ng.size().y / 2.0),
            ng,
            pal.text_subtle,
        );
    }

    if let Some((r, button, action, tip)) = launch
        && ui.put(r, button).on_hover_text(tip).clicked()
    {
        out.push(action);
    }
}

/// "本地路由 · Krill · gpt-6-astra" — a tool's whole route in words.
pub fn route_text(cx: &Cx<'_>, r: &ToolRoute) -> String {
    let tr = cx.tr;
    let via = match r.via {
        Via::Router => tr.t("overview.hero.router"),
        Via::RouterOff => tr.t("overview.lanes.router_off"),
        Via::Direct => tr.t("overview.lanes.direct"),
    };
    let provider = r
        .provider
        .clone()
        .unwrap_or_else(|| tr.t("overview.lanes.no_provider"));
    let model = r
        .model
        .clone()
        .unwrap_or_else(|| tr.t("overview.hero.default_model"));
    let mut s = format!("{via} · {provider} · {model}");
    if r.uncounted {
        s.push('\n');
        s.push_str(&tr.t("overview.lanes.uncounted_tip"));
    }
    s
}
