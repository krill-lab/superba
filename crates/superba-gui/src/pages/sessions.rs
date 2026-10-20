//! Sessions per host (UX §3.7): a host switcher (this computer and every remote
//! host the desktop app is connected to; more than four hosts share a "More"
//! menu), per-host totals (all / visible under the host's current provider /
//! hidden), the distribution bar, a paged provider table and, for this computer,
//! the paged migration history with rollback. This computer behaves as before
//! (scan, migrate = full restart, rollback). Remote hosts are scanned read-only
//! over SSH (`host_sessions`); migrating them is offered as a command to run on
//! the host. A host that cannot be reached keeps its last numbers, greyed, with
//! the error and a retry. A remote-control device or WSL distribution the app
//! reports (user bug 36) has a segment under its own name, but is never
//! scanned: its view says why and where its sessions can be seen.
//!
//! Every host keeps its own scan, so a choice shows only that host's numbers:
//! a host being scanned has a spinner in its segment, and until its first scan
//! arrives its view is a loading state (user bug 83); a failed first scan
//! offers Retry in place.
//!
//! Since sessions2 the page opens on the session browser
//! (`session_browser.rs`); everything above is the migration tool behind the
//! header's 「会话迁移…」 button, with a back button to the list.
//!
//! Commands: `host:<target id>` (the migration tool on a remote host),
//! `host:local`, `migrate`, `browse`, `rescan`.

use std::collections::HashMap;

use egui::{Align, Layout, RichText, Sense, Ui, vec2};
use superba_core::codex::PROVIDER_OPENAI;
use superba_core::{
    Error, HostKind, HostSessions, MigrationReceipt, SessionReport, TargetId, Timestamp,
};

use super::shared::hosts as names;
use super::shared::loading;
use super::{Action, Cx, Page, PageStatus, PaletteGroup, PaletteItem, empty_state, error_text};
use crate::design::fonts::{Weight, font, mono_font};
use crate::design::icons::Icon;
use crate::design::layout::{self, Track};
use crate::design::tokens::{size, text};
use crate::design::widgets::{
    Badge, Button, PagedState, PagedTable, Segmented, SplitBar, Status, StatusDot, Table, Text,
};
use crate::runtime::ReadTask;

/// Hosts shown as segments before the "More" menu.
const SEGMENTS: usize = 4;
/// Longest host segment (a longer name is elided), and the shortest it is
/// squeezed to in a narrow window.
const HOST_MAX: f32 = 160.0;
const HOST_MIN: f32 = 72.0;
/// Room kept for the scan time right of the switcher.
const SCAN_TIME_W: f32 = 180.0;

/// The scan of one remote host.
#[derive(Default)]
pub struct RemoteScan {
    /// Last result (kept while a new scan runs or after a failure).
    pub value: Option<HostSessions>,
    /// When `value` was scanned.
    pub at: Option<Timestamp>,
    /// Last error.
    pub error: Option<Error>,
    /// Running scan.
    task: Option<ReadTask<HostSessions>>,
}

/// A session migration running inside a WSL distribution (user bug 45).
pub struct WslMigration {
    /// The distribution.
    pub target: TargetId,
    /// The source provider.
    pub from: String,
    /// The helper's progress lines so far.
    pub lines: std::sync::Arc<std::sync::Mutex<Vec<superba_core::LocalizedText>>>,
    task: Option<ReadTask<superba_core::HostMigrationReport>>,
    /// The result, once finished.
    pub result: Option<Result<superba_core::HostMigrationReport, Error>>,
}

/// Collects the helper's lines for the page.
struct LineCollector(std::sync::Arc<std::sync::Mutex<Vec<superba_core::LocalizedText>>>);

impl superba_core::SwitchEvents for LineCollector {
    fn emit(&self, event: superba_core::SwitchEvent) {
        if let superba_core::SwitchEvent::Log { text, .. } = event
            && let Ok(mut lines) = self.0.lock()
        {
            lines.push(text);
            let n = lines.len();
            if n > 200 {
                lines.drain(..n - 200);
            }
        }
    }
}

/// Page-local state, kept across navigation.
#[derive(Default)]
pub struct State {
    /// Selected host (`None` = this computer).
    pub host: Option<TargetId>,
    /// Scans of remote hosts.
    pub remote: HashMap<TargetId, RemoteScan>,
    /// A migration inside a WSL distribution.
    pub wsl_migration: Option<WslMigration>,
    /// Paging of the provider table.
    pub providers: PagedState,
    /// Paging of the migration history.
    pub history: PagedState,
    /// The migration tool is open (else the session browser shows).
    pub migration: bool,
    /// The session browser.
    pub browser: super::session_browser::Browser,
}

