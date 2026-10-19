//! Tool inventory boundary. The installer adapter supplies snapshots and owns
//! cooperative cancellation; fixtures implement the same boundary without I/O.
mod adapter;
pub use adapter::ServiceTools;

use super::items::{Entry, Operation, UpdateItem, UpdateStatus};
use crate::design::widgets::{Button, Dropdown, Status, Text, Toggle};
use crate::pages::{Action, Cx, callout};
use egui::Ui;
use std::fmt::Debug;

/// Stable tool identities, matching the installer service's inventory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToolId {
    /// Claude Code CLI.
    ClaudeCode,
    /// DeepSeek Harness.
    Dsh,
    /// OpenCode.
    Opencode,
    /// pi coding agent.
    Pi,
    /// ZCode app.
    Zcode,
    /// WorkBuddy China.
    Workbuddy,
    /// WorkBuddy international.
    WorkbuddyAi,
}
impl ToolId {
    /// Display order.
    pub const ALL: [Self; 7] = [
        Self::ClaudeCode,
        Self::Dsh,
        Self::Opencode,
        Self::Pi,
        Self::Zcode,
        Self::Workbuddy,
        Self::WorkbuddyAi,
    ];
    /// Stable deep-link id.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Dsh => "dsh",
            Self::Opencode => "opencode",
            Self::Pi => "pi",
            Self::Zcode => "zcode",
            Self::Workbuddy => "workbuddy",
            Self::WorkbuddyAi => "workbuddy-ai",
        }
    }
    /// Corresponding service identity.
    fn core(self) -> superba_core::ToolId {
        match self {
            Self::ClaudeCode => superba_core::ToolId::ClaudeCode,
            Self::Dsh => superba_core::ToolId::Dsh,
            Self::Opencode => superba_core::ToolId::Opencode,
            Self::Pi => superba_core::ToolId::Pi,
            Self::Zcode => superba_core::ToolId::Zcode,
            Self::Workbuddy => superba_core::ToolId::Workbuddy,
            Self::WorkbuddyAi => superba_core::ToolId::WorkbuddyAi,
        }
    }
    /// Product name.
    pub fn name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Dsh => "dsh",
            Self::Opencode => "opencode",
            Self::Pi => "pi",
            Self::Zcode => "ZCode",
            Self::Workbuddy => "WorkBuddy",
            Self::WorkbuddyAi => "WorkBuddy AI",
        }
    }
    /// Legible two-letter icon.
    pub fn monogram(self) -> &'static str {
        match self {
            Self::ClaudeCode => "cc",
            Self::Dsh => "ds",
            Self::Opencode => "oc",
            Self::Pi => "pi",
            Self::Zcode => "zc",
            Self::Workbuddy | Self::WorkbuddyAi => "wb",
        }
    }
}

