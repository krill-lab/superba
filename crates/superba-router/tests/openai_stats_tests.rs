//! The request log and the stats line on the OpenAI ingress: rows carry the
//! tool (by its token, the `User-Agent` as fallback), its version, protocol,
//! session / turn / agent, the upstream response and request ids and Chat
//! usage in the router's semantics; the Chat stats line goes into the final
//! content before `finish_reason: "stop"` (never on tool calls or background
//! requests, off by default for these tools), a whole answer gets it at the
//! end, later requests have it stripped; Responses use their own line.

mod common;

use std::sync::Arc;

use common::claude::{Fake, anthropic};
use common::openai::*;
use common::responses::{consistent_text, line_of, parse_sse, request_body, sse, text_answer};
use common::*;
use http::HeaderMap;
use serde_json::{Value, json};
use superba_core::codex::STATS_LINE_MARKER;
use superba_core::{AgentKind, RequestHarness as H, RequestKind, RequestProtocol, RequestStatus};
use superba_router::stats::default_line_harnesses;

async fn post(
    r: &Router,
    path: &str,
    token: &str,
    headers: HeaderMap,
    body: &Value,
) -> reqwest::Response {
    r.http
        .post(r.url(path))
        .headers(headers)
        .bearer_auth(token)
        .body(serde_json::to_vec(body).unwrap())
        .send()
        .await
        .unwrap()
}

fn chat_ok() -> Fake {
    with_request_id(
        Fake::sse(chat_stream(
            &["Hel", "lo."],
            "stop",
            Some(chat_usage(1000, 800, 20)),
        )),
        "req_chat_1",
    )
}

/// The tools of a main-conversation Responses request (requests without
/// tools are titles and other background work).
fn tools() -> Value {
    json!([{"type": "function", "name": "read", "parameters": {"type": "object"}}])
}

/// Requests the fake received on `path`, bodies parsed.
fn bodies(up: &common::claude::Anthropic, path: &str) -> Vec<Value> {
    up.seen()
        .iter()
        .filter(|s| s.path.ends_with(path))
        .map(|s| serde_json::from_slice(&s.body).unwrap())
        .collect()
}

#[tokio::test]
async fn successful_requests_explain_every_member_excluded_by_protocol() {
    exclusions_respect_conversion(false).await;
}

#[tokio::test]
async fn convertible_members_are_not_reported_as_excluded() {
    exclusions_respect_conversion(true).await;
}

async fn exclusions_respect_conversion(enabled: bool) {
    use superba_router::translate::{ir::Protocol, routing::Conversion};

    let up = anthropic(Arc::new(|s| {
        if s.path.ends_with("/chat/completions") {
            chat_ok()
        } else {
            Fake::sse(vec![sse(&text_answer(), false)])
        }
    }))
    .await;
    let mut cfg = config(
        vec![
            compatible("responses", &up),
            compatible("chat", &up),
            compatible("neither", &up),
            krill("both", &up),
        ],
        vec![
            ("chat", chat_entry(false)),
            ("neither", superba_router::openai::OpenAiMember::default()),
        ],
    );
    if !enabled {
        cfg.translation.disabled.extend([
            Conversion {
                from: Protocol::Chat,
                to: Protocol::Responses,
            },
            Conversion {
                from: Protocol::Responses,
                to: Protocol::Chat,
            },
        ]);
    }
    let run = start_recording(cfg, stats(vec![])).await;
    for (path, body, expected, excluded) in [
        (
            "/openai/v1/responses",
            request_body("m", "history", json!({})),
            "This provider has no OpenAI Responses endpoint or enabled protocol conversion",
            ["chat", "neither"],
        ),
        (
            "/openai/v1/chat/completions",
            chat_body("m", "history", true),
            "This provider has no Chat Completions endpoint or enabled protocol conversion",
            ["responses", "neither"],
        ),
    ] {
        let resp = post(
            &run.router,
            path,
            &token_of(H::Dsh),
            tool_headers(H::Dsh, path),
            &body,
        )
        .await;
        assert_eq!(resp.status(), 200);
        let _ = resp.text().await.unwrap();
        let rows = logged(&run.paths, if path.ends_with("/responses") { 1 } else { 2 }).await;
        let reader = superba_router::Reader::open(&run.paths).unwrap();
        let detail = reader.detail(rows[0].id).unwrap();
        let reason = detail
            .log
            .iter()
            .find(|l| l.starts_with("excluded providers: "))
            .unwrap();
        let excluded = if enabled {
            &excluded[1..]
        } else {
            &excluded[..]
        };
        for id in excluded {
            assert!(reason.contains(&format!("({id}): {expected}")), "{reason}");
        }
        assert!(!reason.contains("(both)"));
        if enabled {
            assert!(!reason.contains("(chat)"), "{reason}");
            assert!(!reason.contains("(responses)"), "{reason}");
        }
        assert!(
            detail
                .attempts
                .iter()
                .all(|a| !excluded.contains(&a.member_id.as_str()))
        );
    }
}