impl State {
    /// The page with the migration tool open.
    pub fn migration_tool() -> Self {
        State {
            migration: true,
            ..State::default()
        }
    }
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("sessions::State")
            .field("host", &self.host)
            .field("scanned", &self.remote.len())
            .finish_non_exhaustive()
    }
}

/// One host of the switcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostEntry {
    /// Target.
    pub id: TargetId,
    /// Name ([`names::title`]: never the app's raw host id).
    pub label: String,
    /// How the app reaches it; only SSH hosts are scanned.
    pub kind: HostKind,
    /// Hover text naming the app's host id (hosts that are not scanned).
    pub tip: Option<String>,
    /// The app reports the host as connected (`None` = unknown).
    pub connected: Option<bool>,
    /// The host's current `model_provider` (last probe).
    pub provider: Option<String>,
}

fn entry(cx: &Cx<'_>, host: &superba_core::RemoteHost, provider: Option<String>) -> HostEntry {
    let managed = host.is_managed();
    HostEntry {
        id: host.target_id.clone(),
        label: names::title(cx.tr, host),
        kind: host.kind,
        tip: (!managed).then(|| names::id_tip(cx.tr, host)).flatten(),
        connected: host.connected.filter(|_| managed),
        provider: provider.filter(|_| managed),
    }
}

