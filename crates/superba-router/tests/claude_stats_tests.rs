//! The request log and the stats line on the Claude Code ingress: record
//! fields (harness, version, protocol, requested and served model, session,
//! turn, agent, host, normalised usage), the line on final answers only
//! (streamed and whole), its strip from later requests, turn totals by
//! prompt id, and the per-harness switch.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::anthropic::accumulate;
use common::claude::*;
use common::*;
use http::HeaderMap;
use serde_json::{Value, json};
use superba_core::codex::STATS_LINE_MARKER;
use superba_core::{
    AgentKind, DataDir, RequestHarness, RequestKind, RequestProtocol, RequestQuery, RequestRow,
    RequestStatus, StatsLineStyle,
};
use superba_router::claude::{ClaudeModel, ModelAlias};
use superba_router::paths::RouterPaths;
use superba_router::token::harness_token;
use superba_router::{Reader, RouterConfig, RouterServer, StatsConfig, StatsLineConfig};

const TEMPLATE: &str = "⚡ {input} in / {output} out · {cached} cached · {requests} req";

fn stats(harnesses: Vec<RequestHarness>) -> StatsConfig {
    StatsConfig {
        line: Some(StatsLineConfig {
            template: TEMPLATE.into(),
            style: StatsLineStyle::Quote,
            originators: vec!["Codex Desktop".into()],
            harnesses,
        }),
        ..StatsConfig::default()
    }
}

struct Running {
    router: Router,
    paths: RouterPaths,
    _dir: tempfile::TempDir,
}

/// A router started from files in a temp data dir (so it records).
async fn start_recording(mut cfg: RouterConfig, stats: StatsConfig) -> Running {
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    cfg.stats = Some(stats);
    let secrets = secrets_for(&cfg);
    cfg.save(&paths).unwrap();
    secrets.save(&paths).unwrap();
    let server = RouterServer::bind_from_files(paths.clone()).await.unwrap();
    let router = Router {
        addr: server.local_addr(),
        admin_token: server.admin_token().expose().to_owned(),
        handle: server.shutdown_handle(),
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
    };
    tokio::spawn(server.serve());
    Running {
        router,
        paths,
        _dir: dir,
    }
}

