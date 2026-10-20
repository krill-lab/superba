//! Two sidebar pages share this module and its state (user decision 21):
//!
//! - 路由 / Router ([`show`]): the local smart router's SETTINGS only (opt-in,
//!   v2 decision 6). The page header carries the one on / off action; under it
//!   one scrolling column (`settings.rs`): a status line saying plainly whether
//!   Codex traffic goes through the local process, the members and their line
//!   order (`view.rs`), the entry points (`ingress.rs`), then the closed
//!   「高级」 group: the reply stats line (`stats_line.rs`), prices
//!   (`prices.rs`), recording and the service. Opening 路由 always lands here.
//! - 用量 / Usage ([`show_usage`]): tabs 最近请求 / 活动请求 / 统计 with a search
//!   on the right.
//!   - 最近请求 (`recent.rs`): source list (按会话 / 按供应商), the 13-column log
//!     with totals, toolbar and the detail pane (`detail.rs`: 通用 and 计时).
//!   - 活动请求 (`active.rs`): requests in flight from the router's admin API
//!     (an explanatory empty state while the router is off).
//!   - 统计 (`stats.rs`): daily bars and grouped totals, compare with billing.
//!
//! The log is read through the switch service: the router's database
//! (read-only, also while the router is stopped) merged with the model requests
//! in Codex's rollout files, so 最近请求 and 统计 work without the router; a
//! request both recorded is listed once (the router's row). Columns only the
//! router measures (first token, TPS, line, transport, HTTP status) are "—" on
//! rollout rows. Session names come from the local Codex state at display time
//! (`db.rs`) and are never stored. The official ChatGPT login is never routed;
//! remote hosts stay direct.

mod active;
pub mod backend;
pub mod db;
mod detail;
pub mod doctor;
pub mod eligibility;
pub mod ingress;
pub mod model;
pub mod prices;
mod recent;
pub mod requests;
mod settings;
mod stats;
pub mod stats_line;
pub mod timing;
mod view;

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Align, Layout, Sense, Ui, vec2};
use superba_core::{ClearRequest, RouterStatsSettings, RouterView, Timestamp};

use self::backend::RouterBackend;
use self::model::{Candidate, Mode, Snapshot};
use super::{Action, Cx, Page, PageStatus, PaletteGroup, PaletteItem, page_header};
use crate::design::fonts::{Weight, font};
use crate::design::tokens::{space, text};
use crate::design::widgets::log_table::elided;
use crate::design::widgets::{Button, ButtonKind, Segmented, Status, StatusDot, dialog};
use crate::runtime::ReadTask;

/// How often live status is refreshed while the page is visible.
const POLL: Duration = Duration::from_secs(2);

/// Tabs of the Usage page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Tab {
    /// 最近请求.
    #[default]
    Recent,
    /// 活动请求.
    Active,
    /// 统计.
    Stats,
}

impl Tab {
    /// All tabs in order.
    pub const ALL: [Tab; 3] = [Tab::Recent, Tab::Active, Tab::Stats];

    /// i18n key of the label.
    pub fn key(self) -> &'static str {
        match self {
            Tab::Recent => "usage.tab.recent",
            Tab::Active => "usage.tab.active",
            Tab::Stats => "usage.tab.stats",
        }
    }

    /// Command name (`tab:<name>`).
    pub fn command(self) -> &'static str {
        match self {
            Tab::Recent => "recent",
            Tab::Active => "active",
            Tab::Stats => "stats",
        }
    }
}

/// A change in flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Conversion,
    Order,
    Add,
    Remove,
    Install,
    Restart,
    Uninstall,
    /// Start (or repair) the service, then plan turning router mode on.
    Start,
}

/// A dialog of the page.
#[derive(Clone, Debug, PartialEq)]
pub enum Dialog {
    /// Confirm deleting the request history.
    Clear {
        /// Rows that will be deleted.
        rows: u64,
    },
    /// Apply a ratio computed from billing.
    Calibrate(prices::Calibration),
    /// The price table in use (Prices → View…).
    Prices,
    /// 诊断: the router doctor report with Copy.
    Doctor,
}

