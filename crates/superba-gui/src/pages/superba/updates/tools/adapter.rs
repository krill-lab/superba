//! Nonblocking bridge from the unified inventory to the real installer service.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::{Removal, ToolId, ToolRow, ToolsSource, ToolsView};
use crate::design::widgets::Status;
use crate::pages::superba::updates::items::{Entry, Operation, Progress, UpdateItem, UpdateStatus};
use crate::pages::{Action, Cx, error_text};
use crate::runtime::{ReadTask, task};
use superba_core::{
    Error, SharedToolProgress, SwitchService, ToolInstallKind, ToolOp, ToolProgress, ToolReport,
    ToolRequest, ToolSettingsChange, ToolStage, ToolSupport, ToolUpdater, paths::display_path,
};

/// Production installer adapter. It binds to the page's service on the first
/// snapshot; all filesystem, process and network work runs off the egui thread.
#[derive(Default)]
pub struct ServiceTools(Mutex<State>);
impl ServiceTools {
    /// Bind an explicit service (for isolated probes and embedded inventories).
    pub fn new(service: Arc<dyn SwitchService>) -> Self {
        Self(Mutex::new(State {
            service: Some(service),
            ..Default::default()
        }))
    }
}
impl std::fmt::Debug for ServiceTools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceTools").finish_non_exhaustive()
    }
}
#[derive(Default)]
struct State {
    service: Option<Arc<dyn SwitchService>>,
    view: Option<superba_core::ToolsView>,
    read: Option<(u64, ReadTask<superba_core::ToolsView>)>,
    generation: u64,
    loaded: bool,
    dirty: bool,
    refresh: bool,
    error: Option<Error>,
    running: BTreeMap<ToolId, Running>,
    settings: BTreeMap<ToolId, ReadTask<superba_core::ToolRow>>,
    settings_errors: BTreeMap<ToolId, Error>,
    results: BTreeMap<ToolId, Outcome>,
    notifications: Vec<superba_core::Result<ToolReport>>,
}
struct Running {
    request: ToolRequest,
    progress: Arc<SharedToolProgress>,
    task: ReadTask<ToolReport>,
}
struct Outcome {
    request: ToolRequest,
    result: superba_core::Result<ToolReport>,
}
impl State {
    fn invalidate(&mut self) {
        self.generation += 1;
        self.dirty = true;
    }
    fn tick(&mut self, cx: &Cx<'_>) {
        let service = self
            .service
            .get_or_insert_with(|| cx.service.clone())
            .clone();
        if let Some((generation, read)) = &self.read
            && let Some(result) = read.poll()
        {
            // An inventory started before an install/settings change must not
            // overwrite its newer result, even if it finishes afterwards.
            if *generation == self.generation {
                self.loaded = true;
                match result {
                    Ok(view) => {
                        self.view = Some(view);
                        self.error = None;
                    }
                    Err(error) => self.error = Some(error),
                }
            }
            self.read = None;
        }
        let finished: Vec<_> = self
            .running
            .iter()
            .filter_map(|(id, run)| run.task.poll().map(|r| (*id, r)))
            .collect();
        for (id, result) in finished {
            let run = self.running.remove(&id).expect("completed operation");
            self.notifications.push(result.clone());
            self.results.insert(
                id,
                Outcome {
                    request: run.request,
                    result,
                },
            );
            self.invalidate();
        }
        let finished: Vec<_> = self
            .settings
            .iter()
            .filter_map(|(id, read)| read.poll().map(|r| (*id, r)))
            .collect();
        for (id, result) in finished {
            self.settings.remove(&id);
            match result {
                Ok(row) => {
                    if let Some(view) = &mut self.view
                        && let Some(slot) = view.rows.iter_mut().find(|r| r.id == id.core())
                    {
                        *slot = row;
                    }
                    self.results.remove(&id);
                }
                Err(error) => {
                    self.settings_errors.insert(id, error);
                }
            }
            self.invalidate();
        }
        if self.read.is_none()
            && self.running.is_empty()
            && self.settings.is_empty()
            && (!self.loaded || self.dirty || self.refresh)
        {
            self.dirty = false;
            let refresh = std::mem::take(&mut self.refresh);
            self.read = Some((
                self.generation,
                task(async move { service.tools(refresh).await }),
            ));
        }
    }
    fn row(&self, id: ToolId) -> Result<&superba_core::ToolRow, String> {
        self.view
            .as_ref()
            .and_then(|v| v.row(id.core()))
            .ok_or_else(|| format!("{}: inventory is not ready", id.name()))
    }
    fn available(&self, id: ToolId) -> Result<(), String> {
        if self.running.contains_key(&id) || self.settings.contains_key(&id) {
            Err(format!("{}: an operation is still running", id.name()))
        } else {
            Ok(())
        }
    }
    fn run(&mut self, id: ToolId, request: ToolRequest) -> Result<(), String> {
        self.available(id)?;
        let service = self.service.clone().ok_or("Tool inventory is not ready")?;
        let progress = Arc::new(SharedToolProgress::default());
        let sink = progress.clone();
        let req = request.clone();
        self.results.remove(&id);
        self.settings_errors.remove(&id);
        self.invalidate();
        self.running.insert(
            id,
            Running {
                request,
                progress,
                task: task(async move { service.tool_run(req, sink).await }),
            },
        );
        Ok(())
    }
    fn change(&mut self, id: ToolId, change: ToolSettingsChange) -> Result<(), String> {
        self.available(id)?;
        let service = self.service.clone().ok_or("Tool inventory is not ready")?;
        self.settings_errors.remove(&id);
        self.invalidate();
        self.settings.insert(
            id,
            task(async move { service.tool_settings(id.core(), change).await }),
        );
        Ok(())
    }
}
impl ToolsSource for ServiceTools {
    fn view(&self, cx: &Cx<'_>) -> ToolsView {
        let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
        state.tick(cx);
        let mut view = ToolsView {
            checking: state.read.is_some() || state.dirty || state.refresh,
            error: state.error.as_ref().map(|e| error_text(cx.tr, e)),
            ..Default::default()
        };
        if let Some(raw) = &state.view {
            view.notes = raw.notes.iter().map(|n| cx.localize(&n.text)).collect();
            for id in ToolId::ALL {
                let Some(row) = raw.row(id.core()) else {
                    continue;
                };
                if matches!(row.support, ToolSupport::Unavailable) {
                    view.unavailable += 1;
                } else {
                    view.rows.push(normalize(cx, &state, id, row));
                }
            }
        } else {
            view.rows = ToolId::ALL
                .into_iter()
                .map(|id| ToolRow {
                    entry: Entry::new(UpdateItem::Tool(id), id.name(), UpdateStatus::Unknown)
                        .description(
                            view.error
                                .clone()
                                .unwrap_or_else(|| cx.tr.t("updates.tools.loading")),
                        ),
                    installs: vec![],
                    channels: vec![],
                    channel: String::new(),
                    managed: None,
                    install_cautions: vec![],
                    notes: vec![],
                    log: None,
                    versions: vec![],
                    removal: None,
                })
                .collect();
        }
        view
    }
    fn poll(&self, cx: &Cx<'_>, out: &mut Vec<Action>) {
        let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
        state.tick(cx);
        for result in state.notifications.drain(..) {
            let (status, text) = match result {
                Ok(report) => (Status::Ok, report_line(cx, &report)),
                Err(Error::Cancelled) => (Status::Idle, cx.tr.t("updates.tools.cancelled")),
                Err(error) => (Status::Error, error_text(cx.tr, &error)),
            };
            out.push(Action::Toast { status, text });
        }
    }
    fn check(&self) {
        let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
        state.refresh = true;
        state.invalidate();
    }
    fn start(&self, id: ToolId) -> Result<(), String> {
        let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(outcome) = state.results.get(&id)
            && matches!(&outcome.result, Err(error) if !matches!(error, Error::Cancelled))
            && outcome.request.op != ToolOp::Uninstall
        {
            // Retry the reviewed version and method, including a failed explicit
            // install; do not silently turn it into an update to today's latest.
            let request = outcome.request.clone();
            return state.run(id, request);
        }
        let row = state.row(id)?;
        let op = primary_op(row).ok_or("This tool cannot be installed or updated here")?;
        let mut request = ToolRequest::new(id.core(), op);
        request.channel.clone_from(&row.channel);
        request.version.clone_from(&row.latest);
        request.confirmed = true;
        state.run(id, request)
    }
    fn install_version(&self, id: ToolId, version: String) -> Result<(), String> {
        let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let row = state.row(id)?;
        let mut request = ToolRequest::new(id.core(), ToolOp::Install);
        request.channel.clone_from(&row.channel);
        request.version = Some(version);
        request.confirmed = true;
        state.run(id, request)
    }
    fn remove(&self, id: ToolId, remove_data: bool) -> Result<(), String> {
        let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let row = state.row(id)?;
        if !row.actions.uninstall {
            return Err("This installation cannot be removed here".into());
        }
        let mut request = ToolRequest::new(id.core(), ToolOp::Uninstall);
        request.confirmed = true;
        request.remove_data = remove_data && id == ToolId::Opencode && !row.data_dirs.is_empty();
        state.run(id, request)
    }
    fn cancel(&self, id: ToolId) {
        let state = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(run) = state.running.get(&id) {
            run.progress.cancel();
        }
    }
    fn channel(&self, id: ToolId, channel: String) -> Result<(), String> {
        let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if !state.row(id)?.channels.contains(&channel) {
            return Err("Unknown tool channel".into());
        }
        state.change(
            id,
            ToolSettingsChange {
                channel: Some(channel),
                ..Default::default()
            },
        )
    }
    fn managed(&self, id: ToolId, enabled: bool) -> Result<(), String> {
        let mut state = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if !state.row(id)?.actions.manage_updates {
            return Err("This updater cannot be managed here".into());
        }
        state.change(
            id,
            ToolSettingsChange {
                manage_updates: Some(enabled),
                ..Default::default()
            },
        )
    }
}
fn primary_op(row: &superba_core::ToolRow) -> Option<ToolOp> {
    if row.actions.update {
        Some(ToolOp::Update)
    } else if row.actions.install {
        Some(ToolOp::Install)
    } else {
        None
    }
}
fn normalize(cx: &Cx<'_>, state: &State, id: ToolId, row: &superba_core::ToolRow) -> ToolRow {
    let blocked = matches!(row.updater, ToolUpdater::Blocked { .. });
    let vendor = matches!(
        row.support,
        ToolSupport::InstallOnly | ToolSupport::Link { .. }
    ) || matches!(row.updater, ToolUpdater::Forced);
    let package_manager = matches!(row.updater, ToolUpdater::PackageManager);
    let status = if blocked {
        UpdateStatus::Blocked
    } else if row.primary().is_none() {
        UpdateStatus::Missing
    } else if vendor {
        UpdateStatus::Vendor
    } else if row.actions.update && row.update_available {
        UpdateStatus::Available
    } else if package_manager {
        UpdateStatus::Vendor
    } else if row.latest_error.is_some()
        || row.latest.is_none()
        || row.installed_version().is_none()
    {
        UpdateStatus::Unknown
    } else {
        UpdateStatus::Current
    };
    let mut entry = Entry::new(UpdateItem::Tool(id), &row.name, status);
    entry.current = row.installed_version().map(str::to_owned);
    entry.latest.clone_from(&row.latest);
    entry.description = if row.primary().is_some() {
        updater_line(cx, &row.updater)
    } else {
        row.channel
            .as_deref()
            .map(|channel| channel_label(cx, id, channel))
            .unwrap_or_default()
    };
    let op = primary_op(row);
    entry.operation = if let ToolSupport::Link { url } = &row.support {
        Some(Operation::DownloadPage(url.clone()))
    } else if blocked {
        None
    } else if op == Some(ToolOp::Install) && row.primary().is_some() {
        Some(Operation::ToolInstall(id))
    } else if op.is_some() {
        Some(Operation::Tool(id))
    } else if status == UpdateStatus::Unknown {
        Some(Operation::Check)
    } else {
        None
    };
    let cautions = if op == Some(ToolOp::Update) {
        &row.update_cautions
    } else {
        &row.install_cautions
    };
    entry.cautions = cautions.iter().map(|c| cx.localize(c)).collect();
    let mut notes: Vec<_> = row.notes.iter().map(|n| cx.localize(&n.text)).collect();
    if let Some(error) = &row.latest_error {
        notes.push(error.clone());
        entry.description = error.clone();
    }
    if let Some(hint) = &row.path_hint {
        notes.push(cx.tr.f("updates.tools.path_hint", &[("file", &hint.file)]));
        notes.push(hint.line.clone());
    }
    let mut log = None;
    if let Some(outcome) = state.results.get(&id) {
        match &outcome.result {
            Ok(report) => {
                notes.push(report_line(cx, report));
                notes.extend(report.notes.iter().map(|n| cx.localize(&n.text)));
                log = Some(display_path(&report.log));
            }
            Err(Error::Cancelled) => notes.push(cx.tr.t("updates.tools.cancelled")),
            Err(error) => {
                entry.status = UpdateStatus::Failed;
                entry.description = error_text(cx.tr, error);
                // Removal retries are reviewed again and never join Update all.
                if outcome.request.op == ToolOp::Uninstall {
                    entry.operation = Some(Operation::ToolRemove {
                        id,
                        remove_data: false,
                    });
                    entry.cautions = row
                        .uninstall_cautions
                        .iter()
                        .map(|c| cx.localize(c))
                        .collect();
                } else {
                    entry.latest.clone_from(&outcome.request.version);
                    entry.operation = Some(Operation::Tool(id));
                    let cautions = if outcome.request.op == ToolOp::Update {
                        &row.update_cautions
                    } else {
                        &row.install_cautions
                    };
                    entry.cautions = cautions.iter().map(|c| cx.localize(c)).collect();
                }
            }
        }
    }
    if let Some(error) = state.settings_errors.get(&id) {
        notes.push(error_text(cx.tr, error));
    }
    if let Some(run) = state.running.get(&id) {
        entry.status = UpdateStatus::Installing;
        entry.operation = Some(Operation::Cancel);
        let progress = run.progress.latest();
        entry.progress = Some(Progress {
            fraction: progress
                .as_ref()
                .and_then(superba_core::ToolProgress::fraction),
            label: progress
                .as_ref()
                .map(|p| progress_line(cx, p))
                .unwrap_or_else(|| cx.tr.t("updates.tools.starting")),
        });
    } else if state.settings.contains_key(&id) {
        entry.status = UpdateStatus::Installing;
        entry.operation = None;
        entry.progress = Some(Progress {
            fraction: None,
            label: cx.tr.t("updates.tools.starting"),
        });
    }
    let installs = row
        .installs
        .iter()
        .map(|install| {
            let mut kind = cx.tr.t(kind_key(install.kind));
            if install.by_superba {
                kind = cx.tr.f("updates.tools.by_superba", &[("kind", &kind)]);
            }
            format!(
                "{} · {} · {}",
                kind,
                install
                    .version
                    .clone()
                    .unwrap_or_else(|| cx.tr.t("updates.tools.unknown_version")),
                display_path(&install.path)
            )
        })
        .collect();
    // The service currently offers the installed and channel versions, not a
    // release history. Never fabricate a catalog or offer vendor-owned rollback.
    let versions = if !blocked
        && !vendor
        && !package_manager
        && (row.actions.install || row.actions.uninstall)
    {
        let mut versions: Vec<_> = row.latest.iter().cloned().collect();
        if let Some(current) = row.installed_version()
            && !versions.iter().any(|v| v == current)
        {
            versions.push(current.into());
        }
        versions
    } else {
        vec![]
    };
    ToolRow {
        entry,
        installs,
        channels: row.channels.clone(),
        channel: row.channel.clone().unwrap_or_default(),
        managed: row
            .actions
            .manage_updates
            .then_some(matches!(row.updater, ToolUpdater::Superba)),
        install_cautions: row
            .install_cautions
            .iter()
            .map(|c| cx.localize(c))
            .collect(),
        notes,
        log,
        versions,
        removal: row.actions.uninstall.then(|| Removal {
            cautions: row
                .uninstall_cautions
                .iter()
                .map(|c| cx.localize(c))
                .collect(),
            data_dirs: if id == ToolId::Opencode {
                row.data_dirs.iter().map(|p| display_path(p)).collect()
            } else {
                vec![]
            },
        }),
    }
}
/// The toast / result line of a finished operation.
fn report_line(cx: &Cx<'_>, r: &ToolReport) -> String {
    let name = r.tool.display_name();
    match (&r.version, r.op) {
        (None, _) => cx.tr.f("updates.tools.removed", &[("name", &name)]),
        (Some(v), ToolOp::Update) => cx
            .tr
            .f("updates.tools.updated", &[("name", &name), ("version", v)]),
        (Some(v), _) => cx.tr.f(
            "updates.tools.installed",
            &[("name", &name), ("version", v)],
        ),
    }
}

