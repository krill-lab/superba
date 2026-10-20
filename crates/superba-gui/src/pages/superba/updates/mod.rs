//! Unified inventory for Superba, ChatGPT, Codex and coding tools.
pub mod items;
pub mod panels;
mod queue;
pub mod rows;
pub mod self_update;
pub mod tools;
pub mod windows_app;

use crate::design::widgets::Status;
use crate::pages::router::db::Loader;
use crate::pages::{Action, Cx, PageStatus, error_text};
use crate::runtime::ReadTask;
use egui::Ui;
pub use items::{UpdateItem, UpdateStatus};
use panels::InstallPicker;
pub use panels::{
    Marks, PIN_PICKER, PickerItem, PinChoice, VersionTag, app_line, app_updated, check_path,
    compat_badge, install_key, is_prerelease, local_marks, mismatch_paths, picker_items, pin_for,
    pin_marks, pin_request, remote_marks, row_action, tags_of, update_available, version_tags,
};
pub use queue::{Batch, Queue};
use std::collections::{BTreeMap, BTreeSet};
use superba_core::paths::display_path;
use superba_core::{
    AppCodexPin, AppDownloadReport, CliUpdateAction, CodexVersion, InstallReport, InstallRequest,
    InstallTarget, VersionsView,
};

/// Page-local state, kept across navigation.
#[derive(Debug, Default)]
pub struct State {
    /// The versions.
    pub view: Loader<VersionsView>,
    check_now: bool,
    /// Pin being edited.
    pub choice: PinChoice,
    /// Version chosen for [`PinChoice::Version`].
    pub version: Option<CodexVersion>,
    /// Path typed for [`PinChoice::Path`].
    pub path: String,
    /// "Include pre-releases" of each version picker, by [`PIN_PICKER`] or
    /// [`install_key`] (unset: see [`State::shows_prerelease`]).
    pub prerelease: BTreeMap<String, bool>,
    /// Version picked in each install picker, by [`install_key`] (unset or
    /// filtered out: the newest offered).
    pub picks: BTreeMap<String, CodexVersion>,
    pin_loaded: bool,
    persist_task: Option<ReadTask<()>>,
    /// Persist value saved but not yet visible in the profile set.
    pub persist_pending: Option<bool>,
    installs: BTreeMap<String, ReadTask<InstallReport>>,
    controls: BTreeMap<String, std::sync::Arc<superba_core::UpdateControl>>,
    download_control: Option<std::sync::Arc<superba_core::UpdateControl>>,
    /// Results of installs by [`install_key`] (`Err` = translated error).
    pub results: BTreeMap<String, Result<InstallReport, String>>,
    download: Option<ReadTask<AppDownloadReport>>,
    /// The downloaded, verified app package.
    pub downloaded: Option<AppDownloadReport>,
    /// Why the download failed (translated).
    pub download_error: Option<String>,
    /// Remote row to bring into view.
    pub focus: Option<UpdateItem>,
    /// Expanded rows, retained across navigation.
    pub expanded: BTreeSet<UpdateItem>,
    /// Inventory filter.
    pub filter: rows::Filter,
    /// Tool installer boundary.
    pub tools: tools::Source,
    /// Explicit tool version selected in each expansion panel.
    pub tool_versions: BTreeMap<tools::ToolId, String>,
    /// Reviewed operations awaiting confirmation.
    pub confirm: Option<Batch>,
    /// Sequential installation queue.
    pub queue: Queue,
    /// App version seen when the page first loaded (a later different one means
    /// the app was updated while Superba ran).
    pub first_app_version: Option<String>,
    /// Superba's own update (the first section).
    pub superba: self_update::SelfState,
    /// The Windows installer package of the app.
    pub windows: windows_app::WinState,
    toasts: Vec<(Status, String)>,
    /// The page's view was fetched online or an install finished: the shell
    /// reloads its own copy (the sidebar badge and the Overview read it).
    notify_shell: bool,
    refreshing: bool,
}

impl State {
    /// Check online now.
    pub fn check(&mut self) {
        self.tools.check();
        self.check_now = true;
        self.view.invalidate();
    }

    /// Whether picker `key` offers pre-releases. Until its box is ticked or
    /// cleared, only when the picker's `current` version is a pre-release.
    pub fn shows_prerelease(&self, key: &str, current: Option<&CodexVersion>) -> bool {
        self.prerelease
            .get(key)
            .copied()
            .unwrap_or_else(|| current.is_some_and(|v| v.pre.is_some()))
    }