/// Remote hosts the desktop app is connected to (the Remote hosts page's list,
/// else the overview's app-known hosts): SSH hosts first.
pub fn hosts(cx: &Cx<'_>) -> Vec<HostEntry> {
    let mut out: Vec<HostEntry> = if let Some(list) = &cx.data.hosts.value {
        list.iter()
            .filter(|h| h.host.is_app_known() && h.host.is_listed())
            .map(|h| {
                entry(
                    cx,
                    &h.host,
                    h.state.as_ref().and_then(|s| s.model_provider.clone()),
                )
            })
            .collect()
    } else {
        cx.data
            .overview
            .value
            .as_ref()
            .map(|o| {
                o.remotes
                    .iter()
                    .filter(|r| r.host.is_app_known() && r.host.is_listed())
                    .map(|r| {
                        entry(
                            cx,
                            &r.host,
                            r.state.as_ref().and_then(|s| s.model_provider.clone()),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    // Stable: the app's order within each group.
    out.sort_by_key(|h| !h.kind.is_managed());
    out
}

impl State {
    /// Scan `host` over SSH unless a scan runs; a host Superba does not manage
    /// is never scanned.
    pub fn scan(&mut self, cx: &Cx<'_>, host: &TargetId) {
        if hosts(cx)
            .iter()
            .any(|h| &h.id == host && !h.kind.is_managed())
        {
            return;
        }
        let scan = self.remote.entry(host.clone()).or_default();
        if scan.task.is_some() {
            return;
        }
        let service = cx.service.clone();
        let id = host.clone();
        scan.error = None;
        scan.task = Some(crate::runtime::task(async move {
            service.host_sessions(&id).await
        }));
    }

    /// Show `host` (`None` = this computer); a remote host is scanned on first
    /// selection.
    pub fn select(&mut self, cx: &Cx<'_>, host: Option<TargetId>) {
        if self.host != host {
            self.providers = PagedState::default();
            self.history = PagedState::default();
        }
        self.host = host.clone();
        if let Some(h) = host
            && self
                .remote
                .get(&h)
                .is_none_or(|s| s.value.is_none() && s.error.is_none())
        {
            self.scan(cx, &h);
        }
    }

    /// Set a scan result (tests).
    pub fn set_remote(&mut self, host: TargetId, result: superba_core::Result<HostSessions>) {
        let scan = self.remote.entry(host).or_default();
        scan.task = None;
        match result {
            Ok(v) => {
                scan.value = Some(v);
                scan.at = Some(Timestamp::now());
                scan.error = None;
            }
            Err(e) => scan.error = Some(e),
        }
    }

    fn poll(&mut self) -> bool {
        let mut busy = false;
        for scan in self.remote.values_mut() {
            let Some(task) = &scan.task else { continue };
            match task.poll() {
                Some(Ok(v)) => {
                    scan.value = Some(v);
                    scan.at = Some(Timestamp::now());
                    scan.error = None;
                    scan.task = None;
                }
                Some(Err(e)) => {
                    scan.error = Some(e);
                    scan.task = None;
                }
                None => busy = true,
            }
        }
        busy
    }
}

/// Draw the page: the session browser, or the migration tool.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    if !state.migration {
        browse_header(ui, cx, state);
        super::session_browser::show(ui, cx, &mut state.browser, out);
        return;
    }
    show_migration(ui, cx, state, out);
}

/// The browser's header: the migration tool and Rescan on the right.
fn browse_header(ui: &mut Ui, cx: &Cx<'_>, state: &mut State) {
    let loading = state.browser.loading();
    let mut open = false;
    let mut rescan = false;
    crate::pages::page_header_with(
        ui,
        cx,
        Page::Sessions,
        &cx.tr.t("sessions.browse.subtitle"),
        |ui| {
            rescan = ui
                .add(
                    Button::new(cx.tr.t("action.rescan"))
                        .icon(Icon::Refresh)
                        .small()
                        .loading(loading)
                        .enabled(!loading),
                )
                .clicked();
            open = ui
                .add(
                    Button::new(cx.tr.t("sessions.browse.migrate_tool"))
                        .icon(Icon::ArrowRightLeft)
                        .small(),
                )
                .on_hover_text(cx.tr.t("sessions.browse.migrate_tool_tip"))
                .clicked();
        },
    );
    if rescan {
        state.browser.load(cx);
    }
    if open {
        state.migration = true;
    }
}

/// The migration tool (the page before sessions2).
fn show_migration(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    if state.poll() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(60));
    }
    let hosts = hosts(cx);
    // A selected host that the app no longer knows falls back to this computer.
    if state
        .host
        .as_ref()
        .is_some_and(|h| !hosts.iter().any(|e| &e.id == h))
    {
        state.host = None;
    }
    let local_provider = cx
        .data
        .overview
        .value
        .as_ref()
        .and_then(|o| o.config.as_ref())
        .and_then(|c| c.model_provider.clone())
        .unwrap_or_else(|| PROVIDER_OPENAI.to_owned());
    let selected = state
        .host
        .as_ref()
        .and_then(|h| hosts.iter().find(|e| &e.id == h))
        .cloned();
    let provider = match &selected {
        Some(h) => h
            .provider
            .clone()
            .unwrap_or_else(|| PROVIDER_OPENAI.to_owned()),
        None => local_provider,
    };
    header(ui, cx, state, &selected, &provider, out);
    let body = ui.available_rect_before_wrap();
    let rows = layout::row_rects(
        body,
        &[
            Track::Fixed(size::CONTROL),
            Track::Fixed(48.0),
            Track::Fixed(10.0),
            Track::Fill,
            Track::Fixed(24.0),
            Track::Fill,
        ],
        10.0,
    );
    layout::fixed(ui, rows[0], |ui| switcher(ui, cx, state, &hosts));
    match &selected {
        None => {
            let local = cx.data.sessions.value.as_ref();
            match local {
                Some((report, receipts)) => {
                    layout::fixed(ui, rows[1], |ui| totals(ui, cx, report, &provider, false));
                    layout::fixed(ui, rows[2], |ui| bar(ui, report, &provider));
                    layout::fixed(ui, rows[3], |ui| {
                        provider_table(ui, cx, &mut state.providers, report, &provider, None, out)
                    });
                    layout::fixed(ui, rows[4], |ui| {
                        ui.label(Text::strong(
                            cx.tr.f(
                                "sessions.history_for",
                                &[("host", &cx.tr.t("sessions.host.local"))],
                            ),
                            cx.pal,
                        ));
                    });
                    layout::fixed(ui, rows[5], |ui| {
                        history(ui, cx, &mut state.history, receipts, out)
                    });
                }
                None => {
                    let rest = rows[1].union(rows[5]);
                    let mut retry = false;
                    layout::fixed(ui, rest, |ui| match &cx.data.sessions.error {
                        Some(e) if !cx.data.sessions.loading => {
                            retry = loading::failed_card(
                                ui,
                                cx,
                                &cx.tr.t("sessions.load_failed"),
                                &error_text(cx.tr, e),
                            );
                        }
                        _ => loading::loading_card(
                            ui,
                            cx,
                            &cx.tr.t("sessions.scanning"),
                            &cx.tr.t("sessions.scanning_body"),
                        ),
                    });
                    if retry {
                        out.push(Action::LoadSessions);
                    }
                }
            }
        }
        Some(host) if !host.kind.is_managed() => {
            let rest = rows[1].union(rows[5]);
            layout::fixed(ui, rest, |ui| read_only(ui, cx, host));
        }
        Some(host) => {
            let scan = state.remote.entry(host.id.clone()).or_default();
            let value = scan.value.clone();
            let error = scan.error.clone();
            let scanning = scan.task.is_some();
            match value {
                Some(v) => {
                    let stale = error.is_some();
                    layout::fixed(ui, rows[1], |ui| {
                        totals(ui, cx, &v.report, &provider, stale)
                    });
                    layout::fixed(ui, rows[2], |ui| bar(ui, &v.report, &provider));
                    layout::fixed(ui, rows[3], |ui| {
                        provider_table(
                            ui,
                            cx,
                            &mut state.providers,
                            &v.report,
                            &provider,
                            Some(&v),
                            out,
                        )
                    });
                    let wsl = &mut state.wsl_migration;
                    let mut rescan = false;
                    layout::fixed(ui, rows[4].union(rows[5]), |ui| {
                        rescan = remote_note(ui, cx, host, &v, &provider, error.as_ref(), out);
                        if host.kind == HostKind::Wsl {
                            rescan |= wsl_migrate(ui, cx, host, &v, &provider, wsl);
                        }
                    });
                    if rescan {
                        let id = host.id.clone();
                        state.scan(cx, &id);
                    }
                }
                None => {
                    let rest = rows[1].union(rows[5]);
                    let mut retry = false;
                    layout::fixed(ui, rest, |ui| match (&error, scanning) {
                        (Some(e), false) => retry = unreachable(ui, cx, host, e),
                        _ => loading::loading_card(
                            ui,
                            cx,
                            &cx.tr.f("sessions.scanning_host", &[("host", &host.label)]),
                            &cx.tr.t(if host.kind == HostKind::Wsl {
                                "sessions.scanning_wsl"
                            } else {
                                "sessions.scanning_ssh"
                            }),
                        ),
                    });
                    if retry {
                        let id = host.id.clone();
                        state.scan(cx, &id);
                    }
                }
            }
        }
    }
}

/// Title with the provider in the subtitle, rescan on the right.
fn header(
    ui: &mut Ui,
    cx: &Cx<'_>,
    state: &mut State,
    selected: &Option<HostEntry>,
    provider: &str,
    out: &mut Vec<Action>,
) {
    let scanning = match selected {
        Some(h) => state.remote.get(&h.id).is_some_and(|s| s.task.is_some()),
        None => cx.data.sessions.loading,
    };
    let can_scan = selected.as_ref().is_none_or(|h| h.kind.is_managed());
    let mut rescan = false;
    let subtitle = cx.tr.f(
        "sessions.browse.migration_subtitle",
        &[("provider", &provider)],
    );
    let mut back = false;
    crate::pages::page_header_with(ui, cx, Page::Sessions, &subtitle, |ui| {
        rescan = ui
            .add(
                Button::new(cx.tr.t("action.rescan"))
                    .icon(Icon::Refresh)
                    .small()
                    .loading(scanning)
                    .enabled(!scanning && can_scan),
            )
            .clicked();
        back = ui
            .add(Button::ghost(cx.tr.t("sessions.browse.back")).small())
            .clicked();
    });
    if back {
        state.migration = false;
    }
    if rescan {
        match selected {
            Some(h) => {
                let id = h.id.clone();
                state.scan(cx, &id);
            }
            None => out.push(Action::LoadSessions),
        }
    }
}

/// The host switcher, one segmented control `本机 | devbox | gpu-01 | More`
/// (hosts past four share the More menu), and the scan time on the right.
fn switcher(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, hosts: &[HostEntry]) {
    let mut pick = state.host.clone();
    ui.horizontal(|ui| {
        // Long host names are elided (in full on hover) so that this computer,
        // four hosts and More fit next to the scan time.
        let shown = hosts.len().min(SEGMENTS) + 1 + usize::from(hosts.len() > SEGMENTS);
        let longest =
            ((ui.available_width() - SCAN_TIME_W) / shown as f32).clamp(HOST_MIN, HOST_MAX);
        let mut seg = Segmented::new("sessions-hosts", &mut pick)
            .label(cx.tr.t("sessions.a11y.hosts"))
            .small()
            .max_segment(longest)
            .option(None, cx.tr.t("sessions.host.local"));
        seg = seg.busy(cx.data.sessions.loading && cx.data.sessions.value.is_none());
        for h in hosts.iter().take(SEGMENTS) {
            seg = match &h.tip {
                Some(tip) => seg.option_with_tooltip(Some(h.id.clone()), h.label.clone(), tip),
                None => seg.option(Some(h.id.clone()), h.label.clone()),
            };
            match h.connected {
                Some(true) => seg = seg.dot(Status::Ok),
                Some(false) => seg = seg.dot(Status::Idle),
                None => {}
            }
            // A spinner while the host is scanned (user bug 83).
            seg = seg.busy(state.remote.get(&h.id).is_some_and(|s| s.task.is_some()));
        }
        if hosts.len() > SEGMENTS {
            seg = seg.overflow(
                cx.tr.t("sessions.host.more"),
                hosts[SEGMENTS..]
                    .iter()
                    .map(|h| (Some(h.id.clone()), h.label.clone())),
            );
        }
        seg.show(ui);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let at = match &state.host {
                Some(h) => state.remote.get(h).and_then(|s| s.at).map(|t| {
                    let key = if HostKind::of_target(h) == HostKind::Wsl {
                        "sessions.scanned_wsl"
                    } else {
                        "sessions.scanned_ssh"
                    };
                    cx.tr.f(key, &[("age", &cx.ago(t))])
                }),
                None => cx.data.sessions.value.as_ref().map(|(r, _)| {
                    cx.tr
                        .f("sessions.scanned", &[("age", &cx.ago(r.scanned_at))])
                }),
            };
            if let Some(at) = at {
                ui.label(Text::caption(at, cx.pal));
            }
        });
    });
    if pick != state.host {
        state.select(cx, pick);
    }
}