/// Locale key of an install kind.
fn kind_key(kind: ToolInstallKind) -> &'static str {
    match kind {
        ToolInstallKind::Native => "updates.tools.kind.native",
        ToolInstallKind::Standalone => "updates.tools.kind.standalone",
        ToolInstallKind::NpmPrefix => "updates.tools.kind.npm_prefix",
        ToolInstallKind::Runtime => "updates.tools.kind.runtime",
        ToolInstallKind::NpmGlobal => "updates.tools.kind.npm_global",
        ToolInstallKind::Homebrew => "updates.tools.kind.homebrew",
        ToolInstallKind::Winget => "updates.tools.kind.winget",
        ToolInstallKind::Managed => "updates.tools.kind.managed",
        ToolInstallKind::App => "updates.tools.kind.app",
        ToolInstallKind::SystemPackage => "updates.tools.kind.system",
        _ => "updates.tools.kind.other",
    }
}

/// Badge text and status of the tool's own updater.
fn updater_badge(cx: &Cx<'_>, u: &ToolUpdater) -> (String, Option<Status>) {
    match u {
        ToolUpdater::None => (cx.tr.t("updates.tools.updater.none"), None),
        ToolUpdater::Notifies => (cx.tr.t("updates.tools.updater.notifies"), None),
        ToolUpdater::Automatic => (cx.tr.t("updates.tools.updater.automatic"), None),
        ToolUpdater::Off { by } => (cx.tr.f("updates.tools.updater.off", &[("by", by)]), None),
        ToolUpdater::Superba => (cx.tr.t("updates.tools.updater.superba"), Some(Status::Ok)),
        ToolUpdater::Forced => (cx.tr.t("updates.tools.updater.forced"), Some(Status::Warn)),
        ToolUpdater::PackageManager => (cx.tr.t("updates.tools.updater.package_manager"), None),
        ToolUpdater::Blocked { by } => (
            cx.tr.f("updates.tools.updater.blocked", &[("by", by)]),
            Some(Status::Error),
        ),
        _ => (String::new(), None),
    }
}