#[tokio::test]
async fn dsh_rows_carry_the_harness_session_ids_and_usage() {
    let up = anthropic(Arc::new(|_| {
        with_request_id(Fake::sse(vec![sse(&text_answer(), false)]), "req_resp_1")
    }))
    .await;
    let run = start_recording(config(vec![krill("krill", &up)], vec![]), stats(vec![])).await;
    let resp = post(
        &run.router,
        "/openai/v1/responses",
        &token_of(H::Dsh),
        tool_headers(H::Dsh, "session-7f2"),
        &request_body(
            "gpt-5.6-luna",
            "earlier",
            json!({"store": false, "tools": tools()}),
        ),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await;
    let rows = logged(&run.paths, 1).await;
    let row = &rows[0];
    assert_eq!(row.harness, H::Dsh);
    assert_eq!(row.harness_version.as_deref(), Some("0.2.1-rc.2"));
    assert_eq!(row.protocol, RequestProtocol::Responses);
    assert_eq!(row.session_id.as_deref(), Some("session-7f2"));
    assert_eq!(row.thread_id.as_deref(), Some("session-7f2"));
    assert_eq!(row.agent_kind, Some(AgentKind::Main));
    assert_eq!(row.request_kind, Some(RequestKind::Turn));
    assert_eq!(row.requested_model.as_deref(), Some("gpt-5.6-luna"));
    assert_eq!(row.model.as_deref(), Some("gpt-5.6-luna"));
    assert_eq!(row.response_id.as_deref(), Some("resp_1"));
    assert_eq!(row.upstream_request_id.as_deref(), Some("req_resp_1"));
    assert_eq!(
        (row.input_tokens, row.cached_tokens, row.output_tokens),
        (Some(4002), Some(3328), Some(51))
    );
    assert_eq!(row.status, RequestStatus::Ok);
    assert_eq!(row.member_id.as_str(), "krill");
}

#[tokio::test]
async fn the_token_decides_the_tool_and_the_user_agent_is_only_the_fallback() {
    let up = anthropic(Arc::new(|_| Fake::sse(vec![sse(&text_answer(), false)]))).await;
    let run = start_recording(config(vec![krill("k", &up)], vec![]), stats(vec![])).await;
    let body = request_body("m", "x", json!({"store": false}));
    // pi's token with a Codex-like `session_id` header and another tool's
    // User-Agent: still pi.
    let mut h = tool_headers(H::Pi, "pi-sess");
    h.insert("user-agent", "opencode/latest/2.0.32/cli".parse().unwrap());
    let _ = post(
        &run.router,
        "/openai/v1/responses",
        &token_of(H::Pi),
        h,
        &body,
    )
    .await
    .text()
    .await;
    // The plain router token: the User-Agent says.
    let _ = post(
        &run.router,
        "/openai/v1/responses",
        ROUTER_TOKEN,
        tool_headers(H::Pi, "p2"),
        &body,
    )
    .await
    .text()
    .await;
    let mut anon = HeaderMap::new();
    anon.insert("user-agent", "curl/8.5.0".parse().unwrap());
    let _ = post(
        &run.router,
        "/openai/v1/responses",
        ROUTER_TOKEN,
        anon,
        &body,
    )
    .await
    .text()
    .await;
    let mut rows = logged(&run.paths, 3).await;
    rows.reverse();
    let got: Vec<(H, Option<String>)> = rows
        .iter()
        .map(|r| (r.harness, r.session_id.clone()))
        .collect();
    assert_eq!(
        got,
        [
            (H::Pi, Some("pi-sess".into())),
            (H::Pi, Some("p2".into())),
            (
                H::Unknown,
                Some("019a7c00-0000-7000-8000-0000000000e1".into())
            ),
        ],
        "the last one has only its prompt cache key"
    );
    assert_eq!(rows[0].harness_version, None, "pi sends no version");
}

#[tokio::test]
async fn zcode_and_workbuddy_rows_carry_their_session_turn_and_agent() {
    let up = anthropic(Arc::new(|_| chat_ok())).await;
    let run = start_recording(config(vec![krill("k", &up)], vec![]), stats(vec![])).await;
    let mut zh = tool_headers(H::ZCode, "zc-sess");
    zh.insert("x-zcode-session-type", "subagent".parse().unwrap());
    let _ = post(
        &run.router,
        "/openai/v1/chat/completions",
        &token_of(H::ZCode),
        zh,
        &chat_body("glm-5.3", "x", true),
    )
    .await
    .text()
    .await;
    let mut compact = tool_headers(H::ZCode, "zc-sess");
    compact.insert("x-zcode-query-source", "compact".parse().unwrap());
    let _ = post(
        &run.router,
        "/openai/v1/chat/completions",
        &token_of(H::ZCode),
        compact,
        &chat_body("glm-5.3", "x", true),
    )
    .await
    .text()
    .await;
    let mut wb = tool_headers(H::WorkBuddy, "wb-conv");
    wb.insert("x-api-key", token_of(H::WorkBuddy).parse().unwrap());
    let _ = post(
        &run.router,
        "/openai/v1/chat/completions",
        &token_of(H::WorkBuddy),
        wb,
        &chat_body("deepseek-v4", "x", true),
    )
    .await
    .text()
    .await;
    let mut rows = logged(&run.paths, 3).await;
    rows.reverse();
    let z = &rows[0];
    assert_eq!(
        (z.harness, z.harness_version.as_deref()),
        (H::ZCode, Some("3.15.3"))
    );
    assert_eq!(z.protocol, RequestProtocol::ChatCompletions);
    assert_eq!(z.session_id.as_deref(), Some("zc-sess"));
    assert_eq!(z.turn_id.as_deref(), Some("trace-1"));
    assert_eq!(z.agent_kind, Some(AgentKind::Subagent));
    assert_eq!(rows[1].agent_kind, Some(AgentKind::Background));
    assert_eq!(rows[1].request_kind, Some(RequestKind::Compaction));
    let w = &rows[2];
    assert_eq!(
        (w.harness, w.harness_version.as_deref()),
        (H::WorkBuddy, Some("2.161.0"))
    );
    assert_eq!(w.session_id.as_deref(), Some("wb-conv"));
    assert_eq!(w.turn_id.as_deref(), Some("turn-1"));
    assert_eq!(w.agent_kind, Some(AgentKind::Main));
    for r in &rows {
        assert_eq!(r.response_id.as_deref(), Some("chatcmpl-up-1"));
        assert_eq!(r.upstream_request_id.as_deref(), Some("req_chat_1"));
    }
}

#[tokio::test]
async fn opencode_subagents_and_title_requests_are_told_apart() {
    let up = anthropic(Arc::new(|_| chat_ok())).await;
    let run = start_recording(config(vec![krill("k", &up)], vec![]), stats(vec![])).await;
    let token = token_of(H::OpenCode);
    let mut sub = tool_headers(H::OpenCode, "ses_child");
    sub.insert(
        "x-opencode-parent-session-id",
        "ses_parent".parse().unwrap(),
    );
    let _ = post(
        &run.router,
        "/openai/v1/chat/completions",
        &token,
        sub,
        &chat_body("m", "x", true),
    )
    .await
    .text()
    .await;
    let mut title = chat_body("m", "x", true);
    title.as_object_mut().unwrap().remove("tools");
    let _ = post(
        &run.router,
        "/openai/v1/chat/completions",
        &token,
        tool_headers(H::OpenCode, "ses_parent"),
        &title,
    )
    .await
    .text()
    .await;
    let mut rows = logged(&run.paths, 2).await;
    rows.reverse();
    assert_eq!(rows[0].agent_kind, Some(AgentKind::Subagent));
    assert_eq!(rows[0].session_id.as_deref(), Some("ses_child"));
    assert_eq!(rows[1].agent_kind, Some(AgentKind::Background));
    assert_eq!(rows[1].harness_version.as_deref(), Some("2.0.32"));
}

#[tokio::test]
async fn chat_usage_is_normalised_streamed_and_whole() {
    let up = anthropic(Arc::new(|s| {
        let v: Value = serde_json::from_slice(&s.body).unwrap();
        // DeepSeek reports its cache hits next to prompt_tokens.
        let deepseek = json!({"prompt_tokens": 1000, "completion_tokens": 30, "prompt_cache_hit_tokens": 900, "prompt_cache_miss_tokens": 100});
        if v["stream"] == true {
            Fake::sse(chat_stream(&["ok"], "stop", Some(deepseek)))
        } else {
            Fake::json(200, chat_json("whole", "stop", chat_usage(500, 400, 7)))
        }
    }))
    .await;
    let run = start_recording(config(vec![krill("k", &up)], vec![]), stats(vec![])).await;
    let token = token_of(H::Dsh);
    let _ = post(
        &run.router,
        "/openai/v1/chat/completions",
        &token,
        tool_headers(H::Dsh, "s"),
        &chat_body("deepseek-chat", "x", true),
    )
    .await
    .text()
    .await;
    let resp = post(
        &run.router,
        "/openai/v1/chat/completions",
        &token,
        tool_headers(H::Dsh, "s"),
        &chat_body("deepseek-chat", "x", false),
    )
    .await;
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["choices"][0]["message"]["content"], "whole");
    let mut rows = logged(&run.paths, 2).await;
    rows.reverse();
    assert_eq!(
        (
            rows[0].input_tokens,
            rows[0].cached_tokens,
            rows[0].output_tokens
        ),
        (Some(1000), Some(900), Some(30))
    );
    assert_eq!(
        (
            rows[1].input_tokens,
            rows[1].cached_tokens,
            rows[1].output_tokens
        ),
        (Some(500), Some(400), Some(7))
    );
    assert_eq!(rows[1].response_id.as_deref(), Some("chatcmpl-up-2"));
    assert!(
        rows.iter()
            .all(|r| r.status == RequestStatus::Ok && !r.injected)
    );
}

