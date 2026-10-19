//! One inventory and state mapping, shared by the list, filters and badges.
use super::{
    State,
    tools::{ToolId, ToolsView},
};
use crate::design::widgets::Status;
use crate::pages::{Cx, error_text};
use superba_core::{
    AppCodexPin, AppUpdateAction, CliUpdateAction, CodexInstallKind, CodexVersion, InstallTarget,
    SelfUpdateStatus, TargetId,
};

/// Stable row identity; remote ids are not user-facing host labels.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UpdateItem {
    /// Superba itself.
    Superba,
    /// ChatGPT desktop app.
    App,
    /// Codex selected for the desktop app.
    AppPin,
    /// All local CLI installs.
    CodexCli,
    /// One remote host.
    CodexRemote(TargetId),
    /// One coding tool.
    Tool(ToolId),
}
impl UpdateItem {
    /// Deep-link key.
    pub fn key(&self) -> String {
        match self {
            Self::Superba => "superba".into(),
            Self::App => "app".into(),
            Self::AppPin => "pin".into(),
            Self::CodexCli => "local".into(),
            Self::CodexRemote(id) => format!("remote:{id}"),
            Self::Tool(id) => format!("tool:{}", id.as_str()),
        }
    }
    /// Parse an explicit row key, or an app host id used by the Hosts page.
    pub fn parse(key: &str) -> Option<Self> {
        Some(match key {
            "superba" => Self::Superba,
            "app" => Self::App,
            "pin" => Self::AppPin,
            "local" => Self::CodexCli,
            _ if key.starts_with("tool:") => Self::Tool(
                ToolId::ALL
                    .into_iter()
                    .find(|id| key[5..] == *id.as_str())?,
            ),
            _ if key.starts_with("remote:") => Self::CodexRemote(TargetId::new(&key[7..])),
            _ if key.starts_with("ssh:") || key.starts_with("wsl:") => {
                Self::CodexRemote(TargetId::new(key))
            }
            _ => return None,
        })
    }
    /// Group order.
    pub fn group(&self) -> usize {
        match self {
            Self::Superba => 0,
            Self::Tool(_) => 2,
            _ => 1,
        }
    }
}
/// Inventory states; policy and reachability take precedence over new releases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateStatus {
    /// Installed latest known version.
    Current,
    /// An update can be installed here.
    Available,
    /// No installation was found.
    Missing,
    /// An operation is running.
    Installing,
    /// Last install/update failed.
    Failed,
    /// Vendor or package manager owns updates.
    Vendor,
    /// Policy or compatibility prevents updating.
    Blocked,
    /// A Superba release was explicitly skipped.
    Skipped,
    /// A remote host is not connected.
    Disconnected,
    /// No reliable version check result.
    Unknown,
}
impl UpdateStatus {
    /// Localized status label.
    pub fn key(self) -> &'static str {
        match self {
            Self::Current => "updates.list.current",
            Self::Available => "updates.list.available",
            Self::Missing => "updates.list.missing",
            Self::Installing => "updates.list.installing",
            Self::Failed => "updates.list.failed",
            Self::Vendor => "updates.list.vendor",
            Self::Blocked => "updates.list.blocked",
            Self::Skipped => "updates.superba.skipped",
            Self::Disconnected => "updates.remote.unreachable",
            Self::Unknown => "updates.list.unknown",
        }
    }
    /// Status dot semantics.
    pub fn tone(self) -> Status {
        match self {
            Self::Current => Status::Ok,
            Self::Available | Self::Blocked => Status::Warn,
            Self::Installing => Status::Busy,
            Self::Failed => Status::Error,
            _ => Status::Idle,
        }
    }
    /// The same predicate is used by the filter, rail and sidebar.
    pub fn actionable(self) -> bool {
        matches!(self, Self::Available | Self::Failed)
    }
}
/// The row's one operation; commands carry the exact version chosen at review.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operation {
    /// Install Superba after other queued updates finish.
    SelfUpdate,
    /// Download and verify an app package.
    AppDownload,
    /// Install an exact Codex version.
    Codex {
        /// Version.
        version: CodexVersion,
        /// Destination.
        target: InstallTarget,
    },
    /// Tool service install/update.
    Tool(ToolId),
    /// Install a managed copy beside an existing installation.
    ToolInstall(ToolId),
    /// Retry the online check.
    Check,
    /// Install the explicitly selected tool version.
    ToolVersion {
        /// Tool identity.
        id: ToolId,
        /// Exact version reviewed by the user.
        version: String,
    },
    /// Remove a tool, preserving its settings unless explicitly requested.
    ToolRemove {
        /// Tool identity.
        id: ToolId,
        /// Also remove the directories shown in the confirmation.
        remove_data: bool,
    },
    /// Expand the pin controls.
    Change,
    /// Clear a skipped Superba release.
    Unskip,
    /// Cooperatively cancel the row's operation.
    Cancel,
    /// Open a vendor download page; never part of an update batch.
    DownloadPage(String),
    /// Open a verified package.
    OpenInstaller(String),
}
/// Fraction and accessible stage/transfer description.
#[derive(Clone, Debug)]
pub struct Progress {
    /// None while resolving or verifying.
    pub fraction: Option<f32>,
    /// Already localized by the source.
    pub label: String,
}
/// One complete renderable row, independent of an egui frame.
#[derive(Clone, Debug)]
pub struct Entry {
    /// Stable identity.
    pub id: UpdateItem,
    /// Localized/product name.
    pub name: String,
    /// Installed version, if known.
    pub current: Option<String>,
    /// Offered version, if known.
    pub latest: Option<String>,
    /// State.
    pub status: UpdateStatus,
    /// Description or full error (never lost to visual ellipsis).
    pub description: String,
    /// One operation only.
    pub operation: Option<Operation>,
    /// Transfer/stage progress.
    pub progress: Option<Progress>,
    /// Confirmed together before a batch.
    pub cautions: Vec<String>,
}
impl Entry {
    /// Empty version facts and no implied operation.
    pub fn new(id: UpdateItem, name: impl Into<String>, status: UpdateStatus) -> Self {
        Self {
            id,
            name: name.into(),
            current: None,
            latest: None,
            status,
            description: String::new(),
            operation: None,
            progress: None,
            cautions: vec![],
        }
    }
    /// Description builder.
    pub fn description(mut self, text: impl Into<String>) -> Self {
        self.description = text.into();
        self
    }
    /// Display known versions without implying an unknown installed version.
    pub fn versions(&self) -> String {
        let current = self.current.as_deref().unwrap_or("—");
        match self.latest.as_deref().filter(|v| *v != current) {
            Some(latest) => format!("{current} → {latest}"),
            None => current.into(),
        }
    }
    /// Row is eligible for a batch, not a check retry or a vendor download link.
    pub fn batchable(&self) -> bool {
        self.status.actionable()
            && matches!(
                self.operation,
                Some(
                    Operation::SelfUpdate
                        | Operation::AppDownload
                        | Operation::Codex { .. }
                        | Operation::Tool(_)
                )
            )
    }
    fn failure(&mut self, error: Option<&String>) {
        if let Some(error) = error {
            self.status = UpdateStatus::Failed;
            self.description.clone_from(error);
        }
    }
    fn running(&mut self, cancel: bool, progress: Option<Progress>) {
        self.status = UpdateStatus::Installing;
        self.operation = cancel.then_some(Operation::Cancel);
        self.progress = Some(progress.unwrap_or_else(|| Progress {
            fraction: None,
            label: self.description.clone(),
        }));
    }
}
/// Construct all visible rows from the version services and the tool adapter.
pub fn inventory(cx: &Cx<'_>, state: &State, tools: &ToolsView) -> Vec<Entry> {
    let mut entries = Vec::new();
    let own = cx.data.selfupdate.value.as_ref();
    let mut own_row = Entry::new(UpdateItem::Superba, "Superba", UpdateStatus::Unknown);
    own_row.current = Some(
        own.map_or(env!("CARGO_PKG_VERSION"), |v| v.current.as_str())
            .into(),
    );
    own_row.operation = Some(Operation::Check);
    own_row.description = cx.tr.t("updates.superba.not_checked");
    if let Some(v) = own {
        own_row.latest = v.latest.as_ref().map(|v| v.version.clone());
        own_row.description = super::self_update::channel_line(cx.tr, v);
        own_row.status = match v.status {
            SelfUpdateStatus::Available if v.is_skipped() => UpdateStatus::Skipped,
            SelfUpdateStatus::Available if v.no_package => UpdateStatus::Blocked,
            SelfUpdateStatus::Available => UpdateStatus::Available,
            SelfUpdateStatus::UpToDate => UpdateStatus::Current,
            _ => UpdateStatus::Unknown,
        };
        own_row.operation = match own_row.status {
            UpdateStatus::Available => Some(Operation::SelfUpdate),
            UpdateStatus::Skipped => Some(Operation::Unskip),
            UpdateStatus::Unknown => Some(Operation::Check),
            _ => None,
        };
        if v.status == SelfUpdateStatus::OnlineChecksOff {
            own_row.description = cx.tr.t("superba.network.online_off");
        }
        if let Some(detail) = &v.detail {
            own_row.description.clone_from(detail);
        }
        if v.no_package {
            own_row.description = cx.tr.t("updates.superba.no_package");
        }
        if let Some(problem) = &v.problem
            && !problem.installed
            && !v.no_package
            && state.superba.result.is_none()
        {
            own_row.status = UpdateStatus::Failed;
            own_row.description = cx.localize(&problem.reason);
            own_row.operation = v.latest.as_ref().map(|_| Operation::SelfUpdate);
        }
    }
    own_row.failure(state.superba.result.as_ref().and_then(|r| r.as_ref().err()));
    if own_row.status == UpdateStatus::Failed
        && own.is_some_and(|v| v.latest.is_some() && !v.no_package)
    {
        own_row.operation = Some(Operation::SelfUpdate);
    }
    if state.superba.busy() {
        own_row.description = control_description(cx, state.superba.control.as_deref());
        own_row.running(
            state
                .superba
                .control
                .as_ref()
                .is_some_and(|c| c.can_cancel()),
            None,
        );
    }
    own_row.cautions.push(cx.tr.t("updates.superba.consent"));
    entries.push(own_row);
    let view = state
        .view
        .value
        .as_ref()
        .or(cx.data.versions.value.as_ref());
    if let Some(v) = view {
        let mut app = Entry::new(
            UpdateItem::App,
            cx.tr.t("updates.app.title"),
            UpdateStatus::Unknown,
        );
        app.current = v.app.install.as_ref().and_then(|i| i.app_version.clone());
        app.latest = v.app.latest.as_ref().map(|l| l.version.clone());
        let vendor = matches!(
            v.app.action,
            Some(
                AppUpdateAction::MacSparkle { .. }
                    | AppUpdateAction::WindowsStore { .. }
                    | AppUpdateAction::WindowsPackage { .. }
            )
        );
        app.status = if v.app.install.is_none() {
            UpdateStatus::Missing
        } else if vendor {
            UpdateStatus::Vendor
        } else if v.app.update_available {
            UpdateStatus::Available
        } else if app.latest.is_some() {
            UpdateStatus::Current
        } else {
            UpdateStatus::Unknown
        };
        app.description = cx.tr.t(if vendor {
            "updates.list.app_vendor"
        } else {
            "updates.app.linux_note"
        });
        app.operation = if vendor {
            None
        } else if matches!(v.app.action, Some(AppUpdateAction::LinuxPackage { .. }))
            && matches!(app.status, UpdateStatus::Available | UpdateStatus::Missing)
        {
            Some(Operation::AppDownload)
        } else if app.status == UpdateStatus::Unknown {
            Some(Operation::Check)
        } else {
            None
        };
        app.failure(
            state
                .download_error
                .as_ref()
                .or(state.windows.error.as_ref()),
        );
        if app.status == UpdateStatus::Failed {
            app.operation = Some(Operation::AppDownload);
        }
        if let Some(report) = &state.downloaded {
            app.description = cx.tr.t("updates.list.package_ready");
            app.operation = Some(Operation::OpenInstaller(report.path.display().to_string()));
        }
        if state.download.is_some() || state.windows.busy() {
            let progress = state.windows.progress().map(|p| Progress {
                fraction: p.fraction(),
                label: super::windows_app::progress_line(cx, &p),
            });
            app.running(
                state.windows.downloading()
                    || state
                        .download_control
                        .as_ref()
                        .is_some_and(|c| c.can_cancel()),
                progress,
            );
        }
        app.cautions.push(cx.tr.t("updates.list.app_caution"));
        entries.push(app);
        let mut pin = Entry::new(
            UpdateItem::AppPin,
            cx.tr.t("updates.pin.title"),
            UpdateStatus::Vendor,
        );
        pin.current = super::pin_marks(v).current.map(|v| v.to_string());
        pin.description = cx.tr.t(match v.app_codex.pin {
            AppCodexPin::Bundled => "updates.pin.bundled_unknown",
            AppCodexPin::Version { .. } => "updates.pin.version",
            _ => "updates.pin.path",
        });
        if v.app_codex.supported {
            pin.operation = Some(Operation::Change);
        }
        if let Some(error) = &v.app_codex.mismatch {
            pin.description = cx.localize(error);
            pin.status = UpdateStatus::Blocked;
        }
        entries.push(pin);
        let candidates = super::picker_items(v, false, false, &super::Marks::default());
        let latest = candidates.first().map(|c| c.version.clone());
        let local = v
            .cli
            .iter()
            .find(|r| {
                matches!(r.action, CliUpdateAction::Install { .. })
                    || r.kind == CodexInstallKind::Standalone
            })
            .or(v.cli.first());
        let mut cli = Entry::new(
            UpdateItem::CodexCli,
            cx.tr.t("updates.list.cli"),
            UpdateStatus::Unknown,
        );
        cli.current = local
            .and_then(|r| r.binary.version.as_ref())
            .map(ToString::to_string);
        cli.latest = local
            .and_then(|r| r.latest.as_ref())
            .or(latest.as_ref())
            .map(ToString::to_string);
        let target_version = local
            .and_then(|r| match &r.action {
                CliUpdateAction::Install { version } => Some(version.clone()),
                _ => None,
            })
            .or(latest.clone());
        let managed = local.is_none_or(|r| {
            r.kind == CodexInstallKind::Standalone
                || matches!(r.action, CliUpdateAction::Install { .. })
        });
        cli.status = if local.is_none() {
            UpdateStatus::Missing
        } else if !managed {
            UpdateStatus::Vendor
        } else if local.is_some_and(|r| matches!(r.action, CliUpdateAction::Install { .. })) {
            UpdateStatus::Available
        } else if cli.latest.is_some() {
            UpdateStatus::Current
        } else {
            UpdateStatus::Unknown
        };
        if let Some(row) = local {
            cli.description = format!(
                "{} · {}",
                cx.localize(&row.kind.text()),
                superba_core::paths::display_path(&row.binary.path)
            );
        }
        cli.failure(state.results.get("local").and_then(|r| r.as_ref().err()));
        if managed
            && matches!(
                cli.status,
                UpdateStatus::Available | UpdateStatus::Failed | UpdateStatus::Missing
            )
        {
            cli.operation = target_version.map(|version| Operation::Codex {
                version,
                target: InstallTarget::Local,
            });
        } else if cli.status == UpdateStatus::Unknown {
            cli.operation = Some(Operation::Check);
        }
        if state.installing(&InstallTarget::Local) {
            cli.description =
                control_description(cx, state.controls.get("local").map(|c| c.as_ref()));
            cli.running(
                state.controls.get("local").is_some_and(|c| c.can_cancel()),
                None,
            );
        }
        entries.push(cli);
        for host in &v.remote {
            let target = InstallTarget::Remote {
                target_id: host.target_id.clone(),
            };
            let mut row = Entry::new(
                UpdateItem::CodexRemote(host.target_id.clone()),
                format!("Codex · {}", host.label),
                UpdateStatus::Unknown,
            );
            row.current = host.version.as_ref().map(ToString::to_string);
            row.latest = latest.as_ref().map(ToString::to_string);
            row.status = if host.error.is_some() {
                UpdateStatus::Disconnected
            } else if host.install_kind == Some(CodexInstallKind::Other) && host.version.is_some() {
                UpdateStatus::Vendor
            } else if latest.as_ref().is_some_and(|l| {
                v.app_codex
                    .floor
                    .as_ref()
                    .is_some_and(|floor| !l.meets(floor))
            }) {
                UpdateStatus::Blocked
            } else if host.version.is_none() {
                UpdateStatus::Missing
            } else if latest
                .as_ref()
                .is_some_and(|l| host.version.as_ref().is_some_and(|c| l > c))
            {
                UpdateStatus::Available
            } else if latest.is_some() {
                UpdateStatus::Current
            } else {
                UpdateStatus::Unknown
            };
            if matches!(
                row.status,
                UpdateStatus::Disconnected | UpdateStatus::Blocked
            ) {
                row.latest = None;
            }
            row.description = host
                .error
                .as_ref()
                .map(|e| error_text(cx.tr, e))
                .unwrap_or_else(|| cx.tr.t("updates.remote.note"));
            if row.status != UpdateStatus::Disconnected {
                row.failure(
                    state
                        .results
                        .get(&super::install_key(&target))
                        .and_then(|r| r.as_ref().err()),
                );
            }
            if matches!(
                row.status,
                UpdateStatus::Available | UpdateStatus::Missing | UpdateStatus::Failed
            ) {
                row.operation = latest.clone().map(|version| Operation::Codex {
                    version,
                    target: target.clone(),
                });
            } else if row.status == UpdateStatus::Unknown {
                row.operation = Some(Operation::Check);
            }
            if state.installing(&target) {
                let c = state.controls.get(&super::install_key(&target));
                row.description = control_description(cx, c.map(|c| c.as_ref()));
                row.running(c.is_some_and(|c| c.can_cancel()), None);
            }
            entries.push(row);
        }
    } else {
        for (id, key) in [
            (UpdateItem::App, "updates.app.title"),
            (UpdateItem::AppPin, "updates.pin.title"),
            (UpdateItem::CodexCli, "updates.list.cli"),
        ] {
            let mut row = Entry::new(id, cx.tr.t(key), UpdateStatus::Unknown)
                .description(cx.tr.t("updates.loading"));
            row.operation = Some(Operation::Check);
            if let Some(e) = &state.view.error {
                row.description = error_text(cx.tr, e);
            }
            entries.push(row);
        }
    }
    entries.extend(tools.rows.iter().map(|r| r.entry.clone()));
    entries
}

fn control_description(cx: &Cx<'_>, control: Option<&superba_core::UpdateControl>) -> String {
    cx.tr.t(if control.is_some_and(|c| c.cancelled()) {
        "updates.list.cancelling"
    } else if control.is_some_and(|c| c.committing()) {
        "updates.list.committing"
    } else {
        "updates.downloading"
    })
}
