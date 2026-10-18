//! Remote hosts (UX §3.8): the SSH hosts the ChatGPT desktop app itself knows
//! (app global state, the renderer over CDP, the app's declarative config, live
//! `ssh … app-server` children) — never hosts known only from `~/.ssh/config`.
//!
//! A list on the left (`list.rs`); the detail on the right (`detail.rs`): where
//! the host is, its connection and the "switch with this computer" toggle, its
//! Codex, the settings sync summary, sessions and hot-restart capability. Sync
//! settings… opens a dialog (`sync.rs`) with the version-filtered rows (write /
//! skip / remove / conflict); confirming plans `SwitchIntent::HostSync`, which
//! pushes settings only and hot-restarts that host's Codex core.
//!
//! Remote-control devices the app reports (user bug 36) are listed read-only:
//! titled with the app's display name or their kind, the app's host id only in
//! a tooltip and a copy button, and never probed, followed, synced or offered
//! any other SSH action. WSL distributions of this computer (user bug 45) are
//! managed like SSH hosts (through `wsl.exe`), follow this computer by default
//! and offer router access when they run WSL 2 behind NAT.

mod detail;
mod list;
pub mod sync;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use egui::Ui;
use superba_core::wsl::WslAccessReport;
use superba_core::{HostSyncPlan, HostView, TargetId, VersionsView};

use super::router::db::Loader;
use super::shared::hosts as names;
use super::{Action, Cx, Page, PageStatus, PaletteGroup, PaletteItem, page_header};
use crate::design::icons::Icon;
use crate::design::layout::{self, Track};
use crate::design::widgets::{Button, Status};
use crate::runtime::ReadTask;

/// A probe older than this is refreshed when the page opens.
const PROBE_MAX_AGE: Duration = Duration::from_secs(10 * 60);

/// Page-local state, kept across navigation.
#[derive(Debug, Default)]
pub struct State {
    /// App-known hosts.
    pub hosts: Loader<Vec<HostView>>,
    /// Selected host.
    pub selected: Option<TargetId>,
    probes: BTreeMap<TargetId, ReadTask<HostView>>,
    probed: BTreeSet<TargetId>,
    follow: Option<(TargetId, ReadTask<HostView>)>,
    /// Sync previews of the detail pane, per host.
    pub plans: BTreeMap<TargetId, Loader<HostSyncPlan>>,
    /// Version data (update hints).
    pub versions: Loader<VersionsView>,
    /// The sync dialog.
    pub sync: Option<sync::SyncDialog>,
    /// Router access for a WSL distribution in progress (user bug 45).
    access: Option<(TargetId, ReadTask<WslAccessReport>)>,
    /// The last router access result per distribution.
    pub access_reports: BTreeMap<TargetId, WslAccessReport>,
    errors: Vec<String>,
    /// A probe or the follow toggle changed a host: the shell reloads its own
    /// host list (the sidebar badge and the Overview read it).
    notify_shell: bool,
}