#[tokio::test]
async fn the_chat_stats_line_ends_the_final_answer_and_is_stripped_later() {
    let up = anthropic(Arc::new(|_| chat_ok())).await;
    let run = start_recording(config(vec![krill("k", &up)], vec![]), stats(vec![H::Dsh])).await;
    let token = token_of(H::Dsh);
    let resp = post(
        &run.router,
        "/openai/v1/chat/completions",
        &token,
        tool_headers(H::Dsh, "s"),
        &chat_body("deepseek-chat", "earlier", true),
    )
    .await;
    let (text, finish, usage) = chat_accumulate(&resp.text().await.unwrap());
    assert_eq!(finish.as_deref(), Some("stop"));
    assert!(usage.is_some(), "the usage chunk still arrives");
    let line = line_of(&text).expect("the answer ends with the line");
    assert!(text.starts_with("Hello."));
    assert!(
        line.contains("1,000 in / 20 out · 800 cached · 1 req"),
        "{line}"
    );
    // The tool sends the answer back in its history.
    let resp = post(
        &run.router,
        "/openai/v1/chat/completions",
        &token,
        tool_headers(H::Dsh, "s"),
        &chat_body("deepseek-chat", &text, true),
    )
    .await;
    let _ = resp.text().await;
    let sent = bodies(&up, "/chat/completions");
    assert_eq!(sent[1]["messages"][2]["content"], "Hello.");
    let mut rows = logged(&run.paths, 2).await;
    rows.reverse();
    assert!(rows[0].injected);
    assert_eq!(rows[1].stripped, 1);
}

