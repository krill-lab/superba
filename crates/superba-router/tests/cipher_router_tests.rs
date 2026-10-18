//! GitHub issue 11 through the router: a member that cannot read ciphertexts
//! another provider made refuses the request (as the Krill relay does); the
//! router leaves out exactly what it cannot read, sends the request again once
//! and leaves those items out of later requests before they leave. HTTP and
//! WebSocket, with the request log and the routing event.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::ciphers::*;
use common::*;
use futures_util::{SinkExt, StreamExt};
use http::HeaderMap;
use serde_json::{Value, json};
use superba_core::{DataDir, RequestQuery, RequestStatus};
use superba_router::cipher::rewrite::OMITTED;
use superba_router::paths::RouterPaths;
use superba_router::{MemberKind, Reader, RouteEventKind, RouterServer, StatsConfig};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// What the fake OpenAI backend cannot read in `body`: the refusal it
/// answers with (the relay's wording), checked in input order.
fn refusal(body: &Value) -> Option<String> {
    for item in body["input"].as_array()? {
        let id = item["id"].as_str().unwrap_or_default();
        let foreign = |s: &str| s.contains("foreign") || !s.starts_with("gAAAA");
        if item["type"] == "reasoning" && item["encrypted_content"].as_str().is_some_and(foreign) {
            return Some(refusal_for_item(id));
        }
        for (key, by_item) in [("content", true), ("output", false)] {
            for part in item[key].as_array().into_iter().flatten() {
                if part["type"] == "encrypted_content"
                    && part["encrypted_content"].as_str().is_some_and(foreign)
                {
                    return Some(if by_item {
                        refusal_for_item(id)
                    } else {
                        REFUSAL_FUNCTION_OUTPUT.to_owned()
                    });
                }
            }
        }
    }
    None
}

const OK: &str = r#"{"id":"resp_ok","object":"response","status":"completed","output":[],"usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}"#;

/// The backend: `status` with the refusal, else 200; `always` refuses every
/// request that carries any ciphertext at all.
fn backend(status: u16, always: bool) -> Mode {
    Mode::Reply(Arc::new(move |_: &HeaderMap, body: &[u8]| {
        let v: Value = serde_json::from_slice(body).unwrap_or_default();
        let refused = refusal(&v).or_else(|| {
            (always && body.windows(9).any(|w| w == b"encrypted"))
                .then(|| refusal_for_item("rs_any"))
        });
        match refused {
            Some(r) => (status, leak(r)),
            None => (200, OK),
        }
    }))
}

/// The parent conversation after a third-party provider, plus the member's
/// own reasoning from a turn it answered.
fn conversation() -> Value {
    body(vec![
        developer("<permissions instructions>…"),
        user("Spawn a worker agent to count the files, then report."),
        reasoning("rs_r01", &cipher("foreign-r01")),
        spawn_call(TASK, None),
        notes_call("call_n1"),
        notes_output("fco_01a14f90-4268", "call_n1", &cipher("foreign-notes")),
        reasoning("rs_own", &cipher("own-r02")),
        user("Continue."),
    ])
}

async fn router(up: &Upstream, paths: &RouterPaths, lines: Vec<&str>) -> Router {
    let mut cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        true,
        lines
            .iter()
            .map(|id| line(id, up.base("/backend-api/codex")))
            .collect(),
    )]);
    cfg.stats = Some(StatsConfig::default());
    let secrets = secrets_for(&cfg);
    cfg.save(paths).unwrap();
    secrets.save(paths).unwrap();
    let server = RouterServer::bind_from_files(paths.clone()).await.unwrap();
    let r = Router {
        addr: server.local_addr(),
        admin_token: server.admin_token().expose().to_owned(),
        handle: server.shutdown_handle(),
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
    };
    tokio::spawn(server.serve());
    r
}

async fn post(r: &Router, body: &Value) -> reqwest::Response {
    r.http
        .post(r.url("/backend-api/codex/responses"))
        .bearer_auth(ROUTER_TOKEN)
        .header("content-type", "application/json")
        .header("session-id", "01a14f90-2834-7af2-90de-0cb4d5117756")
        .body(body.to_string())
        .send()
        .await
        .unwrap()
}