/// A normalized tool snapshot, including its expansion panel.
#[derive(Clone, Debug)]
pub struct ToolRow {
    /// Row identity, state, versions and its one operation.
    pub entry: Entry,
    /// All discovered install locations and methods (already localized).
    pub installs: Vec<String>,
    /// Available channels.
    pub channels: Vec<String>,
    /// Current channel.
    pub channel: String,
    /// Whether Superba manages the updater; None if not controllable.
    pub managed: Option<bool>,
    /// Cautions for installing an explicitly chosen version.
    pub install_cautions: Vec<String>,
    /// Full explanation, PATH notes, vendor policy and last operation log.
    pub notes: Vec<String>,
    /// Log path, if present.
    pub log: Option<String>,
    /// Explicit versions offered by the installer; empty when unsupported.
    pub versions: Vec<String>,
    /// Removal policy; None when Superba cannot remove this installation.
    pub removal: Option<Removal>,
}
/// Facts reviewed before removing a tool.
#[derive(Clone, Debug)]
pub struct Removal {
    /// Localized warnings from the installer.
    pub cautions: Vec<String>,
    /// Settings/data directories that can optionally be removed.
    pub data_dirs: Vec<String>,
}
/// One atomic snapshot of the tool service.
#[derive(Clone, Debug, Default)]
pub struct ToolsView {
    /// Supported tools in display order.
    pub rows: Vec<ToolRow>,
    /// Inventory-wide notices such as disabled online checks or WSL scope.
    pub notes: Vec<String>,
    /// Tools without a build on this platform; excluded from the list.
    pub unavailable: usize,
    /// An online check is running.
    pub checking: bool,
    /// A failed inventory read (keeps the previous rows).
    pub error: Option<String>,
}
/// Adapter contract for the installer's ToolsView and tool operations. Reads
/// must be cheap, nonblocking snapshots; operations must never block egui.
pub trait ToolsSource: Debug + Send + Sync {
    /// Localized snapshot.
    fn view(&self, cx: &Cx<'_>) -> ToolsView;
    /// Collect completed operations and their notifications on every page.
    fn poll(&self, _cx: &Cx<'_>, _out: &mut Vec<Action>) {}
    /// Check every tool online.
    fn check(&self);
    /// Start the row's install/update after its cautions were confirmed.
    fn start(&self, id: ToolId) -> Result<(), String>;
    /// Install an exact version selected in the detail panel.
    fn install_version(&self, id: ToolId, version: String) -> Result<(), String>;
    /// Remove only the tool unless removal of its data was explicitly confirmed.
    fn remove(&self, id: ToolId, remove_data: bool) -> Result<(), String>;
    /// Request cooperative cancellation (the row stays busy until it ends).
    fn cancel(&self, id: ToolId);
    /// Save a tool channel.
    fn channel(&self, id: ToolId, channel: String) -> Result<(), String>;
    /// Change ownership of automatic updates.
    fn managed(&self, id: ToolId, enabled: bool) -> Result<(), String>;
}
/// Tool detail body, deliberately independent of page headers or scroll areas.
pub fn section(
    ui: &mut Ui,
    cx: &Cx<'_>,
    source: &dyn ToolsSource,
    row: &ToolRow,
    selected: &mut String,
    out: &mut Vec<Action>,
) -> Option<Entry> {
    let UpdateItem::Tool(id) = row.entry.id else {
        return None;
    };
    let mut request = None;
    for install in &row.installs {
        ui.add(egui::Label::new(Text::mono(install, cx.pal)).wrap());
    }
    ui.add_enabled_ui(row.entry.status != UpdateStatus::Installing, |ui| {
        if !row.channels.is_empty() {
            let mut channel = row.channel.clone();
            let options: Vec<_> = row
                .channels
                .iter()
                .map(|ch| (ch.clone(), adapter::channel_label(cx, id, ch)))
                .collect();
            ui.horizontal_wrapped(|ui| {
                ui.label(Text::small(cx.tr.t("updates.list.channel"), cx.pal));
                Dropdown::new(("tool-channel", id))
                    .label(cx.tr.t("updates.list.channel"))
                    .show_options(ui, &mut channel, &options);
            });
            if channel != row.channel {
                report(source.channel(id, channel), out);
            }
        }
        if let Some(mut managed) = row.managed {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add(Toggle::new(&mut managed, cx.tr.t("updates.list.managed")))
                    .changed()
                {
                    report(source.managed(id, managed), out);
                }
                ui.label(Text::small(cx.tr.t("updates.list.managed"), cx.pal));
            });
        }
    });
    for caution in &row.entry.cautions {
        callout(ui, cx, Status::Warn, caution);
    }
    for note in &row.notes {
        ui.add(egui::Label::new(Text::small(note, cx.pal)).wrap());
    }
    ui.add_enabled_ui(row.entry.status != UpdateStatus::Installing, |ui| {
        if !row.versions.is_empty()
            && matches!(
                row.entry.status,
                UpdateStatus::Current
                    | UpdateStatus::Available
                    | UpdateStatus::Missing
                    | UpdateStatus::Failed
            )
        {
            if !row.versions.contains(selected) {
                selected.clone_from(&row.versions[0]);
            }
            let options: Vec<_> = row
                .versions
                .iter()
                .map(|v| (v.clone(), v.clone()))
                .collect();
            ui.horizontal_wrapped(|ui| {
                Dropdown::new(("tool-version", id))
                    .label(cx.tr.t("updates.pin.version"))
                    .show_options(ui, selected, &options);
                if ui
                    .add(Button::new(cx.tr.t("updates.list.install_version")).small())
                    .clicked()
                {
                    let mut entry = row.entry.clone();
                    entry.latest = Some(selected.clone());
                    entry.cautions.clone_from(&row.install_cautions);
                    entry.operation = Some(Operation::ToolVersion {
                        id,
                        version: selected.clone(),
                    });
                    request = Some(entry);
                }
            });
        }
        if let Some(removal) = &row.removal
            && ui
                .add(Button::new(cx.tr.t("updates.list.remove")).small())
                .clicked()
        {
            let mut entry = row.entry.clone();
            entry.operation = Some(Operation::ToolRemove {
                id,
                remove_data: false,
            });
            entry.cautions.clone_from(&removal.cautions);
            request = Some(entry);
        }
    });
    if let Some(log) = &row.log
        && ui
            .add(Button::new(cx.tr.t("updates.superba.open_log")).small())
            .clicked()
    {
        out.push(Action::OpenUrl(log.clone()));
    }
    request
}
fn report(result: Result<(), String>, out: &mut Vec<Action>) {
    if let Err(text) = result {
        out.push(Action::Toast {
            status: Status::Error,
            text,
        });
    }
}

/// Replaceable installer adapter shared with the page, shell and test fixtures.
#[derive(Clone, Debug)]
pub struct Source(pub std::sync::Arc<dyn ToolsSource>);
impl Default for Source {
    fn default() -> Self {
        Self(std::sync::Arc::new(ServiceTools::default()))
    }
}
impl std::ops::Deref for Source {
    type Target = dyn ToolsSource;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}
impl AsRef<dyn ToolsSource> for Source {
    fn as_ref(&self) -> &(dyn ToolsSource + 'static) {
        self.0.as_ref()
    }
}
