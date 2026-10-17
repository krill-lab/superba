//! The Overview's route hero (user request 27): where Codex's requests go, as a
//! path of nodes across the top of the page —
//! 本机 → 本地路由 → 供应商 · 线路 → 模型 · 速度档.
//!
//! - 本机: the Codex the desktop app runs and the CLI's version; remote hosts
//!   the app is connected to stack behind its tile ("+2").
//! - 本地路由: a node while router mode is on. Off, a dashed ghost sits above
//!   the line and a dashed detour shows the bypass; the line runs straight from
//!   本机 to the provider.
//! - The link into the provider carries the applied line's probe latency in its
//!   status colour.
//! - 供应商 · 线路 and 模型 · 速度档 have a chevron that opens a popover: switch
//!   provider or line; choose the speed tier.
//!
//! A click on a node opens its page (Updates, Router, Providers, Models).
//! Problems (an unhealthy line, a pin mismatch, keys in a plain file, ...) show
//! as a marker on the node they concern, with the details on hover; the 待办
//! card lists them with their fixes.

use egui::{
    Align2, Color32, CursorIcon, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2,
};
use superba_core::{AppCodexPin, AuthShape, Overview, SpeedEffect, SpeedTier};

use super::todo::{self, Spot, Todo};
use super::{Action, Cx, Page, caps_latency, latency_text};
use crate::design::backdrop::Backdrop;
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::icons::{self, Icon};
use crate::design::tokens::{radius, text};
use crate::design::widgets::{
    Latency, SpeedTiers, Status, Text, TierText, focus_ring, hover_t, show_focus,
};

/// Height of the hero card.
pub const HEIGHT: f32 = 148.0;

/// Side of a node's icon tile.
const TILE: f32 = 46.0;
/// How far the router's ghost rises above the line while router mode is off.
const GHOST_RISE: f32 = 26.0;
/// Width of the provider and speed popovers.
const POPOVER_W: f32 = 300.0;
/// Space between a tile and the link that leaves or enters it.
const LINK_GAP: f32 = 10.0;

/// One node of the path.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    /// Where it sits.
    pub spot: Spot,
    /// Icon in the tile.
    pub icon: Icon,
    /// Main line ("本机", "<provider> · <line>").
    pub title: String,
    /// Gray line under it.
    pub detail: String,
    /// Hover text (what it is, where a click goes).
    pub tip: String,
    /// Dashed and muted: off or not set up.
    pub ghost: bool,
    /// Problems on this node (errors and warnings, translated).
    pub problems: Vec<String>,
    /// Marker colour: the worst problem.
    pub marker: Option<Status>,
    /// A chevron opens a popover (provider and line, speed tier).
    pub menu: bool,
}

/// The whole path.
#[derive(Clone, Debug, PartialEq)]
pub struct Hero {
    /// 本机.
    pub this: Node,
    /// 本地路由 (a ghost while router mode is off).
    pub router: Node,
    /// Router mode is on (the router is a node on the path).
    pub router_on: bool,
    /// 供应商 · 线路.
    pub provider: Node,
    /// 模型 · 速度档.
    pub model: Node,
    /// Probe latency of the applied line (the link into the provider).
    pub latency: Latency,
    /// Remote hosts the desktop app is connected to (stacked on 本机).
    pub remotes: usize,
    /// When the line in use was last probed.
    pub probed: Option<superba_core::Timestamp>,
}

impl Hero {
    /// The nodes in path order.
    pub fn nodes(&self) -> [&Node; 4] {
        [&self.this, &self.router, &self.provider, &self.model]
    }
}

fn node(todos: &[Todo], spot: Spot, icon: Icon) -> Node {
    let problems = todo::markers(todos, spot);
    Node {
        spot,
        icon,
        title: String::new(),
        detail: String::new(),
        tip: String::new(),
        ghost: false,
        marker: todo::worst(&problems),
        problems: problems
            .iter()
            .map(|t| match t.detail.as_deref().filter(|d| !d.is_empty()) {
                Some(d) => format!("{}\n    {d}", t.title),
                None => t.title.clone(),
            })
            .collect(),
        menu: false,
    }
}