    /// The version install picker `key` targets among `items` (newest first):
    /// the one picked, else the newest.
    pub fn pick(&self, key: &str, items: &[PickerItem]) -> Option<CodexVersion> {
        self.picks
            .get(key)
            .filter(|v| items.iter().any(|i| &i.version == *v))
            .or_else(|| items.first().map(|i| &i.version))
            .cloned()
    }

    /// Something is running.
    pub fn busy(&self) -> bool {
        self.queue.busy()
            || self.view.loading()
            || self.persist_task.is_some()
            || !self.installs.is_empty()
            || self.download.is_some()
            || self.superba.busy()
            || self.windows.busy()
    }

    /// An install for `target` is running.
    pub fn installing(&self, target: &InstallTarget) -> bool {
        self.installs.contains_key(&install_key(target))
    }

    /// Install `version` at `target` (background).
    pub fn install(&mut self, cx: &Cx<'_>, version: CodexVersion, target: InstallTarget) {
        let key = install_key(&target);
        if self.installs.contains_key(&key) {
            return;
        }
        self.results.remove(&key);
        let control = std::sync::Arc::new(superba_core::UpdateControl::default());
        self.controls.insert(key.clone(), control.clone());
        let service = cx.service.clone();
        self.installs.insert(
            key,
            crate::runtime::task(async move {
                service
                    .install_codex_controlled(InstallRequest { version, target }, control)
                    .await
            }),
        );
    }

    /// Switch `target` back to the release its last install replaced
    /// (background; the result replaces the install result).
    pub fn rollback(&mut self, cx: &Cx<'_>, target: InstallTarget) {
        let key = install_key(&target);
        if self.installs.contains_key(&key) {
            return;
        }
        self.results.remove(&key);
        let service = cx.service.clone();
        self.installs.insert(
            key,
            crate::runtime::task(async move { service.rollback_codex(target).await }),
        );
    }

    /// Download the app update (background).
    pub fn download(&mut self, cx: &Cx<'_>) {
        if self.download.is_some() {
            return;
        }
        self.download_error = None;
        let service = cx.service.clone();
        let control = std::sync::Arc::new(superba_core::UpdateControl::default());
        self.download_control = Some(control.clone());
        self.download = Some(crate::runtime::task(async move {
            // Only a verified cache file is written here. Cancelling this future
            // never interrupts a live binary replacement or system installer.
            let download = service.download_app_update();
            tokio::pin!(download);
            loop {
                tokio::select! {
                    result = &mut download => break result,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(80)) => control.check()?,
                }
            }
        }));
    }

    pub(crate) fn poll(&mut self, cx: &Cx<'_>) {
        let finished = self.view.poll();
        if finished && std::mem::take(&mut self.refreshing) {
            self.notify_shell = true;
        }
        if finished
            && let Some(v) = &self.view.value
            && self.first_app_version.is_none()
        {
            self.first_app_version = v.app.install.as_ref().and_then(|i| i.app_version.clone());
        }
        if let Some(v) = &self.view.value
            && !self.pin_loaded
        {
            self.pin_loaded = true;
            match &v.app_codex.pin {
                AppCodexPin::Version { version } => {
                    self.choice = PinChoice::Version;
                    self.version = Some(version.clone());
                }
                AppCodexPin::Path { path } => {
                    self.choice = PinChoice::Path;
                    self.path = display_path(path);
                }
                _ => self.choice = PinChoice::Bundled,
            }
        }
        let done: Vec<String> = self
            .installs
            .iter()
            .filter_map(|(k, t)| t.poll().map(|r| (k.clone(), r)))
            .map(|(k, r)| {
                if matches!(r, Err(superba_core::Error::Cancelled)) {
                    self.results.remove(&k);
                    self.toasts
                        .push((Status::Idle, cx.tr.t("updates.win.cancelled")));
                    return k;
                }
                let ok = r.is_ok();
                let text = match &r {
                    Ok(rep) => cx.tr.f("updates.installed", &[("version", &rep.version)]),
                    Err(e) => error_text(cx.tr, e),
                };
                self.toasts
                    .push((if ok { Status::Ok } else { Status::Error }, text));
                self.results
                    .insert(k.clone(), r.map_err(|e| error_text(cx.tr, &e)));
                k
            })
            .collect();
        for k in &done {
            self.installs.remove(k);
            self.controls.remove(k);
        }
        if !done.is_empty() {
            self.view.invalidate();
            self.notify_shell = true;
        }
        if let Some(t) = &self.download
            && let Some(r) = t.poll()
        {
            self.download = None;
            self.download_control = None;
            match r {
                Ok(rep) => self.downloaded = Some(rep),
                Err(superba_core::Error::Cancelled) => self
                    .toasts
                    .push((Status::Idle, cx.tr.t("updates.win.cancelled"))),
                Err(e) => self.download_error = Some(error_text(cx.tr, &e)),
            }
        }
        if let Some(t) = &self.persist_task
            && let Some(r) = t.poll()
        {
            self.persist_task = None;
            if let Err(e) = r {
                self.persist_pending = None;
                self.toasts.push((Status::Error, error_text(cx.tr, &e)));
            }
        }
    }

    fn load(&mut self, cx: &Cx<'_>) {
        if self.view.due(None) {
            let refresh = std::mem::take(&mut self.check_now);
            self.refreshing |= refresh;
            let service = cx.service.clone();
            self.view
                .start(async move { service.versions(refresh).await });
        }
    }

    /// The persisted "also outside Superba" choice.
    pub fn persist(&self, cx: &Cx<'_>) -> bool {
        self.persist_pending.unwrap_or_else(|| {
            cx.data
                .profiles
                .value
                .as_ref()
                .is_some_and(|p| p.settings.app_codex.persist_env)
        })
    }

    fn set_persist(&mut self, cx: &Cx<'_>, on: bool) {
        let Some(set) = cx.data.profiles.value.as_ref() else {
            return;
        };
        let mut settings = set.settings.clone();
        settings.app_codex.persist_env = on;
        self.persist_pending = Some(on);
        let service = cx.service.clone();
        self.persist_task = Some(crate::runtime::task(async move {
            service.save_settings(settings).await
        }));
    }
}