async fn rows(paths: &RouterPaths, n: usize) -> Vec<superba_core::RequestRow> {
    let reader = Reader::open(paths).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let page = reader.query(&RequestQuery::default()).unwrap();
        if page.rows.len() >= n || Instant::now() > deadline {
            return page.rows;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn input_of(seen: &Seen) -> Vec<Value> {
    let v: Value = serde_json::from_slice(&seen.body).unwrap();
    v["input"].as_array().unwrap().clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_a_refused_conversation_goes_again_without_what_the_member_cannot_read() {
    let up = upstream(backend(400, false)).await;
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    let r = router(&up, &paths, vec!["a"]).await;

    // The member answered a turn carrying its own reasoning: it reads it.
    let own = body(vec![
        user("hi"),
        reasoning("rs_own", &cipher("own-r02")),
        user("go"),
    ]);
    let resp = post(&r, &own).await;
    assert_eq!(resp.status(), 200);
    resp.text().await.unwrap();
    // Learned when the request ended well (its row is written then).
    rows(&paths, 1).await;
    assert_eq!(up.hits(), 1);

    // The conversation another provider started: refused once, sent again.
    let resp = post(&r, &conversation()).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), OK, "Codex gets the answer");
    let seen = up.seen();
    assert_eq!(seen.len(), 3, "one refusal, one retry");
    assert_eq!(seen[1].body, conversation().to_string().into_bytes());
    let sent = input_of(&seen[2]);
    // Exactly the foreign reasoning left out; its own reasoning stays.
    let ids: Vec<&str> = sent
        .iter()
        .map(|i| i["id"].as_str().unwrap_or(""))
        .collect();
    assert!(!ids.contains(&"rs_r01"), "{ids:?}");
    assert!(ids.contains(&"rs_own"), "{ids:?}");
    assert_eq!(sent.len(), 7);
    // The encrypted notes output (not known to the member) became a note.
    assert_eq!(
        sent[4]["output"],
        json!([{"type": "input_text", "text": OMITTED}])
    );
    // Every other item keeps its bytes.
    let text = String::from_utf8(seen[2].body.to_vec()).unwrap();
    for item in [0usize, 1, 3, 4, 6, 7] {
        let raw = conversation()["input"][item].to_string();
        assert!(text.contains(&raw), "item {item} kept");
    }

    // The next turn: left out before it leaves, the same bytes, no refusal.
    let mut next = conversation();
    next["input"].as_array_mut().unwrap().push(user("And now?"));
    assert_eq!(post(&r, &next).await.status(), 200);
    let seen = up.seen();
    assert_eq!(seen.len(), 4, "no refused round trip");
    let again = String::from_utf8(seen[3].body.to_vec()).unwrap();
    let last = conversation()["input"][7].to_string();
    let cut = text.find(&last).unwrap() + last.len();
    assert!(
        again.starts_with(&text[..cut]),
        "the conversation keeps its bytes (prompt cache)"
    );

    // Recorded: the routing event, the attempt and the note.
    let status = r.status().await;
    let ev = status
        .recent
        .iter()
        .find(|e| e.kind == RouteEventKind::Ciphertext)
        .expect("a ciphertext event");
    let detail = ev.detail.as_deref().unwrap();
    assert!(detail.contains("1 reasoning item"), "{detail}");
    assert!(detail.contains("encrypted payload"), "{detail}");
    let all = rows(&paths, 3).await;
    assert_eq!(all.len(), 3);
    assert!(all.iter().all(|row| row.status == RequestStatus::Ok));
    let reader = Reader::open(&paths).unwrap();
    let refused_row = all
        .iter()
        .map(|row| reader.detail(row.id).unwrap())
        .find(|d| d.attempts.iter().any(|a| a.status == Some(400)))
        .expect("the refused attempt is recorded");
    assert!(
        refused_row
            .attempts
            .iter()
            .any(|a| a.outcome.contains("invalid_encrypted_content: left out")),
        "{:?}",
        refused_row.attempts
    );
    assert_eq!(refused_row.row.failovers, 0, "not a failover");
    let proactive = all
        .iter()
        .map(|row| reader.detail(row.id).unwrap())
        .any(|d| d.log.iter().any(|l| l.contains("it refused before")));
    assert!(proactive, "the next turn notes what was left out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_a_sub_agent_task_in_plain_text_reaches_the_member_as_text() {
    let up = upstream(backend(400, false)).await;
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    let r = router(&up, &paths, vec!["a"]).await;
    let child = body(vec![
        developer("<collaboration_mode>…"),
        user("Spawn a worker agent to count the files, then report."),
        agent_task(TASK_ID, TASK),
    ]);
    assert_eq!(post(&r, &child).await.status(), 200);
    let seen = up.seen();
    assert_eq!(seen.len(), 2);
    let sent = input_of(&seen[1]);
    assert_eq!(
        sent[2]["content"],
        json!([{"type": "input_text", "text": TASK_HEADER}, {"type": "input_text", "text": TASK}])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_one_retry_then_the_refusal_reaches_codex_and_a_lost_task_is_explained() {
    // Refuses whatever it gets: the second refusal is passed on as it is.
    let up = upstream(backend(400, true)).await;
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    let r = router(&up, &paths, vec!["a"]).await;
    let resp = post(&r, &conversation()).await;
    assert_eq!(resp.status(), 400);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["error"]["code"], "invalid_encrypted_content");
    assert_eq!(up.hits(), 2, "exactly one retry");

    // A sub-agent whose task is a real ciphertext the member cannot read.
    let up = upstream(backend(400, false)).await;
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    let r = router(&up, &paths, vec!["a"]).await;
    let child = body(vec![
        user("Spawn a worker."),
        agent_task(TASK_ID, &cipher("foreign-task")),
    ]);
    let resp = post(&r, &child).await;
    assert_eq!(resp.status(), 400);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["error"]["code"], "encrypted_task_unreadable");
    let message = v["error"]["message"].as_str().unwrap();
    assert!(message.contains("sub-agent's task"), "{message}");
    assert_eq!(up.hits(), 1, "never sent without its task");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_a_refusal_with_502_is_no_failover_and_marks_no_line_down() {
    let up = upstream(backend(502, false)).await;
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    let r = router(&up, &paths, vec!["a", "b"]).await;
    let resp = post(&r, &conversation()).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        route(&resp),
        "krill/a",
        "the same line answers the request sent again"
    );
    assert_eq!(up.hits(), 2);
    let status = r.status().await;
    assert_eq!(status.totals.failovers, 0);
    assert!(
        status
            .recent
            .iter()
            .all(|e| e.kind != RouteEventKind::LineDown && e.kind != RouteEventKind::Failover),
        "{:?}",
        status.recent
    );
}

/// A WebSocket backend: an `error` event for a frame it cannot read, else a
/// short completed response.
fn ws_backend() -> WsScript {
    Arc::new(|frame: &str| {
        let v: Value = serde_json::from_str(frame).unwrap_or_default();
        if v["type"] != "response.create" {
            return Vec::new();
        }
        if let Some(r) = refusal(&v) {
            let inner: Value = serde_json::from_str(&r).unwrap();
            return vec![
                json!({"type": "error", "status": 400, "error": inner["error"]}).to_string(),
            ];
        }
        vec![
            json!({"type": "response.created", "response": {"id": "resp_w", "status": "in_progress"}})
                .to_string(),
            json!({"type": "response.completed", "response": {"id": "resp_w", "status": "completed", "output": [], "usage": null}})
                .to_string(),
        ]
    })
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(r: &Router) -> Ws {
    let mut req = format!("ws://{}/backend-api/codex/responses", r.addr)
        .into_client_request()
        .unwrap();
    let h = req.headers_mut();
    h.insert(
        "authorization",
        format!("Bearer {ROUTER_TOKEN}").parse().unwrap(),
    );
    h.insert(
        "session-id",
        "01a14f90-2834-7af2-90de-0cb4d5117756".parse().unwrap(),
    );
    h.insert(
        "openai-beta",
        "responses_websockets=2026-02-06".parse().unwrap(),
    );
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

/// The next message that is not a ping.
async fn next(ws: &mut Ws) -> Message {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("a frame in time")
            .expect("open")
            .expect("readable");
        if !matches!(msg, Message::Ping(_) | Message::Pong(_)) {
            return msg;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn websocket_a_refusal_reconnects_and_the_turn_goes_again_without_it() {
    let up = upstream(Mode::WsScript(ws_backend())).await;
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    let r = router(&up, &paths, vec!["a"]).await;
    let mut frame = conversation();
    frame["type"] = json!("response.create");
    frame.as_object_mut().unwrap().shift_remove("stream");
    let frame = frame.to_string();

    // The refusal is not passed on: the socket closes with 1012.
    let mut ws = connect(&r).await;
    ws.send(Message::text(frame.clone())).await.unwrap();
    match next(&mut ws).await {
        Message::Close(Some(f)) => assert_eq!(u16::from(f.code), 1012),
        other => panic!("expected a close, got {other:?}"),
    }
    // Codex reconnects and sends the same turn: it goes without what the
    // member cannot read, and is answered.
    let mut ws = connect(&r).await;
    ws.send(Message::text(frame.clone())).await.unwrap();
    loop {
        let Message::Text(t) = next(&mut ws).await else {
            continue;
        };
        let v: Value = serde_json::from_str(t.as_str()).unwrap();
        if v["type"] == "response.completed" {
            break;
        }
        assert_ne!(v["type"], "error", "{v}");
    }
    let frames = up.frames();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0], frame, "the first frame went as Codex sent it");
    let sent: Value = serde_json::from_str(&frames[1]).unwrap();
    let ids: Vec<&str> = sent["input"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap_or(""))
        .collect();
    assert!(!ids.contains(&"rs_r01"), "{ids:?}");
    let all = rows(&paths, 2).await;
    assert_eq!(all.len(), 2);
    assert!(all.iter().any(|row| matches!(
        &row.status,
        RequestStatus::Http { status: 400, code: Some(c) } if c == "invalid_encrypted_content"
    )));
    assert!(all.iter().any(|row| row.status == RequestStatus::Ok));
    let status = r.status().await;
    assert!(status.recent.iter().any(|e| {
        e.kind == RouteEventKind::Ciphertext
            && e.detail
                .as_deref()
                .is_some_and(|d| d.contains("reconnects"))
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_200_whose_stream_fails_proves_nothing_was_read() {
    // Measured end to end: a relay without capacity for the model answers 200
    // and fails the stream (`server_busy`) without reading the input. Taking
    // that for a read kept a foreign item in the retry, which was refused
    // again.
    let busy = Mode::Sse {
        first: "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_b\",\"status\":\"in_progress\"}}\n\n",
        rest: "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"id\":\"resp_b\",\"status\":\"failed\",\"error\":{\"code\":\"server_busy\",\"message\":\"Our servers are currently busy.\"}}}\n\n",
        gate: None,
        break_mid: false,
    };
    let up = upstream(busy).await;
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    let r = router(&up, &paths, vec!["a"]).await;
    let resp = post(&r, &conversation()).await;
    assert_eq!(resp.status(), 200);
    resp.text().await.unwrap();
    rows(&paths, 1).await;

    up.set(backend(400, false));
    let resp = post(&r, &conversation()).await;
    assert_eq!(resp.status(), 200, "one retry is enough");
    let seen = up.seen();
    assert_eq!(seen.len(), 3);
    let sent = input_of(&seen[2]);
    assert!(
        sent.iter()
            .all(|i| i["type"] != "reasoning" && i["output"] != json!([{"type": "encrypted_content", "encrypted_content": cipher("foreign-notes")}])),
        "{sent:?}"
    );
}
