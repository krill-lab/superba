//! Fake OpenAI upstreams (Chat Completions and Responses, served by the
//! generic fake of [`super::claude`]), the requests the tools send and router
//! configurations for the OpenAI ingress.
#![allow(dead_code)]

use std::time::{Duration, Instant};

use http::HeaderMap;
use serde_json::{Value, json};
use superba_core::{DataDir, RequestHarness, RequestQuery, RequestRow, StatsLineStyle};
use superba_router::openai::{OpenAiConfig, OpenAiEndpoint, OpenAiMember};
use superba_router::paths::RouterPaths;
use superba_router::token::harness_token;
use superba_router::{
    MemberConfig, MemberKind, Reader, RouterConfig, RouterServer, StatsConfig, StatsLineConfig,
};

use super::claude::{Anthropic, Fake};
use super::{ROUTER_TOKEN, Router, line, member, secrets_for, test_config};

pub const TEMPLATE: &str = "⚡ {input} in / {output} out · {cached} cached · {requests} req";

/// A stats configuration with the line on for `harnesses`.
pub fn stats(harnesses: Vec<RequestHarness>) -> StatsConfig {
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

pub struct Running {
    pub router: Router,
    pub paths: RouterPaths,
    _dir: tempfile::TempDir,
}

/// A router started from files in a temp data dir (so it records).
pub async fn start_recording(mut cfg: RouterConfig, stats: StatsConfig) -> Running {
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
pub async fn logged(paths: &RouterPaths, n: usize) -> Vec<RequestRow> {
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

/// The harness token of `h` on this computer.
pub fn token_of(h: RequestHarness) -> String {
    harness_token(ROUTER_TOKEN.as_bytes(), h, None).unwrap()
}

/// A Krill member served by `up` (its line at `/backend-api/codex`; the
/// ingress's defaults: Responses and Chat at `/codex/v1`).
pub fn krill(id: &str, up: &Anthropic) -> MemberConfig {
    member(
        id,
        MemberKind::Krill,
        false,
        vec![line("main", up.base("/backend-api/codex"))],
    )
}

/// A compatible member served by `up` (its line at `/v1`; default:
/// Responses only, at `/v1`).
pub fn compatible(id: &str, up: &Anthropic) -> MemberConfig {
    member(
        id,
        MemberKind::Compatible,
        false,
        vec![line("main", up.base("/v1"))],
    )
}

/// An entry serving Chat Completions at `/v1` (and Responses at the line base
/// when `responses`).
pub fn chat_entry(responses: bool) -> OpenAiMember {
    OpenAiMember {
        responses: responses.then(OpenAiEndpoint::line_base),
        chat: Some(OpenAiEndpoint::at("/v1")),
        ..OpenAiMember::default()
    }
}

/// A router configuration of these members (priority order) with these
/// entries.
pub fn config(members: Vec<MemberConfig>, entries: Vec<(&str, OpenAiMember)>) -> RouterConfig {
    let mut cfg = test_config(members);
    if !entries.is_empty() {
        let mut o = OpenAiConfig::default();
        for (id, e) in entries {
            o.members.insert(id.to_owned(), e);
        }
        cfg.openai = Some(o);
    }
    cfg
}

/// Headers a tool sends (its `User-Agent` and session headers).
pub fn tool_headers(h: RequestHarness, session: &str) -> HeaderMap {
    let mut m = HeaderMap::new();
    m.insert("content-type", "application/json".parse().unwrap());
    let mut set = |k: &'static str, v: &str| {
        m.insert(k, v.parse().unwrap());
    };
    match h {
        RequestHarness::Dsh => {
            set(
                "user-agent",
                "deepseek-harness/0.2.1-rc.2 (+https://github.com/deepseek-ai/deepseek-harness)",
            );
            set("session_id", session);
            set("x-client-request-id", session);
        }
        RequestHarness::OpenCode => {
            set("user-agent", "opencode/latest/2.0.32/cli");
            set("x-opencode-session-id", session);
            set("x-session-affinity", session);
            set("x-opencode-client", "cli");
        }
        RequestHarness::Pi => {
            set("user-agent", "pi (linux 6.1.0-34-amd64; x64)");
            set("session_id", session);
            set("x-client-request-id", session);
        }
        RequestHarness::ZCode => {
            set(
                "user-agent",
                "ZCode/3.15.3 ai-sdk/provider-utils/4.0.27 runtime/node.js/24",
            );
            set("x-title", "Z Code@electron");
            set("x-session-id", session);
            set("x-zcode-trace-id", "trace-1");
            set("x-zcode-session-type", "main");
            set("x-zcode-query-source", "main_turn");
            set("x-request-id", "zc-req-1");
        }
        RequestHarness::WorkBuddy => {
            set("user-agent", "CLI/2.161.0 WorkBuddy/2.161.0");
            set("x-conversation-id", session);
            set("x-conversation-request-id", "turn-1");
            set("x-root-request-id", "turn-1");
            set("x-request-id", "wb-msg-1");
            set("x-agent-type", "main");
        }
        _ => {}
    }
    m
}

/// One SSE `data:` event.
pub fn data(v: &Value) -> String {
    format!("data: {v}\n\n")
}

/// A chunk with one choice.
pub fn chunk(delta: Value, finish: Option<&str>) -> Value {
    json!({
        "id": "chatcmpl-up-1",
        "object": "chat.completion.chunk",
        "created": 1_760_000_000,
        "model": "deepseek-chat",
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
    })
}

/// Chat usage in OpenAI semantics (`prompt_tokens` includes cached).
pub fn chat_usage(prompt: u64, cached: u64, completion: u64) -> Value {
    json!({
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "total_tokens": prompt + completion,
        "prompt_tokens_details": {"cached_tokens": cached},
    })
}

/// A streamed answer: the text `parts`, `finish` on an empty last delta,
/// then a usage chunk (`include_usage`) and `[DONE]`.
pub fn chat_stream(parts: &[&str], finish: &str, usage: Option<Value>) -> Vec<String> {
    let mut out = vec![data(&chunk(
        json!({"role": "assistant", "content": ""}),
        None,
    ))];
    for p in parts {
        out.push(data(&chunk(json!({"content": p}), None)));
    }
    out.push(data(&chunk(json!({}), Some(finish))));
    if let Some(u) = usage {
        let mut c = chunk(json!({}), None);
        c["choices"] = json!([]);
        c["usage"] = u;
        out.push(data(&c));
    }
    out.push("data: [DONE]\n\n".to_owned());
    out
}

/// A streamed tool call answer (`finish_reason: tool_calls`).
pub fn chat_tool_stream() -> Vec<String> {
    vec![
        data(&chunk(
            json!({"role": "assistant", "content": "Reading it."}),
            None,
        )),
        data(&chunk(
            json!({"tool_calls": [{"index": 0, "id": "call_1", "type": "function", "function": {"name": "read", "arguments": ""}}]}),
            None,
        )),
        data(&chunk(
            json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"path\":\"x\"}"}}]}),
            None,
        )),
        data(&chunk(json!({}), Some("tool_calls"))),
        "data: [DONE]\n\n".to_owned(),
    ]
}

