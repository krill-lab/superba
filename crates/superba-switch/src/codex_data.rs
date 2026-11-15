//! Codex data location (GitHub issue 15): the service side of
//! `superba_storage::codex_data`. It refuses while any Codex process uses the
//! home (the copy must be complete and nothing may keep writing to the old
//! folder), while a switch waits for recovery, and when Superba runs elevated
//! or as another user; it holds `state.lock` for the whole move, so no session
//! migration or switch runs at the same time, and runs the blocking copy on a
//! worker that keeps the lock even if the caller stops waiting.

use std::path::PathBuf;

use superba_core::data::{
    CodexDataLocation, CodexDataMoveReport, CodexDataPlan, CodexDirPlacement, DataMovePhase,
    DataMoveProgress, DataMoveProgressSink,
};
use superba_core::paths::display_path;
use superba_core::{
    CodexProcessKind, DoctorFinding, Error, LocalizedText, ProcessInfo, Result, Severity,
};
use superba_storage::codex_data::{self, CodexDataFs, Scope};

use crate::Switcher;
use crate::journal::StateLock;
use crate::messages::text;

fn label(p: &ProcessInfo) -> String {
    let name = match &p.kind {
        CodexProcessKind::AppMain => "ChatGPT",
        CodexProcessKind::AppServer => "Codex app-server",
        CodexProcessKind::CliTui => "codex",
        CodexProcessKind::CliExec => "codex exec",
        CodexProcessKind::CliDaemon => "codex daemon",
        CodexProcessKind::IdeAppServer => "Codex (IDE)",
        _ => "codex",
    };
    format!("{name} ({})", p.pid)
}

/// Codex processes that read or write the local home's sessions (the app's
/// ssh links to remote hosts do not).
async fn running(service: &Switcher) -> Vec<String> {
    let d = service.deps();
    let home = d.ctx.codex_home();
    let mut out: Vec<String> = d
        .app
        .processes(&home)
        .await
        .unwrap_or_default()
        .iter()
        .filter(|p| !matches!(p.kind, CodexProcessKind::RemoteLink { .. }))
        .filter(|p| crate::rules::uses_home(p, home.root()))
        .map(label)
        .collect();
    if out.is_empty()
        && let Ok(Some(session)) = d.app.running_session().await
    {
        out.push(format!("ChatGPT ({})", session.main_pid));
    }
    out
}

fn running_text(processes: &[String]) -> LocalizedText {
    text("switch.codex_data.running").arg("processes", processes.join(", "))
}

pub(crate) async fn location(service: &Switcher) -> Result<CodexDataLocation> {
    let ctx = service.deps().ctx.clone();
    tokio::task::spawn_blocking(move || codex_data::inspect(&ctx.codex_home(), &ctx.data_dir))
        .await
        .map_err(|e| Error::other(format!("codex data location: {e}")))
}

pub(crate) async fn plan(
    service: &Switcher,
    directory: Option<PathBuf>,
    fs: CodexDataFs,
) -> Result<CodexDataPlan> {
    let ctx = service.deps().ctx.clone();
    let mut plan = tokio::task::spawn_blocking(move || {
        let home = ctx.codex_home();
        codex_data::plan(
            Scope {
                home: &home,
                data: &ctx.data_dir,
                env: ctx.env.as_ref(),
                fs: &fs,
            },
            directory.as_deref(),
        )
    })
    .await
    .map_err(|e| Error::other(format!("codex data plan: {e}")))??;
    let mut first = Vec::new();
    if let Err(e) = service.refuse_elevated() {
        first.push(e.to_text());
    }
    if crate::data::switch_pending(&service.deps().ctx)? {
        first.push(Error::invalid("data.pending_switch").to_text());
    }
    let processes = running(service).await;
    if !processes.is_empty() {
        first.push(running_text(&processes));
    }
    first.append(&mut plan.blockers);
    plan.blockers = first;
    Ok(plan)
}

