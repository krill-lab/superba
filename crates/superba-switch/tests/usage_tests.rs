//! The Usage page's log over Codex rollouts and the router's database: rows
//! without a router, attribution to the profile applied at the time, costs from
//! the price table, deduplication against router rows (the router's row wins),
//! merged totals, groups, statistics and the detail of a rollout row.

#![allow(missing_docs)]

mod fakes;

use std::time::Duration;

use fakes::*;
use pretty_assertions::assert_eq;
use superba_core::{
    GenerationRecord, PriceEntry, ProfileId, RequestGrouping, RequestPage, RequestQuery,
    RequestRow, RequestSort, RequestSortKey, RequestSource, RequestStatus, RequestTotals,
    StatsGroupBy, StatsQuery, SwitchService, Timestamp, Usd,
};
use superba_switch::usage::{dedupe, merge_page};

fn session(id: &str) -> String {
    superba_core::harness_group_key(superba_core::RequestHarness::Codex, id)
}

const T1: &str = "01a1461c-18fa-73b2-8eb4-7917b838bd3e";
const T2: &str = "01a14622-ae42-72c3-ac7e-c37d351480e0";
/// 2026-10-16T19:00:00Z.
const BASE: i64 = 1_792_177_200_000;

fn at(sec: i64) -> Timestamp {
    Timestamp::from_millis(BASE + sec * 1000)
}

fn line(sec: i64, kind: &str, payload: serde_json::Value) -> String {
    let t = at(sec).to_rfc3339();
    serde_json::json!({"timestamp": t, "type": kind, "payload": payload}).to_string() + "\n"
}

fn usage(input: u64, cached: u64, output: u64) -> serde_json::Value {
    serde_json::json!({"input_tokens": input, "cached_input_tokens": cached,
        "cache_write_input_tokens": 0, "output_tokens": output,
        "reasoning_output_tokens": 0, "total_tokens": input + output})
}

/// Thread T1: one turn with two responses (tool loop) on gpt-6-luna.
fn rollout() -> String {
    let mut s = String::new();
    s += &line(
        0,
        "session_meta",
        serde_json::json!({"id": T1, "timestamp": at(0).to_rfc3339(), "cwd": "/w",
            "originator": "Codex Desktop", "cli_version": "0.163.0", "model_provider": "openai"}),
    );
    s += &line(
        10,
        "event_msg",
        serde_json::json!({"type": "task_started", "turn_id": "turn-1"}),
    );
    s += &line(
        10,
        "turn_context",
        serde_json::json!({"turn_id": "turn-1", "model": "gpt-6-luna", "effort": "low"}),
    );
    s += &line(
        11,
        "response_item",
        serde_json::json!({"type": "message", "role": "user", "content": []}),
    );
    s += &line(
        13,
        "token_usage_record",
        serde_json::json!({"thread_id": T1, "turn_id": "turn-1", "response_id": "resp_a",
            "usage": usage(1_000_000, 0, 1000)}),
    );
    s += &line(
        15,
        "response_item",
        serde_json::json!({"type": "function_call_output", "output": "x"}),
    );
    s += &line(
        18,
        "token_usage_record",
        serde_json::json!({"thread_id": T1, "turn_id": "turn-1", "response_id": "resp_b",
            "usage": usage(1_100_000, 1_000_000, 500)}),
    );
    s += &line(
        19,
        "event_msg",
        serde_json::json!({"type": "task_complete", "turn_id": "turn-1"}),
    );
    s
}

fn world() -> World {
    let w = World::new();
    let home = w.ctx.codex_home();
    let dir = home.sessions_dir().join("2026/10/17");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("rollout-2026-10-17T03-00-00-{T1}.jsonl")),
        rollout(),
    )
    .unwrap();
    *w.config.generations.lock().unwrap() = vec![GenerationRecord {
        generation: 1,
        profile_id: Some(ProfileId::new("work")),
        site_kind: None,
        applied_at: at(-3600),
    }];
    let mut prices = w.v3.feeds.prices.lock().unwrap();
    prices.rev = "2026-10-17".into();
    prices.entries.insert(
        "gpt-6-luna".into(),
        PriceEntry {
            input: Usd::from_micros(2_000_000),
            cached_input: Some(Usd::from_micros(200_000)),
            output: Usd::from_micros(8_000_000),
            ..PriceEntry::default()
        },
    );
    drop(prices);
    w
}

