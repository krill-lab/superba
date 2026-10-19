//! `spb tools`: install, update and remove the coding-agent tools Superba
//! configures (Claude Code, dsh, opencode, pi, ZCode, WorkBuddy, WorkBuddy AI)
//! with their official per-user methods; `list` / `check` show what is
//! installed and the newest version on each channel.

use std::sync::Arc;
use std::time::Duration;

use superba_core::{
    Error, LocalizedText, SharedToolProgress, ToolId, ToolInstallKind, ToolOp, ToolProgress,
    ToolProgressSink, ToolReport, ToolRequest, ToolRow, ToolSettingsChange, ToolStage, ToolSupport,
    ToolUpdater, ToolsView,
};

use super::{Result, usage};
use crate::{Output, Runtime, ToolsCommand, exit, text::text};

pub(super) async fn run(command: &ToolsCommand, rt: &Runtime, out: &mut Output) -> Result {
    match command {
        ToolsCommand::List => list(rt, out, false).await,
        ToolsCommand::Check => {
            out.progress(text("cli.tools.checking"))?;
            list(rt, out, true).await
        }
        ToolsCommand::Install {
            tool,
            version,
            channel,
            yes,
        } => {
            let req = ToolRequest {
                version: version.clone(),
                channel: channel.clone(),
                ..ToolRequest::new(tool_id(out, tool)?, ToolOp::Install)
            };
            operate(req, *yes, rt, out).await
        }
        ToolsCommand::Update {
            tool,
            version,
            channel,
            yes,
        } => {
            let req = ToolRequest {
                version: version.clone(),
                channel: channel.clone(),
                ..ToolRequest::new(tool_id(out, tool)?, ToolOp::Update)
            };
            operate(req, *yes, rt, out).await
        }
        ToolsCommand::Uninstall {
            tool,
            remove_data,
            yes,
        } => {
            let req = ToolRequest {
                remove_data: *remove_data,
                ..ToolRequest::new(tool_id(out, tool)?, ToolOp::Uninstall)
            };
            operate(req, *yes, rt, out).await
        }
        ToolsCommand::ManageUpdates { tool, state } => {
            let id = tool_id(out, tool)?;
            let row = rt
                .service
                .tool_settings(
                    id,
                    ToolSettingsChange {
                        manage_updates: Some(state.is_on()),
                        ..Default::default()
                    },
                )
                .await?;
            out.field("cli.tools.col.updates", updater_text(out, &row.updater))?;
            out.value(&row)?;
            Ok(exit::OK)
        }
    }
}