pub(crate) async fn move_data(
    service: &Switcher,
    directory: Option<PathBuf>,
    fs: CodexDataFs,
    progress: DataMoveProgressSink,
) -> Result<CodexDataMoveReport> {
    service.refuse_elevated()?;
    let ctx = service.deps().ctx.clone();
    if crate::data::switch_pending(&ctx)? {
        return Err(Error::invalid("data.pending_switch"));
    }
    if !running(service).await.is_empty() {
        return Err(Error::invalid("data.codex.running"));
    }
    let lock = StateLock::acquire(&ctx.data_dir)?;
    let worker = progress.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _lock = lock;
        let home = ctx.codex_home();
        codex_data::execute(
            Scope {
                home: &home,
                data: &ctx.data_dir,
                env: ctx.env.as_ref(),
                fs: &fs,
            },
            directory.as_deref(),
            &worker,
        )
    })
    .await
    .map_err(|e| Error::other(format!("codex data move: {e}")))?;
    if let Ok(report) = &result {
        let _ = progress(DataMoveProgress {
            phase: DataMovePhase::Done,
            copied_bytes: report.copied_bytes,
            total_bytes: report.copied_bytes,
        });
    }
    result
}

/// Start-up recovery of an interrupted move (the caller holds `state.lock`).
pub(crate) fn recover(service: &Switcher) -> Option<LocalizedText> {
    match codex_data::recover(&service.deps().ctx.data_dir) {
        Ok(None) => None,
        Ok(Some(codex_data::Recovered::RolledBack)) => Some(text("switch.codex_data.rolled_back")),
        Ok(Some(codex_data::Recovered::Completed { leftovers })) if leftovers.is_empty() => {
            Some(text("switch.codex_data.completed"))
        }
        Ok(Some(codex_data::Recovered::Completed { leftovers })) => Some(
            text("switch.codex_data.leftovers").arg(
                "paths",
                leftovers
                    .iter()
                    .map(|p| display_path(p))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        ),
        Err(e) => {
            tracing::warn!(error = %e, "Codex data move recovery failed");
            Some(e.to_text())
        }
    }
}

/// Doctor checks: a moved session folder that cannot be reached (the drive is
/// unplugged: Codex cannot save conversations), an interrupted move, old
/// copies left behind, and where the data is.
pub(crate) async fn doctor(service: &Switcher) -> Vec<DoctorFinding> {
    let ctx = service.deps().ctx.clone();
    let Ok(location) =
        tokio::task::spawn_blocking(move || codex_data::locate(&ctx.codex_home(), &ctx.data_dir))
            .await
    else {
        return Vec::new();
    };
    let finding = |id: &str, severity: Severity, title: LocalizedText| DoctorFinding {
        id: id.to_owned(),
        severity,
        title,
        detail: None,
        fixable: false,
    };
    let mut out = Vec::new();
    for dir in &location.dirs {
        if let CodexDirPlacement::Linked {
            target,
            reachable: false,
            ..
        } = &dir.placement
        {
            out.push(finding(
                "codex_data_unreachable",
                Severity::Danger,
                text("switch.doctor.codex_data_unreachable")
                    .arg("path", display_path(target))
                    .arg("link", display_path(&dir.path)),
            ));
        }
    }
    if location.pending {
        out.push(finding(
            "codex_data_pending",
            Severity::Warning,
            text("switch.doctor.codex_data_pending"),
        ));
    }
    if !location.leftovers.is_empty() {
        out.push(finding(
            "codex_data_leftovers",
            Severity::Info,
            text("switch.doctor.codex_data_leftovers").arg(
                "paths",
                location
                    .leftovers
                    .iter()
                    .map(|p| display_path(p))
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        ));
    }
    if let Some(directory) = &location.directory
        && !location.unreachable()
    {
        out.push(finding(
            "codex_data_moved",
            Severity::Info,
            text("switch.doctor.codex_data_moved").arg("path", display_path(directory)),
        ));
    }
    out
}