fn router_row(id: i64, thread: &str, start: Timestamp, input: u64, output: u64) -> RequestRow {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "started_at": start.as_millis(),
        "prompt_cache_key": thread,
        "thread_id": thread,
        "member_id": "work",
        "member_name": "work",
        "line_id": "overseas",
        "line_name": "Overseas",
        "model": "gpt-6-luna",
        "status": {"kind": "ok"},
        "input_tokens": input,
        "cached_tokens": 0,
        "output_tokens": output,
        "first_token_ms": 800,
        "duration_ms": 1500,
        "cost": 1
    }))
    .unwrap()
}

#[tokio::test]
async fn rollout_rows_are_listed_without_a_router() {
    let w = world();
    let sw = w.switcher();
    let page = sw.router_requests(RequestQuery::default()).await.unwrap();
    assert_eq!(page.rows.len(), 2);
    // Newest first, like the router's log.
    let (b, a) = (&page.rows[0], &page.rows[1]);
    assert!(a.id < 0 && b.id < 0);
    assert_eq!(a.source, RequestSource::Rollout);
    assert_eq!(a.thread_id.as_deref(), Some(T1));
    assert_eq!(a.started_at, at(11));
    assert_eq!(a.duration_ms, Some(2000));
    assert_eq!(b.started_at, at(15));
    // Attributed to the profile applied then, named from the profile set.
    assert_eq!(a.member_id, ProfileId::new("work"));
    assert_eq!(a.member_name, "work");
    assert_eq!(a.model.as_deref(), Some("gpt-6-luna"));
    assert_eq!(a.client, superba_core::RequestClient::Desktop);
    // 1M fresh input x $2 + 1000 output x $8/M.
    assert_eq!(a.cost, Some(Usd::from_micros(2_008_000)));
    // 100k fresh x $2 + 1M cached x $0.20 + 500 x $8/M.
    assert_eq!(b.cost, Some(Usd::from_micros(404_000)));
    assert_eq!(a.price_rev.as_deref(), Some("2026-10-17"));
    // Router-only columns stay empty.
    assert_eq!(a.first_token_ms, None);
    assert_eq!(a.tps_milli, None);
    assert_eq!(a.line_name, None);
    assert_eq!(a.http_status, None);
    assert_eq!(page.totals.requests, 2);
    assert_eq!(page.totals.input_tokens, 2_100_000);
    assert_eq!(page.totals.cost, Usd::from_micros(2_412_000));
    assert_eq!(page.groups.len(), 1);
    assert_eq!(page.groups[0].key, session(T1));
    assert_eq!(page.groups[0].count, 2);
    // Filters apply to rollout rows too.
    let q = RequestQuery {
        model: Some("gpt-6-astra".into()),
        ..RequestQuery::default()
    };
    assert!(sw.router_requests(q).await.unwrap().rows.is_empty());
    let q = RequestQuery {
        status: superba_core::RequestStatusFilter::Failover,
        ..RequestQuery::default()
    };
    assert!(sw.router_requests(q).await.unwrap().rows.is_empty());
    // The detail of a rollout row.
    let d = sw.router_request(a.id).await.unwrap();
    assert_eq!(d.row, *a);
    assert_eq!(d.timing.end_ms, Some(2000));
    assert!(d.attempts.is_empty());
}