impl State {
    /// Hosts in display order: SSH hosts before read-only ones, connected
    /// first, then by name.
    pub fn sorted(&self) -> Vec<&HostView> {
        // Only hosts the desktop app knows; a host known only from ~/.ssh/config
        // is never listed (the service filters too).
        let mut v: Vec<&HostView> = self
            .hosts
            .value
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|h| h.host.is_app_known())
            .collect();
        let name = |h: &HostView| h.host.display_name().unwrap_or_default().to_lowercase();
        v.sort_by(|a, b| {
            let ca = a.host.connected == Some(true);
            let cb = b.host.connected == Some(true);
            b.host
                .is_managed()
                .cmp(&a.host.is_managed())
                .then(cb.cmp(&ca))
                .then_with(|| name(a).cmp(&name(b)))
        });
        v
    }

    /// The listed host with this id, when Superba manages it (SSH).
    fn managed(&self, id: &TargetId) -> bool {
        self.hosts
            .value
            .as_deref()
            .unwrap_or_default()
            .iter()
            .any(|h| &h.host.target_id == id && h.host.is_managed())
    }

    /// The selected host (or the first).
    pub fn current(&self) -> Option<&HostView> {
        let sorted = self.sorted();
        self.selected
            .as_ref()
            .and_then(|id| sorted.iter().find(|h| &h.host.target_id == id).copied())
            .or_else(|| sorted.first().copied())
    }

    fn replace(&mut self, view: HostView) {
        if let Some(list) = self.hosts.value.as_mut()
            && let Some(slot) = list
                .iter_mut()
                .find(|h| h.host.target_id == view.host.target_id)
        {
            *slot = view;
        }
    }

    /// Reload the list and probe connected hosts again.
    pub fn refresh(&mut self) {
        self.hosts.invalidate();
        self.probed.clear();
        self.plans.clear();
        self.versions.invalidate();
    }

    fn poll(&mut self, cx: &Cx<'_>) {
        self.hosts.poll();
        self.versions.poll();
        let mut done = Vec::new();
        for (id, task) in &self.probes {
            if let Some(result) = task.poll() {
                done.push((id.clone(), result));
            }
        }
        for (id, result) in done {
            self.probes.remove(&id);
            match result {
                Ok(view) => {
                    self.plans.remove(&id);
                    self.replace(view);
                    self.notify_shell = true;
                }
                Err(e) => self.errors.push(super::error_text(cx.tr, &e)),
            }
        }
        if let Some((_, task)) = &self.follow
            && let Some(result) = task.poll()
        {
            self.follow = None;
            match result {
                Ok(view) => {
                    self.replace(view);
                    self.notify_shell = true;
                }
                Err(e) => self.errors.push(super::error_text(cx.tr, &e)),
            }
        }
        if let Some((id, task)) = &self.access
            && let Some(result) = task.poll()
        {
            let id = id.clone();
            self.access = None;
            match result {
                Ok(report) => {
                    self.access_reports.insert(id, report);
                }
                Err(e) => self.errors.push(super::error_text(cx.tr, &e)),
            }
        }
        for plan in self.plans.values_mut() {
            plan.poll();
        }
        if let Some(d) = &mut self.sync {
            d.poll();
        }
    }

    /// Start what is due: the list, probes of connected hosts without a recent
    /// probe, the version data and the selected host's sync preview.
    fn load(&mut self, cx: &Cx<'_>) {
        if self.hosts.due(None) {
            let service = cx.service.clone();
            self.hosts.start(async move { service.hosts().await });
        }
        if self.versions.due(None) {
            let service = cx.service.clone();
            self.versions
                .start(async move { service.versions(false).await });
        }
        let stale: Vec<TargetId> = self
            .hosts
            .value
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|h| {
                h.host.is_app_known() && h.host.is_managed() && h.host.connected == Some(true)
            })
            .filter(|h| {
                h.state.as_ref().is_none_or(|s| {
                    cx.now
                        .duration_since(s.probed_at)
                        .is_none_or(|age| age > PROBE_MAX_AGE)
                })
            })
            .map(|h| h.host.target_id.clone())
            .filter(|id| !self.probed.contains(id) && !self.probes.contains_key(id))
            .collect();
        for id in stale {
            self.probe(cx, id);
        }
        if let Some(h) = self.current()
            && h.host.is_managed()
            && h.state.is_some()
            && h.host.connected == Some(true)
        {
            let id = h.host.target_id.clone();
            let plan = self.plans.entry(id.clone()).or_default();
            if plan.due(None) && !self.probes.contains_key(&id) {
                let service = cx.service.clone();
                plan.start(async move { service.host_sync_plan(&id).await });
            }
        }
    }

    /// Probe one host now (SSH hosts only).
    pub fn probe(&mut self, cx: &Cx<'_>, id: TargetId) {
        if self.probes.contains_key(&id) || !self.managed(&id) {
            return;
        }
        self.probed.insert(id.clone());
        let service = cx.service.clone();
        let target = id.clone();
        self.probes.insert(
            id,
            crate::runtime::task(async move { service.host_probe(&target).await }),
        );
    }

    /// A probe of `id` is running.
    pub fn probing(&self, id: &TargetId) -> bool {
        self.probes.contains_key(id)
    }

    /// Turn "switch with this computer" on or off (stored only; SSH hosts).
    pub fn set_follow(&mut self, cx: &Cx<'_>, id: TargetId, on: bool) {
        if self.follow.is_some() || !self.managed(&id) {
            return;
        }
        // Show the new value at once; the service's answer replaces it.
        if let Some(list) = self.hosts.value.as_mut()
            && let Some(h) = list.iter_mut().find(|h| h.host.target_id == id)
        {
            h.settings.follow_switches = on;
        }
        let service = cx.service.clone();
        let target = id.clone();
        self.follow = Some((
            id,
            crate::runtime::task(async move { service.set_host_follow(&target, on).await }),
        ));
    }

    /// Let a WSL distribution reach the local router (may ask Windows for
    /// administrator approval once).
    pub fn router_access(&mut self, cx: &Cx<'_>, id: TargetId) {
        if self.access.is_some() || !self.managed(&id) {
            return;
        }
        let service = cx.service.clone();
        let target = id.clone();
        self.access = Some((
            id,
            crate::runtime::task(async move { service.wsl_router_access(&target).await }),
        ));
    }

    /// Router access for `id` is running.
    pub fn accessing(&self, id: &TargetId) -> bool {
        self.access.as_ref().is_some_and(|(a, _)| a == id)
    }

    /// Open the sync dialog for `id` (SSH hosts only).
    pub fn open_sync(&mut self, cx: &Cx<'_>, id: TargetId) {
        let view = self
            .hosts
            .value
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|h| h.host.target_id == id);
        if view.is_some_and(|h| !h.host.is_managed()) {
            return;
        }
        let label = view.map_or_else(
            || names::target_name(cx, &id),
            |h| names::title(cx.tr, &h.host),
        );
        // The last probe's version titles the dialog while the preview loads or
        // when the host cannot be reached now.
        let version = view
            .and_then(|h| h.state.as_ref())
            .and_then(|s| s.codex_version.clone());
        self.selected = Some(id.clone());
        self.sync = Some(sync::SyncDialog::open(cx, id, label, version));
    }

    /// Hosts with settings not synced (sidebar badge).
    pub fn unsynced(&self) -> u32 {
        self.hosts
            .value
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|h| {
                h.host.is_app_known()
                    && h.host.is_managed()
                    && h.pending_sync.is_some_and(|n| n > 0)
            })
            .count() as u32
    }
}

