//! `spb import ccs|ai-toolbox`: providers that cc-switch or AI Toolbox keep for
//! Codex, read from their files (read-only), previewed and imported into
//! Superba's profiles and key storage. Codex is not touched: switching stays a
//! separate step (`spb switch --profile <id>`).

use superba_core::{
    ConflictAction, ImportApplyRequest, ImportCandidate, ImportKey, ImportOutcome, ImportReport,
    ImportScanRequest, ImportSelection, ImportSource, LocalizedText, SiteKind, SourceScan,
    StoreStatus,
};

use super::{Result, usage};
use crate::{ImportArgs, Output, Runtime, exit, text::text};

const HEADERS: [&str; 6] = [
    "cli.import.col.name",
    "cli.import.col.target",
    "cli.import.col.model",
    "cli.import.col.key",
    "cli.import.col.conflict",
    "cli.import.col.action",
];

pub(super) async fn run(args: &ImportArgs, rt: &Runtime, out: &mut Output) -> Result {
    let Some(source) = args.source.map(ImportSource::from) else {
        return Err(usage(out, text("cli.import.no_source")));
    };
    let on_conflict: ConflictAction = args.on_conflict.into();
    let scan = rt
        .service
        .import_scan(ImportScanRequest::source(source))
        .await?;
    let found = scan.source(source).cloned().unwrap_or(SourceScan {
        source,
        stores: Vec::new(),
        candidates: Vec::new(),
    });
    describe_stores(&found, out)?;
    let selections = if args.select.is_empty() {
        scan.default_selections(on_conflict)
    } else {
        select(&found, &args.select, on_conflict, out)?
    };
    let report = if selections.is_empty() {
        ImportReport {
            dry_run: args.dry_run,
            items: Vec::new(),
        }
    } else {
        rt.service
            .import_apply(ImportApplyRequest {
                selections,
                dry_run: args.dry_run,
            })
            .await?
    };
    if !found.candidates.is_empty() {
        let rows = found
            .candidates
            .iter()
            .map(|c| row(c, &report, out))
            .collect::<Vec<_>>();
        out.table(&HEADERS, rows)?;
    }
    for item in &report.items {
        for note in &item.notes {
            out.line(
                text("cli.import.note")
                    .arg("name", &item.name)
                    .arg_text("note", note.clone()),
            )?;
        }
    }
    summary(&found, &report, out)?;
    out.value(&serde_json::json!({
        "source": source,
        "stores": found.stores,
        "candidates": found.candidates,
        "report": report,
    }))?;
    let failed = report
        .items
        .iter()
        .any(|i| matches!(i.outcome, ImportOutcome::Failed { .. }));
    Ok(if failed { exit::ERROR } else { exit::OK })
}

fn describe_stores(found: &SourceScan, out: &mut Output) -> Result<()> {
    describe(found.source, &found.stores, found.found(), out)
}

fn describe(
    source: ImportSource,
    stores: &[superba_core::ImportStore],
    found: bool,
    out: &mut Output,
) -> Result<()> {
    let tool = source.name();
    if !found {
        let paths = stores
            .iter()
            .map(|s| s.display_path.clone())
            .collect::<Vec<_>>()
            .join(", ");
        out.line(
            text("cli.import.not_found")
                .arg("tool", tool)
                .arg("paths", paths),
        )?;
        return Ok(());
    }
    for store in stores {
        let note = store.note.clone();
        let line = match store.status {
            StoreStatus::Read => text("cli.import.store_read"),
            StoreStatus::Skipped => text("cli.import.store_skipped"),
            StoreStatus::Superseded => text("cli.import.store_superseded"),
            _ => continue,
        };
        out.line(
            line.arg("tool", tool)
                .arg("path", &store.display_path)
                .arg("format", &store.format),
        )?;
        if let Some(note) = note {
            out.line(text("cli.import.store_note").arg_text("note", note))?;
        }
    }
    Ok(())
}