fn tool_id(out: &Output, name: &str) -> Result<ToolId> {
    ToolId::parse(name).ok_or_else(|| {
        usage(
            out,
            text("cli.tools.unknown").arg(
                "tools",
                ToolId::ALL
                    .iter()
                    .map(|t| t.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        )
    })
}

fn kind_key(kind: ToolInstallKind) -> &'static str {
    match kind {
        ToolInstallKind::Native => "cli.tools.kind.native",
        ToolInstallKind::Standalone => "cli.tools.kind.standalone",
        ToolInstallKind::NpmPrefix => "cli.tools.kind.npm_prefix",
        ToolInstallKind::Runtime => "cli.tools.kind.runtime",
        ToolInstallKind::NpmGlobal => "cli.tools.kind.npm_global",
        ToolInstallKind::Homebrew => "cli.tools.kind.homebrew",
        ToolInstallKind::Winget => "cli.tools.kind.winget",
        ToolInstallKind::Managed => "cli.tools.kind.managed",
        ToolInstallKind::App => "cli.tools.kind.app",
        ToolInstallKind::SystemPackage => "cli.tools.kind.system",
        _ => "cli.tools.kind.other",
    }
}

fn updater_text(out: &Output, u: &ToolUpdater) -> String {
    match u {
        ToolUpdater::None => out.label("cli.tools.updater.none"),
        ToolUpdater::Notifies => out.label("cli.tools.updater.notifies"),
        ToolUpdater::Automatic => out.label("cli.tools.updater.automatic"),
        ToolUpdater::Off { by } => out.render(&text("cli.tools.updater.off").arg("by", by)),
        ToolUpdater::Superba => out.label("cli.tools.updater.superba"),
        ToolUpdater::Forced => out.label("cli.tools.updater.forced"),
        ToolUpdater::PackageManager => out.label("cli.tools.updater.package_manager"),
        ToolUpdater::Blocked { by } => out.render(&text("cli.tools.updater.blocked").arg("by", by)),
        _ => out.none(),
    }
}

fn how(out: &Output, row: &ToolRow) -> String {
    match (&row.support, row.primary()) {
        (ToolSupport::Unavailable, _) => out.label("cli.tools.unavailable"),
        (_, Some(i)) => {
            let kind = out.label(kind_key(i.kind));
            if i.by_superba {
                out.render(&text("cli.tools.by_superba").arg("kind", kind))
            } else {
                kind
            }
        }
        (ToolSupport::Link { .. }, None) => out.label("cli.tools.link_only"),
        _ => out.none(),
    }
}

fn print_view(view: &ToolsView, out: &mut Output) -> Result<()> {
    let rows = view
        .rows
        .iter()
        .map(|r| {
            let installed = match r.primary() {
                Some(i) => i
                    .version
                    .clone()
                    .unwrap_or_else(|| out.label("cli.tools.unknown_version")),
                None => out.none(),
            };
            let mut latest = r.latest.clone().unwrap_or_else(|| out.none());
            if r.update_available {
                latest = out.render(&text("cli.tools.newer").arg("version", latest));
            }
            vec![
                r.name.clone(),
                installed,
                latest,
                r.channel.clone().unwrap_or_else(|| out.none()),
                how(out, r),
                updater_text(out, &r.updater),
            ]
        })
        .collect::<Vec<_>>();
    out.table(
        &[
            "cli.tools.col.tool",
            "cli.tools.col.installed",
            "cli.tools.col.latest",
            "cli.tools.col.channel",
            "cli.tools.col.how",
            "cli.tools.col.updates",
        ],
        rows,
    )?;
    for r in &view.rows {
        if let ToolSupport::Link { url } = &r.support {
            out.line(
                text("cli.tools.download_at")
                    .arg("name", &r.name)
                    .arg("url", url),
            )?;
        }
        if let Some(e) = &r.latest_error {
            out.line(
                text("cli.tools.latest_failed")
                    .arg("name", &r.name)
                    .arg("error", e),
            )?;
        }
        for n in &r.notes {
            out.line(
                text("cli.tools.note")
                    .arg("name", &r.name)
                    .arg_text("text", n.text.clone()),
            )?;
        }
        if let Some(h) = &r.path_hint {
            out.line(
                text("cli.tools.path_hint")
                    .arg("name", &r.name)
                    .arg("line", &h.line)
                    .arg("file", &h.file),
            )?;
        }
    }
    for n in &view.notes {
        out.notice(n)?;
    }
    Ok(())
}

async fn list(rt: &Runtime, out: &mut Output, refresh: bool) -> Result {
    let view = rt.service.tools(refresh).await?;
    print_view(&view, out)?;
    out.value(&view)?;
    Ok(exit::OK)
}

/// Ask before an operation: the row's cautions (and, for a removal, what
/// stays); `--yes` answers for the user.
/// `Ok(None)`: go on; `Ok(Some(code))`: stop with that exit code.
fn confirm(
    row: &ToolRow,
    req: &ToolRequest,
    yes: bool,
    rt: &Runtime,
    out: &mut Output,
) -> Result<Option<u8>> {
    let cautions: &[LocalizedText] = match req.op {
        ToolOp::Install => &row.install_cautions,
        ToolOp::Update => &row.update_cautions,
        _ => &row.uninstall_cautions,
    };
    for c in cautions {
        out.line(text("cli.tools.caution").arg_text("text", c.clone()))?;
    }
    if req.op == ToolOp::Uninstall && req.remove_data {
        for d in &row.data_dirs {
            out.line(
                text("cli.tools.removes_data").arg("path", superba_core::paths::display_path(d)),
            )?;
        }
    }
    let ask = !cautions.is_empty() || req.op == ToolOp::Uninstall;
    if !ask || yes {
        return Ok(None);
    }
    if !out.interactive() {
        out.line(text("cli.tools.needs_yes"))?;
        return Ok(Some(exit::NEEDS_CONFIRMATION));
    }
    let key = match req.op {
        ToolOp::Uninstall => "cli.tools.confirm_uninstall",
        _ => "cli.tools.confirm",
    };
    out.line(text(key).arg("name", &row.name))?;
    let answer = (rt.read_stdin_line)().unwrap_or_default();
    if !matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes" | "是"
    ) {
        out.line(text("cli.tools.cancelled"))?;
        return Ok(Some(exit::CANCELLED));
    }
    Ok(None)
}

