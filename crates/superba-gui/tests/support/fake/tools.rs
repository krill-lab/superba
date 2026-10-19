//! Fake service, area "tools": the Updates page's coding-agent tools
//! section (`SwitchService::tools`, `tool_run`, `tool_settings`).
//!
//! Per-area state and the bodies of this area's `SwitchService` methods; the one
//! `impl SwitchService for FakeService` in `fake.rs` delegates here.

use std::path::PathBuf;
use std::sync::Arc;

use superba_core::*;

use super::FakeService;

/// Fake state of the area.
#[derive(Debug, Default)]
pub(super) struct State {
    view: Option<ToolsView>,
    runs: Vec<ToolRequest>,
    changes: Vec<(ToolId, ToolSettingsChange)>,
    fail: Option<Error>,
    reads: Vec<bool>,
    read_fail: Option<Error>,
    read_delay: std::time::Duration,
    hold_runs: bool,
}

fn install(path: &str, kind: ToolInstallKind, version: &str, by_superba: bool) -> ToolInstall {
    ToolInstall {
        version: Some(version.into()),
        path: PathBuf::from(path),
        kind,
        by_superba,
        on_path: true,
    }
}

fn row(id: ToolId, support: ToolSupport, channels: &[&str], latest: &str) -> ToolRow {
    ToolRow {
        id,
        name: id.display_name().into(),
        support,
        installs: Vec::new(),
        channel: channels.first().map(|c| (*c).to_owned()),
        channels: channels.iter().map(|c| (*c).to_owned()).collect(),
        latest: Some(latest.into()),
        checked_at: Some(Timestamp::now()),
        latest_error: None,
        update_available: false,
        updater: ToolUpdater::Notifies,
        actions: ToolActions::default(),
        install_cautions: Vec::new(),
        update_cautions: Vec::new(),
        uninstall_cautions: Vec::new(),
        data_dirs: Vec::new(),
        path_hint: None,
        notes: Vec::new(),
    }
}

/// A view with every kind of row: an update with a caution and the
/// updater switch (Claude Code), not installed (dsh, pi), up to date with
/// a data folder (opencode), links (ZCode, WorkBuddy).
pub fn sample_view() -> ToolsView {
    let mut claude = row(
        ToolId::ClaudeCode,
        ToolSupport::Full,
        &["latest", "stable"],
        "2.1.302",
    );
    claude.installs = vec![install(
        "/home/me/.local/bin/claude",
        ToolInstallKind::Native,
        "2.1.292",
        true,
    )];
    claude.update_available = true;
    claude.updater = ToolUpdater::Automatic;
    claude.actions = ToolActions {
        install: false,
        update: true,
        uninstall: true,
        manage_updates: true,
    };
    claude.update_cautions = vec![LocalizedText::new(
        "tools.claude.caution_npm",
        "Claude Code's installer runs `npm uninstall -g @anthropic-ai/claude-code`: the npm install in {path} will be removed.",
    )
    .arg("path", "/usr/lib/node_modules/@anthropic-ai/claude-code")];
    let mut dsh = row(
        ToolId::Dsh,
        ToolSupport::Full,
        &["latest", "next", "alpha"],
        "0.2.0-rc.2",
    );
    dsh.updater = ToolUpdater::None;
    dsh.actions.install = true;
    let mut opencode = row(
        ToolId::Opencode,
        ToolSupport::Full,
        &["latest", "beta", "v1"],
        "2.0.32",
    );
    opencode.installs = vec![install(
        "/home/me/.opencode/bin/opencode",
        ToolInstallKind::Standalone,
        "2.0.32",
        true,
    )];
    opencode.actions = ToolActions {
        install: false,
        update: false,
        uninstall: true,
        manage_updates: true,
    };
    opencode.data_dirs = vec![PathBuf::from("/home/me/.config/opencode")];
    let mut pi = row(ToolId::Pi, ToolSupport::Full, &["latest"], "1.3.0");
    pi.actions.install = true;
    let zcode = row(
        ToolId::Zcode,
        ToolSupport::Link {
            url: "https://zcode.z.ai".into(),
        },
        &["stable", "preview"],
        "3.15.3",
    );
    let mut wb = row(
        ToolId::Workbuddy,
        ToolSupport::Link {
            url: "https://www.workbuddy.cn".into(),
        },
        &["latest"],
        "5.6.2",
    );
    wb.updater = ToolUpdater::Forced;
    let mut wbi = row(
        ToolId::WorkbuddyAi,
        ToolSupport::Link {
            url: "https://www.workbuddy.ai".into(),
        },
        &["latest"],
        "5.5.2",
    );
    wbi.updater = ToolUpdater::Forced;
    ToolsView {
        rows: vec![claude, dsh, opencode, pi, zcode, wb, wbi],
        notes: Vec::new(),
    }
}