/// Page-local state, kept across navigation.
#[derive(Default)]
pub struct State {
    backend: Option<Arc<dyn RouterBackend>>,
    backend_checked: bool,
    snapshot: Option<Snapshot>,
    error: Option<superba_core::Error>,
    task: Option<ReadTask<Snapshot>>,
    loaded_at: Option<Instant>,
    op: Option<(Op, ReadTask<()>)>,
    op_error: Option<String>,
    conversion_error: Option<String>,
    pick: Option<usize>,
    /// The router-mode switch to plan once [`Op::Start`] has finished.
    then_plan: Option<superba_core::SwitchRequest>,
    /// A switch ready to be planned (handed to the switch flow by `show`).
    ready_plan: Option<superba_core::SwitchRequest>,
    /// Selected tab of the Usage page.
    pub tab: Tab,
    /// Search text (sessions, models, providers, cache keys, status).
    pub search: String,
    focus_search: bool,
    /// 最近请求.
    pub recent: recent::Recent,
    /// 活动请求.
    pub active: active::Active,
    /// 统计.
    pub stats: stats::Stats,
    /// Router settings (edits in progress).
    pub prefs: settings::Prefs,
    /// Recent recorded request counts for the ingress card.
    pub ingress: db::Loader<[u64; 4]>,
    /// Session names.
    pub titles: db::Titles,
    stats_save: Option<ReadTask<RouterView>>,
    /// Saved stats settings not yet visible in the loaded profile set.
    pending_stats: Option<RouterStatsSettings>,
    clear_task: Option<ReadTask<superba_core::ClearReport>>,
    /// Open dialog.
    pub dialog: Option<Dialog>,
    /// 诊断 (the router doctor report).
    pub doctor: doctor::DoctorState,
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("router::State")
            .field("tab", &self.tab)
            .field("backend", &self.backend.is_some())
            .field("members", &self.snapshot.as_ref().map(|s| s.members.len()))
            .field("op", &self.op.as_ref().map(|(op, _)| *op))
            .field("rows", &self.recent.shown.len())
            .field("dialog", &self.dialog)
            .finish_non_exhaustive()
    }
}

impl State {
    /// A state with a given backend (tests).
    pub fn with_backend(backend: Arc<dyn RouterBackend>) -> Self {
        Self {
            backend: Some(backend),
            backend_checked: true,
            ..Self::default()
        }
    }

