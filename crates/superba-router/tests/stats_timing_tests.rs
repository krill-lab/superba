//! Request timing end to end (user bug 12): the marks a running router records
//! when the first event arrives in the same write as the response headers, when
//! events are split across writes (also in the middle of an event), over the
//! WebSocket relay, and for 429 / 5xx answers without events. Every mark is
//! taken when its item is parsed, marks are never out of order (equal marks are
//! fine), and TPS — row, stats line, detail — is one value with a guard for
//! windows of ~0.

mod common;

use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use common::responses::*;
use common::*;
use futures_util::{SinkExt, StreamExt};
use http::{Request, Response, StatusCode};
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use superba_core::{
    DataDir, RequestDetail, RequestQuery, RequestRow, RequestStatus, RequestTiming,
    RequestTransport, StatsLineStyle, tps_milli,
};
use superba_router::paths::RouterPaths;
use superba_router::{
    MemberKind, Reader, RouterConfig, RouterServer, StatsConfig, StatsLineConfig,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;

const TEMPLATE: &str = "⚡ {tps} tok/s · 首字 {ttft} · 本轮 {input} in / {output} out";
const SESSION: &str = "019a7c00-0000-7000-8000-0000000000e1";

// ---------------------------------------------------------------------------
// A raw HTTP/1.1 upstream: writes exactly the given byte runs, with pauses.
// ---------------------------------------------------------------------------

/// One `write_all` on the socket after `pause`.
#[derive(Clone)]
struct Write {
    pause: Duration,
    bytes: Vec<u8>,
}

fn write(pause_ms: u64, parts: &[&[u8]]) -> Write {
    Write {
        pause: Duration::from_millis(pause_ms),
        bytes: parts.concat(),
    }
}

/// The response head of a chunked SSE answer.
fn head() -> Vec<u8> {
    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\
      x-request-id: req_timing\r\nconnection: close\r\n\r\n"
        .to_vec()
}

/// One HTTP chunk carrying `data`.
fn chunk(data: &[u8]) -> Vec<u8> {
    let mut out = format!("{:x}\r\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\r\n");
    out
}

const LAST_CHUNK: &[u8] = b"0\r\n\r\n";

/// Read one request (head and `content-length` body) from `s`; `None` when the
/// peer closed first.
async fn read_request(s: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    let head_end = loop {
        let n = s.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
    let len = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while buf.len() < head_end + len {
        let n = s.read(&mut tmp).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    Some(head)
}

/// An upstream answering every authorized request with `writes` (health probes,
/// without `Authorization`, get a 401).
async fn raw_upstream(writes: Vec<Write>) -> url::Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let writes = Arc::new(writes);
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            let writes = writes.clone();
            tokio::spawn(async move {
                s.set_nodelay(true).unwrap();
                let Some(head) = read_request(&mut s).await else {
                    return;
                };
                if !head.contains("\r\nauthorization:") {
                    let _ = s
                        .write_all(
                            b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\
                              connection: close\r\n\r\n",
                        )
                        .await;
                    return;
                }
                for w in writes.iter() {
                    tokio::time::sleep(w.pause).await;
                    if s.write_all(&w.bytes).await.is_err() {
                        return;
                    }
                    let _ = s.flush().await;
                }
                let _ = s.shutdown().await;
            });
        }
    });
    url::Url::parse(&format!("http://{addr}/backend-api/codex")).unwrap()
}

// ---------------------------------------------------------------------------
// The answer used by the HTTP and WebSocket cases: `response.created`, a final
// answer message in three deltas, `response.completed` with 287 output tokens.
// ---------------------------------------------------------------------------

const OUTPUT: u64 = 287;

fn events() -> Vec<Ev> {
    response(
        &[Item::Message {
            id: "msg_1",
            phase: Some("final_answer"),
            deltas: vec!["Hello", " wor", "ld."],
        }],
        Ending::Completed,
        usage(4002, 3328, OUTPUT),
    )
}

/// The events as SSE text, one string per event.
fn sse_events() -> Vec<String> {
    events()
        .iter()
        .map(|e| sse(std::slice::from_ref(e), false))
        .collect()
}

/// Index of the first event of `kind`.
fn index_of(kind: &str) -> usize {
    events().iter().position(|(t, _)| t == kind).unwrap()
}

