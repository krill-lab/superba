//! Every coding tool on the Overview (user request 118): the tool identity the
//! cards share (the Tools page's monogram and name, so the tool icons that
//! replace the monograms there appear here too), the per-tool chips of the
//! 今日 card and the lanes of the request path ([`lanes`]).

use egui::{Align2, Rect, Sense, Ui, pos2, vec2};
use superba_core::harness::{HarnessId, HarnessRoute};
use superba_core::{ClaudeRoute, ProfileId, RequestHarness};

use super::usage::{Measure, Snapshot, ToolUsage};
use super::{Action, Cx, Page};
use crate::design::fonts::{Weight, font};
use crate::design::tokens::text;
use crate::design::widgets::{MonogramTile, focus_ring, hover_t, kpi, show_focus};
use crate::pages::tools::Selection;

/// The Tools page row of a tool (its name and monogram).
pub fn selection(h: RequestHarness) -> Option<Selection> {
    Some(match h {
        RequestHarness::Codex => Selection::Codex,
        RequestHarness::ClaudeCode => Selection::Claude,
        RequestHarness::Dsh => Selection::Tool(HarnessId::Dsh),
        RequestHarness::OpenCode => Selection::Tool(HarnessId::OpenCode),
        RequestHarness::Pi => Selection::Tool(HarnessId::Pi),
        RequestHarness::ZCode => Selection::Tool(HarnessId::ZCode),
        RequestHarness::WorkBuddy => Selection::Tool(HarnessId::WorkBuddy),
        RequestHarness::Gemini => Selection::Tool(HarnessId::Gemini),
        _ => return None,
    })
}

/// Display name of a tool ("Codex", "Claude Code", "Gemini CLI", "其他工具").
pub fn name(cx: &Cx<'_>, h: RequestHarness) -> String {
    match selection(h) {
        Some(s) => s.name().to_owned(),
        None => cx.tr.text(&h.text()),
    }
}

/// Two-letter monogram of a tool ("cx", "cc", "oc", "··"): the shared tool
/// label's identity, so the official icons follow the rest of the app.
pub fn monogram(h: RequestHarness) -> &'static str {
    crate::pages::shared::tool_label::monogram(h).unwrap_or("··")
}

/// The Tools page command that selects the tool.
pub fn tools_command(h: RequestHarness) -> Action {
    let command = match h {
        RequestHarness::Codex => "codex".to_owned(),
        RequestHarness::ClaudeCode => "claude".to_owned(),
        _ => match selection(h) {
            Some(Selection::Tool(id)) => id.as_str().to_owned(),
            _ => return Action::Go(Page::Tools),
        },
    };
    Action::PageCommand {
        page: Page::Tools,
        command,
    }
}

/// Side of a chip's monogram tile.
const CHIP_TILE: f32 = 18.0;
/// Height of the chip row.
pub const CHIP_ROW: f32 = 24.0;

/// A tool's value in the card's measure: its cost when the measure is cost and
/// the tool is priced; its tokens otherwise ("未定价" marks a cost that is
/// unknown, never zero).
pub fn chip_value(cx: &Cx<'_>, t: &ToolUsage, measure: Measure) -> String {
    let tokens = || {
        format!(
            "{} {}",
            kpi::si(t.tokens).text(),
            cx.tr.t("overview.unit.tokens")
        )
    };
    match (measure, t.cost()) {
        (Measure::Cost, Some(c)) => kpi::usd(c).text(),
        (Measure::Cost, None) if t.tokens > 0 => {
            format!("{} · {}", tokens(), cx.tr.t("overview.tools.unpriced"))
        }
        _ => tokens(),
    }
}