    /// The last snapshot.
    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.snapshot.as_ref()
    }

    fn backend(&mut self, cx: &Cx<'_>) -> Option<Arc<dyn RouterBackend>> {
        if !self.backend_checked {
            self.backend_checked = true;
            self.backend = backend::default_backend(cx);
        }
        self.backend.clone()
    }

    /// Reload now (after a change) or when the poll interval has passed.
    fn refresh(&mut self, cx: &Cx<'_>, force: bool) {
        if self.task.is_some() {
            return;
        }
        let due = self.loaded_at.is_none_or(|t| t.elapsed() >= POLL);
        if !(force || due) {
            return;
        }
        if let Some(b) = self.backend(cx) {
            self.task = Some(crate::runtime::task(b.snapshot()));
        }
    }

    fn start(
        &mut self,
        cx: &Cx<'_>,
        op: Op,
        run: impl FnOnce(&dyn RouterBackend) -> backend::BoxFut<()>,
    ) {
        if self.op.is_some() {
            return;
        }
        if let Some(b) = self.backend(cx) {
            self.op_error = None;
            if op == Op::Conversion {
                self.conversion_error = None;
            }
            self.op = Some((op, crate::runtime::task(run(b.as_ref()))));
        }
    }

    fn poll(&mut self, cx: &Cx<'_>, out: &mut Vec<Action>) {
        self.doctor.poll();
        if let Some(task) = &self.task
            && let Some(result) = task.poll()
        {
            self.task = None;
            self.loaded_at = Some(Instant::now());
            match result {
                Ok(s) => {
                    self.snapshot = Some(s);
                    self.error = None;
                }
                Err(e) => self.error = Some(e),
            }
        }
        if let Some((op, task)) = &self.op
            && let Some(result) = task.poll()
        {
            let op = *op;
            self.op = None;
            match result {
                Ok(()) => {
                    if op == Op::Start {
                        self.ready_plan = self.then_plan.take();
                    }
                    // The members (and the service) changed: the shell's copy
                    // of the profile set too, not only this page's snapshot
                    // (user bug 39).
                    out.push(Action::Refresh);
                }
                Err(e) => {
                    self.then_plan = None;
                    self.op_error = Some(super::error_text(cx.tr, &e));
                    if op == Op::Conversion {
                        self.conversion_error = self.op_error.clone();
                    }
                }
            }
            if op == Op::Conversion {
                // An earlier snapshot may still contain the old directions.
                // Refresh after the write/reload, including after a reload error.
                self.task = None;
            }
            self.refresh(cx, true);
        }
        if let Some(task) = &self.stats_save
            && let Some(result) = task.poll()
        {
            self.stats_save = None;
            match result {
                Ok(_) => out.push(Action::Refresh),
                Err(e) => {
                    self.pending_stats = None;
                    out.push(Action::Toast {
                        status: Status::Error,
                        text: super::error_text(cx.tr, &e),
                    });
                }
            }
        }
        if let Some(task) = &self.clear_task
            && let Some(result) = task.poll()
        {
            self.clear_task = None;
            match result {
                Ok(report) => {
                    self.recent.after_clear();
                    self.stats.stats.invalidate();
                    out.push(Action::Toast {
                        status: Status::Ok,
                        text: cx.tr.f("requests.cleared", &[("n", &report.rows)]),
                    });
                }
                Err(e) => out.push(Action::Toast {
                    status: Status::Error,
                    text: super::error_text(cx.tr, &e),
                }),
            }
        }
        if self.titles.poll() {
            self.recent.titles_changed();
        }
        // The saved settings became visible in the profile set.
        if let Some(p) = &self.pending_stats
            && cx
                .data
                .profiles
                .value
                .as_ref()
                .is_some_and(|s| &s.settings.router.stats == p)
            && self.stats_save.is_none()
        {
            self.pending_stats = None;
        }
    }

    /// Turn router mode on or off through the switch flow. Like `spb router
    /// enable`, turning it on first starts (or repairs) the service when the
    /// router is not answering, so the plan is not blocked by a stopped router.
    fn switch_mode(&mut self, cx: &Cx<'_>, on: bool, out: &mut Vec<Action>) {
        let Ok(request) = backend::mode_request(cx, on) else {
            return;
        };
        let running = self.snapshot.as_ref().is_none_or(|s| s.live.is_ok());
        if on && !running && self.op.is_none() {
            self.then_plan = Some(request);
            self.start(cx, Op::Start, |b| b.install());
        } else {
            out.push(Action::Plan(Box::new(request)));
        }
    }

    /// Move member `index` up or down.
    fn move_member(&mut self, cx: &Cx<'_>, index: usize, up: bool) {
        let Some(s) = &self.snapshot else {
            return;
        };
        let ids: Vec<String> = s.members.iter().map(|m| m.id.clone()).collect();
        if let Some(order) = model::reorder(&ids, index, up) {
            self.start(cx, Op::Order, |b| b.set_order(order));
        }
    }

    fn add(&mut self, cx: &Cx<'_>, candidate: Candidate) {
        self.start(cx, Op::Add, |b| b.add_member(candidate));
    }

    fn remove(&mut self, cx: &Cx<'_>, id: String) {
        self.start(cx, Op::Remove, |b| b.remove_member(id));
    }

    /// Recording, stats line and price settings in effect (a save in flight
    /// counts as done).
    pub fn stats_settings(&self, cx: &Cx<'_>) -> RouterStatsSettings {
        self.pending_stats.clone().unwrap_or_else(|| {
            cx.data
                .profiles
                .value
                .as_ref()
                .map(|p| p.settings.router.stats.clone())
                .unwrap_or_default()
        })
    }

    /// Save recording / stats line / price settings (router reload, never a
    /// Codex restart).
    pub fn save_stats(&mut self, cx: &Cx<'_>, settings: RouterStatsSettings) {
        if self.stats_settings(cx) == settings && self.pending_stats.is_none() {
            return;
        }
        self.pending_stats = Some(settings.clone());
        let service = cx.service.clone();
        // A newer save replaces an older one still in flight (last write wins
        // in the service as well).
        self.stats_save = Some(crate::runtime::task(async move {
            service.router_set_stats(settings).await
        }));
    }

    /// A stats settings save is running.
    pub fn saving_stats(&self) -> bool {
        self.stats_save.is_some()
    }

    fn clear(&mut self, cx: &Cx<'_>, request: ClearRequest) {
        if self.clear_task.is_some() {
            return;
        }
        let service = cx.service.clone();
        self.clear_task = Some(crate::runtime::task(async move {
            service.router_clear(request).await
        }));
    }
}

