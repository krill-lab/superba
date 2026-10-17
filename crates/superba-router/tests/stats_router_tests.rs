//! The request log and the stats line through a running router: strip of the
//! outgoing request, injection gated by client and turn kind, rows in
//! `usage.sqlite3` read through `Reader`, live and in-flight rows from the admin
//! API, numbers only (never headers or bodies), clear and retention.

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use common::responses::*;
use common::*;
use serde_json::{Value, json};
use superba_core::{
    ClearRequest, DataDir, Multiplier, PriceEntry, PriceTable, RequestClient, RequestGrouping,
    RequestKind, RequestQuery, RequestSort, RequestSortKey, RequestStatus, RequestStatusFilter,
    RequestTransport, StatsGroupBy, StatsLineStyle, StatsQuery, Timestamp, Usd, provider_line_key,
};
use superba_router::client::AdminClient;
use superba_router::paths::RouterPaths;
use superba_router::{
    MemberKind, PriceConfig, Reader, RouterConfig, RouterServer, ShutdownHandle, StatsConfig,
    StatsLineConfig,
};
use tokio::sync::Notify;

const LINE_TEMPLATE: &str = "⚡ {tps} tok/s · 首字 {ttft} · 本轮 {input} in / {output} out";

fn stats(line: bool) -> StatsConfig {
    let mut entries = BTreeMap::new();
    entries.insert(
        "gpt-x".to_owned(),
        PriceEntry {
            input: Usd::from_dollars(1.25).unwrap(),
            cached_input: Some(Usd::from_dollars(0.125).unwrap()),
            output: Usd::from_dollars(10.0).unwrap(),
            ..PriceEntry::default()
        },
    );
    StatsConfig {
        line: line.then(|| StatsLineConfig {
            template: LINE_TEMPLATE.into(),
            style: StatsLineStyle::Quote,
            originators: vec!["Codex Desktop".into()],
        }),
        prices: PriceConfig {
            table: PriceTable {
                rev: "test-rev".into(),
                entries,
            },
            ratio_by_member: [("krill".to_owned(), Multiplier::ONE)].into(),
            overrides: BTreeMap::new(),
        },
        ..StatsConfig::default()
    }
}

struct Running {
    router: Router,
    paths: RouterPaths,
    admin: AdminClient,
    _dir: tempfile::TempDir,
}

/// A router started from files in a temp data dir (so it records).
async fn start_recording(mut cfg: RouterConfig, stats: StatsConfig) -> Running {
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    cfg.stats = Some(stats);
    let router = start_from(&paths, cfg).await;
    let admin = AdminClient::new(
        router.addr.port(),
        superba_core::Secret::new(&router.admin_token),
    )
    .unwrap();
    Running {
        router,
        paths,
        admin,
        _dir: dir,
    }
}

async fn start_from(paths: &RouterPaths, cfg: RouterConfig) -> Router {
    let secrets = secrets_for(&cfg);
    cfg.save(paths).unwrap();
    secrets.save(paths).unwrap();
    let server = RouterServer::bind_from_files(paths.clone()).await.unwrap();
    let router = Router {
        addr: server.local_addr(),
        admin_token: server.admin_token().expose().to_owned(),
        handle: server.shutdown_handle(),
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
    };
    tokio::spawn(server.serve());
    router
}

/// Stop a router and wait until its recorder wrote everything.
async fn stop(handle: &ShutdownHandle, paths: &RouterPaths) {
    handle.shutdown();
    let deadline = Instant::now() + Duration::from_secs(10);
    while paths.runtime_json().exists() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn post(
    r: &Router,
    originator: Option<&str>,
    turn: Option<String>,
    body: &Value,
) -> reqwest::Response {
    let mut rb = r
        .http
        .post(r.url("/backend-api/codex/responses"))
        .bearer_auth(ROUTER_TOKEN)
        .header("content-type", "application/json")
        .header("session-id", "019a7c00-0000-7000-8000-0000000000e1")
        .body(serde_json::to_vec(body).unwrap());
    if let Some(o) = originator {
        rb = rb.header("originator", o);
    }
    if let Some(t) = turn {
        rb = rb.header("x-codex-turn-metadata", t);
    }
    rb.send().await.unwrap()
}

/// Rows of the request log once at least `n` are there.
async fn rows(paths: &RouterPaths, n: usize) -> superba_core::RequestPage {
    let reader = Reader::open(paths).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let page = reader.query(&RequestQuery::default()).unwrap();
        if page.rows.len() >= n || Instant::now() > deadline {
            return page;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn chunks(events: &[Ev]) -> Mode {
    Mode::Chunks(Arc::new(
        events
            .iter()
            .map(|e| Bytes::from(sse(std::slice::from_ref(e), false)))
            .collect(),
    ))
}

fn krill(up: &Upstream) -> RouterConfig {
    test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", up.base("/backend-api/codex"))],
    )])
}