/// The Codex version the desktop app runs (the pin, else the running or
/// bundled one) and the CLI's version (first on PATH).
pub fn codex_versions(cx: &Cx<'_>, overview: &Overview) -> (Option<String>, Option<String>) {
    let section = cx.data.versions.value.as_ref().map(|v| &v.app_codex);
    let app = match section.map(|s| &s.pin) {
        Some(AppCodexPin::Version { version }) => Some(version.to_string()),
        _ => section
            .and_then(|s| s.effective_version.as_ref())
            .map(ToString::to_string)
            .or_else(|| {
                overview
                    .app
                    .installs
                    .first()
                    .and_then(|i| i.bundled_codex_version.as_ref())
                    .map(ToString::to_string)
            }),
    };
    let cli = overview
        .cli
        .binaries
        .iter()
        .find(|b| b.path_rank == Some(0))
        .or(overview.cli.binaries.first())
        .and_then(|b| b.version.as_ref())
        .map(ToString::to_string);
    (app, cli)
}

/// Build the path from the shell's data, the dashboard's snapshot and the
/// to-do list (problem markers).
pub fn model(
    cx: &Cx<'_>,
    overview: &Overview,
    todos: &[Todo],
    snap: Option<&super::usage::Snapshot>,
) -> Hero {
    let tr = cx.tr;
    let data = cx.data;

    // 本机.
    let mut this = node(todos, Spot::ThisComputer, Icon::AppWindow);
    this.title = tr.t("overview.hero.this");
    let (app, cli) = codex_versions(cx, overview);
    this.detail = match (&app, &cli) {
        (Some(a), Some(c)) => tr.f("overview.hero.app_cli", &[("app", a), ("cli", c)]),
        (Some(a), None) => tr.f("overview.hero.app_only", &[("app", a)]),
        (None, Some(c)) => tr.f("overview.hero.cli_only", &[("cli", c)]),
        (None, None) => tr.t("overview.hero.no_codex"),
    };
    // The remote SSH hosts the desktop app knows, and how many are connected
    // now; hosts Superba does not manage (remote-control devices, WSL
    // distributions; user bug 36) are named apart and not stacked on the tile.
    let hosts: Vec<&superba_core::RemoteHost> = match data.hosts.value.as_ref() {
        Some(hosts) => hosts.iter().map(|h| &h.host).collect(),
        None => overview.remotes.iter().map(|r| &r.host).collect(),
    };
    let hosts: Vec<&superba_core::RemoteHost> = hosts
        .into_iter()
        .filter(|h| h.is_app_known() && h.is_listed())
        .collect();
    let remotes = hosts.iter().filter(|h| h.is_managed()).count();
    let live = hosts
        .iter()
        .filter(|h| h.is_managed() && h.connected == Some(true))
        .count();
    let read_only: Vec<String> = hosts
        .iter()
        .filter(|h| !h.is_managed())
        .map(|h| crate::pages::shared::hosts::title(tr, h))
        .collect();
    let (_, hot) = cx.hot_status(&overview.app.hot_switch);
    let mut tip = vec![tr.f("overview.hero.this_tip", &[("app", &hot)])];
    if remotes > 0 {
        tip.push(tr.f("overview.hero.remotes", &[("n", &remotes), ("live", &live)]));
        let unsynced = data.unsynced_hosts();
        if unsynced > 0 {
            tip.push(tr.f("overview.hero.unsynced", &[("n", &unsynced)]));
        }
    }
    if !read_only.is_empty() {
        tip.push(tr.f(
            "overview.hero.read_only",
            &[("hosts", &read_only.join(", "))],
        ));
    }
    tip.push(tr.t("overview.hero.open_updates"));
    this.tip = tip.join("\n");

    // 本地路由.
    let router_on = data.router_enabled();
    let mut router = node(todos, Spot::Router, Icon::Route);
    router.title = tr.t("overview.hero.router");
    if router_on {
        router.detail = match super::routed_addr(cx) {
            Some(addr) => addr,
            None => tr.t("overview.hero.router_not_routed"),
        };
        router.tip = format!(
            "{}\n{}",
            tr.t("overview.hero.router_on_tip"),
            tr.t("overview.hero.open_router")
        );
    } else {
        router.ghost = true;
        router.detail = tr.t("overview.hero.router_off");
        router.tip = format!(
            "{}\n{}",
            tr.t("overview.hero.router_off_tip"),
            tr.t("overview.hero.open_router")
        );
    }

    // 供应商 · 线路.
    let mut provider = node(todos, Spot::Provider, Icon::Cloud);
    let site = data.active_site();
    let profile = data.active_profile();
    let active = data.active_state();
    if data.official_active() {
        provider.title = tr.t("overview.official_chip");
        provider.detail = overview
            .config
            .as_ref()
            .and_then(|c| match &c.auth {
                AuthShape::ChatGpt { email, .. } => email.clone(),
                _ => None,
            })
            .unwrap_or_else(|| tr.t("overview.account.official"));
        provider.menu = !data.providers().is_empty();
    } else if let (Some(p), Some(s)) = (profile, &site) {
        let name = crate::app::provider_name(tr, p, s);
        let line = super::lines::effective(cx, snap)
            .filter(|_| s.lines.len() > 1)
            .map(|r| cx.tr.text(&r.line.name));
        provider.title = match line {
            Some(l) => format!("{name} · {l}"),
            None => name,
        };
        let auth = active.and_then(|a| a.auth_mode).map(|m| cx.auth_short(m));
        let balance = super::balance(cx);
        provider.detail = [auth, balance]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        provider.menu = true;
    } else {
        provider.ghost = true;
        provider.title = tr.t("overview.hero.no_provider");
        provider.detail = tr.t("overview.hero.no_provider_detail");
        provider.menu = !data.providers().is_empty();
    }
    provider.tip = tr.t(if provider.ghost && data.providers().is_empty() {
        "overview.hero.start_wizard"
    } else {
        "overview.hero.open_providers"
    });
    let in_use = super::lines::effective(cx, snap);
    let latency = in_use.as_ref().map_or(Latency::Unknown, |r| r.latency);
    let probed = in_use
        .as_ref()
        .and_then(|r| data.site_caps(data.active_site_id()?, &r.line.id))
        .map(|c| c.checked_at)
        .filter(|t| *t > superba_core::Timestamp::EPOCH);

    // 模型 · 速度档.
    let mut model = node(todos, Spot::Model, Icon::Model);
    // config.toml names no model: Codex uses its default, which the newest
    // request shows.
    let configured = overview.config.as_ref().and_then(|c| c.model.clone());
    let used = snap
        .and_then(|s| s.sessions.first())
        .and_then(|s| s.model.clone());
    model.title = match (&configured, &used) {
        (Some(m), _) | (None, Some(m)) => m.clone(),
        (None, None) => tr.t("overview.hero.default_model"),
    };
    match super::speed_support(cx) {
        Some((p, support)) => {
            let tier = p.model.speed.unwrap_or_default();
            model.detail = tr.f("overview.hero.tier", &[("tier", &tr.text(&tier.text()))]);
            model.menu = true;
            if tier != SpeedTier::Standard && support.is_some_and(|s| !s.offered.contains(&tier)) {
                model.problems.push(tr.t("overview.speed.not_offered"));
                model.marker = Some(model.marker.unwrap_or(Status::Warn));
            }
        }
        None if data.official_active() => model.detail = tr.t("overview.hero.tier_app"),
        None => model.detail = tr.t("overview.hero.tier_none"),
    }
    model.ghost = active.is_none();
    model.tip = match (&configured, &used) {
        (None, Some(m)) => format!(
            "{}\n{}",
            tr.f("overview.hero.model_default_tip", &[("model", m)]),
            tr.t("overview.hero.open_models")
        ),
        _ => tr.t("overview.hero.open_models"),
    };

    Hero {
        this,
        router,
        router_on,
        provider,
        model,
        latency,
        remotes,
        probed,
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// A rounded rectangle outline made of dashes.
fn dashed_rrect(painter: &egui::Painter, rect: Rect, r: f32, stroke: Stroke) {
    let mut pts = Vec::new();
    let corners = [
        (pos2(rect.max.x - r, rect.min.y + r), -90.0f32),
        (pos2(rect.max.x - r, rect.max.y - r), 0.0),
        (pos2(rect.min.x + r, rect.max.y - r), 90.0),
        (pos2(rect.min.x + r, rect.min.y + r), 180.0),
    ];
    pts.push(pos2(rect.min.x + r, rect.min.y));
    for (c, a0) in corners {
        for k in 0..=6 {
            let a = (a0 + 90.0 * k as f32 / 6.0).to_radians();
            pts.push(c + vec2(a.cos(), a.sin()) * r);
        }
    }
    pts.push(pos2(rect.min.x + r, rect.min.y));
    painter.extend(Shape::dashed_line(&pts, stroke, 4.0, 3.0));
}

/// Points of a cubic Bézier curve.
fn bezier(p0: Pos2, p1: Pos2, p2: Pos2, p3: Pos2) -> Vec<Pos2> {
    (0..=16)
        .map(|i| {
            let t = i as f32 / 16.0;
            let u = 1.0 - t;
            let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
            pos2(
                w[0] * p0.x + w[1] * p1.x + w[2] * p2.x + w[3] * p3.x,
                w[0] * p0.y + w[1] * p1.y + w[2] * p2.y + w[3] * p3.y,
            )
        })
        .collect()
}

/// A one-line galley no wider than `max`.
fn line_galley(
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
    ui.painter().layout_job(job)
}

/// Paint the problem marker on a tile's top-right corner: a status disc with an
/// exclamation mark drawn as a bar and a dot (no glyph).
fn marker(ui: &Ui, tile: Rect, status: Status, ring: Color32) {
    let pal = crate::design::Palette::of(ui.ctx());
    let c = pos2(tile.max.x - 2.0, tile.min.y + 2.0);
    let p = ui.painter();
    p.circle_filled(c, 9.0, ring);
    p.circle_filled(c, 7.0, pal.status(status));
    let white = Color32::WHITE;
    p.line_segment(
        [c + vec2(0.0, -3.6), c + vec2(0.0, 0.6)],
        Stroke::new(1.6, white),
    );
    p.circle_filled(c + vec2(0.0, 3.2), 0.95, white);
}

/// What one node's drawing returned.
struct Drawn {
    /// Node clicked (open its page).
    clicked: bool,
    /// The chevron, when the node has a menu.
    chevron: Option<egui::Response>,
}

/// Draw one node centred on `center_x`, its tile centred on `tile_y`.
#[allow(clippy::too_many_arguments)]
fn draw_node(
    ui: &mut Ui,
    cx: &Cx<'_>,
    n: &Node,
    center_x: f32,
    tile_y: f32,
    label_y: f32,
    col_w: f32,
    tint: Color32,
    remotes: usize,
) -> Drawn {
    let pal = cx.pal;
    let b = Backdrop::of(ui.ctx());
    let tile = Rect::from_center_size(pos2(center_x, tile_y), vec2(TILE, TILE));
    let max_text = (col_w - 28.0).max(60.0);
    let chev_w = if n.menu { 20.0 } else { 0.0 };
    let title_color = if n.ghost { pal.text_muted } else { pal.text };
    let title = line_galley(
        ui,
        &n.title,
        font(text::BODY + 2.0, Weight::Semibold),
        title_color,
        max_text - chev_w,
    );
    let detail = line_galley(
        ui,
        &n.detail,
        font(text::CAPTION, Weight::Regular),
        pal.text_subtle,
        max_text,
    );
    let group_w = title.size().x + chev_w;
    let label_w = group_w.max(detail.size().x).max(TILE);
    let area = Rect::from_min_max(
        pos2(center_x - label_w / 2.0 - 12.0, tile.min.y - 8.0),
        pos2(center_x + label_w / 2.0 + 12.0, label_y + 30.0),
    );
    let id = ui.scope_id().with(("overview-hero-node", n.spot));
    let resp = ui
        .interact(area, id, Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand);
    let tip = if n.problems.is_empty() {
        n.tip.clone()
    } else {
        format!("{}\n\n{}", n.problems.join("\n"), n.tip)
    };
    let resp = resp.on_hover_text(tip);
    let label = n.title.clone();
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
    let t = hover_t(ui, &resp);
    if t > 0.0 {
        ui.painter().rect_filled(
            area,
            radius::CARD,
            Color32::TRANSPARENT.lerp_to_gamma(b.hover, t),
        );
    }
    let p = ui.painter();
    // Remote hosts: cards stacked behind the tile.
    if remotes > 0 && !n.ghost {
        for k in [2.0f32, 1.0] {
            let r = tile.translate(vec2(5.0 * k, -5.0 * k));
            p.rect(
                r,
                13,
                b.card,
                Stroke::new(1.0, pal.border_strong),
                StrokeKind::Inside,
            );
        }
    }
    if n.ghost {
        p.rect_filled(tile, 13, b.card);
        dashed_rrect(
            p,
            tile.shrink(0.5),
            12.5,
            Stroke::new(1.2, pal.border_strong),
        );
        icons::paint(
            p,
            Rect::from_center_size(tile.center(), vec2(TILE * 0.52, TILE * 0.52)),
            n.icon,
            pal.text_subtle,
        );
    } else {
        p.rect_filled(tile, 13, b.card);
        crate::design::widgets::list_row::icon_tile(ui, tile.center(), TILE, n.icon, Some(tint));
    }
    if remotes > 0 && !n.ghost {
        let s = format!("+{remotes}");
        let g = ui.painter().layout_no_wrap(
            s,
            font(text::CAPTION - 1.0, Weight::Semibold),
            pal.on_primary,
        );
        let w = g.size().x + 10.0;
        let pill = Rect::from_center_size(
            pos2(tile.max.x - 1.0, tile.max.y - 2.0),
            vec2(w.max(18.0), 16.0),
        );
        let p = ui.painter();
        p.rect_filled(pill.expand(2.0), radius::PILL, b.card);
        p.rect_filled(pill, radius::PILL, pal.text_muted);
        p.galley(
            pos2(
                pill.center().x - g.size().x / 2.0,
                pill.center().y - g.size().y / 2.0,
            ),
            g,
            pal.on_primary,
        );
    }
    if let Some(status) = n.marker {
        marker(ui, tile, status, b.card);
    }
    // Labels.
    let p = ui.painter();
    let tx = center_x - group_w / 2.0;
    let th = title.size().y;
    let tw = title.size().x;
    p.galley(pos2(tx, label_y - th / 2.0), title, title_color);
    let dh = detail.size().y;
    let dw = detail.size().x;
    p.galley(
        pos2(center_x - dw / 2.0, label_y + 20.0 - dh / 2.0),
        detail,
        pal.text_subtle,
    );
    if show_focus(&resp) {
        focus_ring(ui.painter(), area, radius::CARD, pal);
    }
    let chevron = n.menu.then(|| {
        let r = Rect::from_center_size(pos2(tx + tw + 12.0, label_y), vec2(20.0, 20.0));
        let cid = ui.scope_id().with(("overview-hero-menu", n.spot));
        let c = ui
            .interact(r, cid, Sense::click())
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text(cx.tr.t(match n.spot {
                Spot::Model => "overview.hero.speed_menu",
                _ => "overview.hero.switch_menu",
            }));
        let label = cx.tr.t("overview.hero.switch_menu");
        c.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
        let ct = hover_t(ui, &c);
        let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&c));
        let fill = if open {
            b.selected
        } else {
            Color32::TRANSPARENT.lerp_to_gamma(b.selected, ct)
        };
        ui.painter().rect_filled(r, radius::INNER, fill);
        icons::paint(
            ui.painter(),
            r.shrink(4.0),
            Icon::ChevronDown,
            pal.text_muted.lerp_to_gamma(pal.text, ct),
        );
        if show_focus(&c) {
            focus_ring(ui.painter(), r, radius::INNER, pal);
        }
        c
    });
    Drawn {
        clicked: resp.clicked(),
        chevron,
    }
}