#[tokio::test]
async fn a_request_the_router_recorded_is_listed_once() {
    let w = world();
    // The router saw request 1 (same thread and tokens, t0 just before the
    // rollout's derived start) and one more request of another thread.
    let mine = router_row(
        41,
        T1,
        at(11).saturating_sub(Duration::from_millis(300)),
        1_000_000,
        1000,
    );
    let other = router_row(42, T2, at(30), 10, 1);
    let mut totals = RequestTotals::default();
    superba_switch::usage::add_row(&mut totals, &mine);
    superba_switch::usage::add_row(&mut totals, &other);
    *w.router.page.lock().unwrap() = RequestPage {
        rows: vec![other.clone(), mine.clone()],
        totals,
        groups: vec![
            superba_core::RequestGroup {
                key: session(T2),
                label: T2.into(),
                count: 1,
                last: at(30),
                upstream_host: None,
                lines: Vec::new(),
            },
            superba_core::RequestGroup {
                key: session(T1),
                label: T1.into(),
                count: 1,
                last: mine.started_at,
                upstream_host: None,
                lines: Vec::new(),
            },
        ],
        truncated: false,
        recording: false,
    };
    let sw = w.switcher();
    let page = sw.router_requests(RequestQuery::default()).await.unwrap();
    let ids: Vec<(i64, RequestSource)> = page.rows.iter().map(|r| (r.id, r.source)).collect();
    assert_eq!(ids.len(), 3, "{ids:?}");
    assert_eq!(ids[0], (42, RequestSource::Router));
    assert_eq!(ids[1].1, RequestSource::Rollout);
    assert_eq!(ids[2], (41, RequestSource::Router));
    assert_eq!(page.totals.requests, 3);
    let t1 = page.groups.iter().find(|g| g.key == session(T1)).unwrap();
    assert_eq!(t1.count, 2);
    // Statistics count it once as well.
    let stats = sw
        .router_stats(StatsQuery {
            by: StatsGroupBy::Model,
            ..StatsQuery::default()
        })
        .await
        .unwrap();
    // The fake router has no statistics of its own: only the unmatched
    // rollout row is added.
    assert_eq!(stats.totals.requests, 1);
    assert_eq!(stats.buckets[0].key, "gpt-6-luna");
    assert_eq!(stats.daily.len(), 1);
    assert_eq!(stats.daily[0].key, "2026-10-16");
}

fn rollout_row(
    id: i64,
    thread: &str,
    start: i64,
    end: i64,
    tokens: Option<(u64, u64)>,
) -> RequestRow {
    let mut r = router_row(id, thread, at(start), 0, 0);
    r.source = RequestSource::Rollout;
    r.prompt_cache_key = None;
    r.duration_ms = Some(u64::try_from((end - start) * 1000).unwrap());
    r.input_tokens = tokens.map(|t| t.0);
    r.output_tokens = tokens.map(|t| t.1);
    r.first_token_ms = None;
    if tokens.is_none() {
        r.status = RequestStatus::Cancelled;
        r.turn_id = Some("turn-9".into());
        r.cost = None;
    }
    r
}

#[test]
fn dedupe_matches_once_by_thread_tokens_and_time() {
    let router = vec![
        router_row(1, T1, at(9), 100, 5),
        // Same tokens, other thread: no match.
        router_row(2, T2, at(9), 200, 5),
        // A cancelled request of the turn.
        {
            let mut r = router_row(3, T1, at(40), 0, 0);
            r.input_tokens = None;
            r.output_tokens = None;
            r.status = RequestStatus::Cancelled;
            r.turn_id = Some("turn-9".into());
            r
        },
    ];
    let rollout = vec![
        rollout_row(-1, T1, 10, 12, Some((100, 5))),
        // A second identical response: the router row is already used.
        rollout_row(-2, T1, 11, 12, Some((100, 5))),
        rollout_row(-3, T2, 10, 12, Some((201, 5))),
        // Far from the router row's start.
        rollout_row(-4, T1, 500, 502, Some((100, 5))),
        rollout_row(-5, T1, 40, 45, None),
    ];
    let left: Vec<i64> = dedupe(rollout, &router).iter().map(|r| r.id).collect();
    assert_eq!(left, [-2, -3, -4]);
}