/// Total / visible / hidden.
fn totals(ui: &mut Ui, cx: &Cx<'_>, report: &SessionReport, provider: &str, stale: bool) {
    let total: u32 = report.distribution().values().sum();
    let visible = report.by_provider.get(provider).map_or(0, |p| p.distinct);
    let hidden = report.hidden_under(provider);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 40.0;
        for (label, value) in [
            (cx.tr.t("sessions.total"), total),
            (cx.tr.t("sessions.visible"), visible),
            (cx.tr.t("sessions.hidden_count"), hidden),
        ] {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(Text::caption(label, cx.pal));
                let color = if stale {
                    cx.pal.text_disabled
                } else {
                    cx.pal.text
                };
                ui.label(
                    RichText::new(value.to_string())
                        .font(font(22.0, Weight::Semibold))
                        .color(color),
                );
            });
        }
    });
}

/// Providers ordered: the current one first, then by count.
fn ordered(report: &SessionReport, current: &str) -> Vec<(String, u32)> {
    let mut providers: Vec<(String, u32)> = report.distribution().into_iter().collect();
    providers.sort_by(|a, b| (b.0 == current).cmp(&(a.0 == current)).then(b.1.cmp(&a.1)));
    providers
}

fn bar(ui: &mut Ui, report: &SessionReport, current: &str) {
    let counts: Vec<u32> = ordered(report, current).iter().map(|(_, c)| *c).collect();
    ui.add(SplitBar::new(&counts));
}