#[tokio::test]
async fn the_line_is_off_by_default_for_the_tools_but_old_lines_are_stripped() {
    let up = anthropic(Arc::new(|_| chat_ok())).await;
    let run = start_recording(
        config(vec![krill("k", &up)], vec![]),
        stats(default_line_harnesses()),
    )
    .await;
    let earlier = format!("Hi.\n\n> {STATS_LINE_MARKER} 1 in / 2 out");
    for h in [H::Dsh, H::OpenCode, H::Pi, H::ZCode, H::WorkBuddy] {
        let resp = post(
            &run.router,
            "/openai/v1/chat/completions",
            &token_of(h),
            tool_headers(h, "s"),
            &chat_body("m", &earlier, true),
        )
        .await;
        let (text, _, _) = chat_accumulate(&resp.text().await.unwrap());
        assert_eq!(text, "Hello.", "{h:?}");
    }
    assert!(
        bodies(&up, "/chat/completions")
            .iter()
            .all(|b| b["messages"][2]["content"] == "Hi.")
    );
    let rows = logged(&run.paths, 5).await;
    assert!(rows.iter().all(|r| !r.injected && r.stripped == 1));
}

#[tokio::test]
async fn no_line_on_tool_calls_background_requests_or_structured_output() {
    let up = anthropic(Arc::new(|s| {
        let v: Value = serde_json::from_slice(&s.body).unwrap();
        if v["messages"][3]["content"] == "call a tool" {
            Fake::sse(chat_tool_stream())
        } else {
            chat_ok()
        }
    }))
    .await;
    let run = start_recording(config(vec![krill("k", &up)], vec![]), stats(vec![H::Pi])).await;
    let token = token_of(H::Pi);
    let mut tool = chat_body("m", "x", true);
    tool["messages"][3]["content"] = json!("call a tool");
    let mut title = chat_body("m", "x", true);
    title.as_object_mut().unwrap().remove("tools");
    let mut structured = chat_body("m", "x", true);
    structured["response_format"] = json!({"type": "json_object"});
    for body in [tool, title, structured] {
        let resp = post(
            &run.router,
            "/openai/v1/chat/completions",
            &token,
            tool_headers(H::Pi, "s"),
            &body,
        )
        .await;
        let (text, _, _) = chat_accumulate(&resp.text().await.unwrap());
        assert!(!text.contains(STATS_LINE_MARKER), "{text}");
    }
    let rows = logged(&run.paths, 3).await;
    assert!(rows.iter().all(|r| !r.injected));
}

