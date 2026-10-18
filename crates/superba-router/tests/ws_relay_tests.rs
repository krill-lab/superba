//! The frame-terminating WebSocket relay used while request stats are
//! configured: no per-message deflate, the stats line injected into event
//! frames, continuation frames untouched, earlier lines stripped from a
//! reconnect's full input, one row per `response.create`, both legs bounded
//! (a message or frame over the limits closes both with 1009).

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::responses::*;
use common::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use superba_core::{DataDir, RequestKind, RequestQuery, RequestStatus, RequestTransport};
use superba_router::paths::RouterPaths;
use superba_router::{MemberKind, Reader, RouterServer, StatsConfig, StatsLineConfig};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// The upstream answers every `response.create` with the fixture's events
/// (prewarm: created + completed only).
fn script() -> WsScript {
    Arc::new(|frame: &str| {
        let v: Value = serde_json::from_str(frame).unwrap_or_default();
        if v["type"] != "response.create" {
            return Vec::new();
        }
        if v["generate"] == json!(false) {
            return vec![
                json!({"type": "response.created", "response": {"id": "resp_w", "status": "in_progress"}})
                    .to_string(),
                json!({"type": "response.completed", "response": {"id": "resp_w", "status": "completed", "output": [], "usage": null}})
                    .to_string(),
            ];
        }
        text_answer()
            .into_iter()
            .map(|(_, e)| e.to_string())
            .collect()
    })
}

const ANSWER: &str = "Earlier answer.\n\n> ⚡\u{2060} 87 tok/s · 首字 0.82s · 本轮 1 in / 1 out";

async fn router(up: &Upstream, paths: &RouterPaths) -> Router {
    router_with(up, paths, |_| {}).await
}

async fn router_with(
    up: &Upstream,
    paths: &RouterPaths,
    tweak: impl FnOnce(&mut superba_router::RouterConfig),
) -> Router {
    let mut cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        true,
        vec![line("a", up.base("/backend-api/codex"))],
    )]);
    cfg.stats = Some(StatsConfig {
        line: Some(StatsLineConfig {
            template: "⚡ {tps} tok/s · 首字 {ttft} · 本轮 {input} in / {output} out".into(),
            style: Default::default(),
            originators: vec!["Codex Desktop".into()],
        }),
        ..StatsConfig::default()
    });
    tweak(&mut cfg);
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

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(r: &Router) -> (Ws, http::Response<Option<Vec<u8>>>) {
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
        "019a7c00-0000-7000-8000-0000000000e1".parse().unwrap(),
    );
    h.insert("originator", "Codex Desktop".parse().unwrap());
    h.insert(
        "openai-beta",
        "responses_websockets=2026-02-06".parse().unwrap(),
    );
    h.insert(
        "sec-websocket-extensions",
        "permessage-deflate; client_max_window_bits"
            .parse()
            .unwrap(),
    );
    tokio_tungstenite::connect_async(req).await.unwrap()
}

fn create(input: Value, extra: Value, turn: &str) -> String {
    let mut v = json!({
        "type": "response.create",
        "model": "gpt-x",
        "input": input,
        "prompt_cache_key": "019a7c00-0000-7000-8000-0000000000e1",
        "client_metadata": {"x-codex-turn-metadata": turn_metadata(turn, "turn", "user")},
    });
    for (k, val) in extra.as_object().unwrap() {
        v[k] = val.clone();
    }
    v.to_string()
}