// ---------------------------------------------------------------------------
// Router fixture.
// ---------------------------------------------------------------------------

fn stats() -> StatsConfig {
    StatsConfig {
        line: Some(StatsLineConfig {
            template: TEMPLATE.into(),
            style: StatsLineStyle::Quote,
            originators: vec!["Codex Desktop".into()],
        }),
        ..StatsConfig::default()
    }
}

struct Running {
    router: Router,
    paths: RouterPaths,
    _dir: tempfile::TempDir,
}

async fn start(base: url::Url, websocket: bool) -> Running {
    let dir = tempfile::tempdir().unwrap();
    let paths = RouterPaths::new(&DataDir::new(dir.path()));
    let router = start_in(&paths, base, websocket).await;
    Running {
        router,
        paths,
        _dir: dir,
    }
}

async fn start_in(paths: &RouterPaths, base: url::Url, websocket: bool) -> Router {
    let mut cfg: RouterConfig = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        websocket,
        vec![line("a", base)],
    )]);
    cfg.stats = Some(stats());
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

/// A desktop user turn (eligible for the stats line).
async fn post_turn(r: &Router, turn: &str) -> reqwest::Response {
    r.http
        .post(r.url("/backend-api/codex/responses"))
        .bearer_auth(ROUTER_TOKEN)
        .header("content-type", "application/json")
        .header("session-id", SESSION)
        .header("originator", "Codex Desktop")
        .header("x-codex-turn-metadata", turn_metadata(turn, "turn", "user"))
        .body(serde_json::to_vec(&request_body("gpt-x", "Earlier.", json!({}))).unwrap())
        .send()
        .await
        .unwrap()
}