#[tokio::test]
async fn a_whole_chat_answer_gets_the_line_at_the_end() {
    let up = anthropic(Arc::new(|_| {
        Fake::json(200, chat_json("Done.", "stop", chat_usage(50, 0, 4)))
    }))
    .await;
    let run = start_recording(
        config(vec![krill("k", &up)], vec![]),
        stats(vec![H::WorkBuddy]),
    )
    .await;
    let resp = post(
        &run.router,
        "/openai/v1/chat/completions",
        &token_of(H::WorkBuddy),
        tool_headers(H::WorkBuddy, "c"),
        &chat_body("m", "x", false),
    )
    .await;
    let v: Value = resp.json().await.unwrap();
    let text = v["choices"][0]["message"]["content"].as_str().unwrap();
    assert!(text.starts_with("Done.\n\n> "));
    assert!(line_of(text).unwrap().contains("50 in / 4 out"));
    assert!(logged(&run.paths, 1).await[0].injected);
}

#[tokio::test]
async fn responses_get_their_own_line_for_tools_that_have_it_on() {
    let up = anthropic(Arc::new(|_| Fake::sse(vec![sse(&text_answer(), false)]))).await;
    let run = start_recording(
        config(vec![krill("k", &up)], vec![]),
        stats(vec![H::OpenCode]),
    )
    .await;
    let earlier = format!("Before.\n\n> {STATS_LINE_MARKER} 1 in / 2 out");
    let resp = post(
        &run.router,
        "/openai/v1/responses",
        &token_of(H::OpenCode),
        tool_headers(H::OpenCode, "ses_1"),
        &request_body(
            "gpt-5.6-luna",
            &earlier,
            json!({"store": false, "tools": tools()}),
        ),
    )
    .await;
    let events = parse_sse(&resp.bytes().await.unwrap());
    let text = consistent_text(&events, "msg_1");
    assert!(line_of(&text).is_some(), "{text}");
    let sent = bodies(&up, "/responses");
    assert_eq!(sent[0]["input"][1]["content"][0]["text"], "Before.");
    let row = &logged(&run.paths, 1).await[0];
    assert!(row.injected);
    assert_eq!(row.stripped, 1);
    // dsh is off: no line.
    let resp = post(
        &run.router,
        "/openai/v1/responses",
        &token_of(H::Dsh),
        tool_headers(H::Dsh, "s"),
        &request_body(
            "gpt-5.6-luna",
            "x",
            json!({"store": false, "tools": tools()}),
        ),
    )
    .await;
    let events = parse_sse(&resp.bytes().await.unwrap());
    assert!(line_of(&consistent_text(&events, "msg_1")).is_none());
}

