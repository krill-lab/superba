//! Overview (user request 27, 118): an original composition around what
//! Superba is for — where every coding tool's traffic goes and what it costs.
//!
//! - With one tool (Codex) the route hero below; with several, one lane per
//!   tool ([`lanes`], [`tools`]): tool → 本地路由 | 直连 → provider → model, the
//!   rarely used tools folded into 「+N」.
//! - 今日, the heat-strip and 最近会话 count every tool in the request log
//!   (router, Codex rollouts, Claude Code transcripts); 今日 adds a chip per
//!   tool.
//!
//! - A route hero across the top ([`hero`]): 本机 → 本地路由 → 供应商 · 线路 →
//!   模型 · 速度档, the latency on the link into the provider, problem markers
//!   on the nodes they concern, popovers for switching provider or line and for
//!   the speed tier; a click on a node opens its page.
//! - Below, an asymmetric bento grid ([`cards`]): 今日 (cost, delta against
//!   yesterday, token split, 7 × 24 heat-strip) and 最近会话 on the wide left
//!   column; 线路 (latency, 30-minute trace, failover order, switch, probe) and
//!   待办 (actionable checklist) on the narrow right one.
//!
//! Generic: every provider is a profile; preset- or site-specific pieces appear
//! only through the site's capabilities (lines, PAT, billing endpoints). Data
//! comes from the service: line probes, the request log (router log merged with
//! Codex's rollouts, [`usage`]), session titles, prices (in the log's costs),
//! versions, diagnostics and the router's status. With nothing applied and no
//! provider the left column offers the setup wizard.

mod cards;
pub mod hero;
pub mod lanes;
mod lines;
mod sessions;
mod simple;
pub mod simple_speed;
mod today;
pub mod todo;
pub mod tools;
pub mod usage;

use std::time::{Duration, Instant};

use egui::{Align, Layout, Rect, Sense, Ui, vec2};
use superba_core::{
    BillingSummary, LineCaps, Profile, RestoreTarget, Site, SpeedSupport, SpeedTier, catalog,
};

use super::{Action, Cx, Page, PageStatus, empty_state, error_state};
use crate::design::backdrop::Backdrop;
use crate::design::icons::Icon;
use crate::design::widgets::{Button, Latency, Text};
use crate::runtime::ReadTask;
pub use cards::{GAP, Grid, layout, layout_with};
pub use lines::LineRow;
pub use sessions::{host_label, usage_command};
pub use simple::show as show_simple;
pub use today::{delta_text, figure};
use usage::Snapshot;

/// How often the dashboard re-reads the request log and the router while it
/// is shown.
const RELOAD: Duration = Duration::from_secs(20);

/// Height of the title row.
pub const TITLE_ROW: f32 = 40.0;

/// Height of the title, its spacing and the dashboard's minimum 600-point grid.
pub const MIN_HEIGHT: f32 = TITLE_ROW + 16.0 + 600.0;

/// Page state: the dismissed setup card and the dashboard's data.
#[derive(Debug, Default)]
pub struct State {
    /// The user chose "I already have a configuration" on the setup card.
    pub empty_dismissed: bool,
    /// The request log and router snapshot.
    pub usage: UsageLoader,
    /// Codex-only dashboard data for simple mode.
    pub simple_usage: UsageLoader,
    /// Diagnostics were asked for once (findings feed 待办 and the markers).
    doctor_asked: bool,
}

/// The dashboard's background read.
#[derive(Debug, Default)]
pub struct UsageLoader {
    task: Option<ReadTask<Snapshot>>,
    /// Last snapshot.
    pub value: Option<Snapshot>,
    /// Last error.
    pub error: Option<superba_core::Error>,
    at: Option<Instant>,
}

impl UsageLoader {
    /// A loader that already holds `snapshot` and reads again only after
    /// [`RELOAD`] (a page shown right after a read; tests).
    pub fn preloaded(snapshot: Snapshot) -> Self {
        Self {
            task: None,
            value: Some(snapshot),
            error: None,
            at: Some(Instant::now()),
        }
    }

    /// Poll a running read and start one when none ran for [`RELOAD`].
    fn tick(&mut self, ui: &Ui, cx: &Cx<'_>) {
        self.tick_for(ui, cx, false);
    }