/// Run a command addressed to this page: `check`, `update_cli` (update the
/// first updatable CLI), `focus:<target id>` (bring a remote row into view).
pub fn command(state: &mut State, cx: &Cx<'_>, command: &str, out: &mut Vec<Action>) {
    match command {
        "check" => {
            state.check();
            out.push(Action::CheckSelfUpdate);
        }
        "check_superba" => out.push(Action::CheckSelfUpdate),
        "update_cli" if !state.queue.busy() => {
            // The version the page's local picker targets, when it is newer.
            let version = state.view.value.as_ref().and_then(|v| {
                let key = install_key(&InstallTarget::Local);
                let target = InstallPicker::new(state, v, key, &local_marks(v)).target;
                v.cli
                    .iter()
                    .find_map(|r| match row_action(r, target.as_ref()) {
                        CliUpdateAction::Install { version }
                            if r.binary.version.as_ref().is_none_or(|h| &version > h) =>
                        {
                            Some(version)
                        }
                        _ => None,
                    })
            });
            if let Some(version) = version {
                state.install(cx, version, InstallTarget::Local);
            }
        }
        _ => {
            if let Some(id) = command.strip_prefix("focus:") {
                state.focus = UpdateItem::parse(id);
                state.filter = rows::Filter::All;
            }
        }
    }
}

/// Dialogs and the sidebar badge (an update is available).
pub fn status(state: &State) -> PageStatus {
    // The sidebar dot comes from the shell's own version data
    // (`app::shell_badges`), one source for every page.
    PageStatus {
        modal: state.confirm.is_some(),
        ..Default::default()
    }
}

