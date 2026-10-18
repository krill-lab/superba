//! Usage history imported from cc-switch and AI Toolbox (GitHub issue 1):
//! preview without writing, an idempotent import (stable source ids), rows
//! cc-switch read from Codex session files skipped where Superba reads those
//! files itself, recorded costs kept, imported rows in the request log with
//! their source (and filterable by it), daily totals in the statistics, and
//! removal.

mod fakes;

use std::path::{Path, PathBuf};

use fakes::*;
use pretty_assertions::assert_eq;
use superba_core::{
    ImportSource, RequestQuery, RequestSource, StatsGroupBy, StatsQuery, SwitchService, Timestamp,
    UsageImportRequest, Usd,
};

const ON_DISK: &str = "019a7c00-0000-7000-8000-0000000d15c0";
const GONE: &str = "019a7c00-0000-7000-8000-00000000901e";

fn fixture(rel: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../superba-import/tests/fixtures")
        .join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn db(path: PathBuf, sql: &str, wal: bool) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(sql).unwrap();
    if wal {
        conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))
            .unwrap();
    }
}

/// A Codex rollout of thread `ON_DISK` (2026-10-11), so cc-switch's session row
/// of that thread is one Superba reads itself.
fn rollout(w: &World) {
    let at = |sec: i64| Timestamp::from_millis(1_791_709_000_000 + sec * 1000).to_rfc3339();
    let line = |sec: i64, kind: &str, payload: serde_json::Value| {
        serde_json::json!({"timestamp": at(sec), "type": kind, "payload": payload}).to_string()
            + "\n"
    };
    let mut s = String::new();
    s += &line(
        0,
        "session_meta",
        serde_json::json!({"id": ON_DISK, "timestamp": at(0), "cwd": "/w",
        "originator": "Codex Desktop", "cli_version": "0.162.0", "model_provider": "openai"}),
    );
    s += &line(
        1,
        "turn_context",
        serde_json::json!({"turn_id": "t1", "model": "gpt-6.1-sol"}),
    );
    s += &line(
        2,
        "response_item",
        serde_json::json!({"type": "message", "role": "user", "content": []}),
    );
    s += &line(
        9,
        "token_usage_record",
        serde_json::json!({"thread_id": ON_DISK, "turn_id": "t1",
        "response_id": "resp_1", "usage": {"input_tokens": 5000, "cached_input_tokens": 4000,
        "cache_write_input_tokens": 0, "output_tokens": 100, "reasoning_output_tokens": 0, "total_tokens": 5100}}),
    );
    let dir = w.ctx.codex_home().sessions_dir().join("2026/10/11");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("rollout-2026-10-11T09-00-00-{ON_DISK}.jsonl")),
        s,
    )
    .unwrap();
}

fn ccs_world() -> World {
    let w = World::new();
    db(
        w.ctx.home_dir.join(".cc-switch/cc-switch.db"),
        &(fixture("ccs/schema-v20.sql") + &fixture("usage/ccs-v20.sql")),
        false,
    );
    rollout(&w);
    w
}

fn store(w: &World) -> PathBuf {
    w.ctx.data_dir.root().join("imported-usage.sqlite3")
}

#[tokio::test]
async fn a_preview_counts_without_writing_and_the_import_is_idempotent() {
    let w = ccs_world();
    let svc = w.switcher();
    let p = svc
        .import_usage(UsageImportRequest::preview(ImportSource::Ccs))
        .await
        .unwrap();
    assert!(p.dry_run && p.found());
    // req-total, req-fresh, req-failed and the session row of the deleted thread.
    assert_eq!(p.requests, 4);
    // The session row of the thread still on disk.
    assert_eq!(p.covered, 1);
    assert_eq!(p.other_apps, 2, "Claude Code's request and day");
    // The proxy day and the session day (before Superba's oldest rollout).
    assert_eq!(p.days, 2);
    assert_eq!(p.day_requests, 52);
    assert_eq!(p.already, 0);
    // 0.03125 + 0 (failed, recorded zero) + 0.006 + 1.25 + 0.12; req-fresh has no cost.
    assert_eq!(
        p.cost,
        Usd::from_micros(31_250 + 6_000 + 1_250_000 + 120_000)
    );
    assert_eq!(p.unpriced, 1);
    assert_eq!(
        p.first,
        Some(Timestamp::from_millis(1_785_542_400_000)),
        "2026-08-01"
    );
    assert!(p.last.unwrap().as_millis() >= 1_791_709_300_000);
    assert!(!store(&w).exists(), "a preview writes nothing");

    let r = svc
        .import_usage(UsageImportRequest::import(ImportSource::Ccs))
        .await
        .unwrap();
    assert!(!r.dry_run);
    assert_eq!((r.requests, r.days, r.already), (4, 2, 0));
    assert_eq!(r.cost, p.cost);
    assert!(store(&w).exists());
    // Again: nothing new.
    let again = svc
        .import_usage(UsageImportRequest::import(ImportSource::Ccs))
        .await
        .unwrap();
    assert_eq!((again.requests, again.days, again.already), (0, 0, 6));
    let preview = svc
        .import_usage(UsageImportRequest::preview(ImportSource::Ccs))
        .await
        .unwrap();
    assert_eq!((preview.requests, preview.already), (0, 6));
}

