use std::sync::{Arc, Mutex};
use std::time::Duration;

use superba_core::data::{DataMoveProgress, DataMoveProgressSink};
use superba_core::paths::display_path;

use crate::{DataCommand, Output, Runtime, exit, text::text};

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
    }
    Ok(exit::OK)
}