/// The newest row once `n` rows are recorded, with its detail.
async fn newest(paths: &RouterPaths, n: usize) -> (RequestRow, RequestDetail) {
    let reader = Reader::open(paths).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let page = reader.query(&RequestQuery::default()).unwrap();
        if page.rows.len() >= n {
            let row = page.rows[0].clone();
            let d = reader.detail(row.id).unwrap();
            return (row, d);
        }
        assert!(Instant::now() < deadline, "{n} rows not recorded");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn marks(t: &RequestTiming) -> Vec<u64> {
    [
        t.sent_ms,
        t.headers_ms,
        t.first_event_ms,
        t.first_token_ms,
        t.last_delta_ms,
        t.end_ms,
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The invariants every recorded request holds: marks in order, the row
/// repeats the detail's marks, TPS is the shared function of the marks.
fn check_consistent(row: &RequestRow, d: &RequestDetail) {
    let t = &d.timing;
    let m = marks(t);
    assert!(m.windows(2).all(|w| w[0] <= w[1]), "out of order: {t:?}");
    assert_eq!(t, &t.normalized());
    assert_eq!(row.first_token_ms, t.first_token_ms);
    assert_eq!(row.duration_ms, t.end_ms);
    if row.transport == RequestTransport::Http {
        assert_eq!(row.ttfb_ms, t.headers_ms);
    } else {
        assert_eq!(t.headers_ms, None, "a WebSocket request has no headers");
    }
    let expected = match (row.output_tokens, t.first_event_ms, t.end_ms) {
        (Some(o), Some(f), Some(e)) => tps_milli(o, f, e),
        _ => None,
    };
    assert_eq!(row.tps_milli, expected, "{t:?}");
}

/// The `{tps}` and `{ttft}` of an injected stats line.
fn line_numbers(events: &[Value]) -> (String, String) {
    let text = consistent_text(events, "msg_1");
    let line = line_of(&text).expect("stats line").to_owned();
    // "> ⚡⁠ 80 tok/s · 首字 0.31s · 本轮 ..."
    let after = line.split("\u{2060} ").nth(1).unwrap();
    let tps = after.split(" tok/s").next().unwrap().to_owned();
    let ttft = after
        .split("首字 ")
        .nth(1)
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .to_owned();
    (tps, ttft)
}

fn tps_text(milli: Option<u64>) -> String {
    milli.map_or_else(|| "—".to_owned(), |m| ((m + 500) / 1000).to_string())
}

fn ttft_text(ms: Option<u64>) -> String {
    format!("{:.2}s", ms.unwrap() as f64 / 1000.0)
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn headers_and_the_first_event_in_one_write() {
    let ev = sse_events();
    let first_delta = index_of("response.output_text.delta");
    // The head, `response.created`, the item / part added events and the first
    // delta leave in one write after 250 ms; the rest later.
    let first: Vec<u8> = ev[..=first_delta].concat().into_bytes();
    let middle: Vec<u8> = ev[first_delta + 1..first_delta + 3].concat().into_bytes();
    let rest: Vec<u8> = ev[first_delta + 3..].concat().into_bytes();
    let base = raw_upstream(vec![
        write(250, &[&head(), &chunk(&first)]),
        write(300, &[&chunk(&middle)]),
        write(60, &[&chunk(&rest), LAST_CHUNK]),
    ])
    .await;
    let run = start(base, false).await;
    let resp = post_turn(&run.router, "turn-1").await;
    assert_eq!(resp.status(), 200);
    let events = parse_sse(&resp.bytes().await.unwrap());
    let (row, d) = newest(&run.paths, 1).await;
    check_consistent(&row, &d);
    let t = &d.timing;
    let headers = t.headers_ms.unwrap();
    let first_event = t.first_event_ms.unwrap();
    let first_token = t.first_token_ms.unwrap();
    assert!(headers >= 240, "{t:?}");
    // Same write: the same moment (a 0 ms phase, or a ms of processing).
    assert!(first_event - headers <= 25, "{t:?}");
    assert!(first_token - first_event <= 5, "{t:?}");
    assert!(t.sent_ms.unwrap() <= headers);
    let last = t.last_delta_ms.unwrap();
    let end = t.end_ms.unwrap();
    assert!(last >= first_token + 280, "{t:?}");
    assert!(end - first_event >= 330, "{t:?}");
    assert_eq!(row.status, RequestStatus::Ok);
    assert_eq!(row.output_tokens, Some(OUTPUT));
    assert_eq!(row.upstream_request_id.as_deref(), Some("req_timing"));
    // The window is real, so TPS is a plausible number (287 tokens / ~0.36 s),
    // and the stats line shows exactly the row's values.
    let tps = row.tps_milli.expect("tps");
    assert!(tps < 1_000_000, "{tps}");
    assert!(row.injected);
    let (line_tps, line_ttft) = line_numbers(&events);
    assert_eq!(line_tps, tps_text(row.tps_milli));
    assert_eq!(line_ttft, ttft_text(row.first_token_ms));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_whole_answer_in_one_write_has_no_tps() {
    let all = sse_events().concat().into_bytes();
    let base = raw_upstream(vec![write(120, &[&head(), &chunk(&all), LAST_CHUNK])]).await;
    let run = start(base, false).await;
    let events = parse_sse(
        &post_turn(&run.router, "turn-1")
            .await
            .bytes()
            .await
            .unwrap(),
    );
    let (row, d) = newest(&run.paths, 1).await;
    check_consistent(&row, &d);
    let t = &d.timing;
    // Everything arrived together: equal (or near-equal) marks, no division.
    assert!(t.end_ms.unwrap() - t.headers_ms.unwrap() <= 25, "{t:?}");
    assert_eq!(row.tps_milli, None, "a window of ~0 ms shows —");
    assert_eq!(line_numbers(&events).0, "—");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_split_across_writes_are_timed_when_complete() {
    let ev = sse_events();
    let created = ev[0].as_bytes();
    let (c1, c2) = created.split_at(created.len() / 2);
    let delta = index_of("response.output_text.delta");
    let between: Vec<u8> = ev[1..delta].concat().into_bytes();
    let d = ev[delta].as_bytes();
    let (d1, d2) = d.split_at(d.len() / 3);
    let rest: Vec<u8> = ev[delta + 1..].concat().into_bytes();
    // A keep-alive comment comes with the headers; `response.created` arrives
    // in two halves 200 ms apart, the first delta split in the middle too.
    let base = raw_upstream(vec![
        write(50, &[&head(), &chunk(b": keep-alive\n\n")]),
        write(150, &[&chunk(c1)]),
        write(200, &[&chunk(c2), &chunk(&between), &chunk(d1)]),
        write(200, &[&chunk(d2)]),
        write(300, &[&chunk(&rest), LAST_CHUNK]),
    ])
    .await;
    let run = start(base, false).await;
    let bytes = post_turn(&run.router, "turn-1")
        .await
        .bytes()
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    // The client gets the stream unchanged apart from the stats line.
    let text = text
        .strip_prefix(": keep-alive\n\n")
        .expect("the comment passes first");
    let events = parse_sse(text.as_bytes());
    assert!(consistent_text(&events, "msg_1").starts_with("Hello world.\n\n> "));
    let (row, det) = newest(&run.paths, 1).await;
    check_consistent(&row, &det);
    let t = &det.timing;
    let headers = t.headers_ms.unwrap();
    assert!((40..190).contains(&headers), "{t:?}");
    // The comment is not the first event; `response.created` counts when its
    // second half arrived (~400 ms), not when its first bytes did (~200 ms).
    let first_event = t.first_event_ms.unwrap();
    assert!(first_event >= headers + 300, "{t:?}");
    // The first delta completes ~200 ms later.
    let first_token = t.first_token_ms.unwrap();
    assert!(first_token >= first_event + 150, "{t:?}");
    assert!(t.end_ms.unwrap() >= first_token + 250, "{t:?}");
    let (line_tps, line_ttft) = line_numbers(&events);
    assert_eq!(line_tps, tps_text(row.tps_milli));
    assert_eq!(line_ttft, ttft_text(row.first_token_ms));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn errors_without_events_record_headers_and_end_only() {
    for (status, failover) in [(429u16, false), (500, false), (503, true)] {
        let body = r#"{"error":{"code":"busy","message":"try later"}}"#;
        let resp_head = format!(
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
             connection: close\r\n\r\n",
            body.len()
        );
        let base = raw_upstream(vec![write(80, &[resp_head.as_bytes(), body.as_bytes()])]).await;
        let run = start(base, false).await;
        let resp = post_turn(&run.router, "turn-e").await;
        let (row, d) = newest(&run.paths, 1).await;
        check_consistent(&row, &d);
        let t = &d.timing;
        assert_eq!(t.first_event_ms, None, "{status}");
        assert_eq!(t.first_token_ms, None, "{status}");
        assert_eq!(t.last_delta_ms, None, "{status}");
        assert_eq!(row.tps_milli, None, "{status}");
        assert!(t.end_ms.is_some(), "{status}");
        if failover {
            // 503 fails over; with no other line the router answers 502.
            assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
            assert_eq!(t.headers_ms, None);
            assert!(matches!(
                row.status,
                RequestStatus::Http { status: 502, .. }
            ));
        } else {
            assert_eq!(resp.status().as_u16(), status);
            let headers = t.headers_ms.expect("headers");
            assert!(headers >= 70, "{status} {t:?}");
            assert!(t.sent_ms.unwrap() <= headers);
            assert!(t.end_ms.unwrap() >= headers);
            assert!(
                matches!(&row.status, RequestStatus::Http { status: s, code } if *s == status && code.as_deref() == Some("busy")),
                "{:?}",
                row.status
            );
        }
    }
}

// ---------------------------------------------------------------------------
// WebSocket
// ---------------------------------------------------------------------------

/// A WebSocket member that answers each `response.create` with the events of
/// [`events`], `response.created` after `first_ms` and the other events
/// `gap_ms` apart (0: all at once).
async fn ws_upstream(first_ms: u64, gap_ms: u64) -> url::Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            // Frames leave as they are written (no Nagle delay between them).
            stream.set_nodelay(true).unwrap();
            tokio::spawn(async move {
                let svc = service_fn(move |mut req: Request<Incoming>| async move {
                    if !req.headers().contains_key("authorization") {
                        let mut r = Response::new(Full::new(Bytes::new()));
                        *r.status_mut() = StatusCode::UNAUTHORIZED;
                        return Ok::<_, Infallible>(r);
                    }
                    let key = req
                        .headers()
                        .get("sec-websocket-key")
                        .map(|k| derive_accept_key(k.as_bytes()))
                        .unwrap_or_default();
                    let on_upgrade = hyper::upgrade::on(&mut req);
                    tokio::spawn(async move {
                        let Ok(io) = on_upgrade.await else { return };
                        let mut ws =
                            WebSocketStream::from_raw_socket(TokioIo::new(io), Role::Server, None)
                                .await;
                        while let Some(Ok(msg)) = ws.next().await {
                            let Message::Text(t) = msg else { continue };
                            let v: Value = serde_json::from_str(t.as_str()).unwrap_or_default();
                            if v["type"] != "response.create" {
                                continue;
                            }
                            tokio::time::sleep(Duration::from_millis(first_ms)).await;
                            for (i, (_, e)) in events().into_iter().enumerate() {
                                if i > 0 && gap_ms > 0 {
                                    tokio::time::sleep(Duration::from_millis(gap_ms)).await;
                                }
                                if ws.send(Message::text(e.to_string())).await.is_err() {
                                    return;
                                }
                            }
                        }
                    });
                    let mut r = Response::new(Full::new(Bytes::new()));
                    *r.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
                    let h = r.headers_mut();
                    h.insert("upgrade", "websocket".parse().unwrap());
                    h.insert("connection", "Upgrade".parse().unwrap());
                    h.insert("sec-websocket-accept", key.parse().unwrap());
                    Ok(r)
                });
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), svc)
                    .with_upgrades()
                    .await;
            });
        }
    });
    url::Url::parse(&format!("http://{addr}/backend-api/codex")).unwrap()
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn ws_turn(r: &Router, turn: &str) -> Vec<Value> {
    let mut req = format!("ws://{}/backend-api/codex/responses", r.addr)
        .into_client_request()
        .unwrap();
    let h = req.headers_mut();
    h.insert(
        "authorization",
        format!("Bearer {ROUTER_TOKEN}").parse().unwrap(),
    );
    h.insert("session-id", SESSION.parse().unwrap());
    h.insert("originator", "Codex Desktop".parse().unwrap());
    let (mut ws, resp): (Ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    assert_eq!(resp.status(), 101);
    let create = json!({
        "type": "response.create",
        "model": "gpt-x",
        "input": request_body("gpt-x", "Earlier.", json!({}))["input"].clone(),
        "prompt_cache_key": SESSION,
        "client_metadata": {"x-codex-turn-metadata": turn_metadata(turn, "turn", "user")},
    });
    ws.send(Message::text(create.to_string())).await.unwrap();
    let mut out = Vec::new();
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("a frame in time")
            .unwrap()
            .unwrap();
        let Message::Text(t) = msg else { continue };
        let v: Value = serde_json::from_str(t.as_str()).unwrap();
        let done = v["type"] == "response.completed";
        out.push(v);
        if done {
            let _ = ws.close(None).await;
            return out;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_websocket_turn_is_timed_per_frame() {
    // Frames 40 ms apart, the first after 150 ms.
    let run = start(ws_upstream(150, 40).await, true).await;
    let events = ws_turn(&run.router, "turn-w").await;
    let (row, d) = newest(&run.paths, 1).await;
    assert_eq!(row.transport, RequestTransport::WebSocket);
    check_consistent(&row, &d);
    let t = &d.timing;
    assert!(t.sent_ms.unwrap() <= 20, "{t:?}");
    let first_event = t.first_event_ms.unwrap();
    assert!(first_event >= 140, "{t:?}");
    // TTFB of a WebSocket request is its first frame.
    assert_eq!(row.ttfb_ms, Some(first_event));
    let first_delta = index_of("response.output_text.delta") as u64;
    assert!(
        t.first_token_ms.unwrap() >= first_event + first_delta * 35,
        "{t:?}"
    );
    let n = events.len() as u64;
    assert!(t.end_ms.unwrap() >= first_event + (n - 1) * 35, "{t:?}");
    assert!(row.tps_milli.is_some(), "{t:?}");
    let (line_tps, line_ttft) = line_numbers(&events);
    assert_eq!(line_tps, tps_text(row.tps_milli));
    assert_eq!(line_ttft, ttft_text(row.first_token_ms));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_websocket_burst_has_equal_marks_and_no_tps() {
    let run = start(ws_upstream(60, 0).await, true).await;
    let events = ws_turn(&run.router, "turn-b").await;
    let (row, d) = newest(&run.paths, 1).await;
    check_consistent(&row, &d);
    let t = &d.timing;
    assert!(t.end_ms.unwrap() - t.first_event_ms.unwrap() <= 25, "{t:?}");
    assert_eq!(row.tps_milli, None);
    assert_eq!(line_numbers(&events).0, "—");
}