/// Router mode as observed in the local configuration.
pub fn mode(cx: &Cx<'_>) -> Mode {
    let overview = cx.data.overview.value.as_ref();
    Mode::from_config(
        overview.and_then(|o| o.config.as_ref()),
        cx.data.router_enabled(),
        overview.and_then(|o| o.router_base_url.as_ref()),
    )
}

/// The router's loopback address for the banner.
fn address(state: &State, mode: &Mode) -> String {
    let port = match mode {
        Mode::Router { port } => Some(*port),
        _ => state.snapshot.as_ref().and_then(|s| s.port),
    };
    port.map(|p| format!("127.0.0.1:{p}"))
        .unwrap_or_else(|| "127.0.0.1".into())
}

/// Poll background work and schedule the next repaint (both pages).
fn tick(ui: &Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>, usage: bool) {
    state.poll(cx, out);
    if let Some(request) = state.ready_plan.take() {
        out.push(Action::Plan(Box::new(request)));
    }
    state.refresh(cx, false);
    let busy = state.task.is_some()
        || state.op.is_some()
        || state.stats_save.is_some()
        || state.clear_task.is_some()
        || state.doctor.loading()
        || state.titles.loading()
        || state.recent.loading()
        || state.active.loading()
        || state.stats.loading();
    ui.ctx().request_repaint_after(if busy {
        Duration::from_millis(100)
    } else if usage && state.tab == Tab::Active {
        Duration::from_secs(1)
    } else {
        POLL
    });
}

/// Draw the Router page: the header with the one on / off action, then one
/// scrolling column with the status, members, entry points and the collapsed
/// Advanced group (`settings.rs`).
pub fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    tick(ui, cx, state, out, false);
    page_header(ui, cx, Page::Router, |ui| switch_button(ui, cx, state, out));
    let rect = ui.available_rect_before_wrap();
    crate::design::layout::fixed(ui, rect, |ui| settings::show(ui, cx, state, out));
    dialogs(ui, cx, state, out);
}

/// Draw the Usage page: where the rows come from, the tabs and their content.
pub fn show_usage(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    tick(ui, cx, state, out, true);
    let tab = state.tab;
    page_header(ui, cx, Page::Usage, |ui| {
        if tab == Tab::Recent {
            let r = ui
                .add(
                    Button::new(cx.tr.t("requests.export_csv"))
                        .small()
                        .loading(state.recent.exporting())
                        .enabled(!state.recent.shown.is_empty()),
                )
                .on_hover_text(cx.tr.t("requests.export_hint"));
            if r.clicked() {
                state.recent.export(cx, state.titles.map());
            }
        }
    });
    usage_banner(ui, cx, state, out);
    tab_row(ui, cx, state);
    let rect = ui.available_rect_before_wrap();
    crate::design::layout::fixed(ui, rect, |ui| match state.tab {
        Tab::Recent => recent::show(ui, cx, state, out),
        Tab::Active => active::show(ui, cx, state, out),
        Tab::Stats => stats::show(ui, cx, state, out),
    });
    dialogs(ui, cx, state, out);
}

/// A one-line banner: a status dot, a line elided to the width (full text in
/// `tip`) and an optional small button on the right; true when it was clicked.
fn banner_line(
    ui: &mut Ui,
    cx: &Cx<'_>,
    status: Status,
    line: &str,
    tip: &str,
    button: Option<(String, bool)>,
) -> bool {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    child.spacing_mut().item_spacing.x = 8.0;
    child.add(StatusDot::new(status));
    let button_w = button.as_ref().map_or(0.0, |(label, _)| {
        Button::new(label.clone()).small().desired_size(&child).x + 8.0
    });
    let g = elided(
        &child,
        line,
        font(text::SMALL, Weight::Regular),
        cx.pal.text,
        child.available_width() - button_w - 8.0,
    );
    let (r, resp) = child.allocate_exact_size(g.size(), Sense::hover());
    child.painter().galley(r.min, g, cx.pal.text);
    // The whole line for screen readers (user bug 56), the tip as its description.
    crate::design::a11y::text(&resp, line);
    resp.on_hover_text(tip);
    let mut clicked = false;
    if let Some((label, enabled)) = button {
        child.with_layout(Layout::right_to_left(Align::Center), |ui| {
            clicked = ui
                .add(Button::new(label).small().enabled(enabled))
                .clicked();
        });
    }
    clicked
}