#[test]
fn merged_pages_sort_limit_and_filter_by_group() {
    let router = RequestPage {
        rows: vec![router_row(7, T2, at(20), 10, 1)],
        totals: RequestTotals {
            requests: 1,
            ..RequestTotals::default()
        },
        ..RequestPage::default()
    };
    let rollout = vec![
        rollout_row(-1, T1, 10, 12, Some((100, 5))),
        rollout_row(-2, T1, 30, 31, Some((300, 9))),
    ];
    let q = RequestQuery {
        limit: 2,
        ..RequestQuery::default()
    };
    let page = merge_page(router.clone(), &rollout, &q, false);
    assert_eq!(page.rows.iter().map(|r| r.id).collect::<Vec<_>>(), [-2, 7]);
    assert!(page.truncated);
    assert_eq!(page.totals.requests, 3);
    // Sorted by output, ascending.
    let q = RequestQuery {
        sort: RequestSort {
            key: RequestSortKey::Output,
            descending: false,
        },
        ..RequestQuery::default()
    };
    let page = merge_page(router.clone(), &rollout, &q, false);
    assert_eq!(
        page.rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        [7, -1, -2]
    );
    // Only one session of the source list (the router's rows were filtered by
    // the router already).
    let q = RequestQuery {
        group_by: RequestGrouping::Session,
        group: Some(session(T1)),
        ..RequestQuery::default()
    };
    let page = merge_page(RequestPage::default(), &rollout, &q, false);
    assert_eq!(page.rows.len(), 2);
    let q = RequestQuery {
        group: Some(session(T2)),
        ..q
    };
    let page = merge_page(RequestPage::default(), &rollout, &q, false);
    assert!(page.rows.is_empty());
    // The source list still lists T1.
    assert_eq!(page.groups.len(), 1);
    // A live tail of router rows never adds rollout rows.
    let q = RequestQuery {
        after_id: Some(7),
        ..RequestQuery::default()
    };
    assert!(
        merge_page(RequestPage::default(), &rollout, &q, false)
            .rows
            .is_empty()
    );
}

#[test]
fn tool_filters_and_groups_apply_to_merged_rows_without_cross_tool_deduplication() {
    use superba_core::{RequestHarness, RequestStats, harness_group_key};
    let codex = rollout_row(-1, T1, 10, 12, Some((100, 5)));
    let mut claude = codex.clone();
    claude.id = -2;
    claude.harness = RequestHarness::ClaudeCode;
    claude.session_id = Some(T1.into());
    claude.model = Some("claude-alias".into());
    claude.upstream_model = Some("gpt-mini".into());
    claude.requested_model = Some("haiku".into());
    claude.codex_host = Some("wsl:Ubuntu".into());
    let mut recorded = claude.clone();
    recorded.id = 3;
    recorded.source = RequestSource::Router;
    assert_eq!(dedupe(vec![codex.clone()], &[recorded]).len(), 1);
    let rows = vec![codex, claude];
    let q = RequestQuery {
        harnesses: vec![RequestHarness::ClaudeCode],
        model: Some("haiku".into()),
        host: Some("wsl:Ubuntu".into()),
        ..RequestQuery::default()
    };
    let page = merge_page(RequestPage::default(), &rows, &q, false);
    assert_eq!(page.rows.len(), 1);
    for model in ["claude-alias", "gpt-mini", "haiku"] {
        let filtered = merge_page(
            RequestPage::default(),
            &rows,
            &RequestQuery {
                model: Some(model.into()),
                ..q.clone()
            },
            false,
        );
        assert_eq!(filtered.totals.requests, 1, "{model}");
    }
    assert_eq!(
        page.groups[0].key,
        harness_group_key(RequestHarness::ClaudeCode, T1)
    );
    let stats = superba_switch::usage::merge_stats(
        RequestStats::default(),
        &rows,
        &StatsQuery {
            harnesses: q.harnesses,
            model: q.model,
            host: q.host,
            by: StatsGroupBy::HarnessModel,
            ..StatsQuery::default()
        },
    );
    assert_eq!(stats.totals.requests, 1);
    assert!(stats.buckets[0].key.starts_with("claude-code"));
    let page = merge_page(
        RequestPage::default(),
        &rows,
        &RequestQuery::default(),
        false,
    );
    assert_eq!(page.groups.len(), 2);
}