impl FakeService {
    /// What the tools section shows (default: [`sample_view`]).
    pub fn set_tools(&self, view: ToolsView) {
        self.areas.tools.lock().unwrap().view = Some(view);
    }

    /// Make tool operations fail.
    pub fn fail_tools(&self, error: Error) {
        self.areas.tools.lock().unwrap().fail = Some(error);
    }

    /// Record online flags, including refreshes after mutations.
    pub fn tool_reads(&self) -> Vec<bool> {
        self.areas.tools.lock().unwrap().reads.clone()
    }

    /// Slow snapshot reads to exercise stale-read handling.
    pub fn delay_tool_reads(&self, delay: std::time::Duration) {
        self.areas.tools.lock().unwrap().read_delay = delay;
    }

    /// Fail inventory reads, keeping any previously loaded rows.
    pub fn fail_tool_reads(&self, error: Option<Error>) {
        self.areas.tools.lock().unwrap().read_fail = error;
    }

    /// Keep operations running until released or cooperatively cancelled.
    pub fn hold_tool_runs(&self, hold: bool) {
        self.areas.tools.lock().unwrap().hold_runs = hold;
    }

    /// Clear an operation failure for a user-initiated retry.
    pub fn clear_tool_failure(&self) {
        self.areas.tools.lock().unwrap().fail = None;
    }

    /// Operations started.
    pub fn tool_runs(&self) -> Vec<ToolRequest> {
        self.areas.tools.lock().unwrap().runs.clone()
    }

    /// Settings changes asked.
    pub fn tool_changes(&self) -> Vec<(ToolId, ToolSettingsChange)> {
        self.areas.tools.lock().unwrap().changes.clone()
    }
}

pub(super) async fn tools(fake: &FakeService, refresh: bool) -> Result<ToolsView> {
    let (view, delay, error) = {
        let mut state = fake.areas.tools.lock().unwrap();
        state.reads.push(refresh);
        (
            state.view.clone().unwrap_or_else(sample_view),
            state.read_delay,
            state.read_fail.clone(),
        )
    };
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    error.map_or(Ok(view), Err)
}

pub(super) async fn tool_run(
    fake: &FakeService,
    request: ToolRequest,
    progress: Arc<dyn ToolProgressSink>,
) -> Result<ToolReport> {
    progress.report(ToolProgress {
        done: 50,
        total: Some(100),
        detail: Some("file".into()),
        ..ToolProgress::stage(request.tool, ToolStage::Downloading)
    });
    fake.areas.tools.lock().unwrap().runs.push(request.clone());
    while fake.areas.tools.lock().unwrap().hold_runs {
        if progress.cancel_requested() {
            // Cancellation is a request: cleanup must finish before the next job.
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            return Err(Error::Cancelled);
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let mut state = fake.areas.tools.lock().unwrap();
    if let Some(error) = state.fail.clone() {
        return Err(error);
    }
    let version = (request.op != ToolOp::Uninstall)
        .then(|| request.version.clone().unwrap_or_else(|| "9.9.9".into()));
    let view = state.view.get_or_insert_with(sample_view);
    let row = view.rows.iter_mut().find(|r| r.id == request.tool).unwrap();
    if let Some(version) = &version {
        if let Some(install) = row.installs.first_mut() {
            install.version = Some(version.clone());
        } else {
            row.installs.push(install(
                "/example/bin/tool",
                ToolInstallKind::Standalone,
                version,
                true,
            ));
        }
        row.actions.install = false;
        row.actions.uninstall = true;
        row.actions.update = false;
        row.update_available = false;
    } else {
        row.installs.clear();
        row.actions.install = true;
        row.actions.uninstall = false;
        row.actions.update = false;
        row.update_available = false;
    }
    Ok(ToolReport {
        tool: request.tool,
        op: request.op,
        version,
        path: None,
        notes: Vec::new(),
        log: PathBuf::from("/home/me/.local/share/superba/logs/tools/x.log"),
    })
}

pub(super) async fn tool_settings(
    fake: &FakeService,
    tool: ToolId,
    change: ToolSettingsChange,
) -> Result<ToolRow> {
    let mut st = fake.areas.tools.lock().unwrap();
    st.changes.push((tool, change.clone()));
    let mut view = st.view.clone().unwrap_or_else(sample_view);
    let row = view
        .rows
        .iter_mut()
        .find(|r| r.id == tool)
        .ok_or_else(|| Error::not_implemented("tool"))?;
    if let Some(c) = change.channel {
        row.channel = Some(c);
    }
    if let Some(on) = change.manage_updates {
        row.updater = if on {
            ToolUpdater::Superba
        } else {
            ToolUpdater::Automatic
        };
    }
    let out = row.clone();
    st.view = Some(view);
    Ok(out)
}