/// A link between two tiles: a line with an arrowhead at its end.
fn link(ui: &Ui, from: f32, to: f32, y: f32, color: Color32) {
    let p = ui.painter();
    if to - from < 12.0 {
        return;
    }
    p.line_segment([pos2(from, y), pos2(to, y)], Stroke::new(2.0, color));
    let s = Stroke::new(2.0, color);
    p.line_segment([pos2(to - 6.0, y - 4.5), pos2(to, y)], s);
    p.line_segment([pos2(to - 6.0, y + 4.5), pos2(to, y)], s);
}

/// The latency pill on the link into the provider; returns its rectangle.
fn latency_pill(ui: &Ui, cx: &Cx<'_>, center: Pos2, latency: Latency) -> Rect {
    let pal = cx.pal;
    let b = Backdrop::of(ui.ctx());
    let s = latency_text(cx, latency);
    let g = ui
        .painter()
        .layout_no_wrap(s, mono_font(text::CAPTION, Weight::Medium), pal.text);
    let w = g.size().x + 26.0;
    let r = Rect::from_center_size(center, vec2(w, 22.0));
    let p = ui.painter();
    p.rect(
        r,
        radius::PILL,
        b.card,
        Stroke::new(1.0, pal.border),
        StrokeKind::Inside,
    );
    let dot = pos2(r.min.x + 11.0, r.center().y);
    if latency == Latency::Probing {
        icons::spinner(
            p,
            Rect::from_center_size(dot, vec2(10.0, 10.0)),
            pal.text_muted,
            ui.input(|i| i.time),
        );
        ui.ctx().request_repaint();
    } else {
        p.circle_filled(dot, 3.5, pal.status(latency.status()));
    }
    p.galley(
        pos2(r.min.x + 19.0, r.center().y - g.size().y / 2.0),
        g,
        pal.text,
    );
    r
}