/// The candidates `--select` names: by name (ignoring case), by candidate id or
/// by the tool's own id.
fn select(
    found: &SourceScan,
    names: &[String],
    on_conflict: ConflictAction,
    out: &Output,
) -> Result<Vec<ImportSelection>> {
    let mut picked: Vec<ImportSelection> = Vec::new();
    for raw in names {
        let name = raw.trim();
        if name.is_empty() {
            continue;
        }
        let matches = |c: &ImportCandidate| {
            c.name.trim().eq_ignore_ascii_case(name)
                || c.name.trim() == name
                || c.id == name
                || c.id.split_once(':').is_some_and(|(_, local)| {
                    local == name || local.rsplit('/').next() == Some(name)
                })
        };
        let hits: Vec<&ImportCandidate> = found.candidates.iter().filter(|c| matches(c)).collect();
        if hits.is_empty() {
            let available = found
                .candidates
                .iter()
                .filter(|c| c.importable())
                .map(|c| c.name.clone())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(usage(
                out,
                text("cli.import.unknown_name")
                    .arg("name", name)
                    .arg("available", available),
            ));
        }
        for c in hits {
            if !picked.iter().any(|s| s.candidate == c.id) {
                picked.push(ImportSelection::new(c.id.clone(), on_conflict));
            }
        }
    }
    Ok(picked)
}

fn render(out: &Output, t: &LocalizedText) -> String {
    out.render(t)
}

fn target(c: &ImportCandidate, out: &Output) -> String {
    let Some(t) = &c.target else {
        return out.none();
    };
    match t.kind {
        SiteKind::Krill => {
            let lines = t
                .lines
                .iter()
                .map(|l| render(out, &l.name))
                .collect::<Vec<_>>()
                .join(", ");
            out.render(&text("cli.import.target_krill").arg("lines", lines))
        }
        _ => {
            let host = t
                .base_url
                .as_ref()
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_default();
            let mut s = out.render(&text("cli.import.target_site").arg("host", host));
            if t.lines.len() > 1 {
                s.push_str(&out.render(&text("cli.import.target_lines").arg("n", t.lines.len())));
            }
            if !t.model_map.is_empty() {
                s.push_str(&out.render(&text("cli.import.target_map").arg("n", t.model_map.len())));
            }
            s
        }
    }
}

fn key(c: &ImportCandidate, out: &Output) -> String {
    match &c.key {
        ImportKey::Present { masked } => masked.clone(),
        ImportKey::Keychain => out.label("cli.import.key_keychain"),
        ImportKey::EnvVar { name } => out.render(&text("cli.import.key_env").arg("name", name)),
        _ => out.label("cli.import.key_missing"),
    }
}

fn conflict(c: &ImportCandidate, out: &Output) -> String {
    let Some(x) = &c.conflict else {
        return out.none();
    };
    let key = match (x.same_name, x.same_base_url) {
        (true, true) => "cli.import.conflict_both",
        (true, false) => "cli.import.conflict_name",
        _ => "cli.import.conflict_url",
    };
    out.render(&text(key).arg("name", &x.profile_name))
}

fn action(c: &ImportCandidate, report: &ImportReport, out: &Output) -> String {
    let Some(item) = report.items.iter().find(|i| i.candidate == c.id) else {
        return match c.not_importable_reason() {
            Some(reason) => out.render(
                &text("cli.import.action_not_importable").arg_text("reason", reason.clone()),
            ),
            None => out.label("cli.import.action_not_selected"),
        };
    };
    let dry = report.dry_run;
    let t = match &item.outcome {
        ImportOutcome::Created { profile_name, .. } => text(if dry {
            "cli.import.action_would_create"
        } else {
            "cli.import.action_created"
        })
        .arg("name", profile_name),
        ImportOutcome::Updated { profile_name, .. } => text(if dry {
            "cli.import.action_would_update"
        } else {
            "cli.import.action_updated"
        })
        .arg("name", profile_name),
        ImportOutcome::Skipped { reason } => {
            text("cli.import.action_skipped").arg_text("reason", reason.clone())
        }
        ImportOutcome::Failed { error } => text("cli.import.action_failed").arg("error", error),
        _ => return out.none(),
    };
    out.render(&t)
}