/// The 今日 card's per-tool chips: one per tool with use today, largest first,
/// as many as fit; the rest fold into 「+N」. Nothing with fewer than two tools.
pub fn chips(
    ui: &mut Ui,
    cx: &Cx<'_>,
    tools: &[ToolUsage],
    measure: Measure,
    out: &mut Vec<Action>,
) {
    if tools.len() < 2 {
        return;
    }
    let pal = cx.pal;
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), CHIP_ROW), Sense::hover());
    let total_req: u64 = tools.iter().map(|t| t.requests).sum::<u64>().max(1);
    let gap = 8.0;
    let mut x = row.min.x;
    for (i, t) in tools.iter().enumerate() {
        let label = name(cx, t.harness);
        let value = chip_value(cx, t, measure);
        let name_g = ui.painter().layout_no_wrap(
            label.clone(),
            font(text::CAPTION, Weight::Medium),
            pal.text_muted,
        );
        let value_g = ui.painter().layout_no_wrap(
            value.clone(),
            font(text::CAPTION, Weight::Semibold),
            pal.text,
        );
        let w = 4.0 + CHIP_TILE + 6.0 + name_g.size().x + 6.0 + value_g.size().x + 10.0;
        let rest = tools.len() - i;
        // Leave room for 「+N」 unless this is the last chip.
        let more_w = if rest > 1 { 40.0 } else { 0.0 };
        if x + w + more_w > row.max.x && i > 0 {
            more_chip(ui, cx, &tools[i..], measure, pos2(x, row.min.y), out);
            return;
        }
        let r = Rect::from_min_size(pos2(x, row.min.y), vec2(w.min(row.max.x - x), CHIP_ROW));
        let id = ui.scope_id().with(("overview-tool-chip", t.harness));
        let resp = ui.interact(r, id, Sense::click());
        let tip = cx.tr.f(
            "overview.tools.chip_tip",
            &[
                ("tool", &label),
                ("requests", &kpi::group(t.requests as i64)),
                (
                    "share",
                    &format!("{:.0}%", t.requests as f64 * 100.0 / total_req as f64),
                ),
                ("tokens", &kpi::si(t.tokens).text()),
                (
                    "cost",
                    &t.cost().map_or_else(
                        || cx.tr.t("overview.tools.unpriced"),
                        |c| kpi::usd(c).text(),
                    ),
                ),
            ],
        );
        let a11y = format!("{label} {value}");
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &a11y));
        let resp = resp
            .on_hover_text(tip)
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let ht = hover_t(ui, &resp);
        let p = ui.painter();
        p.rect_filled(
            r,
            r.height() / 2.0,
            pal.hover.gamma_multiply(0.6 + 0.4 * ht),
        );
        MonogramTile::new(monogram(t.harness))
            .size(CHIP_TILE)
            .paint_at(ui, pos2(r.min.x + 4.0 + CHIP_TILE / 2.0, r.center().y));
        let p = ui.painter();
        let nx = r.min.x + 4.0 + CHIP_TILE + 6.0;
        p.galley(
            pos2(nx, r.center().y - name_g.size().y / 2.0),
            name_g.clone(),
            pal.text_muted,
        );
        p.galley(
            pos2(
                nx + name_g.size().x + 6.0,
                r.center().y - value_g.size().y / 2.0,
            ),
            value_g,
            pal.text,
        );
        if show_focus(&resp) {
            focus_ring(ui.painter(), r, 11, pal);
        }
        if resp.clicked() {
            out.push(tools_command(t.harness));
        }
        x += w + gap;
    }
}

/// 「+N」 with the folded tools in its tooltip; a click opens Tools.
fn more_chip(
    ui: &mut Ui,
    cx: &Cx<'_>,
    rest: &[ToolUsage],
    measure: Measure,
    at: egui::Pos2,
    out: &mut Vec<Action>,
) {
    let pal = cx.pal;
    let label = format!("+{}", rest.len());
    let g = ui.painter().layout_no_wrap(
        label.clone(),
        font(text::CAPTION, Weight::Semibold),
        pal.text_muted,
    );
    let r = Rect::from_min_size(at, vec2(g.size().x + 16.0, CHIP_ROW));
    let tip = rest
        .iter()
        .map(|t| format!("{} — {}", name(cx, t.harness), chip_value(cx, t, measure)))
        .collect::<Vec<_>>()
        .join("\n");
    let resp = ui.interact(
        r,
        ui.scope_id().with("overview-tool-chip-more"),
        Sense::click(),
    );
    let a11y = cx.tr.f("overview.tools.more", &[("n", &rest.len())]);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &a11y));
    let resp = resp.on_hover_text(tip);
    ui.painter().rect_filled(r, r.height() / 2.0, pal.hover);
    ui.painter().text(
        r.center(),
        Align2::CENTER_CENTER,
        label,
        font(text::CAPTION, Weight::Semibold),
        pal.text_muted,
    );
    if show_focus(&resp) {
        focus_ring(ui.painter(), r, 11, pal);
    }
    if resp.clicked() {
        out.push(Action::Go(Page::Tools));
    }
}

// ---------------------------------------------------------------------------
// Routes per tool
// ---------------------------------------------------------------------------

/// How a tool's requests leave this computer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// Through the local router (running).
    Router,
    /// Set to go through the router while router mode is off: they fail.
    RouterOff,
    /// Straight to the provider.
    Direct,
}

/// One tool's route, as a lane of the request path.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolRoute {
    /// The tool.
    pub harness: RequestHarness,
    /// Name.
    pub name: String,
    /// How its requests leave.
    pub via: Via,
    /// Provider (· line), when known.
    pub provider: Option<String>,
    /// Default model, when known.
    pub model: Option<String>,
    /// Requests in the seven days (the lane order).
    pub week_requests: u64,
    /// Its direct requests are not in the numbers (no transcript Superba
    /// reads).
    pub uncounted: bool,
}

/// Name of a Superba provider by id.
fn provider_name(cx: &Cx<'_>, id: &ProfileId) -> String {
    let set = cx.data.profiles.value.as_ref();
    set.and_then(|s| s.profiles.iter().find(|p| &p.id == id))
        .map(|p| p.name.clone())
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| id.to_string())
}