/// The provider table; hidden providers of this computer get "Migrate to
/// openai…", those of a remote host the command to run there.
fn provider_table(
    ui: &mut Ui,
    cx: &Cx<'_>,
    paging: &mut PagedState,
    report: &SessionReport,
    current: &str,
    remote: Option<&HostSessions>,
    out: &mut Vec<Action>,
) {
    let providers = ordered(report, current);
    let headers = vec![
        cx.tr.t("sessions.col.provider"),
        cx.tr.t("sessions.col.sessions"),
        cx.tr.t("sessions.col.threads"),
        cx.tr.t("sessions.col.rollouts"),
        cx.tr.t("sessions.col.archived"),
        String::new(),
    ];
    let table = Table::new(&[1.4, 0.8, 1.0, 1.0, 0.8, 1.4]);
    let mut copy = None;
    PagedTable::new("sessions-providers", table, providers.len(), paging)
        .label(cx.tr.t("sessions.a11y.providers"))
        .header(headers)
        .row_height(size::ROW + 8.0)
        .show(ui, |ui, row, col| {
            let (name, _) = &providers[row];
            let Some(p) = report.by_provider.get(name) else {
                return;
            };
            match col {
                0 => {
                    // An icon tile in the distribution bar's colour (Surge list look).
                    let (r, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
                    crate::design::widgets::list_row::icon_tile(
                        ui,
                        r.center(),
                        24.0,
                        Icon::Chat,
                        Some(SplitBar::color(cx.pal, row)),
                    );
                    ui.label(Text::mono(name.clone(), cx.pal));
                    if name == current {
                        ui.add(Badge::new(cx.tr.t("sessions.current_provider")));
                    } else if p.distinct > 0 {
                        ui.add(Badge::new(cx.tr.t("sessions.hidden_badge")).status(Status::Warn));
                    }
                }
                1 => {
                    ui.label(Text::strong(p.distinct.to_string(), cx.pal));
                }
                2 => {
                    ui.label(Text::small(p.threads.to_string(), cx.pal));
                }
                3 => {
                    let mut s = p.rollouts.to_string();
                    if p.compressed > 0 {
                        s.push_str(&format!(" ({} zst)", p.compressed));
                    }
                    ui.label(Text::small(s, cx.pal));
                }
                4 => {
                    ui.label(Text::small(p.archived.to_string(), cx.pal));
                }
                _ => {
                    if name == current || p.distinct == 0 {
                        return;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| match remote {
                        None => {
                            if ui
                                .add(
                                    Button::new(cx.tr.f("sessions.migrate", &[("to", &current)]))
                                        .small()
                                        .enabled(!cx.busy),
                                )
                                .on_hover_text(cx.tr.t("sessions.hidden.body"))
                                .clicked()
                            {
                                out.push(Action::MigrateProvider(name.clone()));
                            }
                        }
                        Some(_) => {
                            let cmd = superba_switch::hosts::migrate_command_for([name.as_str()]);
                            if ui
                                .add(
                                    Button::ghost(cx.tr.t("sessions.copy_command"))
                                        .icon(Icon::Copy)
                                        .small(),
                                )
                                .on_hover_text(cmd.clone())
                                .clicked()
                            {
                                copy = Some(cmd);
                            }
                        }
                    });
                }
            }
        });
    if let Some(c) = copy {
        out.push(Action::Copy(c));
    }
    if report.inconsistent > 0 {
        ui.horizontal(|ui| {
            ui.add(StatusDot::new(Status::Warn));
            ui.label(Text::small(
                cx.tr
                    .f("sessions.inconsistent", &[("n", &report.inconsistent)]),
                cx.pal,
            ));
        });
    }
}

/// The migration history of this computer.
fn history(
    ui: &mut Ui,
    cx: &Cx<'_>,
    paging: &mut PagedState,
    history: &[MigrationReceipt],
    out: &mut Vec<Action>,
) {
    if history.is_empty() {
        ui.add_space(8.0);
        ui.label(Text::small(cx.tr.t("sessions.history.empty"), cx.pal));
        ui.label(Text::caption(
            cx.tr.t("sessions.history.empty_body"),
            cx.pal,
        ));
        return;
    }
    let headers = vec![
        cx.tr.t("sessions.col.when"),
        cx.tr.t("sessions.col.change"),
        cx.tr.t("sessions.col.sessions"),
        cx.tr.t("sessions.col.status"),
        String::new(),
    ];
    let table = Table::new(&[1.2, 1.8, 0.8, 1.0, 1.0]);
    let note = cx.tr.t("sessions.history.note");
    PagedTable::new("sessions-history", table, history.len(), paging)
        .label(cx.tr.t("sessions.a11y.history"))
        .header(headers)
        .row_height(size::ROW + 4.0)
        .footer(|ui| {
            ui.add(egui::Label::new(Text::caption(note, cx.pal)).truncate());
        })
        .show(ui, |ui, row, col| {
            let m = &history[row];
            match col {
                0 => {
                    ui.label(Text::small(cx.ago(m.migrated_at), cx.pal))
                        .on_hover_text(superba_switch::localtime::local(m.migrated_at).to_string());
                }
                1 => {
                    let from: Vec<&str> = m.by_source_provider.keys().map(String::as_str).collect();
                    ui.label(
                        RichText::new(format!("{} → {}", from.join(", "), m.to_provider))
                            .font(mono_font(text::SMALL, Weight::Regular))
                            .color(cx.pal.text),
                    )
                    .on_hover_text(&m.id);
                }
                2 => {
                    ui.label(Text::small(m.threads.to_string(), cx.pal));
                }
                3 => {
                    if m.rolled_back {
                        ui.add(StatusDot::new(Status::Idle));
                        ui.label(Text::small(cx.tr.t("sessions.rolled_back"), cx.pal));
                    } else {
                        ui.add(StatusDot::new(Status::Ok));
                        ui.label(Text::small(cx.tr.t("sessions.status.done"), cx.pal));
                    }
                }
                _ => {
                    if !m.rolled_back {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .add(
                                    Button::new(cx.tr.t("sessions.roll_back"))
                                        .small()
                                        .enabled(!cx.busy),
                                )
                                .clicked()
                            {
                                out.push(Action::RollbackMigration(m.id.clone()));
                            }
                        });
                    }
                }
            }
        });
}