const ANSWER: &str = "Earlier answer.\n\n> ⚡\u{2060} 87 tok/s · 首字 0.82s · 本轮 1 in / 1 out";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_turns_get_the_line_and_every_request_is_recorded() {
    let up = upstream(chunks(&text_answer())).await;
    let run = start_recording(krill(&up), stats(true)).await;
    let r = &run.router;
    let body = request_body("gpt-x", ANSWER, json!({}));

    // 1. The desktop app's user turn: line added, earlier line stripped.
    let resp = post(
        r,
        Some("Codex Desktop"),
        Some(turn_metadata("turn-1", "turn", "user")),
        &body,
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert!(resp.headers().get("content-length").is_none());
    let events = parse_sse(&resp.bytes().await.unwrap());
    let text = consistent_text(&events, "msg_1");
    let line = line_of(&text).expect("stats line");
    assert!(line.starts_with("> ⚡\u{2060} "), "{line}");
    assert!(line.ends_with("本轮 4,002 in / 51 out"), "{line}");
    let sent: Value = serde_json::from_slice(&up.seen()[0].body).unwrap();
    assert_eq!(sent["input"][1]["content"][0]["text"], "Earlier answer.");
    assert_eq!(
        sent["input"][0]["content"][0]["text"],
        "hi ⚡\u{2060} user text stays"
    );

    // 2. A tool-loop request of the same turn: totals add up.
    let resp = post(
        r,
        Some("Codex Desktop"),
        Some(turn_metadata("turn-1", "turn", "user")),
        &body,
    )
    .await;
    let text = consistent_text(&parse_sse(&resp.bytes().await.unwrap()), "msg_1");
    assert!(
        line_of(&text).unwrap().ends_with("本轮 8,004 in / 102 out"),
        "{text}"
    );

    // 3. codex exec, a title request, the TUI (not enabled): never a line.
    for (originator, source) in [
        ("codex_exec", "user"),
        ("Codex Desktop", "thread_title"),
        ("codex_cli_rs", "user"),
    ] {
        let resp = post(
            r,
            Some(originator),
            Some(turn_metadata("turn-2", "turn", source)),
            &body,
        )
        .await;
        let text = consistent_text(&parse_sse(&resp.bytes().await.unwrap()), "msg_1");
        assert_eq!(text, "Hello world.", "{originator} {source}");
    }
    // Every request was stripped (the strip does not depend on the line).
    for s in up.seen() {
        let v: Value = serde_json::from_slice(&s.body).unwrap();
        assert_eq!(v["input"][1]["content"][0]["text"], "Earlier answer.");
    }

    let page = rows(&run.paths, 5).await;
    assert_eq!(page.rows.len(), 5);
    assert_eq!(page.totals.requests, 5);
    assert_eq!(page.totals.ok, 5);
    assert_eq!(page.totals.input_tokens, 5 * 4002);
    assert_eq!(page.totals.cached_tokens, 5 * 3328);
    assert_eq!(page.totals.output_tokens, 5 * 51);
    assert_eq!(page.totals.cost, Usd::from_micros(5 * 1769));
    assert_eq!(page.totals.unpriced, 0);
    // Newest first.
    let first = &page.rows[4];
    assert!(first.injected);
    assert_eq!(first.stripped, 1);
    assert_eq!(first.client, RequestClient::Desktop);
    assert_eq!(first.transport, RequestTransport::Http);
    assert_eq!(first.request_kind, Some(RequestKind::Turn));
    assert_eq!(first.thread_source.as_deref(), Some("user"));
    assert_eq!(first.turn_id.as_deref(), Some("turn-1"));
    assert_eq!(
        first.prompt_cache_key.as_deref(),
        Some("019a7c00-0000-7000-8000-0000000000e1")
    );
    assert_eq!(first.member_id.as_str(), "krill");
    assert_eq!(first.line_name.as_deref(), Some("A"));
    assert_eq!(first.upstream_host.as_deref(), Some("127.0.0.1"));
    assert_eq!(first.model.as_deref(), Some("gpt-x"));
    assert_eq!(first.response_model.as_deref(), Some("gpt-x-2026"));
    assert_eq!(first.service_tier_answered.as_deref(), Some("default"));
    assert_eq!(first.http_status, Some(200));
    assert_eq!(first.status, RequestStatus::Ok);
    assert_eq!(first.fresh_input_tokens(), Some(674));
    assert_eq!(first.cost, Some(Usd::from_micros(1769)));
    assert_eq!(first.price_rev.as_deref(), Some("test-rev"));
    assert!(first.ttfb_ms.is_some() && first.first_token_ms.is_some());
    assert!(first.duration_ms >= first.first_token_ms);
    // Timing sums of the totals (statistics show average first token / TPS).
    let with_ttft: Vec<u64> = page.rows.iter().filter_map(|r| r.first_token_ms).collect();
    assert_eq!(page.totals.first_token_rows, with_ttft.len() as u64);
    assert_eq!(
        page.totals.first_token_ms_sum,
        with_ttft.iter().sum::<u64>()
    );
    let weighted: u64 = page
        .rows
        .iter()
        .filter_map(|r| Some(r.tps_milli? * r.output_tokens?))
        .sum();
    assert_eq!(page.totals.tps_weighted_milli, weighted);
    assert_eq!(
        page.totals.tps_milli(),
        (page.totals.tps_output_tokens > 0).then(|| weighted / page.totals.tps_output_tokens)
    );
    let exec = page
        .rows
        .iter()
        .find(|r| r.client == RequestClient::Exec)
        .unwrap();
    assert!(!exec.injected, "codex exec never gets the line");
    assert_eq!(page.rows.iter().filter(|r| r.injected).count(), 2);
    // Ids increase with time.
    assert!(page.rows.windows(2).all(|w| w[0].id > w[1].id));

    // Source list: one session, one provider with its line.
    assert_eq!(page.groups.len(), 1);
    assert_eq!(page.groups[0].key, "019a7c00-0000-7000-8000-0000000000e1");
    assert_eq!(page.groups[0].count, 5);
    let reader = Reader::open(&run.paths).unwrap();
    let by_provider = reader
        .query(&RequestQuery {
            group_by: RequestGrouping::Provider,
            ..RequestQuery::default()
        })
        .unwrap();
    assert_eq!(by_provider.groups.len(), 1);
    let krill = &by_provider.groups[0];
    assert_eq!(
        (krill.key.as_str(), krill.label.as_str()),
        ("krill", "KRILL")
    );
    assert_eq!(krill.count, 5);
    assert_eq!(krill.upstream_host.as_deref(), Some("127.0.0.1"));
    assert_eq!(krill.lines.len(), 1);
    assert_eq!(krill.lines[0].label, "A");
    assert_eq!(krill.lines[0].count, 5);
    // Filters, search, sort, paging, live updates.
    let q = |q: RequestQuery| reader.query(&q).unwrap();
    assert_eq!(
        q(RequestQuery {
            client: Some(RequestClient::Exec),
            ..RequestQuery::default()
        })
        .rows
        .len(),
        1
    );
    assert_eq!(
        q(RequestQuery {
            search: Some("e1".into()),
            ..RequestQuery::default()
        })
        .totals
        .requests,
        5
    );
    assert_eq!(
        q(RequestQuery {
            search: Some("no such thing".into()),
            ..RequestQuery::default()
        })
        .rows
        .len(),
        0
    );
    let limited = q(RequestQuery {
        limit: 2,
        sort: RequestSort {
            key: RequestSortKey::Time,
            descending: false,
        },
        ..RequestQuery::default()
    });
    assert_eq!(limited.rows.len(), 2);
    assert!(limited.truncated);
    assert_eq!(
        limited.totals.requests, 5,
        "totals cover every matching row"
    );
    assert!(limited.rows[0].id < limited.rows[1].id);
    let after = q(RequestQuery {
        after_id: Some(page.rows[1].id),
        ..RequestQuery::default()
    });
    assert_eq!(after.rows.len(), 1);
    assert_eq!(
        q(RequestQuery {
            group: Some("019a7c00-0000-7000-8000-0000000000e1".into()),
            ..RequestQuery::default()
        })
        .rows
        .len(),
        5
    );

    // Detail: timing, attempts and log lines; no content (not recorded).
    let d = reader.detail(first.id).unwrap();
    assert_eq!(d.row, *first);
    assert!(d.timing.first_event_ms.is_some());
    assert_eq!(d.attempts.len(), 1);
    assert_eq!(d.attempts[0].outcome, "answered");
    assert!(d.log.iter().any(|l| l == "stats line added"), "{:?}", d.log);
    assert!(d.log.iter().any(|l| l.contains("removed from the input")));
    assert!(reader.detail(999_999).is_err());

    // Statistics.
    let st = reader
        .stats(&StatsQuery {
            by: StatsGroupBy::Model,
            ..StatsQuery::default()
        })
        .unwrap();
    assert_eq!(st.buckets.len(), 1);
    assert_eq!(st.buckets[0].key, "gpt-x");
    assert_eq!(st.buckets[0].totals.requests, 5);
    assert_eq!(st.daily.len(), 1);
    assert_eq!(st.totals.cost, Usd::from_micros(5 * 1769));
    let day = reader
        .stats(&StatsQuery {
            by: StatsGroupBy::Day,
            utc_offset_minutes: 480,
            ..StatsQuery::default()
        })
        .unwrap();
    assert_eq!(day.buckets[0].key.len(), 10, "YYYY-MM-DD");
    let provider = reader
        .stats(&StatsQuery {
            by: StatsGroupBy::Provider,
            ..StatsQuery::default()
        })
        .unwrap();
    assert_eq!(provider.buckets[0].label, "KRILL");
    assert_eq!(provider.buckets[0].lines.len(), 1);
    assert_eq!(provider.buckets[0].lines[0].totals.requests, 5);

    // The live ring and the status.
    let live = run.admin.requests(None, None).await.unwrap();
    assert_eq!(live.len(), 5);
    let newest = run
        .admin
        .requests(Some(page.rows[1].id), Some(10))
        .await
        .unwrap();
    assert_eq!(newest.len(), 1);
    let status = run.admin.status().await.unwrap();
    let s = status.stats.unwrap();
    assert!(s.recording);
    assert_eq!(s.rows_written, 5);
    assert_eq!(s.dropped, 0);
    assert!(s.db_bytes.unwrap() > 0);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(run.paths.usage_db())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // Numbers only: no prompt text in the database.
    assert!(!contains(&run.paths.usage_db(), b"user text stays"));
    // No headers or bodies anywhere: the request log is the only database.
    assert!(
        !run.paths
            .usage_db()
            .with_file_name("content.sqlite3")
            .exists()
    );
}

fn contains(path: &Path, needle: &[u8]) -> bool {
    let mut all = std::fs::read(path).unwrap_or_default();
    for ext in ["sqlite3-wal", "sqlite3-shm"] {
        all.extend(std::fs::read(path.with_extension(ext)).unwrap_or_default());
    }
    memchr::memmem::find(&all, needle).is_some()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn errors_and_failovers_are_recorded() {
    let limited = upstream(Mode::Json(
        429,
        r#"{"error":{"message":"slow down","type":"rate_limit","code":"rate_limit_exceeded"}}"#,
    ))
    .await;
    let cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![
            line("a", dead_base()),
            line("b", limited.base("/backend-api/codex")),
        ],
    )]);
    let run = start_recording(cfg, stats(true)).await;
    let body = request_body("gpt-x", "a", json!({}));
    // Failover to line b, which answers 429 (passed through, never retried).
    let resp = post(
        &run.router,
        Some("Codex Desktop"),
        Some(turn_metadata("t", "turn", "user")),
        &body,
    )
    .await;
    assert_eq!(resp.status(), 429);
    resp.bytes().await.unwrap();
    let page = rows(&run.paths, 1).await;
    let row = &page.rows[0];
    assert_eq!(
        row.status,
        RequestStatus::Http {
            status: 429,
            code: Some("rate_limit_exceeded".into())
        }
    );
    assert_eq!(row.failovers, 1);
    assert_eq!(row.line_name.as_deref(), Some("B"));
    assert!(row.input_tokens.is_none() && row.cost.is_none());
    let reader = Reader::open(&run.paths).unwrap();
    let d = reader.detail(row.id).unwrap();
    assert_eq!(d.attempts.len(), 2);
    assert!(
        d.attempts[0].outcome.starts_with("connect"),
        "{:?}",
        d.attempts
    );
    assert_eq!(d.attempts[1].status, Some(429));
    let errors = reader
        .query(&RequestQuery {
            status: RequestStatusFilter::Errors,
            ..RequestQuery::default()
        })
        .unwrap();
    assert_eq!(errors.rows.len(), 1);
    let ok = reader
        .query(&RequestQuery {
            status: RequestStatusFilter::Ok,
            ..RequestQuery::default()
        })
        .unwrap();
    assert!(ok.rows.is_empty());
    let failed_over = reader
        .query(&RequestQuery {
            status: RequestStatusFilter::Failover,
            ..RequestQuery::default()
        })
        .unwrap();
    assert_eq!(failed_over.rows.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_in_flight_are_listed_and_a_client_that_leaves_cancels() {
    let gate = Arc::new(Notify::new());
    let slow = upstream(Mode::Sse {
        first: "event: response.created\ndata: {\"type\":\"response.created\"}\n\n",
        rest: "",
        gate: Some(gate.clone()),
        break_mid: false,
    })
    .await;
    let cfg = test_config(vec![member(
        "slow",
        MemberKind::Compatible,
        false,
        vec![line("s", slow.base("/v1"))],
    )]);
    let run = start_recording(cfg, stats(true)).await;
    let body = request_body("gpt-x", "a", json!({}));
    let mut resp = post(
        &run.router,
        Some("codex_cli_rs"),
        Some(turn_metadata("t", "turn", "user")),
        &body,
    )
    .await;
    assert_eq!(resp.status(), 200);
    let _ = resp.chunk().await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let active = loop {
        let active = run.admin.inflight().await.unwrap();
        if !active.is_empty() || Instant::now() > deadline {
            break active;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].member_id.as_str(), "slow");
    assert_eq!(active[0].client, RequestClient::Tui);
    assert_eq!(active[0].model.as_deref(), Some("gpt-x"));
    assert_eq!(active[0].phase, superba_core::InflightPhase::Streaming);
    // Codex goes away (Stop).
    drop(resp);
    let page = rows(&run.paths, 1).await;
    assert_eq!(page.rows[0].status, RequestStatus::Cancelled);
    assert!(run.admin.inflight().await.unwrap().is_empty());
    gate.notify_waiters();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn router_unavailable_and_broken_streams_are_recorded() {
    let broken = upstream(Mode::Sse {
        first: "event: response.created\ndata: {\"type\":\"response.created\"}\n\n",
        rest: "",
        gate: None,
        break_mid: true,
    })
    .await;
    let cfg = test_config(vec![
        member(
            "dead",
            MemberKind::Krill,
            false,
            vec![line("x", dead_base())],
        ),
        member(
            "flaky",
            MemberKind::Compatible,
            false,
            vec![line("f", broken.base("/v1"))],
        ),
    ]);
    let run = start_recording(cfg, stats(false)).await;
    let r = &run.router;
    let body = request_body("gpt-x", "a", json!({}));
    let mut resp = post(r, None, None, &body).await;
    assert_eq!(resp.status(), 200);
    while let Ok(Some(_)) = resp.chunk().await {}
    let page = rows(&run.paths, 1).await;
    assert_eq!(page.rows[0].status, RequestStatus::Broken);
    assert_eq!(page.rows[0].member_id.as_str(), "flaky");

    // Every line down.
    let cfg2 = test_config(vec![member(
        "dead",
        MemberKind::Krill,
        false,
        vec![line("x", dead_base())],
    )]);
    let run2 = start_recording(cfg2, stats(false)).await;
    let resp = post(&run2.router, None, None, &body).await;
    assert_eq!(resp.status(), 502);
    let page = rows(&run2.paths, 1).await;
    assert_eq!(
        page.rows[0].status,
        RequestStatus::Http {
            status: 502,
            code: Some("router_unavailable".into())
        }
    );
    assert!(page.rows[0].error.as_deref().unwrap().contains("dead/x"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_numbers_are_recorded_and_a_clear_reaches_the_running_router() {
    let up = upstream(chunks(&text_answer())).await;
    let run = start_recording(krill(&up), stats(true)).await;
    let mut body = request_body("gpt-x", ANSWER, json!({}));
    body["api_key"] = json!("sk-body-secret-123456");
    let resp = post(
        &run.router,
        Some("Codex Desktop"),
        Some(turn_metadata("t", "turn", "user")),
        &body,
    )
    .await;
    resp.bytes().await.unwrap();
    let page = rows(&run.paths, 1).await;
    assert_eq!(page.rows[0].input_tokens, Some(4002), "numbers stay");
    let reader = Reader::open(&run.paths).unwrap();
    // Neither headers nor bodies: no prompt text, no keys, no second database.
    for needle in [
        ROUTER_TOKEN.to_owned(),
        key_of("krill"),
        "sk-body-secret".into(),
        "user text stays".into(),
        "authorization".into(),
    ] {
        assert!(
            !contains(&run.paths.usage_db(), needle.as_bytes()),
            "{needle} in the request log"
        );
    }
    assert!(
        !run.paths
            .usage_db()
            .with_file_name("content.sqlite3")
            .exists()
    );
    // A full clear through the running router.
    let report = superba_router::store::clear(&run.paths, &ClearRequest::default())
        .await
        .unwrap();
    assert_eq!(report.rows, 1);
    assert!(
        reader
            .query(&RequestQuery::default())
            .unwrap()
            .rows
            .is_empty()
    );
    assert!(run.admin.requests(None, None).await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retention_and_ids_survive_restarts_and_clear_works_while_stopped() {
    let up = upstream(chunks(&text_answer())).await;
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    let mut cfg = krill(&up);
    let mut s = stats(false);
    s.max_rows = 2;
    cfg.stats = Some(s);
    let body = request_body("gpt-x", "a", json!({}));

    let r = start_from(&paths, cfg.clone()).await;
    for _ in 0..3 {
        post(&r, None, None, &body).await.bytes().await.unwrap();
    }
    assert_eq!(rows(&paths, 3).await.rows.len(), 3);
    stop(&r.handle, &paths).await;
    // Age one row past the retention period.
    {
        let conn = rusqlite::Connection::open(paths.usage_db()).unwrap();
        let old = Timestamp::now().as_millis() - 100 * 86_400_000;
        conn.execute(
            "UPDATE requests SET started_ms = ?1 WHERE id = (SELECT MIN(id) FROM requests)",
            [old],
        )
        .unwrap();
    }
    // Retention runs at start: 90 days and 2 rows.
    let r = start_from(&paths, cfg.clone()).await;
    let reader = Reader::open(&paths).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let ids = loop {
        let ids: Vec<i64> = reader
            .query(&RequestQuery::default())
            .unwrap()
            .rows
            .iter()
            .map(|r| r.id)
            .collect();
        if ids.len() <= 2 || Instant::now() > deadline {
            break ids;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(ids, [3, 2]);
    post(&r, None, None, &body).await.bytes().await.unwrap();
    let page = rows(&paths, 3).await;
    assert_eq!(page.rows[0].id, 4, "ids continue after a restart");
    stop(&r.handle, &paths).await;
    // Clear directly in the files while the router is stopped.
    let before = Timestamp::from_millis(Timestamp::now().as_millis() + 1000);
    let report = superba_router::store::clear(
        &paths,
        &ClearRequest {
            before: Some(before),
        },
    )
    .await
    .unwrap();
    assert_eq!(report.rows, 3);
    assert!(
        Reader::open(&paths)
            .unwrap()
            .query(&RequestQuery::default())
            .unwrap()
            .rows
            .is_empty()
    );
    // A missing database reads as empty.
    let empty = RouterPaths::new(&DataDir::new(dir.path().join("none")));
    let reader = Reader::open(&empty).unwrap();
    assert!(
        reader
            .query(&RequestQuery::default())
            .unwrap()
            .rows
            .is_empty()
    );
    assert_eq!(
        reader
            .stats(&StatsQuery::default())
            .unwrap()
            .totals
            .requests,
        0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_stats_nothing_is_recorded_and_bodies_pass_byte_for_byte() {
    let up = upstream(chunks(&text_answer())).await;
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    let r = start_from(&paths, krill(&up)).await;
    let body = request_body("gpt-x", ANSWER, json!({}));
    let raw = serde_json::to_vec(&body).unwrap();
    let resp = post(
        &r,
        Some("Codex Desktop"),
        Some(turn_metadata("t", "turn", "user")),
        &body,
    )
    .await;
    let out = resp.bytes().await.unwrap();
    assert_eq!(
        String::from_utf8(out.to_vec()).unwrap(),
        sse(&text_answer(), false)
    );
    assert_eq!(&up.seen()[0].body[..], &raw[..], "no strip without stats");
    assert!(!paths.usage_db().exists());
    let status: superba_router::RouterStatus = r
        .http
        .get(r.url("/_superba/v1/status"))
        .bearer_auth(&r.admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(status.stats.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paused_recording_still_strips_and_injects() {
    let up = upstream(chunks(&text_answer())).await;
    let mut s = stats(true);
    s.record = false;
    let run = start_recording(krill(&up), s).await;
    let body = request_body("gpt-x", ANSWER, json!({}));
    let resp = post(
        &run.router,
        Some("Codex Desktop"),
        Some(turn_metadata("t", "turn", "user")),
        &body,
    )
    .await;
    let text = consistent_text(&parse_sse(&resp.bytes().await.unwrap()), "msg_1");
    assert!(line_of(&text).is_some());
    let sent: Value = serde_json::from_slice(&up.seen()[0].body).unwrap();
    assert_eq!(sent["input"][1]["content"][0]["text"], "Earlier answer.");
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(!run.paths.usage_db().exists());
    assert!(!run.admin.status().await.unwrap().stats.unwrap().recording);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_log_groups_by_provider_with_each_providers_lines() {
    let up = upstream(chunks(&text_answer())).await;
    let run = start_recording(krill(&up), stats(false)).await;
    stop(&run.router.handle, &run.paths).await;
    assert!(
        run.paths.usage_db().exists(),
        "the recorder created the log"
    );
    // Two providers, Krill on two lines; one request failed before a line
    // answered (only the last line tried is known), one never reached a member.
    let now = Timestamp::now().as_millis();
    type Fixture = (
        i64,
        &'static str,
        &'static str,
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static str>,
        &'static str,
        Option<i64>,
    );
    let fixture: [Fixture; 6] = [
        (
            50,
            "krill",
            "Krill (before)",
            Some("cn"),
            Some("China Express"),
            Some("slb.test"),
            "ok",
            Some(100),
        ),
        (
            40,
            "krill",
            "Krill",
            Some("cn"),
            Some("China Express"),
            Some("slb.test"),
            "ok",
            Some(100),
        ),
        (
            30,
            "krill",
            "Krill",
            None,
            Some("China Express"),
            Some("slb.test"),
            "failed",
            None,
        ),
        (
            20,
            "krill",
            "Krill",
            Some("overseas"),
            Some("Overseas"),
            Some("api.test"),
            "ok",
            Some(300),
        ),
        (
            10,
            "my-api",
            "My API",
            Some("default"),
            None,
            Some("my.test"),
            "ok",
            Some(50),
        ),
        (5, "", "", None, None, None, "failed", None),
    ];
    {
        let conn = rusqlite::Connection::open(run.paths.usage_db()).unwrap();
        for (age, member, name, line_id, line_name, host, status, cost) in fixture {
            conn.execute(
                "INSERT INTO requests (started_ms, member_id, member_name, line_id, line_name, \
                 upstream_host, transport, client, status_kind, cost_micros) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'http', 'desktop', ?7, ?8)",
                rusqlite::params![
                    now - age * 1000,
                    member,
                    name,
                    line_id,
                    line_name,
                    host,
                    status,
                    cost
                ],
            )
            .unwrap();
        }
    }
    let reader = Reader::open(&run.paths).unwrap();
    let q = |group: Option<String>| {
        reader
            .query(&RequestQuery {
                group_by: RequestGrouping::Provider,
                group,
                ..RequestQuery::default()
            })
            .unwrap()
    };
    let page = q(None);
    let keys: Vec<&str> = page.groups.iter().map(|g| g.key.as_str()).collect();
    assert_eq!(
        keys,
        ["my-api", "krill"],
        "newest activity first, no member left out"
    );
    let krill = &page.groups[1];
    assert_eq!(krill.label, "Krill", "the newest row's name");
    assert_eq!(krill.count, 4);
    assert_eq!(krill.upstream_host.as_deref(), Some("api.test"));
    let lines: Vec<(&str, u64)> = krill
        .lines
        .iter()
        .map(|l| (l.label.as_str(), l.count))
        .collect();
    assert_eq!(lines, [("China Express", 3), ("Overseas", 1)]);
    assert_eq!(
        krill.lines[0].key,
        provider_line_key("krill", "China Express")
    );
    assert_eq!(krill.lines[0].upstream_host.as_deref(), Some("slb.test"));
    // A line without a name is listed by its id.
    assert_eq!(page.groups[0].lines[0].label, "default");
    // Selecting a provider or one of its lines.
    assert_eq!(q(Some("krill".into())).totals.requests, 4);
    let cn = q(Some(provider_line_key("krill", "China Express")));
    assert_eq!(cn.rows.len(), 3);
    assert_eq!(cn.totals.errors, 1);
    assert_eq!(cn.groups.len(), 2, "the source list ignores the selection");
    assert_eq!(
        q(Some(provider_line_key("krill", "Overseas"))).rows.len(),
        1
    );
    assert_eq!(
        q(Some(provider_line_key("my-api", "default"))).rows.len(),
        1
    );
    assert!(
        q(Some("slb.test".into())).rows.is_empty(),
        "hosts are not providers"
    );

    // Statistics by provider, largest cost first, with each provider's lines.
    let st = reader
        .stats(&StatsQuery {
            by: StatsGroupBy::Provider,
            ..StatsQuery::default()
        })
        .unwrap();
    let buckets: Vec<(&str, &str, u64)> = st
        .buckets
        .iter()
        .map(|b| (b.key.as_str(), b.label.as_str(), b.totals.requests))
        .collect();
    assert_eq!(
        buckets,
        [("krill", "Krill", 4), ("my-api", "My API", 1), ("", "", 1)]
    );
    let lines: Vec<(&str, u64, i64)> = st.buckets[0]
        .lines
        .iter()
        .map(|l| (l.label.as_str(), l.totals.requests, l.totals.cost.micros()))
        .collect();
    assert_eq!(lines, [("Overseas", 1, 300), ("China Express", 3, 200)]);
    assert_eq!(
        st.buckets[0].lines[1].key,
        provider_line_key("krill", "China Express")
    );
    assert_eq!(st.totals.requests, 6);

    // An empty line selects the provider's rows without a known line (the
    // source list's "line unknown" entry, review ux-visual #17).
    assert!(q(Some(provider_line_key("krill", ""))).rows.is_empty());
    rusqlite::Connection::open(run.paths.usage_db())
        .unwrap()
        .execute(
            "INSERT INTO requests (started_ms, member_id, member_name, transport, client, \
             status_kind) VALUES (?1, 'krill', 'Krill', 'http', 'desktop', 'failed')",
            rusqlite::params![now - 1000],
        )
        .unwrap();
    let unknown = q(Some(provider_line_key("krill", "")));
    assert_eq!(unknown.rows.len(), 1);
    assert_eq!(unknown.rows[0].line_label(), None);
    assert_eq!(q(Some("krill".into())).totals.requests, 5);
}