/// Every tool with a route to draw: Codex (always, from the single-tool hero's
/// provider and model), Claude Code when Superba configured it, the other
/// tools when Superba applied a configuration to them; any tool with requests
/// in the seven days too. Order: through the router first, then most requests
/// in the week, Codex first on ties.
pub fn routes(
    cx: &Cx<'_>,
    snap: Option<&Snapshot>,
    codex_provider: &str,
    codex_model: &str,
) -> Vec<ToolRoute> {
    let router_on = cx.data.router_enabled();
    let week = |h: RequestHarness| {
        snap.and_then(|s| s.tools_week.iter().find(|t| t.harness == h))
            .map_or(0, |t| t.requests)
    };
    let via = |routed: bool| match (routed, router_on) {
        (true, true) => Via::Router,
        (true, false) => Via::RouterOff,
        _ => Via::Direct,
    };
    let mut out = vec![ToolRoute {
        harness: RequestHarness::Codex,
        name: "Codex".into(),
        via: if router_on && super::routed_addr(cx).is_some() {
            Via::Router
        } else {
            Via::Direct
        },
        provider: Some(codex_provider.to_owned()).filter(|p| !p.is_empty()),
        model: Some(codex_model.to_owned()).filter(|m| !m.is_empty()),
        week_requests: week(RequestHarness::Codex),
        uncounted: false,
    }];
    if let Some(c) = snap.and_then(|s| s.claude.as_ref()) {
        // Configured through Superba (applied, or routed), not merely installed:
        // an installed but unused Claude Code keeps the single-tool hero.
        let configured =
            c.applied.is_some() || cx.data.claude_routed() || c.config.route == ClaudeRoute::Router;
        if configured || week(RequestHarness::ClaudeCode) > 0 {
            let provider = c.provider.as_ref().map(|p| provider_name(cx, p));
            let model = c.provider.as_ref().and_then(|p| c.config.models_of(p).main);
            out.push(ToolRoute {
                harness: RequestHarness::ClaudeCode,
                name: "Claude Code".into(),
                via: via(cx.data.claude_routed() || c.config.route == ClaudeRoute::Router),
                provider,
                model,
                week_requests: week(RequestHarness::ClaudeCode),
                uncounted: false,
            });
        }
    }
    for h in snap.map(|s| s.harnesses.as_slice()).unwrap_or_default() {
        let harness = match h.harness {
            HarnessId::Gemini => RequestHarness::Gemini,
            HarnessId::ClaudeCode => continue,
            HarnessId::Dsh => RequestHarness::Dsh,
            HarnessId::OpenCode => RequestHarness::OpenCode,
            HarnessId::Pi => RequestHarness::Pi,
            HarnessId::ZCode => RequestHarness::ZCode,
            HarnessId::WorkBuddy => RequestHarness::WorkBuddy,
        };
        let w = week(harness);
        if !h.applied && w == 0 {
            continue;
        }
        let routed = h.settings.route == HarnessRoute::Routed;
        let provider = h
            .applied_provider
            .as_ref()
            .or(h.settings.provider.as_ref())
            .map(|p| provider_name(cx, p));
        out.push(ToolRoute {
            harness,
            name: h.harness.display_name().to_owned(),
            via: via(routed),
            provider,
            model: h
                .settings
                .default_model
                .clone()
                .or_else(|| h.settings.models.first().cloned()),
            week_requests: w,
            // Direct with no request seen: nothing Superba reads counts it.
            uncounted: !routed && w == 0,
        });
    }
    // Tools seen in the log that Superba does not configure (an unknown
    // client through the router).
    for t in snap.map(|s| s.tools_week.as_slice()).unwrap_or_default() {
        if t.requests > 0 && !out.iter().any(|r| r.harness == t.harness) {
            out.push(ToolRoute {
                harness: t.harness,
                name: name(cx, t.harness),
                via: if router_on { Via::Router } else { Via::Direct },
                provider: None,
                model: None,
                week_requests: t.requests,
                uncounted: false,
            });
        }
    }
    order(&mut out);
    out
}

/// Lane order: through the router first, then the most requests in the week,
/// Codex first on ties, then the tools' display order.
pub fn order(routes: &mut [ToolRoute]) {
    let pos = |h: RequestHarness| RequestHarness::ALL.iter().position(|x| *x == h);
    routes.sort_by(|a, b| {
        (a.via == Via::Direct)
            .cmp(&(b.via == Via::Direct))
            .then(b.week_requests.cmp(&a.week_requests))
            .then(pos(a.harness).cmp(&pos(b.harness)))
    });
}

/// How many lanes the request path shows in a hero `height` points tall, and
/// how many tools fold into 「+N」 (beside the caption).
pub fn visible_lanes(total: usize, height: f32) -> (usize, usize) {
    let cap = super::lanes::capacity(height);
    (total.min(cap), total.saturating_sub(cap))
}