/// Remote hosts: read-only, with the command to migrate on the host; a failed
/// rescan keeps the numbers with the error.
fn remote_note(
    ui: &mut Ui,
    cx: &Cx<'_>,
    host: &HostEntry,
    v: &HostSessions,
    provider: &str,
    error: Option<&Error>,
    out: &mut Vec<Action>,
) -> bool {
    let mut retry = false;
    if let Some(e) = error {
        ui.horizontal(|ui| {
            ui.add(StatusDot::new(Status::Error));
            ui.label(Text::small(
                cx.tr.f(
                    "sessions.unreachable",
                    &[("host", &host.label), ("detail", &error_text(cx.tr, e))],
                ),
                cx.pal,
            ));
            retry = ui
                .add(
                    Button::ghost(cx.tr.t("action.retry"))
                        .icon(Icon::Refresh)
                        .small(),
                )
                .clicked();
        });
        ui.add_space(6.0);
    }
    ui.label(Text::strong(
        cx.tr.f("sessions.history_for", &[("host", &host.label)]),
        cx.pal,
    ));
    let history = if host.kind == HostKind::Wsl {
        "sessions.wsl_history"
    } else {
        "sessions.remote_history"
    };
    ui.add(egui::Label::new(Text::small(cx.tr.t(history), cx.pal)).wrap());
    if host.kind == HostKind::Wsl {
        // WSL distributions migrate from here (`wsl_migrate`).
        return retry;
    }
    let hidden: Vec<String> = v
        .report
        .by_provider
        .iter()
        .filter(|(p, s)| p.as_str() != provider && s.distinct > 0)
        .map(|(p, _)| p.clone())
        .collect();
    if hidden.is_empty() {
        return retry;
    }
    ui.add_space(6.0);
    ui.add(egui::Label::new(Text::small(cx.tr.t("sessions.remote_readonly"), cx.pal)).wrap());
    let command = v.migrate_command.clone().unwrap_or_else(|| {
        superba_switch::hosts::migrate_command_for(hidden.iter().map(String::as_str))
    });
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(&command)
                .font(mono_font(text::SMALL, Weight::Medium))
                .color(cx.pal.text),
        );
        if ui
            .add(
                Button::ghost(cx.tr.t("sessions.copy"))
                    .icon(Icon::Copy)
                    .small(),
            )
            .clicked()
        {
            out.push(Action::Copy(command.clone()));
        }
    });
    retry
}