async fn operate(mut req: ToolRequest, yes: bool, rt: &Runtime, out: &mut Output) -> Result {
    // Only this tool's row (an empty change writes nothing): no lookups of
    // the other tools' newest versions before the operation starts.
    let row = rt
        .service
        .tool_settings(req.tool, ToolSettingsChange::default())
        .await?;
    if let ToolSupport::Link { url } = &row.support {
        out.line(
            text("cli.tools.link_only_hint")
                .arg("name", &row.name)
                .arg("url", url),
        )?;
        return Ok(exit::BLOCKED);
    }
    if let Some(code) = confirm(&row, &req, yes, rt, out)? {
        return Ok(code);
    }
    req.confirmed = true;
    let report = with_progress(rt, req, out).await?;
    print_report(&report, out)?;
    out.value(&report)?;
    Ok(exit::OK)
}

fn mb(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1_000_000.0)
}

fn progress_text(p: &ToolProgress) -> LocalizedText {
    let detail = p.detail.clone().unwrap_or_default();
    match p.stage {
        ToolStage::Downloading => {
            let percent = p
                .fraction()
                .map(|f| format!("{:.0}%", f * 100.0))
                .unwrap_or_else(|| "?".into());
            text("cli.tools.stage.downloading")
                .arg("file", detail)
                .arg("done", mb(p.done))
                .arg("total", p.total.map(mb).unwrap_or_else(|| "?".into()))
                .arg("percent", percent)
        }
        ToolStage::Resolving => text("cli.tools.stage.resolving").arg("detail", detail),
        ToolStage::Verifying => text("cli.tools.stage.verifying").arg("detail", detail),
        ToolStage::Extracting => text("cli.tools.stage.extracting").arg("detail", detail),
        ToolStage::Installing => text("cli.tools.stage.installing").arg("detail", detail),
        ToolStage::Removing => text("cli.tools.stage.removing").arg("detail", detail),
        _ => text("cli.tools.stage.checking").arg("detail", detail),
    }
}

/// Run the operation, showing its progress on stderr and stopping it on
/// Ctrl-C.
async fn with_progress(
    rt: &Runtime,
    req: ToolRequest,
    out: &mut Output,
) -> superba_core::Result<ToolReport> {
    let shared = Arc::new(SharedToolProgress::default());
    let sink: Arc<dyn ToolProgressSink> = shared.clone();
    let work = rt.service.tool_run(req, sink);
    tokio::pin!(work);
    let cancel = tokio::signal::ctrl_c();
    tokio::pin!(cancel);
    let mut cancelled = false;
    let mut last: Option<ToolStage> = None;
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let result = loop {
        tokio::select! {
            biased;
            r = &mut work => break r,
            signal = &mut cancel, if !cancelled => {
                signal.map_err(|e| Error::io("signal", &e))?;
                cancelled = true;
                shared.cancel();
            }
            _ = tick.tick() => {
                if let Some(p) = shared.latest() {
                    let milestone = last != Some(p.stage);
                    last = Some(p.stage);
                    out.live_progress(progress_text(&p), milestone)?;
                }
            }
        }
    };
    out.finish_progress()?;
    if cancelled && matches!(result, Err(Error::Cancelled)) {
        out.line(text("cli.tools.cancelled"))?;
    }
    result
}

fn print_report(r: &ToolReport, out: &mut Output) -> Result<()> {
    let name = r.tool.display_name();
    match (&r.version, r.op) {
        (None, _) => out.line(text("cli.tools.removed").arg("name", name))?,
        (Some(v), ToolOp::Update) => out.line(
            text("cli.tools.updated")
                .arg("name", name)
                .arg("version", v),
        )?,
        (Some(v), _) => out.line(
            text("cli.tools.installed")
                .arg("name", name)
                .arg("version", v),
        )?,
    }
    if let Some(p) = &r.path {
        out.field("cli.tools.path", superba_core::paths::display_path(p))?;
    }
    for n in &r.notes {
        out.notice(n)?;
    }
    out.field("cli.tools.log", superba_core::paths::display_path(&r.log))?;
    Ok(())
}