#[tokio::test]
async fn imported_rows_are_listed_with_their_source_and_filterable() {
    let w = ccs_world();
    let svc = w.switcher();
    svc.import_usage(UsageImportRequest::import(ImportSource::Ccs))
        .await
        .unwrap();
    let page = svc.router_requests(RequestQuery::default()).await.unwrap();
    let imported: Vec<_> = page
        .rows
        .iter()
        .filter(|r| r.source == RequestSource::CcSwitch)
        .collect();
    assert_eq!(imported.len(), 4);
    let rollout: Vec<_> = page
        .rows
        .iter()
        .filter(|r| r.source == RequestSource::Rollout)
        .collect();
    assert_eq!(
        rollout.len(),
        1,
        "Superba's own row of the covered thread, once"
    );
    let total = imported
        .iter()
        .find(|r| r.input_tokens == Some(12_000))
        .unwrap();
    assert_eq!(total.member_name, "PackyCode");
    assert_eq!(
        total.member_id.as_str(),
        "import:ccs:3f6a9b2c-2222-4d1e-8f00-000000000002"
    );
    assert_eq!(total.cost, Some(Usd::from_micros(31_250)));
    assert_eq!(total.error, None);
    let failed = imported
        .iter()
        .find(|r| r.http_status == Some(502))
        .unwrap();
    assert!(failed.status.is_error());
    assert_eq!(failed.error, None, "error texts are never imported");
    let gone = imported
        .iter()
        .find(|r| r.thread_id.as_deref() == Some(GONE))
        .unwrap();
    assert_eq!(gone.member_name, "Codex sessions");
    // The detail of an imported row.
    let d = svc.router_request(total.id).await.unwrap();
    assert_eq!(d.row, **total);

    // Filter by source.
    let only = svc
        .router_requests(RequestQuery {
            source: Some(RequestSource::CcSwitch),
            ..RequestQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(only.rows.len(), 4);
    assert_eq!(only.totals.requests, 4);
    let none = svc
        .router_requests(RequestQuery {
            source: Some(RequestSource::Rollout),
            ..RequestQuery::default()
        })
        .await
        .unwrap();
    assert!(none.rows.iter().all(|r| r.source == RequestSource::Rollout));

    // The days count in the statistics.
    let stats = svc
        .router_stats(StatsQuery {
            from: None,
            to: None,
            by: StatsGroupBy::Day,
            member: None,
            utc_offset_minutes: 0,
            source: Some(RequestSource::CcSwitch),
        })
        .await
        .unwrap();
    assert_eq!(stats.totals.requests, 4 + 52);
    let aug = stats
        .buckets
        .iter()
        .find(|b| b.key == "2026-08-01")
        .unwrap();
    assert_eq!(aug.totals.requests, 52);
    assert_eq!(aug.totals.errors, 1);
    assert_eq!(aug.totals.cost, Usd::from_micros(1_370_000));
}

#[tokio::test]
async fn removing_takes_a_tool_s_rows_out_again() {
    let w = ccs_world();
    let svc = w.switcher();
    svc.import_usage(UsageImportRequest::import(ImportSource::Ccs))
        .await
        .unwrap();
    let mut remove = UsageImportRequest::import(ImportSource::Ccs);
    remove.remove = true;
    remove.dry_run = true;
    let p = svc.import_usage(remove).await.unwrap();
    assert!(p.removed);
    assert_eq!((p.requests, p.days), (4, 2));
    remove.dry_run = false;
    let r = svc.import_usage(remove).await.unwrap();
    assert_eq!((r.requests, r.days), (4, 2));
    let page = svc.router_requests(RequestQuery::default()).await.unwrap();
    assert!(page.rows.iter().all(|r| r.source.imported_from().is_none()));
    // Removing what is not there is no error.
    let r = svc.import_usage(remove).await.unwrap();
    assert_eq!((r.requests, r.days), (0, 0));
}

#[tokio::test]
async fn ai_toolbox_imports_alike_and_a_missing_tool_imports_nothing() {
    let w = World::new();
    let svc = w.switcher();
    let r = svc
        .import_usage(UsageImportRequest::preview(ImportSource::AiToolbox))
        .await
        .unwrap();
    assert!(!r.found());
    assert_eq!(r.requests, 0);
    db(
        w.ctx
            .home_dir
            .join(".local/share/com.ai-toolbox/ai-toolbox.db"),
        &(fixture("ai-toolbox/schema-v29.sql") + &fixture("usage/ai-toolbox-v29.sql")),
        true,
    );
    // No rollouts at all: the session row and the session day are imported too.
    let r = svc
        .import_usage(UsageImportRequest::import(ImportSource::AiToolbox))
        .await
        .unwrap();
    assert_eq!(r.requests, 3);
    assert_eq!(r.days, 3);
    assert_eq!(r.covered, 0);
    let page = svc
        .router_requests(RequestQuery {
            source: Some(RequestSource::AiToolbox),
            ..RequestQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 3);
    assert!(page.rows.iter().any(|r| r.member_name == "Example Relay"));
    let tier = page
        .rows
        .iter()
        .find(|r| r.input_tokens == Some(12_000))
        .unwrap();
    assert_eq!(tier.service_tier_requested.as_deref(), Some("priority"));
}
