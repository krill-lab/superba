use std::sync::{Arc, Mutex};
use std::time::Duration;

use superba_core::LocalizedText;
use superba_core::data::{
    CodexDataDir, CodexDataPlan, CodexDirPlacement, DataMoveProgress, DataMoveProgressSink,
};
use superba_core::paths::display_path;

use crate::{CodexDataCommand, DataCommand, Output, Runtime, exit, text::text};

pub(super) async fn run(command: &DataCommand, rt: &Runtime, out: &mut Output) -> super::Result {
    match command {
        DataCommand::Where => {
            let location = rt.service.data_where().await?;
            out.data_line(&display_path(&location.directory))?;
            out.value(&location)?;
        }
        DataCommand::Usage => {
            let usage = rt.service.data_usage().await?;
            out.field(
                "cli.data.directory",
                display_path(&usage.location.directory),
            )?;
            let rows: Vec<_> = usage
                .categories
                .iter()
                .map(|c| {
                    vec![
                        out.label(&format!("data.category.{}", c.category)),
                        c.bytes.to_string(),
                        c.files.to_string(),
                    ]
                })
                .collect();
            out.table(
                &["cli.data.category", "cli.data.bytes", "cli.data.files"],
                rows,
            )?;
            out.field("cli.data.total", usage.total_bytes)?;
            out.field(
                "cli.data.codex_home",
                display_path(&usage.location.codex_home),
            )?;
            out.field("cli.data.codex_total", usage.codex_home_bytes)?;
            out.field("cli.data.codex_sessions", usage.codex_sessions_bytes)?;
            if usage.codex_sqlite_bytes > 0 {
                out.field("cli.data.codex_sqlite", usage.codex_sqlite_bytes)?;
            }
            if usage.skipped > 0 {
                out.line(text("cli.data.partial").arg("count", usage.skipped))?;
            }
            out.value(&usage)?;
        }
        DataCommand::Move { directory } => {
            let latest = Arc::new(Mutex::new(DataMoveProgress::default()));
            let sender = latest.clone();
            let progress: DataMoveProgressSink = Arc::new(move |p| {
                *sender.lock().unwrap_or_else(|p| p.into_inner()) = p;
                true
            });
            let operation = rt.service.data_move(directory.clone(), progress);
            tokio::pin!(operation);
            let result = loop {
                tokio::select! {
                    result = &mut operation => break result,
                    () = tokio::time::sleep(Duration::from_millis(250)) => {
                        let p = latest.lock().unwrap_or_else(|p| p.into_inner()).clone();
                        out.live_progress(text("cli.data.progress")
                            .arg("stage", out.label(superba_core::data::phase_key(p.phase)))
                            .arg("copied", p.copied_bytes).arg("total", p.total_bytes), false)?;
                    }
                }
            };
            out.finish_progress()?;
            let moved = result?;
            out.line(text("cli.data.moved").arg("path", display_path(&moved.location.directory)))?;
            if moved.source_cleanup_pending {
                out.line(text("cli.data.leftovers").arg("path", display_path(&moved.previous)))?;
            }
            out.value(&moved)?;
        }
        DataCommand::Codex { command } => return codex(command, rt, out).await,
    }
    Ok(exit::OK)
}

fn placement_line(dir: &CodexDataDir) -> LocalizedText {
    let size = |t: LocalizedText| t.arg("bytes", dir.bytes).arg("files", dir.files);
    match &dir.placement {
        CodexDirPlacement::Missing => text("cli.data.codex.missing").arg("name", &dir.name),
        CodexDirPlacement::InHome => {
            size(text("cli.data.codex.in_home")).arg("path", display_path(&dir.path))
        }
        CodexDirPlacement::Linked {
            target,
            managed,
            reachable,
        } => {
            let key = match (reachable, managed) {
                (false, _) => "cli.data.codex.unreachable",
                (true, true) => "cli.data.codex.linked",
                (true, false) => "cli.data.codex.foreign",
            };
            size(text(key))
                .arg("path", display_path(&dir.path))
                .arg("target", display_path(target))
        }
        _ => text("cli.data.codex.other").arg("path", display_path(&dir.path)),
    }
}