/// Start a migration of `from` sessions inside the WSL distribution `host`.
fn start_wsl_migration(cx: &Cx<'_>, host: &HostEntry, from: &str, to: &str) -> WslMigration {
    let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let events = std::sync::Arc::new(LineCollector(lines.clone()));
    let request = superba_core::MigrationRequest {
        to_provider: to.to_owned(),
        selectors: vec![superba_core::MigrationSelector::Provider {
            provider: from.to_owned(),
        }],
        include_archived: true,
        exclude: Vec::new(),
    };
    let service = cx.service.clone();
    let target = host.id.clone();
    let locale = cx.tr.locale();
    WslMigration {
        target: host.id.clone(),
        from: from.to_owned(),
        lines,
        task: Some(crate::runtime::task(async move {
            service
                .host_migrate_sessions(&target, &request, locale, events)
                .await
        })),
        result: None,
    }
}

/// WSL distributions migrate here (Superba's Linux helper runs inside the
/// distribution): a button per provider whose sessions are hidden, the
/// helper's latest line while it runs, and the result.
fn wsl_migrate(
    ui: &mut Ui,
    cx: &Cx<'_>,
    host: &HostEntry,
    v: &HostSessions,
    provider: &str,
    state: &mut Option<WslMigration>,
) -> bool {
    let mut finished = false;
    if let Some(m) = state.as_mut()
        && let Some(task) = &m.task
        && let Some(result) = task.poll()
    {
        m.task = None;
        m.result = Some(result);
        finished = true;
    }
    let running = state
        .as_ref()
        .is_some_and(|m| m.target == host.id && m.task.is_some());
    if running {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(200));
    }
    let hidden: Vec<String> = v
        .report
        .by_provider
        .iter()
        .filter(|(p, s)| p.as_str() != provider && s.distinct > 0)
        .map(|(p, _)| p.clone())
        .collect();
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        for from in &hidden {
            let label = cx
                .tr
                .f("sessions.wsl_migrate", &[("from", from), ("to", &provider)]);
            if ui
                .add(
                    Button::new(label)
                        .small()
                        .loading(running)
                        .enabled(!running && !cx.busy),
                )
                .clicked()
            {
                *state = Some(start_wsl_migration(cx, host, from, provider));
            }
        }
    });
    let Some(m) = state.as_ref().filter(|m| m.target == host.id) else {
        return finished;
    };
    let last = m
        .lines
        .lock()
        .ok()
        .and_then(|l| l.last().map(|t| cx.tr.text(t)))
        .unwrap_or_default();
    match &m.result {
        None if !last.is_empty() => {
            ui.label(Text::small(last, cx.pal));
        }
        Some(Ok(r)) if r.ok => {
            ui.horizontal(|ui| {
                ui.add(StatusDot::new(Status::Ok));
                ui.label(Text::small(
                    cx.tr.f("sessions.wsl_migrated", &[("from", &m.from)]),
                    cx.pal,
                ));
            });
        }
        Some(Ok(r)) => {
            ui.horizontal(|ui| {
                ui.add(StatusDot::new(Status::Error));
                ui.label(Text::small(cx.tr.t("sessions.wsl_migrate_failed"), cx.pal));
            });
            if let Some(line) = r.run.tail.last() {
                ui.add(egui::Label::new(Text::small(line.clone(), cx.pal)).wrap());
            }
        }
        Some(Err(e)) => {
            ui.horizontal(|ui| {
                ui.add(StatusDot::new(Status::Error));
                ui.add(egui::Label::new(Text::small(error_text(cx.tr, e), cx.pal)).wrap());
            });
        }
        None => {}
    }
    finished
}