    fn tick_for(&mut self, ui: &Ui, cx: &Cx<'_>, codex_only: bool) {
        if let Some(task) = &self.task {
            match task.poll() {
                Some(result) => {
                    self.task = None;
                    self.at = Some(Instant::now());
                    match result {
                        Ok(v) => {
                            self.value = Some(v);
                            self.error = None;
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
                None => ui.ctx().request_repaint_after(Duration::from_millis(100)),
            }
        }
        if self.task.is_none() && self.at.is_none_or(|t| t.elapsed() >= RELOAD) {
            let service = cx.service.clone();
            let now = cx.now;
            let offset = super::local_offset_secs(now).unwrap_or(0);
            self.task = Some(crate::runtime::task(async move {
                if codex_only {
                    usage::load_codex(service, now, offset).await
                } else {
                    usage::load(service, now, offset).await
                }
            }));
            // A failed or empty read is retried on the next interval.
            self.at = Some(Instant::now());
        }
        ui.ctx().request_repaint_after(RELOAD);
    }
}

/// Nothing pending, no dialog, no badge.
pub fn status(_state: &State) -> PageStatus {
    PageStatus::default()
}

/// Commands: `wizard`, `restore:original`, `restore:official`, `refresh`.
pub fn command(state: &mut State, _cx: &Cx<'_>, command: &str, out: &mut Vec<Action>) {
    match command {
        "wizard" => out.push(Action::StartWizard),
        "restore:original" => out.push(Action::restore_original()),
        "restore:official" => out.push(Action::Restore(RestoreTarget::Official)),
        "refresh" => {
            state.usage.at = None;
            state.simple_usage.at = None;
        }
        other => tracing::debug!(command = other, "unknown overview command"),
    }
}

/// Draw the overview page.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    state.usage.tick(ui, cx);
    if !state.doctor_asked {
        state.doctor_asked = true;
        if cx.data.doctor.value.is_none() && !cx.data.doctor.loading {
            out.push(Action::RunDoctor);
        }
    }
    title_row(ui, cx);
    ui.add_space(8.0);

    let Some(overview) = cx.data.overview.value.as_ref() else {
        if let Some(e) = &cx.data.overview.error {
            error_state(ui, cx, &cx.tr.t("overview.load_failed"), e);
        } else {
            empty_state(
                ui,
                cx,
                Icon::Overview,
                &cx.tr.t("overview.loading"),
                &cx.tr.t("overview.loading_body"),
            );
        }
        return;
    };

    let snap = state.usage.value.as_ref();
    let todos = todo::todos(cx, snap);
    let hero_model = hero::model(cx, overview, &todos, snap);
    let routes = tools::routes(
        cx,
        snap,
        if hero_model.provider.ghost {
            ""
        } else {
            &hero_model.provider.title
        },
        if hero_model.model.ghost {
            ""
        } else {
            &hero_model.model.title
        },
    );
    let area = ui.available_rect_before_wrap();
    // One tool: the single-tool hero; several: one lane per tool.
    let multi = routes.len() > 1;
    // 今日 makes room for its per-tool chips.
    let chips = snap.is_some_and(|s| s.tools_today.len() > 1) && !is_empty(cx);
    let extra = if chips { tools::CHIP_ROW + 4.0 } else { 0.0 };
    let g = if multi {
        let tall = cards::page_height(area) >= 660.0;
        let lanes = routes.len().min(if tall { 4 } else { 3 });
        let hero_h = lanes::height(lanes, if tall { 182.0 } else { hero::HEIGHT });
        layout_with(area, Some(hero_h), extra)
    } else {
        layout_with(area, None, extra)
    };
    let used = g.sessions.max.y.max(g.todo.max.y) - area.min.y;
    ui.allocate_rect(
        Rect::from_min_size(area.min, vec2(area.width(), used)),
        Sense::hover(),
    );
    if multi {
        lanes::show(ui, cx, &hero_model, &routes, g.hero, out);
    } else {
        hero::show(ui, cx, &hero_model, g.hero, out);
    }
    let mut dismissed = false;
    if is_empty(cx) && !state.empty_dismissed {
        let left = g.today.union(g.sessions);
        cards::card(ui, left, |ui| setup_card(ui, cx, &mut dismissed, out));
    } else {
        cards::card(ui, g.today, |ui| today::show(ui, cx, snap, out));
        cards::card(ui, g.sessions, |ui| sessions::show(ui, cx, snap, out));
    }
    cards::card(ui, g.lines, |ui| lines::show(ui, cx, snap, out));
    cards::card(ui, g.todo, |ui| cards::todo_card(ui, cx, &todos, out));
    if dismissed {
        state.empty_dismissed = true;
    }
}

/// Nothing applied and no key-based provider: the setup card takes the left
/// column (UX §3.1).
pub fn is_empty(cx: &Cx<'_>) -> bool {
    cx.data.active_state().is_none() && cx.data.providers().is_empty()
}

/// The large title with a short tool-neutral line after it. No menu: its
/// actions live on the Superba page (user request 118).
fn title_row(ui: &mut Ui, cx: &Cx<'_>) {
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), TITLE_ROW),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_min_height(TITLE_ROW);
            ui.label(Text::page_title(
                cx.tr.t(Page::Overview.title_key()),
                cx.pal,
            ));
            ui.add_space(6.0);
            ui.with_layout(Layout::left_to_right(Align::Max), |ui| {
                ui.add_space(0.0);
                ui.label(Text::small(cx.tr.t("overview.tagline"), cx.pal));
            });
        },
    );
}