fn row(c: &ImportCandidate, report: &ImportReport, out: &Output) -> Vec<String> {
    let mut name = c.name.clone();
    if c.current {
        name = out.render(&text("cli.import.current").arg("name", name));
    }
    if c.app != superba_core::ForeignApp::Codex {
        name = out.render(
            &text("cli.import.app")
                .arg("name", name)
                .arg("app", c.app.label()),
        );
    }
    vec![
        name,
        target(c, out),
        c.model.clone().unwrap_or_else(|| out.none()),
        if c.importable() {
            key(c, out)
        } else {
            out.none()
        },
        conflict(c, out),
        action(c, report, out),
    ]
}

fn summary(found: &SourceScan, report: &ImportReport, out: &mut Output) -> Result<()> {
    if !found.found() {
        return Ok(());
    }
    let n = report.imported();
    if report.dry_run {
        out.line(text("cli.import.dry_run").arg("n", n))?;
    } else if let Some(first) = report.first_profile() {
        out.line(text("cli.import.done").arg("n", n).arg("id", first))?;
    } else {
        out.line(text("cli.import.nothing"))?;
    }
    Ok(())
}

/// `spb import usage ccs|ai-toolbox [--dry-run] [--remove]`: the usage history
/// another tool recorded for Codex, previewed (counts, date range, recorded
/// cost) and imported into Superba's usage history, or taken out again.
pub(super) async fn usage_run(
    args: &crate::ImportUsageArgs,
    rt: &Runtime,
    out: &mut Output,
) -> Result {
    let source: ImportSource = args.source.into();
    let request = superba_core::UsageImportRequest {
        source,
        dry_run: args.dry_run,
        remove: args.remove,
    };
    let r = rt.service.import_usage(request).await?;
    let tool = source.name();
    if r.removed {
        out.line(
            text(if r.dry_run {
                "cli.import_usage.would_remove"
            } else {
                "cli.import_usage.removed"
            })
            .arg("tool", tool)
            .arg("n", r.requests)
            .arg("days", r.days),
        )?;
        out.value(&r)?;
        return Ok(exit::OK);
    }
    describe(source, &r.stores, r.found(), out)?;
    if !r.found() {
        out.value(&r)?;
        return Ok(exit::OK);
    }
    let range = match (r.first, r.last) {
        (Some(a), Some(b)) => out.render(
            &text("cli.import_usage.range")
                .arg("from", a.to_rfc3339().chars().take(10).collect::<String>())
                .arg("to", b.to_rfc3339().chars().take(10).collect::<String>()),
        ),
        _ => String::new(),
    };
    out.line(
        text(if r.dry_run {
            "cli.import_usage.would_add"
        } else {
            "cli.import_usage.added"
        })
        .arg("n", r.requests)
        .arg("days", r.days)
        .arg("day_requests", r.day_requests)
        .arg("range", range)
        .arg("cost", r.cost.to_string()),
    )?;
    if r.unpriced > 0 {
        out.line(
            text("cli.import_usage.unpriced")
                .arg("n", r.unpriced)
                .arg("tool", tool),
        )?;
    }
    if r.already > 0 {
        out.line(text("cli.import_usage.already").arg("n", r.already))?;
    }
    if r.covered > 0 {
        out.line(text("cli.import_usage.covered").arg("n", r.covered))?;
    }
    if r.other_apps > 0 {
        out.line(text("cli.import_usage.other_apps").arg("n", r.other_apps))?;
    }
    for note in &r.notes {
        out.line(text("cli.import.store_note").arg_text("note", note.clone()))?;
    }
    if r.dry_run && (r.requests > 0 || r.days > 0) {
        out.line(text("cli.import_usage.dry_run_hint").arg("source", source.id()))?;
    }
    out.value(&r)?;
    Ok(exit::OK)
}