fn show_plan(plan: &CodexDataPlan, out: &mut Output) -> super::Result {
    let rows: Vec<_> = plan
        .moves
        .iter()
        .map(|m| {
            vec![
                m.name.clone(),
                display_path(&m.from),
                display_path(&m.to),
                m.bytes.to_string(),
                m.files.to_string(),
            ]
        })
        .collect();
    if !rows.is_empty() {
        out.table(
            &[
                "cli.data.codex.folder",
                "cli.data.codex.from",
                "cli.data.codex.to",
                "cli.data.bytes",
                "cli.data.files",
            ],
            rows,
        )?;
        out.field("cli.data.codex.total", plan.total_bytes)?;
        if let Some(available) = plan.available_bytes {
            out.field("cli.data.codex.available", available)?;
        }
    }
    for note in &plan.notes {
        out.line(note.clone())?;
    }
    for blocker in &plan.blockers {
        out.line(text("cli.data.codex.blocked").arg_text("reason", blocker.clone()))?;
    }
    Ok(exit::OK)
}

async fn codex(command: &CodexDataCommand, rt: &Runtime, out: &mut Output) -> super::Result {
    let (directory, dry_run) = match command {
        CodexDataCommand::Where => {
            let location = rt.service.codex_data_where().await?;
            out.field("cli.data.codex.home", display_path(&location.codex_home))?;
            for dir in &location.dirs {
                out.line(placement_line(dir))?;
            }
            if location.pending {
                out.line(text("cli.data.codex.pending"))?;
            }
            for path in &location.leftovers {
                out.line(text("cli.data.leftovers").arg("path", display_path(path)))?;
            }
            out.value(&location)?;
            return Ok(if location.unreachable() {
                exit::ERROR
            } else {
                exit::OK
            });
        }
        CodexDataCommand::Move { directory, dry_run } => (Some(directory.clone()), *dry_run),
        CodexDataCommand::Restore { dry_run } => (None, *dry_run),
    };
    let plan = rt.service.codex_data_plan(directory.clone()).await?;
    show_plan(&plan, out)?;
    if !plan.blockers.is_empty() {
        out.value(&plan)?;
        return Ok(exit::BLOCKED);
    }
    if dry_run {
        out.value(&plan)?;
        return Ok(exit::OK);
    }
    let latest = Arc::new(Mutex::new(DataMoveProgress::default()));
    let sender = latest.clone();
    let progress: DataMoveProgressSink = Arc::new(move |p| {
        *sender.lock().unwrap_or_else(|p| p.into_inner()) = p;
        true
    });
    let operation = rt.service.codex_data_move(directory, progress);
    tokio::pin!(operation);
    let result = loop {
        tokio::select! {
            result = &mut operation => break result,
            () = tokio::time::sleep(Duration::from_millis(250)) => {
                let p = latest.lock().unwrap_or_else(|p| p.into_inner()).clone();
                out.live_progress(text("cli.data.progress")
                    .arg("stage", out.label(superba_core::data::phase_key(p.phase)))
                    .arg("copied", p.copied_bytes).arg("total", p.total_bytes), false)?;
            }
        }
    };
    out.finish_progress()?;
    let moved = result?;
    out.line(match &moved.location.directory {
        Some(dir) => text("cli.data.codex.moved").arg("path", display_path(dir)),
        None => {
            text("cli.data.codex.restored").arg("path", display_path(&moved.location.codex_home))
        }
    })?;
    for path in &moved.leftovers {
        out.line(text("cli.data.leftovers").arg("path", display_path(path)))?;
    }
    out.value(&moved)?;
    Ok(exit::OK)
}
