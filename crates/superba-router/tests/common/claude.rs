//! A fake Anthropic Messages upstream and router configurations for the
//! Claude Code ingress.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, Request, Response, StatusCode};
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use superba_router::claude::{ClaudeConfig, ClaudeMember, Endpoint};
use superba_router::{MemberConfig, MemberKind, RouterConfig};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use url::Url;

use super::{ChanBody, Seen, line, member, test_config};

/// An answer of the fake.
#[derive(Clone, Debug)]
pub struct Fake {
    pub status: u16,
    pub content_type: &'static str,
    pub chunks: Vec<String>,
    pub headers: Vec<(&'static str, String)>,
}

impl Fake {
    /// A JSON answer.
    pub fn json(status: u16, body: Value) -> Self {
        Fake {
            status,
            content_type: "application/json",
            chunks: vec![body.to_string()],
            headers: Vec::new(),
        }
    }

    /// An SSE answer in these chunks.
    pub fn sse(chunks: Vec<String>) -> Self {
        Fake {
            status: 200,
            content_type: "text/event-stream",
            chunks,
            headers: Vec::new(),
        }
    }
}

/// How the fake answers a request.
pub type Handler = Arc<dyn Fn(&Seen) -> Fake + Send + Sync>;

#[derive(Clone)]
pub struct Anthropic {
    pub addr: SocketAddr,
    pub seen: Arc<Mutex<Vec<Seen>>>,
    pub handler: Arc<Mutex<Handler>>,
}

impl Anthropic {
    pub fn base(&self, path: &str) -> Url {
        Url::parse(&format!("http://{}{path}", self.addr)).unwrap()
    }
    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
    /// Requests to `/v1/messages` (list and count requests left out).
    pub fn messages(&self) -> Vec<Seen> {
        self.seen()
            .into_iter()
            .filter(|s| s.path.contains("/v1/messages") && !s.path.contains("count_tokens"))
            .collect()
    }
    pub fn set(&self, handler: Handler) {
        *self.handler.lock().unwrap() = handler;
    }
}

/// Start a fake answering with `handler`.
pub async fn anthropic(handler: Handler) -> Anthropic {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let up = Anthropic {
        addr: listener.local_addr().unwrap(),
        seen: Arc::default(),
        handler: Arc::new(Mutex::new(handler)),
    };
    let state = up.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let state = state.clone();
            tokio::spawn(async move {
                let svc = service_fn(move |req| {
                    let state = state.clone();
                    async move { Ok::<_, Infallible>(handle(state, req).await) }
                });
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), svc)
                    .await;
            });
        }
    });
    up
}

async fn handle(
    state: Anthropic,
    req: Request<Incoming>,
) -> Response<http_body_util::combinators::BoxBody<Bytes, std::io::Error>> {
    let (parts, body) = req.into_parts();
    let body = body
        .collect()
        .await
        .map(|c| c.to_bytes())
        .unwrap_or_default();
    let seen = Seen {
        method: parts.method.to_string(),
        path: parts
            .uri
            .path_and_query()
            .map(|p| p.to_string())
            .unwrap_or_default(),
        headers: parts.headers.clone(),
        body,
    };
    state.seen.lock().unwrap().push(seen.clone());
    let handler = state.handler.lock().unwrap().clone();
    let fake = handler(&seen);
    let (tx, rx) = mpsc::channel(8);
    let chunks = fake.chunks.clone();
    tokio::spawn(async move {
        for c in chunks {
            if tx.send(Ok(Bytes::from(c))).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(3)).await;
        }
    });
    let mut r = Response::new(ChanBody(rx).boxed());
    *r.status_mut() = StatusCode::from_u16(fake.status).unwrap();
    r.headers_mut()
        .insert("content-type", fake.content_type.parse().unwrap());
    for (k, v) in fake.headers {
        r.headers_mut()
            .insert(http::HeaderName::from_static(k), v.parse().unwrap());
    }
    r
}