#[tokio::test]
async fn a_refusal_is_recorded_with_its_reason() {
    let up = anthropic(Arc::new(|_| chat_ok())).await;
    let mut cfg = config(vec![compatible("site", &up)], vec![]);
    use superba_router::translate::{ir::Protocol, routing::Conversion};
    cfg.translation.disabled.insert(Conversion {
        from: Protocol::Chat,
        to: Protocol::Responses,
    });
    let run = start_recording(cfg, stats(vec![])).await;
    let resp = post(
        &run.router,
        "/openai/v1/chat/completions",
        &token_of(H::WorkBuddy),
        tool_headers(H::WorkBuddy, "c"),
        &chat_body("m", "x", true),
    )
    .await;
    assert_eq!(resp.status(), 404);
    let row = &logged(&run.paths, 1).await[0];
    assert_eq!(
        row.status,
        RequestStatus::Http {
            status: 404,
            code: Some("no_provider".into())
        }
    );
    assert_eq!(row.harness, H::WorkBuddy);
    assert!(
        row.error
            .as_deref()
            .unwrap()
            .contains("no Chat Completions endpoint")
    );
}

#[tokio::test]
async fn a_whole_responses_answer_passes_through_and_is_recorded() {
    let answer = json!({
        "id": "resp_whole_1", "object": "response", "status": "completed", "model": "gpt-5.6-luna",
        "output": [{"id": "msg_1", "type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Hi."}]}],
        "usage": {"input_tokens": 300, "input_tokens_details": {"cached_tokens": 200}, "output_tokens": 3},
    });
    let a = answer.clone();
    let up = anthropic(Arc::new(move |_| {
        with_request_id(Fake::json(200, a.clone()), "req_whole")
    }))
    .await;
    let run = start_recording(config(vec![krill("k", &up)], vec![]), stats(vec![H::Pi])).await;
    let resp = post(
        &run.router,
        "/openai/v1/responses",
        &token_of(H::Pi),
        tool_headers(H::Pi, "p"),
        &request_body(
            "gpt-5.6-luna",
            "x",
            json!({"stream": false, "store": false, "tools": tools()}),
        ),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v, answer, "unchanged");
    let row = &logged(&run.paths, 1).await[0];
    assert_eq!(
        (row.input_tokens, row.cached_tokens, row.output_tokens),
        (Some(300), Some(200), Some(3))
    );
    assert_eq!(row.response_id.as_deref(), Some("resp_whole_1"));
    assert_eq!(row.upstream_request_id.as_deref(), Some("req_whole"));
    assert!(!row.injected);
}