/// Draw the hero card in `rect`.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, hero: &Hero, rect: Rect, out: &mut Vec<Action>) {
    let pal = cx.pal;
    let b = Backdrop::of(ui.ctx());
    crate::design::backdrop::panel(ui.painter(), rect, &b);
    let inner = rect.shrink2(vec2(20.0, 14.0));
    ui.painter().text(
        pos2(inner.min.x, inner.min.y + 10.0),
        Align2::LEFT_CENTER,
        cx.tr.t("overview.hero.caption"),
        font(text::SMALL + 1.0, Weight::Medium),
        pal.text_muted,
    );
    // When the line in use was last probed.
    if let Some(at) = hero.probed.filter(|_| !hero.provider.ghost) {
        ui.painter().text(
            pos2(inner.max.x, inner.min.y + 10.0),
            Align2::RIGHT_CENTER,
            cx.tr.f("overview.hero.probed", &[("ago", &cx.ago(at))]),
            font(text::CAPTION, Weight::Regular),
            pal.text_subtle,
        );
    }
    let col_w = inner.width() / 4.0;
    let cx_of = |i: usize| inner.min.x + col_w * (i as f32 + 0.5);
    // The block (tiles and two label lines, 98 px) sits below the caption,
    // centred in what is left; the router's ghost rises into the space above.
    let block_top = inner.min.y + 22.0 + ((inner.height() - 22.0 - 98.0) / 2.0).max(0.0);
    let tile_y = block_top + 4.0 + TILE / 2.0;
    let label_y = tile_y + TILE / 2.0 + 15.0;
    let neutral = pal.border_strong;
    let lat_color = match hero.latency {
        Latency::Ms(_) | Latency::Unreachable => {
            pal.status(hero.latency.status()).gamma_multiply(0.8)
        }
        _ => neutral,
    };
    let half = TILE / 2.0 + LINK_GAP;
    // Links first, nodes on top.
    if hero.router_on {
        link(ui, cx_of(0) + half, cx_of(1) - half, tile_y, neutral);
        link(ui, cx_of(1) + half, cx_of(2) - half, tile_y, lat_color);
    } else {
        // Straight from this computer to the provider; the router is a dashed
        // detour above the line.
        link(ui, cx_of(0) + half, cx_of(2) - half, tile_y, lat_color);
        let gy = tile_y - GHOST_RISE;
        let dash = Stroke::new(1.4, pal.border_strong);
        let (l, r) = (cx_of(1) - TILE / 2.0, cx_of(1) + TILE / 2.0);
        let up = bezier(
            pos2(l - 34.0, tile_y),
            pos2(l - 14.0, tile_y),
            pos2(l - 14.0, gy),
            pos2(l, gy),
        );
        let down = bezier(
            pos2(r, gy),
            pos2(r + 14.0, gy),
            pos2(r + 14.0, tile_y),
            pos2(r + 34.0, tile_y),
        );
        ui.painter().extend(Shape::dashed_line(&up, dash, 4.0, 3.0));
        ui.painter()
            .extend(Shape::dashed_line(&down, dash, 4.0, 3.0));
    }
    link(ui, cx_of(2) + half, cx_of(3) - half, tile_y, neutral);
    // The latency on the link into the provider.
    let pill_x = if hero.router_on {
        (cx_of(1) + cx_of(2)) / 2.0
    } else {
        (cx_of(1) + TILE / 2.0 + 34.0 + cx_of(2) - half) / 2.0
    };
    if !hero.provider.ghost && !cx.data.official_active() {
        let r = latency_pill(ui, cx, pos2(pill_x, tile_y), hero.latency);
        ui.interact(
            r,
            ui.scope_id().with("overview-hero-latency"),
            Sense::hover(),
        )
        .on_hover_text(cx.tr.t("overview.hero.latency_tip"));
    }

    let tints = [b.chart[0], b.chart[1], b.chart[0], b.chart[1]];
    let mut drawn = Vec::new();
    for (i, n) in hero.nodes().into_iter().enumerate() {
        let ty = if i == 1 && !hero.router_on {
            tile_y - GHOST_RISE
        } else {
            tile_y
        };
        let remotes = if i == 0 { hero.remotes } else { 0 };
        drawn.push(draw_node(
            ui,
            cx,
            n,
            cx_of(i),
            ty,
            label_y,
            col_w,
            tints[i],
            remotes,
        ));
    }
    for (i, d) in drawn.iter().enumerate() {
        if d.clicked {
            out.push(match i {
                0 => Action::Go(Page::Updates),
                1 => Action::Go(Page::Router),
                2 if hero.provider.ghost && cx.data.providers().is_empty() => Action::StartWizard,
                2 => Action::Go(Page::Providers),
                _ => Action::Go(Page::Models),
            });
        }
    }
    if let Some(c) = &drawn[2].chevron {
        provider_popover(c, cx, out);
    }
    if let Some(c) = &drawn[3].chevron {
        speed_popover(c, cx, out);
    }
}