/// "Choose a provider for Codex" (UX §3.1 empty state), in the left column.
fn setup_card(ui: &mut Ui, cx: &Cx<'_>, dismissed: &mut bool, out: &mut Vec<Action>) {
    let b = Backdrop::of(ui.ctx());
    let full = ui.max_rect();
    let top = full.min.y + (full.height() * 0.28).min(140.0);
    ui.add_space(top - full.min.y);
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        crate::design::widgets::list_row::icon_tile(
            ui,
            ui.cursor().min + vec2(26.0, 26.0),
            52.0,
            Icon::Plug,
            Some(b.chart[0]),
        );
        ui.add_space(68.0);
        ui.vertical(|ui| {
            ui.set_max_width((full.width() - 120.0).min(520.0));
            ui.label(Text::h2(cx.tr.t("overview.empty.title"), cx.pal));
            ui.add_space(4.0);
            ui.add(egui::Label::new(Text::muted(cx.tr.t("overview.empty.body"), cx.pal)).wrap());
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if ui
                    .add(Button::primary(cx.tr.t("overview.empty.start")).enabled(!cx.busy))
                    .clicked()
                {
                    out.push(Action::StartWizard);
                }
                if ui
                    .add(Button::ghost(cx.tr.t("overview.empty.skip")))
                    .clicked()
                {
                    *dismissed = true;
                }
            });
        });
    });
}

// ---------------------------------------------------------------------------
// Helpers shared by the hero and the cards
// ---------------------------------------------------------------------------

/// The loopback router address Codex points at (`127.0.0.1:<port>`), when the
/// applied configuration goes through the local router.
pub fn routed_addr(cx: &Cx<'_>) -> Option<String> {
    if !crate::pages::router::mode(cx).is_router() {
        return None;
    }
    let url = cx.data.overview.value.as_ref()?.router_base_url.as_ref()?;
    Some(format!(
        "{}:{}",
        url.host_str()?,
        url.port_or_known_default()?
    ))
}

/// What the speed control offers and says for the applied provider: `None` for
/// the official login (the app's own selector works there) and while nothing
/// key-based is applied.
pub fn speed_support<'a>(cx: &'a Cx<'_>) -> Option<(&'a Profile, Option<&'a SpeedSupport>)> {
    let profile = cx.data.active_profile()?;
    if cx.data.official_active() {
        return None;
    }
    let support = cx
        .data
        .active_models
        .value
        .as_ref()
        .filter(|m| m.profile_id == profile.id && !m.read_only)
        .map(|m| &m.speed);
    Some((profile, support))
}

