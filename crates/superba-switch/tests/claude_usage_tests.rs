//! Claude Code transcripts in the request log: rows without a router, the
//! source filter, pricing, deduplication against the router's Claude Code
//! rows (same response id, or same session in the time window) and the
//! detail of a transcript row.

#![allow(missing_docs)]

mod fakes;

use fakes::*;
use pretty_assertions::assert_eq;
use superba_core::{
    AgentKind, PriceEntry, RequestHarness, RequestPage, RequestQuery, RequestRow, RequestSource,
    StatsQuery, SwitchService, Timestamp, Usd,
};

/// 2026-10-16T19:00:00Z.
const BASE: i64 = 1_792_177_200_000;

fn at(sec: i64) -> Timestamp {
    Timestamp::from_millis(BASE + sec * 1000)
}

fn line(sec: i64, session: &str, msg: &str, out: u64, side: bool) -> String {
    serde_json::json!({
        "type": "assistant", "timestamp": at(sec).to_rfc3339(), "sessionId": session,
        "requestId": format!("req_{msg}"), "isSidechain": side,
        "message": {"id": msg, "model": "claude-test-1", "role": "assistant",
            "content": [{"type": "text", "text": "never kept"}],
            "usage": {"input_tokens": 1000, "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0, "output_tokens": out}}
    })
    .to_string()
        + "\n"
}

fn world() -> World {
    let w = World::new();
    let dir = w.ctx.home_dir.join(".claude/projects/-w");
    std::fs::create_dir_all(dir.join("s1/subagents")).unwrap();
    let mut main = String::new();
    main += &line(10, "s1", "msg_a", 1, false);
    main += &line(12, "s1", "msg_a", 100, false);
    main += &line(100, "s1", "msg_b", 50, false);
    main += &line(101, "s1", "msg_syn", 1, false).replace("claude-test-1", "<synthetic>");
    std::fs::write(dir.join("s1.jsonl"), main).unwrap();
    std::fs::write(
        dir.join("s1/subagents/agent-1.jsonl"),
        line(200, "s1", "msg_c", 10, true),
    )
    .unwrap();
    let mut prices = w.v3.feeds.prices.lock().unwrap();
    prices.entries.insert(
        "claude-test-1".into(),
        PriceEntry {
            input: Usd::from_micros(3_000_000),
            output: Usd::from_micros(15_000_000),
            ..PriceEntry::default()
        },
    );
    drop(prices);
    w
}

fn router_row(id: i64, session: &str, response: Option<&str>, start: Timestamp) -> RequestRow {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "started_at": start.as_millis(),
        "member_id": "work",
        "member_name": "work",
        "model": "claude-test-1",
        "status": {"kind": "ok"},
        "input_tokens": 1,
        "output_tokens": 1,
        "duration_ms": 1000,
        "harness": "claude_code",
        "session_id": session,
        "response_id": response,
    }))
    .unwrap()
}

#[tokio::test]
async fn transcript_rows_are_listed_without_a_router() {
    let w = world();
    let sw = w.switcher();
    let page = sw.router_requests(RequestQuery::default()).await.unwrap();
    let msgs: Vec<&str> = page
        .rows
        .iter()
        .filter_map(|r| r.response_id.as_deref())
        .collect();
    assert_eq!(msgs, ["msg_c", "msg_b", "msg_a"]);
    let a = &page.rows[2];
    assert_eq!(a.source, RequestSource::ClaudeTranscript);
    assert_eq!(a.harness, RequestHarness::ClaudeCode);
    assert_eq!(a.session_id.as_deref(), Some("s1"));
    assert_eq!(a.upstream_request_id.as_deref(), Some("req_msg_a"));
    assert_eq!(a.member_id.as_str(), "claude:anthropic");
    assert_eq!(a.output_tokens, Some(100));
    assert_eq!(a.duration_ms, Some(2000));
    assert_eq!(a.agent_kind, Some(AgentKind::Main));
    assert_eq!(page.rows[0].agent_kind, Some(AgentKind::Subagent));
    // 1000 x $3/M + 100 x $15/M.
    assert_eq!(a.cost, Some(Usd::from_micros(4_500)));
    use superba_sessions::claude_usage::{ID_CEIL, ID_FLOOR};
    assert!((ID_FLOOR..ID_CEIL).contains(&a.id));
    // Detail lookup.
    let d = sw.router_request(a.id).await.unwrap();
    assert_eq!(d.row, *a);
    // An id in the range that is not there is NotFound.
    let missing = ID_FLOOR + 1;
    assert!(sw.router_request(missing).await.is_err());
    // Source and harness filters.
    let q = RequestQuery {
        source: Some(RequestSource::Rollout),
        ..RequestQuery::default()
    };
    assert!(sw.router_requests(q).await.unwrap().rows.is_empty());
    let q = RequestQuery {
        harnesses: vec![RequestHarness::Codex],
        ..RequestQuery::default()
    };
    assert!(sw.router_requests(q).await.unwrap().rows.is_empty());
    let q = RequestQuery {
        source: Some(RequestSource::ClaudeTranscript),
        harnesses: vec![RequestHarness::ClaudeCode],
        ..RequestQuery::default()
    };
    assert_eq!(sw.router_requests(q).await.unwrap().rows.len(), 3);
    let stats = sw.router_stats(StatsQuery::default()).await.unwrap();
    assert_eq!(stats.totals.requests, 3);
    assert_eq!(stats.totals.output_tokens, 160);
}

#[tokio::test]
async fn requests_the_router_recorded_are_dropped() {
    let w = world();
    // msg_a by response id (far away in time), msg_b by session and time
    // window; a row of another session near msg_c does not match.
    let rows = vec![
        router_row(1, "other", Some("msg_a"), at(5000)),
        router_row(2, "s1", None, at(100 - 60)),
        router_row(3, "s2", None, at(200)),
    ];
    *w.router.page.lock().unwrap() = RequestPage {
        rows,
        ..RequestPage::default()
    };
    let sw = w.switcher();
    let page = sw
        .router_requests(RequestQuery {
            source: Some(RequestSource::ClaudeTranscript),
            ..RequestQuery::default()
        })
        .await
        .unwrap();
    let msgs: Vec<&str> = page
        .rows
        .iter()
        .filter_map(|r| r.response_id.as_deref())
        .collect();
    assert_eq!(msgs, ["msg_c"]);
}

#[test]
fn dedupe_window_bounds() {
    let mut row = router_row(-5, "s1", Some("msg_x"), at(1000));
    row.source = RequestSource::ClaudeTranscript;
    row.duration_ms = Some(10_000);
    let keep = |router: Vec<RequestRow>| {
        superba_switch::claude_usage::dedupe(vec![row.clone()], &router).len()
    };
    assert_eq!(keep(vec![router_row(1, "s1", None, at(1000 - 120))]), 0);
    assert_eq!(keep(vec![router_row(1, "s1", None, at(1000 - 121))]), 1);
    assert_eq!(keep(vec![router_row(1, "s1", None, at(1015))]), 0);
    assert_eq!(keep(vec![router_row(1, "s1", None, at(1016))]), 1);
    let mut codex = router_row(1, "s1", None, at(1000));
    codex.harness = RequestHarness::Codex;
    assert_eq!(keep(vec![codex]), 1);
}