/// The router can be turned on: members exist and one of them can carry
/// traffic (user bug 28; both from the snapshot that lists them, user bug 39).
fn ready(cx: &Cx<'_>, state: &State) -> bool {
    eligibility::has_members(cx, state) && eligibility::has_eligible_member(cx, state)
}

/// The weight of the header's on / off button (ui-spec rule 5, one primary
/// button per view): primary only while turning on is possible; while no
/// member can carry traffic the members section's fix is the primary action;
/// turning off is never the page's main task.
pub fn switch_kind(on: bool, ready: bool) -> ButtonKind {
    if !on && ready {
        ButtonKind::Primary
    } else {
        ButtonKind::Secondary
    }
}

/// The header's one action: 开启路由… / 关闭路由…. Turning on is the page's
/// primary action once a member can carry traffic; until then the members
/// section's fix is, and this button is a disabled secondary one. Turning off
/// is secondary.
fn switch_button(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let on = mode(cx).is_router();
    let members = eligibility::has_members(cx, state);
    let eligible = eligibility::has_eligible_member(cx, state);
    let request = backend::mode_request(cx, !on);
    let ready = on || ready(cx, state);
    let enabled = request.is_ok() && !cx.busy && state.op.is_none() && ready;
    let starting = state.op.as_ref().is_some_and(|(op, _)| *op == Op::Start);
    let label = cx.tr.t(if on {
        "router.banner.turn_off"
    } else {
        "router.banner.turn_on"
    });
    let button = Button::new(label).kind(switch_kind(on, ready));
    // The existing UIA / AX names of the on and off flows (e2e/windows, e2e/macos).
    let name = cx.tr.t(if on {
        "router.turn_off"
    } else {
        "router.banner.turn_on"
    });
    let response = ui
        .add(button.small().loading(starting).enabled(enabled))
        .accessible_name(name);
    let hint = match &request {
        Err(key) => cx.tr.t(key),
        Ok(_) if !members && !on => cx.tr.t("router.needs_member"),
        Ok(_) if !eligible && !on => cx.tr.t("router.eligibility.none"),
        Ok(_) => cx.tr.t("router.preview_hint"),
    };
    if response
        .on_hover_text(&hint)
        .on_disabled_hover_text(hint)
        .clicked()
    {
        state.switch_mode(cx, !on, out);
    }
}

/// What the router does right now, in one wrapped line under the header: a
/// status dot, the state in bold ("路由已开启") and what it means for Codex's
/// requests, with the listen address. The full rules are its hover text.
pub(super) fn status_line(ui: &mut Ui, cx: &Cx<'_>, state: &State) {
    let mode = mode(cx);
    let (status, line) = if mode.is_router() {
        let addr = address(state, &mode);
        (Status::Ok, cx.tr.f("router.banner.on", &[("addr", &addr)]))
    } else if let Mode::Local { address } = &mode {
        (
            Status::Idle,
            cx.tr.f("router.banner.local", &[("addr", address)]),
        )
    } else {
        (Status::Idle, cx.tr.t("router.banner.off"))
    };
    // "路由已开启 —— …" / "Router on — …": the state before the dash in bold.
    let (head, tail) = line
        .split_once(" —— ")
        .map(|(h, t)| (h, format!(" —— {t}")))
        .or_else(|| line.split_once(" — ").map(|(h, t)| (h, format!(" — {t}"))))
        .unwrap_or((line.as_str(), String::new()));
    let mut job = egui::text::LayoutJob::default();
    job.append(
        head,
        0.0,
        egui::TextFormat::simple(font(text::BODY, Weight::Medium), cx.pal.text),
    );
    job.append(
        &tail,
        0.0,
        egui::TextFormat::simple(font(text::BODY, Weight::Regular), cx.pal.text_muted),
    );
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = space::S;
        ui.add(StatusDot::new(status));
        ui.add(egui::Label::new(job).wrap())
            .on_hover_text(cx.tr.t("router.banner.tip"));
    });
}

/// The Usage page's banner: where the rows come from (router log and Codex
/// session files, or session files only while the router is off).
fn usage_banner(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let mode = mode(cx);
    let on = mode.is_router();
    let (status, line, tip) = if on {
        let addr = address(state, &mode);
        (
            Status::Ok,
            cx.tr.f("usage.banner.on", &[("addr", &addr)]),
            cx.tr.t("usage.banner.on_tip"),
        )
    } else {
        (
            Status::Idle,
            cx.tr.t("usage.banner.off"),
            cx.tr.t("usage.banner.off_tip"),
        )
    };
    let button = Some((cx.tr.t("usage.banner.settings"), true));
    if banner_line(ui, cx, status, &line, &tip, button) {
        out.push(Action::Go(Page::Router));
    }
}