/// Frames until (and including) the terminal event.
async fn response(ws: &mut Ws) -> Vec<Value> {
    let mut out = Vec::new();
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("a frame in time")
            .unwrap()
            .unwrap();
        let Message::Text(t) = msg else { continue };
        let v: Value = serde_json::from_str(t.as_str()).unwrap();
        let done = matches!(
            v["type"].as_str(),
            Some("response.completed" | "response.failed" | "response.incomplete" | "error")
        );
        out.push(v);
        if done {
            return out;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frames_are_relayed_with_the_line_and_without_deflate() {
    let up = upstream(Mode::WsScript(script())).await;
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    let r = router(&up, &paths).await;

    let (mut ws, resp) = connect(&r).await;
    assert_eq!(resp.status(), 101);
    assert!(
        !resp.headers().contains_key("sec-websocket-extensions"),
        "no per-message deflate toward Codex"
    );
    assert!(
        !up.seen()[0]
            .headers
            .contains_key("sec-websocket-extensions"),
        "none offered to the member"
    );

    // Prewarm: answered, never a line.
    ws.send(Message::text(
        json!({"type": "response.create", "model": "gpt-x", "generate": false, "input": [],
               "client_metadata": {"x-codex-turn-metadata": turn_metadata("t0", "prewarm", "user")}})
        .to_string(),
    ))
    .await
    .unwrap();
    let pre = response(&mut ws).await;
    assert_eq!(pre.len(), 2);

    // Turn 1 with full input: the earlier line is stripped, the answer gets one.
    let full = request_body("gpt-x", ANSWER, json!({}))["input"].clone();
    ws.send(Message::text(create(full.clone(), json!({}), "t1")))
        .await
        .unwrap();
    let events = response(&mut ws).await;
    let text = consistent_text(&events, "msg_1");
    assert!(line_of(&text).is_some(), "{text}");
    assert!(text.starts_with("Hello world.\n\n> ⚡\u{2060} "));
    let frames = up.frames();
    let sent: Value = serde_json::from_str(&frames[1]).unwrap();
    assert_eq!(sent["input"][1]["content"][0]["text"], "Earlier answer.");
    assert_eq!(
        sent["input"][0]["content"][0]["text"],
        "hi ⚡\u{2060} user text stays"
    );

    // Turn 2 continues the stored response: only the new item, byte for byte.
    let continuation = create(
        json!([{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "more"}]}]),
        json!({"previous_response_id": "resp_1"}),
        "t2",
    );
    ws.send(Message::text(continuation.clone())).await.unwrap();
    let events = response(&mut ws).await;
    assert!(line_of(&consistent_text(&events, "msg_1")).is_some());
    assert_eq!(up.frames()[2], continuation, "continuation untouched");
    ws.close(None).await.unwrap();

    // A reconnect re-sends the full input with the lines: stripped again.
    let (mut ws2, _) = connect(&r).await;
    let mut history = full.as_array().unwrap().clone();
    history.push(json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": consistent_text(&events, "msg_1")}]}));
    ws2.send(Message::text(create(json!(history), json!({}), "t3")))
        .await
        .unwrap();
    response(&mut ws2).await;
    let sent: Value = serde_json::from_str(up.frames().last().unwrap()).unwrap();
    assert_eq!(sent["input"][1]["content"][0]["text"], "Earlier answer.");
    assert_eq!(sent["input"][4]["content"][0]["text"], "Hello world.");
    ws2.close(None).await.unwrap();

    // One row per response.create.
    let reader = Reader::open(&paths).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let page = loop {
        let page = reader.query(&RequestQuery::default()).unwrap();
        if page.rows.len() >= 4 || Instant::now() > deadline {
            break page;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(page.rows.len(), 4);
    assert!(
        page.rows
            .iter()
            .all(|r| r.transport == RequestTransport::WebSocket)
    );
    let prewarm = page
        .rows
        .iter()
        .find(|r| r.request_kind == Some(RequestKind::Prewarm))
        .unwrap();
    assert!(!prewarm.injected);
    let turns: Vec<_> = page
        .rows
        .iter()
        .filter(|r| r.request_kind == Some(RequestKind::Turn))
        .collect();
    assert_eq!(turns.len(), 3);
    assert!(
        turns
            .iter()
            .all(|r| r.injected && r.input_tokens == Some(4002))
    );
    assert_eq!(turns.iter().map(|r| r.stripped).sum::<u32>(), 3);
    assert!(
        turns
            .iter()
            .all(|r| r.first_token_ms.is_some() && r.ttfb_ms.is_some())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_socket_closed_mid_response_records_a_cancelled_request() {
    // The upstream never answers this frame.
    let up = upstream(Mode::WsScript(Arc::new(|_: &str| Vec::new()))).await;
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    let r = router(&up, &paths).await;
    let (mut ws, _) = connect(&r).await;
    ws.send(Message::text(create(json!([]), json!({}), "t1")))
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while up.frames().is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    ws.close(None).await.unwrap();
    let reader = Reader::open(&paths).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let page = loop {
        let page = reader.query(&RequestQuery::default()).unwrap();
        if !page.rows.is_empty() || Instant::now() > deadline {
            break page;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].status, superba_core::RequestStatus::Cancelled);
}

/// A frame header as a server sends it (unmasked) announcing `len` bytes.
fn frame_header(opcode: u8, fin: bool, len: u64) -> Vec<u8> {
    let mut v = vec![if fin { 0x80 } else { 0 } | opcode];
    match len {
        0..126 => v.push(len as u8),
        126..=0xFFFF => {
            v.push(126);
            v.extend((len as u16).to_be_bytes());
        }
        _ => {
            v.push(127);
            v.extend(len.to_be_bytes());
        }
    }
    v
}

/// The close code Codex gets next (frames before it are skipped). The router
/// then ends the socket gracefully: the stream ends after the close frame,
/// never with a reset (an abortive close of a socket with unread input sends
/// RST, and Windows drops the close frame queued before it).
async fn close_code(ws: &mut Ws) -> Option<u16> {
    let code = loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("closed in time");
        match msg {
            Some(Ok(Message::Close(f))) => break f.map(|f| f.code.into()),
            Some(Ok(_)) => {}
            other => panic!("expected a close frame, got {other:?}"),
        }
    };
    // Our close reply goes out, then the router's end of the stream.
    let end = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("ended in time");
    assert!(end.is_none(), "expected the end of the stream, got {end:?}");
    code
}

/// The request log once it holds `n` rows.
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

/// The member's close codes once it got one.
async fn member_closes(up: &Upstream) -> Vec<Option<u16>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while up.closes().is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    up.closes()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_member_frame_over_the_limit_closes_both_legs_before_its_payload_arrives() {
    // A header announcing 1 TiB: without a frame limit tungstenite reserves the
    // announced length up front, and this test process dies on the allocation.
    let mut raw = frame_header(0x1, true, 1 << 40);
    raw.extend_from_slice(br#"{"type":"response.output_text.delta","delta":""#);
    let up = upstream(Mode::WsRaw(Arc::new(raw))).await;
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    let r = router(&up, &paths).await;
    let (mut ws, _) = connect(&r).await;
    ws.send(Message::text(create(json!([]), json!({}), "t1")))
        .await
        .unwrap();
    assert_eq!(close_code(&mut ws).await, Some(1009), "message too big");
    assert_eq!(member_closes(&up).await, vec![Some(1009)]);
    let rows = rows(&paths, 1).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, RequestStatus::Broken);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fragmented_member_message_over_the_limit_closes_both_legs() {
    // Two 40 000-byte fragments: each frame fits the 64 KiB limit, the
    // message does not.
    let mut raw = frame_header(0x1, false, 40_000);
    raw.extend(std::iter::repeat_n(b'a', 40_000));
    raw.extend(frame_header(0x0, true, 40_000));
    raw.extend(std::iter::repeat_n(b'a', 40_000));
    let up = upstream(Mode::WsRaw(Arc::new(raw))).await;
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    let r = router_with(&up, &paths, |cfg| cfg.tuning.max_body_bytes = 64 * 1024).await;
    let (mut ws, _) = connect(&r).await;
    ws.send(Message::text(create(json!([]), json!({}), "t1")))
        .await
        .unwrap();
    assert_eq!(close_code(&mut ws).await, Some(1009));
    assert_eq!(member_closes(&up).await, vec![Some(1009)]);
    let rows = rows(&paths, 1).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, RequestStatus::Broken);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_message_over_the_limit_closes_both_legs_and_never_reaches_the_member() {
    let up = upstream(Mode::WsScript(script())).await;
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    let r = router_with(&up, &paths, |cfg| cfg.tuning.max_body_bytes = 64 * 1024).await;
    let (mut ws, _) = connect(&r).await;
    // Under the limit: relayed as usual.
    ws.send(Message::text(create(json!([]), json!({}), "t1")))
        .await
        .unwrap();
    response(&mut ws).await;
    let big = "x".repeat(100 * 1024);
    let input = json!([{"type": "message", "role": "user", "content": [{"type": "input_text", "text": big}]}]);
    ws.send(Message::text(create(input, json!({}), "t2")))
        .await
        .unwrap();
    assert_eq!(close_code(&mut ws).await, Some(1009));
    assert_eq!(member_closes(&up).await, vec![Some(1009)]);
    assert_eq!(
        up.frames().len(),
        1,
        "the big frame never reached the member"
    );
    let rows = rows(&paths, 1).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, RequestStatus::Ok);
}