/// A host Superba does not scan (a remote-control device, a WSL distribution).
fn read_only(ui: &mut Ui, cx: &Cx<'_>, host: &HostEntry) {
    let body = match host.kind {
        HostKind::RemoteControl => "sessions.read_only_body.remote_control",
        _ => "sessions.read_only_body.other",
    };
    empty_state(
        ui,
        cx,
        names::icon(host.kind),
        &cx.tr.f("sessions.read_only", &[("host", &host.label)]),
        &cx.tr.t(body),
    );
}

/// A host that could not be scanned (no earlier numbers): the reason and
/// Retry (true when clicked).
fn unreachable(ui: &mut Ui, cx: &Cx<'_>, host: &HostEntry, e: &Error) -> bool {
    loading::failed_card(
        ui,
        cx,
        &cx.tr.f(
            "sessions.unreachable",
            &[("host", &host.label), ("detail", &error_text(cx.tr, e))],
        ),
        &cx.tr.t("sessions.unreachable_body"),
    )
}

/// Command palette entries: show the sessions of each remote SSH host.
pub fn palette(cx: &Cx<'_>, state: &State) -> Vec<PaletteItem> {
    let _ = state;
    hosts(cx)
        .into_iter()
        .filter(|h| h.kind.is_managed())
        .map(|h| PaletteItem {
            title: cx.tr.f("sessions.palette.host", &[("host", &h.label)]),
            detail: None,
            keywords: vec!["sessions".into(), h.label.clone()],
            group: PaletteGroup::Navigate,
            action: Action::PageCommand {
                page: Page::Sessions,
                command: format!("host:{}", h.id),
            },
        })
        .collect()
}

/// Run a command addressed to this page (see the module docs).
pub fn command(state: &mut State, cx: &Cx<'_>, command: &str, out: &mut Vec<Action>) {
    if let Some(id) = command.strip_prefix("host:") {
        state.migration = true;
        if id == "local" {
            state.select(cx, None);
            return;
        }
        if let Some(h) = hosts(cx).into_iter().find(|h| h.id.as_str() == id) {
            state.select(cx, Some(h.id));
        }
    } else if command == "migrate" {
        state.migration = true;
    } else if command == "browse" {
        state.migration = false;
    } else if command == "rescan" && !state.migration {
        state.browser.load(cx);
    } else if command == "rescan" {
        match state.host.clone() {
            Some(h) => state.scan(cx, &h),
            None => out.push(Action::LoadSessions),
        }
    }
}

/// Dialogs and pending edits of the page (none).
pub fn status(state: &State) -> PageStatus {
    let _ = state;
    PageStatus::default()
}