/// The provider node's popover: the applied provider's lines with their
/// latency (a click plans the switch), the other providers, adding one and the
/// Providers page.
fn provider_popover(response: &egui::Response, cx: &Cx<'_>, out: &mut Vec<Action>) {
    egui::Popup::from_toggle_button_response(response)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
        .show(|ui| {
            // A fixed width: rows fill it instead of the popup growing to them.
            ui.set_width(POPOVER_W);
            let account = super::account_lines(cx);
            if !account.is_empty() {
                ui.label(Text::caption(cx.tr.t("overview.hero.account"), cx.pal));
                for line in account {
                    ui.label(Text::small(line, cx.pal).color(cx.pal.text));
                }
                ui.separator();
            }
            let row = |ui: &mut Ui, enabled: bool, selected: bool, label: String, right: String| {
                ui.add_enabled(
                    enabled,
                    egui::Button::selectable(selected, label)
                        .right_text(Text::small(right, cx.pal))
                        .min_size(vec2(ui.available_width(), 28.0)),
                )
                .clicked()
            };
            if let (Some(site), Some(profile)) = (cx.data.active_site(), cx.data.active_profile())
                && site.lines.len() > 1
            {
                let active = cx.data.active_state().and_then(|a| a.line_id.clone());
                ui.label(Text::caption(
                    cx.tr.f(
                        "overview.lines_of",
                        &[(
                            "provider",
                            &crate::app::provider_name(cx.tr, profile, &site),
                        )],
                    ),
                    cx.pal,
                ));
                for line in crate::pages::shared::ordered_lines(&site, Some(profile)) {
                    let latency = caps_latency(
                        cx.data.site_caps(&site.id, &line.id),
                        cx.data.probing_site(&site.id),
                    );
                    let selected = active.as_ref() == Some(&line.id);
                    if row(
                        ui,
                        !cx.busy && !selected,
                        selected,
                        cx.tr.text(&line.name),
                        latency_text(cx, latency),
                    ) {
                        out.push(crate::pages::shared::switch_action(
                            cx.data, profile, &line.id,
                        ));
                    }
                }
                ui.separator();
            }
            ui.label(Text::caption(cx.tr.t("overview.other_providers"), cx.pal));
            let active = cx.data.active_profile().map(|p| p.id.clone());
            if !cx.data.official_active()
                && row(
                    ui,
                    !cx.busy,
                    false,
                    cx.tr.t("overview.official_chip"),
                    String::new(),
                )
            {
                out.push(Action::Restore(superba_core::RestoreTarget::Official));
            }
            for profile in cx.data.providers() {
                if Some(&profile.id) == active.as_ref() {
                    continue;
                }
                let site = cx.data.site(&profile.site_id);
                let name = site.as_ref().map_or_else(
                    || profile.name.clone(),
                    |s| crate::app::provider_name(cx.tr, profile, s),
                );
                let host = site
                    .as_ref()
                    .filter(|s| s.lines.len() == 1)
                    .and_then(|s| s.default_line())
                    .map(|l| l.origin.host().to_owned())
                    .unwrap_or_default();
                if row(ui, !cx.busy, false, name, host) {
                    out.push(Action::Switch {
                        profile: profile.id.clone(),
                        line: None,
                        auth: None,
                    });
                }
            }
            ui.separator();
            if row(
                ui,
                true,
                false,
                cx.tr.t("overview.add_provider"),
                String::new(),
            ) {
                out.push(Action::PageCommand {
                    page: Page::Providers,
                    command: "add".into(),
                });
            }
            if row(
                ui,
                true,
                false,
                cx.tr.t("overview.manage_providers"),
                String::new(),
            ) {
                out.push(Action::Go(Page::Providers));
            }
        });
}