/// A whole Chat answer.
pub fn chat_json(content: &str, finish: &str, usage: Value) -> Value {
    json!({
        "id": "chatcmpl-up-2",
        "object": "chat.completion",
        "created": 1_760_000_000,
        "model": "deepseek-chat",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": content}, "finish_reason": finish}],
        "usage": usage,
    })
}

/// What a client assembles from a Chat stream: the text, the finish reason,
/// the usage; panics when content follows the finish or `[DONE]` is not last.
pub fn chat_accumulate(sse: &str) -> (String, Option<String>, Option<Value>) {
    let mut text = String::new();
    let mut finish: Option<String> = None;
    let mut usage = None;
    let mut done = false;
    for raw in sse.split("\n\n").filter(|b| !b.trim().is_empty()) {
        let payload: String = raw
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(|d| d.strip_prefix(' ').unwrap_or(d))
            .collect();
        assert!(!done, "event after [DONE]: {raw}");
        if payload == "[DONE]" {
            done = true;
            continue;
        }
        let v: Value = serde_json::from_str(&payload).unwrap_or_else(|e| panic!("{payload}: {e}"));
        if !v["usage"].is_null() {
            usage = Some(v["usage"].clone());
        }
        for c in v["choices"].as_array().into_iter().flatten() {
            if let Some(t) = c["delta"]["content"].as_str() {
                assert!(
                    finish.is_none() || t.is_empty(),
                    "content after finish_reason: {raw}"
                );
                text.push_str(t);
            }
            if let Some(f) = c["finish_reason"].as_str() {
                finish = Some(f.to_owned());
            }
        }
    }
    assert!(done, "no [DONE]");
    (text, finish, usage)
}

/// A Chat request: a system prompt, a user message, an earlier assistant
/// answer (`assistant_text`), a new user message and one tool.
pub fn chat_body(model: &str, assistant_text: &str, stream: bool) -> Value {
    json!({
        "model": model,
        "stream": stream,
        "stream_options": {"include_usage": true},
        "messages": [
            {"role": "system", "content": "be brief"},
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": assistant_text},
            {"role": "user", "content": "next"},
        ],
        "tools": [{"type": "function", "function": {"name": "read", "parameters": {"type": "object"}}}],
    })
}

/// A JSON answer (`Fake::json`) or a stream (`Fake::sse`) with an upstream
/// request id.
pub fn with_request_id(mut f: Fake, id: &str) -> Fake {
    f.headers.push(("x-request-id", id.to_owned()));
    f
}