/// The progress line of `p`.
fn progress_line(cx: &Cx<'_>, p: &ToolProgress) -> String {
    let detail = p.detail.clone().unwrap_or_default();
    match p.stage {
        ToolStage::Downloading => {
            let mb = |b: u64| format!("{:.1}", b as f64 / 1_000_000.0);
            let percent = p
                .fraction()
                .map(|f| format!("{:.0}%", f * 100.0))
                .unwrap_or_else(|| "?".into());
            cx.tr.f(
                "updates.tools.stage.downloading",
                &[
                    ("file", &detail),
                    ("done", &mb(p.done)),
                    ("total", &p.total.map(mb).unwrap_or_else(|| "?".into())),
                    ("percent", &percent),
                ],
            )
        }
        ToolStage::Resolving => cx
            .tr
            .f("updates.tools.stage.resolving", &[("detail", &detail)]),
        ToolStage::Verifying => cx
            .tr
            .f("updates.tools.stage.verifying", &[("detail", &detail)]),
        ToolStage::Extracting => cx
            .tr
            .f("updates.tools.stage.extracting", &[("detail", &detail)]),
        ToolStage::Installing => cx
            .tr
            .f("updates.tools.stage.installing", &[("detail", &detail)]),
        ToolStage::Removing => cx
            .tr
            .f("updates.tools.stage.removing", &[("detail", &detail)]),
        _ => cx
            .tr
            .f("updates.tools.stage.checking", &[("detail", &detail)]),
    }
}

fn updater_line(cx: &Cx<'_>, updater: &ToolUpdater) -> String {
    updater_badge(cx, updater).0
}
/// The label of a channel (`latest`, `v1` = "OpenCode 1", `pypi` = the
/// Node-free build).
pub(super) fn channel_label(cx: &Cx<'_>, tool: ToolId, channel: &str) -> String {
    match (tool, channel) {
        (ToolId::Opencode, "latest") => cx.tr.t("updates.tools.channel_oc2"),
        (ToolId::Opencode, "beta") => cx.tr.t("updates.tools.channel_oc2_beta"),
        (ToolId::Opencode, "v1") => cx.tr.t("updates.tools.channel_oc1"),
        (ToolId::Dsh, "pypi") => cx.tr.t("updates.tools.channel_pypi"),
        (_, c) => cx.tr.f("updates.tools.channel", &[("channel", &c)]),
    }
}