/// Render the inventory in the Superba detail area.
pub fn show(ui: &mut Ui, cx: &Cx<'_>, state: &mut State, out: &mut Vec<Action>) {
    rows::show(ui, cx, state, out);
}
impl State {
    /// Poll on every page so a batch continues while the user navigates away.
    pub fn background(&mut self, cx: &Cx<'_>, out: &mut Vec<Action>) {
        self.tools.poll(cx, out);
        self.poll(cx);
        self.windows.poll(cx);
        self.load(cx);
        if let Some(toast) = self.superba.poll(cx, out) {
            self.toasts.push(toast);
        }
        for (status, text) in self.toasts.drain(..) {
            out.push(Action::Toast { status, text });
        }
        if std::mem::take(&mut self.notify_shell) {
            out.push(Action::LoadVersions);
        }
        if self.persist_pending.is_some()
            && self.persist_task.is_none()
            && cx
                .data
                .profiles
                .value
                .as_ref()
                .is_some_and(|p| Some(p.settings.app_codex.persist_env) == self.persist_pending)
        {
            self.persist_pending = None;
        }
        self.advance_queue(cx, out);
    }
    /// An operation is still running, including a cancellation not yet acknowledged.
    pub fn operation_busy(&self, cx: &Cx<'_>) -> bool {
        !self.installs.is_empty()
            || self.download.is_some()
            || self.superba.busy()
            || self.windows.busy()
            || self
                .tools
                .view(cx)
                .rows
                .iter()
                .any(|r| r.entry.status == UpdateStatus::Installing)
    }
    /// Running status for a particular queued job.
    pub fn running(&self, cx: &Cx<'_>, item: &UpdateItem) -> bool {
        match item {
            UpdateItem::Superba => self.superba.busy(),
            UpdateItem::App => self.download.is_some() || self.windows.busy(),
            UpdateItem::CodexCli => self.installing(&InstallTarget::Local),
            UpdateItem::CodexRemote(id) => self.installing(&InstallTarget::Remote {
                target_id: id.clone(),
            }),
            UpdateItem::Tool(_) => self
                .tools
                .view(cx)
                .rows
                .iter()
                .any(|r| &r.entry.id == item && r.entry.status == UpdateStatus::Installing),
            UpdateItem::AppPin => false,
        }
    }
    /// Advance only after the active backend operation has ended.
    pub fn advance_queue(&mut self, cx: &Cx<'_>, out: &mut Vec<Action>) {
        let active = self
            .queue
            .active()
            .is_some_and(|item| self.running(cx, item));
        let busy = self.operation_busy(cx);
        if let Some(entry) = self.queue.next(active, busy)
            && let Some(operation) = &entry.operation
        {
            if *operation == items::Operation::SelfUpdate {
                self.superba.install_version(cx, entry.latest.clone());
            } else {
                self.perform(cx, &entry.id, operation, out);
            }
        }
    }
    /// Stop scheduling remaining jobs and request cancellation of a cancellable active job.
    pub fn cancel_queue(&mut self) {
        self.queue.cancel_pending();
        if let Some(item) = self.queue.active().cloned() {
            self.cancel(&item);
        }
    }
    fn cancel(&mut self, item: &UpdateItem) {
        match item {
            UpdateItem::Superba => self.superba.cancel(),
            UpdateItem::App => {
                self.windows.cancel();
                if let Some(c) = &self.download_control {
                    c.cancel();
                }
            }
            UpdateItem::Tool(id) => self.tools.cancel(*id),
            UpdateItem::CodexCli | UpdateItem::CodexRemote(_) => {
                let key = item.key();
                if let Some(c) = self.controls.get(&key) {
                    c.cancel();
                }
            }
            _ => {}
        }
    }
    /// Execute a reviewed operation. Expanding or checking never installs anything.
    pub fn perform(
        &mut self,
        cx: &Cx<'_>,
        item: &UpdateItem,
        operation: &items::Operation,
        out: &mut Vec<Action>,
    ) {
        use items::Operation;
        match operation {
            Operation::SelfUpdate => self.superba.install(cx),
            Operation::AppDownload => {
                if self
                    .view
                    .value
                    .as_ref()
                    .or(cx.data.versions.value.as_ref())
                    .is_some_and(|v| {
                        matches!(
                            v.app.action,
                            Some(superba_core::AppUpdateAction::WindowsPackage { .. })
                        )
                    })
                {
                    self.windows.start(cx);
                } else {
                    self.download(cx);
                }
            }
            Operation::Codex { version, target } => {
                self.install(cx, version.clone(), target.clone())
            }
            Operation::Tool(id) | Operation::ToolInstall(id) => {
                if let Err(text) = self.tools.start(*id) {
                    out.push(Action::Toast {
                        status: Status::Error,
                        text,
                    });
                }
            }
            Operation::ToolVersion { id, version } => {
                if let Err(text) = self.tools.install_version(*id, version.clone()) {
                    out.push(Action::Toast {
                        status: Status::Error,
                        text,
                    });
                }
            }
            Operation::ToolRemove { id, remove_data } => {
                if let Err(text) = self.tools.remove(*id, *remove_data) {
                    out.push(Action::Toast {
                        status: Status::Error,
                        text,
                    });
                }
            }
            Operation::Check => {
                self.check();
                out.push(Action::CheckSelfUpdate);
            }
            Operation::Change => {
                self.expanded.insert(item.clone());
                self.focus = Some(item.clone());
            }
            Operation::Unskip => out.push(Action::SkipSelfUpdate(None)),
            Operation::Cancel => self.cancel(item),
            Operation::OpenInstaller(path) | Operation::DownloadPage(path) => {
                out.push(Action::OpenUrl(path.clone()))
            }
        }
    }
}