/// Rows of the request log once at least `n` are there (newest first).
async fn logged(paths: &RouterPaths, n: usize) -> Vec<RequestRow> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(reader) = Reader::open(paths)
            && let Ok(page) = reader.query(&RequestQuery::default())
            && (page.rows.len() >= n || Instant::now() > deadline)
        {
            return page.rows;
        }
        if Instant::now() > deadline {
            return Vec::new();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn post(r: &Router, headers: HeaderMap, body: &Value) -> reqwest::Response {
    r.http
        .post(r.url("/claude/v1/messages"))
        .headers(headers)
        .bearer_auth(ROUTER_TOKEN)
        .body(serde_json::to_vec(body).unwrap())
        .send()
        .await
        .unwrap()
}

fn usage() -> Value {
    json!({"input_tokens": 10, "cache_read_input_tokens": 100, "cache_creation_input_tokens": 5, "output_tokens": 20})
}

fn one_member(up: &Anthropic) -> RouterConfig {
    let (m, mut c) = claude_member("deepseek", up);
    c.models = vec![ClaudeModel {
        id: "deepseek-chat".into(),
        display_name: None,
    }];
    let mut cfg = claude_config(vec![(m, c)]);
    cfg.claude.as_mut().unwrap().aliases.push(ModelAlias {
        from: "claude-haiku-*".into(),
        to: "deepseek-chat".into(),
    });
    cfg
}

#[tokio::test]
async fn rows_carry_the_harness_its_session_and_normalised_usage() {
    let up = anthropic(Arc::new(|_| {
        let mut f = Fake::sse(text_stream("deepseek-chat", &["ok"], "end_turn", usage()));
        f.headers.push(("request-id", "req_011CUpstream".into()));
        f
    }))
    .await;
    let run = start_recording(one_member(&up), stats(vec![])).await;
    let resp = post(
        &run.router,
        claude_headers("sess-1", Some("prompt-1")),
        &body("claude-haiku-4-5-20251001", true),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await;
    let rows = logged(&run.paths, 1).await;
    let row = &rows[0];
    assert_eq!(row.harness, RequestHarness::ClaudeCode);
    assert_eq!(row.harness_version.as_deref(), Some("2.1.302"));
    assert_eq!(row.protocol, RequestProtocol::Messages);
    assert_eq!(
        row.requested_model.as_deref(),
        Some("claude-haiku-4-5-20251001")
    );
    assert_eq!(row.model.as_deref(), Some("deepseek-chat"));
    assert_eq!(row.upstream_model, None, "sent under the routed name");
    assert_eq!(row.response_model.as_deref(), Some("deepseek-chat"));
    assert_eq!(row.session_id.as_deref(), Some("sess-1"));
    assert_eq!(row.thread_id.as_deref(), Some("sess-1"));
    assert_eq!(row.turn_id.as_deref(), Some("prompt-1"));
    assert_eq!(row.agent_kind, Some(AgentKind::Main));
    assert_eq!(row.request_kind, Some(RequestKind::Turn));
    assert_eq!(row.codex_host, None);
    assert_eq!(row.status, RequestStatus::Ok);
    assert_eq!(row.http_status, Some(200));
    assert_eq!(row.member_id.as_str(), "deepseek");
    assert_eq!(row.line_id.as_deref(), Some("main"));
    // Input = input_tokens + cache reads + cache writes.
    assert_eq!(row.input_tokens, Some(115));
    assert_eq!(row.cached_tokens, Some(100));
    assert_eq!(row.cache_write_tokens, Some(5));
    assert_eq!(row.output_tokens, Some(20));
    assert!(row.first_token_ms.is_some() && row.duration_ms.is_some());
    assert_eq!(row.client_text().key, "request_harness.claude_code");
    // The ids tools keep in their transcripts (for de-duplication).
    assert_eq!(row.response_id.as_deref(), Some("msg_up_1"));
    assert_eq!(row.upstream_request_id.as_deref(), Some("req_011CUpstream"));

    // A harness token decides the harness (and names a WSL host); the
    // User-Agent is only the fallback.
    let token = harness_token(
        ROUTER_TOKEN.as_bytes(),
        RequestHarness::ClaudeCode,
        Some("wsl:Ubuntu"),
    )
    .unwrap();
    let mut h = claude_headers("sess-2", None);
    h.insert("user-agent", "some-client/1.0".parse().unwrap());
    let resp = run
        .router
        .http
        .post(run.router.url("/claude/v1/messages"))
        .headers(h)
        .header("x-api-key", token)
        .body(serde_json::to_vec(&body("deepseek-chat", true)).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await;
    let rows = logged(&run.paths, 2).await;
    let row = &rows[0];
    assert_eq!(row.harness, RequestHarness::ClaudeCode);
    assert_eq!(row.harness_version, None);
    assert_eq!(row.codex_host.as_deref(), Some("wsl:Ubuntu"));
    assert_eq!(row.requested_model.as_deref(), Some("deepseek-chat"));

    // A refused request is recorded too.
    let resp = post(
        &run.router,
        claude_headers("sess-3", None),
        &body("no-such-model", true),
    )
    .await;
    assert_eq!(resp.status(), 404);
    let rows = logged(&run.paths, 3).await;
    assert_eq!(
        rows[0].status,
        RequestStatus::Http {
            status: 404,
            code: Some("not_found_error".into())
        }
    );
}

#[tokio::test]
async fn codex_rows_say_codex_and_responses() {
    let up = upstream(Mode::Json(
        200,
        r#"{"id":"r","usage":{"input_tokens":3,"output_tokens":1}}"#,
    ))
    .await;
    let cfg = test_config(vec![member(
        "krill",
        superba_router::MemberKind::Krill,
        false,
        vec![line("a", up.base("/backend-api/codex"))],
    )]);
    let run = start_recording(cfg, StatsConfig::default()).await;
    let resp = run
        .router
        .http
        .post(run.router.url("/backend-api/codex/responses"))
        .bearer_auth(ROUTER_TOKEN)
        .header("content-type", "application/json")
        .header("session-id", "codex-sess")
        .header("user-agent", "codex_cli_rs/0.162.0 (Linux; x86_64)")
        .body(r#"{"model":"gpt-x","input":[]}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await;
    let rows = logged(&run.paths, 1).await;
    let row = &rows[0];
    assert_eq!(row.harness, RequestHarness::Codex);
    assert_eq!(row.harness_version.as_deref(), Some("0.162.0"));
    assert_eq!(row.protocol, RequestProtocol::Responses);
    assert_eq!(row.requested_model.as_deref(), Some("gpt-x"));
    assert_eq!(row.model.as_deref(), Some("gpt-x"));
    assert_eq!(row.session_id.as_deref(), Some("codex-sess"));
    assert_eq!(row.response_id.as_deref(), Some("r"));
}

#[tokio::test]
async fn the_stats_line_ends_final_answers_and_is_stripped_again() {
    let up = anthropic(Arc::new(|s: &Seen| {
        let v: Value = serde_json::from_slice(&s.body).unwrap();
        let last = v["messages"].as_array().unwrap().last().unwrap().clone();
        if last["content"][0]["text"] == "use a tool" {
            return Fake::sse(tool_stream("deepseek-chat"));
        }
        if v["stream"] == false {
            return Fake::json(
                200,
                json!({"id": "msg_j", "type": "message", "role": "assistant", "model": "deepseek-chat", "content": [{"type": "text", "text": "whole"}], "stop_reason": "end_turn", "stop_sequence": null, "usage": usage()}),
            );
        }
        Fake::sse(text_stream("deepseek-chat", &["Hel", "lo"], "end_turn", usage()))
    }))
    .await;
    let run = start_recording(
        one_member(&up),
        stats(vec![RequestHarness::Codex, RequestHarness::ClaudeCode]),
    )
    .await;
    let r = &run.router;

    // A tool call first (same prompt id): no line, but its tokens count.
    let mut tool_body = body("deepseek-chat", true);
    tool_body["messages"][0]["content"][0]["text"] = "use a tool".into();
    let resp = post(r, claude_headers("s", Some("p-1")), &tool_body).await;
    let text = resp.text().await.unwrap();
    assert_eq!(text, tool_stream("deepseek-chat").concat());

    // The final answer of the same prompt: the line on its last text block,
    // with the turn's totals (50 in / 30 out before, 115 / 20 now).
    let resp = post(
        r,
        claude_headers("s", Some("p-1")),
        &body("deepseek-chat", true),
    )
    .await;
    assert!(resp.headers().get("content-length").is_none());
    let msg = accumulate(&resp.text().await.unwrap());
    let expected_line = format!("\n\n> {STATS_LINE_MARKER} 165 in / 50 out · 100 cached · 2 req");
    assert_eq!(msg["content"][0]["text"], format!("Hello{expected_line}"));

    // Not streamed.
    let resp = post(
        r,
        claude_headers("s", Some("p-2")),
        &body("deepseek-chat", false),
    )
    .await;
    let v: Value = resp.json().await.unwrap();
    let t = v["content"][0]["text"].as_str().unwrap();
    assert!(
        t.starts_with("whole\n\n> ") && t.contains(STATS_LINE_MARKER),
        "{t}"
    );

    // The next request carries the answer back: the upstream never sees the line.
    let mut next = body("deepseek-chat", true);
    next["messages"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role": "assistant", "content": [{"type": "text", "text": format!("Hello{expected_line}")}]}));
    next["messages"]
        .as_array_mut()
        .unwrap()
        .push(json!({"role": "user", "content": "thanks"}));
    let resp = post(r, claude_headers("s", Some("p-3")), &next).await;
    let _ = resp.text().await;
    let sent: Value = serde_json::from_slice(&up.messages().last().unwrap().body).unwrap();
    assert_eq!(sent["messages"][1]["content"][0]["text"], "Hello");
    let rows = logged(&run.paths, 4).await;
    assert_eq!(rows[0].stripped, 1);
    assert!(rows[0].injected);
    let tool_row = rows.iter().find(|r| r.output_tokens == Some(30)).unwrap();
    assert!(!tool_row.injected);
    assert_eq!(tool_row.response_id.as_deref(), Some("msg_up_2"));
    let whole = rows
        .iter()
        .find(|r| r.response_id.as_deref() == Some("msg_j"))
        .expect("the whole answer's id");
    assert!(whole.injected);
}

#[tokio::test]
async fn the_line_follows_the_harness_switch_and_the_main_conversation() {
    let up = anthropic(Arc::new(|_| {
        Fake::sse(text_stream(
            "deepseek-chat",
            &["plain"],
            "end_turn",
            usage(),
        ))
    }))
    .await;
    let plain = text_stream("deepseek-chat", &["plain"], "end_turn", usage()).concat();
    // Switched off for Claude Code.
    let run = start_recording(one_member(&up), stats(vec![RequestHarness::Codex])).await;
    let resp = post(
        &run.router,
        claude_headers("s", Some("p")),
        &body("deepseek-chat", true),
    )
    .await;
    assert_eq!(resp.text().await.unwrap(), plain);
    // On, but a subagent, a background call and another tool get none.
    let run = start_recording(one_member(&up), stats(vec![RequestHarness::ClaudeCode])).await;
    let mut sub = claude_headers("s", Some("p"));
    sub.insert("x-claude-code-request-class", "subagent".parse().unwrap());
    let resp = post(&run.router, sub, &body("deepseek-chat", true)).await;
    assert_eq!(resp.text().await.unwrap(), plain);
    let mut bg = claude_headers("s", None);
    bg.insert("x-app", "cli-bg".parse().unwrap());
    let resp = post(&run.router, bg, &body("deepseek-chat", true)).await;
    assert_eq!(resp.text().await.unwrap(), plain);
    let mut other = claude_headers("s", None);
    other.insert("user-agent", "opencode/0.15".parse().unwrap());
    let resp = post(&run.router, other, &body("deepseek-chat", true)).await;
    assert_eq!(resp.text().await.unwrap(), plain);
    // Claude Code's main conversation (no hint headers: tools offered) gets it.
    let resp = post(
        &run.router,
        claude_headers("s", None),
        &body("deepseek-chat", true),
    )
    .await;
    let msg = accumulate(&resp.text().await.unwrap());
    assert!(
        msg["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains(STATS_LINE_MARKER)
    );
}

#[tokio::test]
async fn a_request_log_of_an_earlier_format_starts_again() {
    let up = anthropic(Arc::new(|_| {
        Fake::sse(text_stream("deepseek-chat", &["ok"], "end_turn", usage()))
    }))
    .await;
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    std::fs::create_dir_all(paths.dir()).unwrap();
    {
        let old = rusqlite::Connection::open(paths.usage_db()).unwrap();
        old.execute_batch(
            "CREATE TABLE requests (id INTEGER PRIMARY KEY, started_ms INTEGER NOT NULL); \
             INSERT INTO requests VALUES (1, 1); PRAGMA user_version = 2;",
        )
        .unwrap();
    }
    // Not read while it has the old format.
    let page = Reader::open(&paths)
        .unwrap()
        .query(&RequestQuery::default())
        .unwrap();
    assert!(page.rows.is_empty());
    let mut cfg = one_member(&up);
    cfg.stats = Some(StatsConfig::default());
    let secrets = secrets_for(&cfg);
    cfg.save(&paths).unwrap();
    secrets.save(&paths).unwrap();
    let server = RouterServer::bind_from_files(paths.clone()).await.unwrap();
    let router = Router {
        addr: server.local_addr(),
        admin_token: server.admin_token().expose().to_owned(),
        handle: server.shutdown_handle(),
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
    };
    tokio::spawn(server.serve());
    let resp = post(
        &router,
        claude_headers("s", None),
        &body("deepseek-chat", true),
    )
    .await;
    let _ = resp.text().await;
    let rows = logged(&paths, 1).await;
    assert_eq!(rows.len(), 1, "the new log holds only the new request");
    assert_eq!(rows[0].harness, RequestHarness::ClaudeCode);
}