/// One SSE event.
pub fn ev(name: &str, data: Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

/// A streamed answer with text `parts` (one delta each), `stop` and usage.
pub fn text_stream(model: &str, parts: &[&str], stop: &str, usage: Value) -> Vec<String> {
    let mut out = vec![
        ev(
            "message_start",
            json!({"type": "message_start", "message": {"id": "msg_up_1", "type": "message", "role": "assistant", "model": model, "content": [], "stop_reason": null, "stop_sequence": null, "usage": {"input_tokens": usage["input_tokens"], "cache_read_input_tokens": usage["cache_read_input_tokens"], "cache_creation_input_tokens": usage["cache_creation_input_tokens"], "output_tokens": 1}}}),
        ),
        ev("ping", json!({"type": "ping"})),
        ev(
            "content_block_start",
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        ),
    ];
    for p in parts {
        out.push(ev(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": p}}),
        ));
    }
    out.push(ev(
        "content_block_stop",
        json!({"type": "content_block_stop", "index": 0}),
    ));
    out.push(ev(
        "message_delta",
        json!({"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": null}, "usage": {"output_tokens": usage["output_tokens"]}}),
    ));
    out.push(ev("message_stop", json!({"type": "message_stop"})));
    out
}

/// A streamed answer: thinking, a text, then a tool call (`tool_use`).
pub fn tool_stream(model: &str) -> Vec<String> {
    vec![
        ev(
            "message_start",
            json!({"type": "message_start", "message": {"id": "msg_up_2", "type": "message", "role": "assistant", "model": model, "content": [], "stop_reason": null, "stop_sequence": null, "usage": {"input_tokens": 50, "output_tokens": 1}}}),
        ),
        ev(
            "content_block_start",
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
        ),
        ev(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "need the file"}}),
        ),
        ev(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig-abc"}}),
        ),
        ev(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": 0}),
        ),
        ev(
            "content_block_start",
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
        ),
        ev(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "Reading it."}}),
        ),
        ev(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": 1}),
        ),
        ev(
            "content_block_start",
            json!({"type": "content_block_start", "index": 2, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {}}}),
        ),
        ev(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "{\"file_path\":"}}),
        ),
        ev(
            "content_block_delta",
            json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "\"/tmp/x\"}"}}),
        ),
        ev(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": 2}),
        ),
        ev(
            "message_delta",
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use", "stop_sequence": null}, "usage": {"output_tokens": 30}}),
        ),
        ev("message_stop", json!({"type": "message_stop"})),
    ]
}

/// A Claude Code member served by `up` (path `""`: `<origin>/v1/messages`).
pub fn claude_member(id: &str, up: &Anthropic) -> (MemberConfig, ClaudeMember) {
    let m = member(
        id,
        MemberKind::Compatible,
        false,
        vec![line("main", up.base("/v1"))],
    );
    let c = ClaudeMember {
        anthropic: Some(Endpoint::default()),
        ..ClaudeMember::default()
    };
    (m, c)
}

/// A router configuration of these members (priority order).
pub fn claude_config(members: Vec<(MemberConfig, ClaudeMember)>) -> RouterConfig {
    let mut claude = ClaudeConfig::default();
    let mut list = Vec::new();
    for (m, c) in members {
        claude.members.insert(m.id.clone(), c);
        list.push(m);
    }
    let mut cfg = test_config(list);
    cfg.claude = Some(claude);
    cfg
}

/// Headers of a request Claude Code sends.
pub fn claude_headers(session: &str, prompt: Option<&str>) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert("content-type", "application/json".parse().unwrap());
    h.insert("anthropic-version", "2023-06-01".parse().unwrap());
    h.insert(
        "anthropic-beta",
        "claude-code-20250219,interleaved-thinking-2025-05-14"
            .parse()
            .unwrap(),
    );
    h.insert(
        "user-agent",
        "claude-cli/2.1.302 (external, cli)".parse().unwrap(),
    );
    h.insert("x-app", "cli".parse().unwrap());
    h.insert("x-claude-code-session-id", session.parse().unwrap());
    if let Some(p) = prompt {
        h.insert("x-claude-code-prompt-id", p.parse().unwrap());
        h.insert("x-claude-code-request-class", "main".parse().unwrap());
    }
    h
}

/// A Messages request body with one user prompt and a tool.
pub fn body(model: &str, stream: bool) -> Value {
    json!({
        "model": model,
        "max_tokens": 32000,
        "stream": stream,
        "system": [{"type": "text", "text": "You are Claude Code."}],
        "messages": [{"role": "user", "content": [{"type": "text", "text": "hello"}]}],
        "tools": [{"name": "Read", "description": "read a file", "input_schema": {"type": "object", "properties": {"file_path": {"type": "string"}}}}],
        "metadata": {"user_id": "{\"device_id\":\"d\",\"session_id\":\"s-meta\"}"},
    })
}