/// The model node's popover: the speed tier (the tiers some listed model
/// offers; a change plans the hot restart that applies it), where it applies,
/// and the Models page.
fn speed_popover(response: &egui::Response, cx: &Cx<'_>, out: &mut Vec<Action>) {
    let Some((profile, support)) = super::speed_support(cx) else {
        return;
    };
    egui::Popup::from_toggle_button_response(response)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_width(POPOVER_W);
            // Room for the speed control's outside focus ring.
            ui.add_space(4.0);
            ui.label(Text::caption(cx.tr.t("overview.speed.label"), cx.pal));
            ui.add_space(2.0);
            let current = profile.model.speed.unwrap_or_default();
            let offered: Vec<SpeedTier> =
                support.map_or_else(|| vec![SpeedTier::Standard], |s| s.offered.clone());
            let mut value = current;
            let mut control = SpeedTiers::new("overview-speed", &mut value, &offered)
                .small()
                .enabled(!cx.busy && support.is_some())
                .not_offered(cx.tr.t("overview.speed.not_offered"));
            for tier in SpeedTier::ALL {
                let tip = (tier != SpeedTier::Standard).then(|| {
                    let billing = cx.tr.t("overview.speed.billing_note");
                    // The model-name caveat only where the tier is given that way.
                    let by_name = support.is_some_and(|s| {
                        let (cli, app) = s.effect(tier);
                        matches!(cli, SpeedEffect::ViaModelName { .. })
                            || matches!(app, SpeedEffect::ViaModelName { .. })
                    });
                    if by_name {
                        format!("{billing}\n{}", cx.tr.t("overview.speed.model_change"))
                    } else {
                        billing
                    }
                });
                control = control.text(
                    tier,
                    TierText {
                        label: cx.tr.text(&tier.text()),
                        tooltip: tip,
                    },
                );
            }
            let response = ui
                .horizontal(|ui| {
                    ui.add_space(4.0);
                    control.show(ui)
                })
                .inner;
            if offered == [SpeedTier::Standard] {
                response.on_hover_text(cx.tr.t("overview.speed.none"));
            } else if value != current {
                out.push(Action::SetSpeed {
                    profile: profile.id.clone(),
                    tier: value,
                });
            }
            if let Some(support) = support
                && let Some(line) = super::speed_effect_line(cx, current, support)
            {
                ui.add_space(4.0);
                ui.add(egui::Label::new(Text::small(line, cx.pal)).wrap());
            }
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(2.0);
            if crate::design::widgets::link_button(
                ui,
                &cx.tr.t("overview.hero.open_models_short"),
                false,
            )
            .clicked()
            {
                out.push(Action::Go(Page::Models));
            }
            ui.add_space(2.0);
        });
}