/// Tabs on the left, search on the right.
fn tab_row(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    ui.horizontal(|ui| {
        let mut seg =
            Segmented::new("router-tabs", &mut state.tab).label(cx.tr.t(Page::Usage.title_key()));
        for t in Tab::ALL {
            seg = seg.option(t, cx.tr.t(t.key()));
        }
        seg.show(ui);
        if matches!(state.tab, Tab::Recent | Tab::Active) {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let edit = egui::TextEdit::singleline(&mut state.search)
                    .hint_text(cx.tr.t("requests.search"))
                    .desired_width(260.0);
                // Named by what it searches (its only visible label is the hint).
                let r = ui.add(edit).accessible_name(cx.tr.t("requests.search"));
                if state.focus_search {
                    state.focus_search = false;
                    r.request_focus();
                }
                if r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    state.search.clear();
                }
            });
        }
    });
    ui.add_space(2.0);
}

fn dialogs(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    let Some(d) = state.dialog.clone() else {
        return;
    };
    let mut close = false;
    let id = egui::Id::unique("router-dialog");
    let width = if d == Dialog::Doctor { 760.0 } else { 460.0 };
    let resp = dialog(ui.ctx(), id, width, |ui| match &d {
        Dialog::Clear { rows } => {
            ui.label(crate::design::widgets::Text::title(
                cx.tr.t("requests.clear.title"),
                cx.pal,
            ));
            crate::design::widgets::dialog_body(ui, |ui| {
                ui.add(
                    egui::Label::new(crate::design::widgets::Text::small(
                        cx.tr.f("requests.clear.confirm", &[("n", rows)]),
                        cx.pal,
                    ))
                    .wrap(),
                );
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add(Button::danger(cx.tr.t("requests.toolbar.clear")).small())
                        .clicked()
                    {
                        state.clear(
                            cx,
                            ClearRequest {
                                before: Some(Timestamp::now()),
                            },
                        );
                        close = true;
                    }
                    if ui
                        .add(Button::ghost(cx.tr.t("action.cancel")).small())
                        .clicked()
                    {
                        close = true;
                    }
                });
            });
        }
        Dialog::Calibrate(c) => {
            prices::calibrate_dialog(ui, cx, state, c, &mut close, out);
        }
        Dialog::Prices => prices::table_dialog(ui, cx, state, &mut close),
        Dialog::Doctor => doctor::dialog(ui, cx, state, &mut close),
    });
    if resp.should_close() || close {
        state.dialog = None;
    }
}

/// Dialogs and pending edits of the Router page.
pub fn status(state: &State) -> PageStatus {
    PageStatus {
        modal: state.dialog.is_some(),
        ..PageStatus::default()
    }
}

/// Dialogs of the Usage page (the two pages share the dialog slot).
pub fn usage_status(state: &State) -> PageStatus {
    status(state)
}

fn page_command(page: Page, command: &str) -> Action {
    Action::PageCommand {
        page,
        command: command.into(),
    }
}