/// Draw the page.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    state.poll(cx);
    state.load(cx);
    for e in state.errors.drain(..) {
        out.push(Action::Toast {
            status: Status::Error,
            text: e,
        });
    }
    if std::mem::take(&mut state.notify_shell) {
        out.push(Action::LoadHosts);
    }
    let busy = state.hosts.loading()
        || !state.probes.is_empty()
        || state.follow.is_some()
        || state.access.is_some();
    if busy || state.sync.as_ref().is_some_and(|d| d.loading()) {
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }
    let refreshing = state.hosts.loading() || !state.probes.is_empty();
    page_header(ui, cx, Page::Hosts, |ui| {
        if ui
            .add(
                Button::new(cx.tr.t("hosts.refresh"))
                    .icon(Icon::Refresh)
                    .small()
                    .loading(refreshing)
                    .enabled(!refreshing),
            )
            .clicked()
        {
            state.refresh();
        }
    });
    let hosts = state.sorted();
    if hosts.is_empty() {
        let rect = ui.available_rect_before_wrap();
        layout::fixed(ui, rect, |ui| list::empty(ui, cx, state));
    } else {
        layout::columns(ui, &[Track::Fixed(260.0), Track::Fill], 20.0, |ui, i| {
            if i == 0 {
                list::show(ui, cx, state);
            } else {
                detail::show(ui, cx, state, out);
            }
        });
    }
    if state.sync.is_some() {
        sync::show(ui, cx, state, out);
    }
}

/// Command palette entries: sync settings to each SSH host.
pub fn palette(cx: &Cx<'_>, state: &State) -> Vec<PaletteItem> {
    let hosts: Vec<(TargetId, String)> = match &state.hosts.value {
        Some(list) => list
            .iter()
            .filter(|h| h.host.is_app_known() && h.host.is_managed())
            .map(|h| (h.host.target_id.clone(), names::title(cx.tr, &h.host)))
            .collect(),
        None => cx
            .data
            .overview
            .value
            .as_ref()
            .map(|o| {
                o.remotes
                    .iter()
                    .filter(|r| r.host.is_app_known() && r.host.is_managed())
                    .map(|r| (r.host.target_id.clone(), names::title(cx.tr, &r.host)))
                    .collect()
            })
            .unwrap_or_default(),
    };
    hosts
        .into_iter()
        .map(|(id, label)| PaletteItem {
            title: cx.tr.f("hosts.palette.sync", &[("host", &label)]),
            detail: Some(cx.tr.t("hosts.palette.sync_detail")),
            keywords: vec!["remote".into(), "ssh".into(), "sync".into(), label.clone()],
            group: PaletteGroup::Commands,
            action: Action::PageCommand {
                page: Page::Hosts,
                command: format!("sync:{id}"),
            },
        })
        .collect()
}

/// Run a command addressed to this page: `refresh`, `select:<target id>`,
/// `sync:<target id>` (opens the sync dialog), `probe:<target id>`.
pub fn command(state: &mut State, cx: &Cx<'_>, command: &str, out: &mut Vec<Action>) {
    let _ = out;
    if command == "refresh" {
        state.refresh();
    } else if let Some(id) = command.strip_prefix("select:") {
        state.selected = Some(TargetId::new(id));
    } else if let Some(id) = command.strip_prefix("sync:") {
        state.open_sync(cx, TargetId::new(id));
    } else if let Some(id) = command.strip_prefix("probe:") {
        state.probe(cx, TargetId::new(id));
    }
}

/// Dialogs and the sidebar badge (hosts whose settings are not synced).
pub fn status(state: &State) -> PageStatus {
    // The sidebar count comes from the shell's own host list
    // (`app::shell_badges`), one source for every page.
    PageStatus {
        modal: state.sync.is_some(),
        ..PageStatus::default()
    }
}