/// The per-consumer effect line ("Terminal: applies · Desktop app: …") of a
/// tier other than Standard; `None` at Standard (nothing to explain).
pub fn speed_effect_line(cx: &Cx<'_>, tier: SpeedTier, support: &SpeedSupport) -> Option<String> {
    if tier == SpeedTier::Standard {
        return None;
    }
    Some(cx.tr.f(
        "overview.speed.effect",
        &[
            ("cli", &cx.tr.text(&support.cli.text())),
            ("app", &cx.tr.text(&support.app.text())),
        ],
    ))
}

/// "nb_…7a3c" with the preset's key prefix, "••••7a3c" otherwise.
pub fn masked_key(site: &Site, last4: &str) -> String {
    catalog::presets()
        .into_iter()
        .find(|p| p.site.id == site.id || (p.site.kind == site.kind && p.key_placeholder.is_some()))
        .and_then(|p| p.key_placeholder)
        .map_or_else(
            || format!("••••{last4}"),
            |prefix| format!("{prefix}{last4}"),
        )
}

/// The billing summary of the applied key-based provider (sites with a
/// billing endpoint).
fn billing<'a>(cx: &'a Cx<'_>) -> Option<&'a BillingSummary> {
    cx.data
        .account
        .value
        .as_ref()
        .and_then(|r| r.summary.as_ref())
        .and_then(|s| s.billing.as_ref())
        .filter(|_| !cx.data.official_active())
}

/// Remaining balance ("$157.81", "unlimited"), when the site reports one.
pub fn balance(cx: &Cx<'_>) -> Option<String> {
    let b = billing(cx)?;
    if b.unlimited {
        return Some(cx.tr.t("overview.account.unlimited"));
    }
    b.remaining.or(b.limit).map(|u| u.to_string())
}

/// The account lines of the applied provider (identity, key, balance, access
/// or monthly use) for the provider popover.
pub fn account_lines(cx: &Cx<'_>) -> Vec<String> {
    let mut out = Vec::new();
    let summary = cx
        .data
        .account
        .value
        .as_ref()
        .and_then(|r| r.summary.as_ref());
    if let Some(who) = summary.and_then(|s| s.email.clone().or(s.display_name.clone())) {
        out.push(who);
    }
    let last4 = summary
        .and_then(|s| s.key_last4.clone())
        .or_else(|| cx.data.active_key_last4().map(str::to_owned));
    if let (Some(site), Some(k)) = (cx.data.active_site(), last4)
        && !cx.data.official_active()
    {
        let auth = cx
            .data
            .active_state()
            .and_then(|a| a.auth_mode)
            .map(|m| cx.auth_short(m));
        let key = masked_key(&site, &k);
        out.push(match auth {
            Some(a) => format!("{a} · {key}"),
            None => key,
        });
    }
    if let Some(b) = billing(cx) {
        let mut line = format!(
            "{} {}",
            cx.tr.t("overview.account.balance"),
            balance(cx).unwrap_or_else(|| "—".into())
        );
        if let Some(until) = b.access_until {
            line.push_str(&format!(
                " · {} {}",
                cx.tr.t("overview.account.expires"),
                until.utc_date_string()
            ));
        } else if let Some(used) = b.used {
            line.push_str(" · ");
            line.push_str(&cx.tr.f(
                "overview.account.used_month",
                &[("used", &used.to_string())],
            ));
        }
        out.push(line);
    }
    out
}

/// Latency of a probed line.
pub fn caps_latency(caps: Option<&LineCaps>, probing: bool) -> Latency {
    match caps {
        _ if probing && caps.is_none() => Latency::Probing,
        Some(c) if !c.reachable => Latency::Unreachable,
        Some(c) => c.latency_ms.map_or(Latency::Unknown, Latency::Ms),
        None if probing => Latency::Probing,
        None => Latency::Unknown,
    }
}

/// "97 ms", "unreachable", "testing…", "—".
pub fn latency_text(cx: &Cx<'_>, latency: Latency) -> String {
    match latency {
        Latency::Ms(ms) => format!("{ms} ms"),
        Latency::Unreachable => cx.tr.t("overview.latency.unreachable"),
        Latency::Probing => cx.tr.t("overview.latency.probing"),
        Latency::Unknown => "—".into(),
    }
}