/// Command palette entries: router mode and the stats line (Router), the Usage
/// tabs and recording (Usage).
pub fn palette(cx: &Cx<'_>, state: &State) -> Vec<PaletteItem> {
    let on = mode(cx).is_router();
    let members = eligibility::has_members(cx, state);
    let ready = members && eligibility::has_eligible_member(cx, state);
    let keywords = || vec!["router".into(), "failover".into(), "proxy".into()];
    let mut items = Vec::new();
    if !on && !ready {
        // Nothing to turn on yet: the Router page explains how to add members
        // (or why none can carry traffic), as the disabled 开启 does.
        items.push(PaletteItem {
            title: cx.tr.t("router.palette.setup"),
            detail: Some(cx.tr.t(if members {
                "router.eligibility.none"
            } else {
                "router.needs_member"
            })),
            keywords: keywords(),
            group: PaletteGroup::Router,
            action: Action::Go(Page::Router),
        });
    } else {
        let (title, command) = if on {
            ("router.palette.off", "off")
        } else {
            ("router.palette.on", "on")
        };
        items.push(PaletteItem {
            title: cx.tr.t(title),
            detail: Some(cx.tr.t("router.palette.detail")),
            keywords: keywords(),
            group: PaletteGroup::Router,
            action: page_command(Page::Router, command),
        });
    }
    items.push(PaletteItem {
        title: cx.tr.t("router.palette.doctor"),
        detail: Some(cx.tr.t("router.doctor.hint")),
        keywords: vec![
            "router".into(),
            "doctor".into(),
            "diagnostics".into(),
            "诊断".into(),
        ],
        group: PaletteGroup::Router,
        action: page_command(Page::Router, "doctor"),
    });
    let stats = state.stats_settings(cx);
    let entries: [(Page, &str, &str, Vec<String>); 5] = [
        (
            Page::Usage,
            "usage.palette.open_recent",
            "tab:recent",
            vec![
                "requests".into(),
                "log".into(),
                "usage".into(),
                "请求记录".into(),
                "用量".into(),
            ],
        ),
        (
            Page::Usage,
            "usage.palette.open_active",
            "tab:active",
            vec!["requests".into(), "in flight".into(), "usage".into()],
        ),
        (
            Page::Usage,
            "usage.palette.open_stats",
            "tab:stats",
            vec![
                "statistics".into(),
                "cost".into(),
                "usage".into(),
                "统计".into(),
            ],
        ),
        (
            Page::Router,
            if stats.line.enabled {
                "router.palette.stats_off"
            } else {
                "router.palette.stats_on"
            },
            if stats.line.enabled {
                "stats_line:off"
            } else {
                "stats_line:on"
            },
            vec!["stats line".into(), "tok/s".into(), "统计行".into()],
        ),
        (
            Page::Router,
            if stats.record {
                "router.palette.pause_recording"
            } else {
                "router.palette.resume_recording"
            },
            if stats.record {
                "record:off"
            } else {
                "record:on"
            },
            vec!["recording".into(), "记录".into()],
        ),
    ];
    for (page, title, command, keywords) in entries {
        items.push(PaletteItem {
            title: cx.tr.t(title),
            detail: None,
            keywords,
            group: PaletteGroup::Router,
            action: page_command(page, command),
        });
    }
    items
}

/// Commands both pages understand: `refresh` reloads, `stats_line:on|off` and
/// `record:on|off` save those settings. False when `command` is not one.
fn shared_command(state: &mut State, cx: &Cx<'_>, command: &str) -> bool {
    if command == "refresh" {
        state.refresh(cx, true);
        state.recent.page.invalidate();
        return true;
    }
    if let Some(v) = command.strip_prefix("stats_line:") {
        let s = stats_line::with_enabled(&state.stats_settings(cx), v == "on");
        state.save_stats(cx, s);
        return true;
    }
    if let Some(v) = command.strip_prefix("record:") {
        let mut s = state.stats_settings(cx);
        s.record = v == "on";
        state.save_stats(cx, s);
        return true;
    }
    false
}

/// Run a command addressed to the Router page: `on` / `off` plan the router
/// mode switch, `doctor` opens 诊断, plus the shared commands.
pub fn command(state: &mut State, cx: &Cx<'_>, command: &str, out: &mut Vec<Action>) {
    match command {
        "on" | "off" => state.switch_mode(cx, command == "on", out),
        "doctor" => doctor::open(state, cx),
        _ => {
            if !shared_command(state, cx, command) {
                tracing::debug!(%command, "router page: unknown command");
            }
        }
    }
}

/// Run a command addressed to the Usage page: `search` focuses the search,
/// `tab:<name>` opens a tab, `export` writes the shown rows as CSV,
/// `session:<key>` shows one session's requests (the Overview's recent
/// sessions), plus the shared commands.
pub fn usage_command(state: &mut State, cx: &Cx<'_>, command: &str, _out: &mut Vec<Action>) {
    if let Some(key) = command.strip_prefix("session:") {
        state.tab = Tab::Recent;
        state.search.clear();
        state.recent.show_session(key);
        return;
    }
    match command {
        "search" => {
            if !matches!(state.tab, Tab::Recent | Tab::Active) {
                state.tab = Tab::Recent;
            }
            state.focus_search = true;
        }
        "export" => state.recent.export(cx, state.titles.map()),
        _ => {
            if let Some(name) = command.strip_prefix("tab:") {
                if let Some(t) = Tab::ALL.into_iter().find(|t| t.command() == name) {
                    state.tab = t;
                }
            } else if !shared_command(state, cx, command) {
                tracing::debug!(%command, "usage page: unknown command");
            }
        }
    }
}
