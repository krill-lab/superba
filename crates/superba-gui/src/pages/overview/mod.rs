//! Overview (user request 27): an original composition around what Superba is
//! for — where Codex's traffic goes and what it costs.
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
mod lines;
mod sessions;
mod today;
pub mod todo;
pub mod usage;

use std::time::{Duration, Instant};

use egui::{Align, Layout, Rect, RichText, Sense, Ui, vec2};
use superba_core::{
    BillingSummary, LineCaps, Profile, ProviderRestore, RestoreTarget, Site, SpeedSupport,
    SpeedTier, catalog,
};

use super::{Action, Cx, Page, PageStatus, PaletteItem, empty_state, error_state};
use crate::design::backdrop::Backdrop;
use crate::design::fonts::{Weight, font};
use crate::design::icons::Icon;
use crate::design::tokens::text;
use crate::design::widgets::{Button, IconButton, Latency, Text};
use crate::runtime::ReadTask;
pub use cards::{GAP, Grid, layout};
pub use lines::LineRow;
pub use sessions::{host_label, usage_command};
pub use today::{delta_text, figure};
use usage::Snapshot;

/// How often the dashboard re-reads the request log and the router while it
/// is shown.
const RELOAD: Duration = Duration::from_secs(20);

/// Height of the title row.
pub const TITLE_ROW: f32 = 40.0;

/// Page state: the dismissed setup card and the dashboard's data.
#[derive(Debug, Default)]
pub struct State {
    /// The user chose "I already have a configuration" on the setup card.
    pub empty_dismissed: bool,
    /// The request log and router snapshot.
    pub usage: UsageLoader,
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
            self.task = Some(crate::runtime::task(usage::load(service, now, offset)));
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

/// "Codex in effect" (the shell lists switching and restore).
pub fn palette(cx: &Cx<'_>, _state: &State) -> Vec<PaletteItem> {
    vec![PaletteItem {
        title: cx.tr.t("overview.palette.in_effect"),
        detail: Some(cx.tr.t("overview.palette.in_effect_detail")),
        keywords: vec![
            "runtime".into(),
            "processes".into(),
            "restart".into(),
            "stale".into(),
            "vscode".into(),
        ],
        group: super::PaletteGroup::Commands,
        action: Action::ShowRuntime,
    }]
}

/// Commands: `wizard`, `restore:original`, `restore:official`, `refresh`.
pub fn command(state: &mut State, _cx: &Cx<'_>, command: &str, out: &mut Vec<Action>) {
    match command {
        "wizard" => out.push(Action::StartWizard),
        "restore:original" => out.push(Action::Restore(RestoreTarget::Original {
            provider: ProviderRestore::KeepOpenai,
        })),
        "restore:official" => out.push(Action::Restore(RestoreTarget::Official)),
        "refresh" => state.usage.at = None,
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
    title_row(ui, cx, state, out);
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
    let area = ui.available_rect_before_wrap();
    let g = layout(area);
    let used = g.sessions.max.y.max(g.todo.max.y) - area.min.y;
    ui.allocate_rect(
        Rect::from_min_size(area.min, vec2(area.width(), used)),
        Sense::hover(),
    );
    hero::show(ui, cx, &hero_model, g.hero, out);
    let mut dismissed = false;
    if is_empty(cx) && !state.empty_dismissed {
        let left = g.today.union(g.sessions);
        cards::card(ui, left, |ui| setup_card(ui, cx, &mut dismissed, out));
    } else {
        cards::card(ui, g.today, |ui| today::show(ui, cx, snap));
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

/// The large title with a gray line after it, and the menu on the right.
fn title_row(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), TITLE_ROW),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_min_height(TITLE_ROW);
            ui.label(
                RichText::new(cx.tr.t(Page::Overview.title_key()))
                    .font(font(text::TITLE_XL, Weight::Semibold))
                    .color(cx.pal.text)
                    .extra_letter_spacing(-0.8),
            );
            ui.add_space(6.0);
            ui.with_layout(Layout::left_to_right(Align::Max), |ui| {
                ui.add_space(0.0);
                ui.label(Text::small(cx.tr.t("overview.tagline"), cx.pal));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                menu(ui, cx, state, out);
            });
        },
    );
}

fn menu(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let response = ui.add(IconButton::new(Icon::More, cx.tr.t("overview.menu.title")));
    egui::Popup::menu(&response)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
        .show(|ui| {
            ui.set_min_width(220.0);
            let baseline = cx
                .data
                .overview
                .value
                .as_ref()
                .is_some_and(|o| o.baseline_captured);
            if ui
                .add_enabled(
                    baseline && !cx.busy,
                    egui::Button::new(cx.tr.t("overview.menu.restore_original")),
                )
                .clicked()
            {
                out.push(Action::Restore(RestoreTarget::Original {
                    provider: ProviderRestore::KeepOpenai,
                }));
            }
            if ui
                .add_enabled(
                    !cx.busy && !cx.data.official_active(),
                    egui::Button::new(cx.tr.t("overview.menu.restore_official")),
                )
                .clicked()
            {
                out.push(Action::Restore(RestoreTarget::Official));
            }
            ui.separator();
            if ui.button(cx.tr.t("overview.menu.wizard")).clicked() {
                out.push(Action::StartWizard);
            }
            if ui.button(cx.tr.t("action.refresh")).clicked() {
                out.push(Action::Refresh);
                state.usage.at = None;
            }
        });
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
    if !cx.data.router_enabled() || cx.data.active_profile().is_none() {
        return None;
    }
    let base = cx
        .data
        .overview
        .value
        .as_ref()?
        .config
        .as_ref()?
        .openai_base_url
        .as_deref()?;
    let url = url::Url::parse(base).ok()?;
    let host = url.host_str()?;
    matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1")
        .then(|| format!("{host}:{}", url.port_or_known_default().unwrap_or(80)))
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
