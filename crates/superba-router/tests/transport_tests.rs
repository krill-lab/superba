//! Upstream transports per line: zstd request bodies for lines that accept
//! them (and the fallback when one refuses), passthrough of Codex's own zstd
//! bodies, and WebSocket handshakes that skip lines which refused one.

mod common;

use std::sync::Arc;

use common::*;
use http::HeaderMap;
use pretty_assertions::assert_eq;
use superba_router::{LineConfig, MemberConfig, MemberKind};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const OK: &str = r#"{"id":"resp_1","status":"completed","output":[]}"#;
const BAD_JSON: &str =
    r#"{"error":{"message":"We could not parse the JSON body of your request."}}"#;
const BAD_PARAM: &str = r#"{"error":{"message":"Unknown parameter: 'x'."}}"#;

/// A Codex-sized Responses body (well above the compression threshold).
fn big_body() -> Vec<u8> {
    let text = "Explain the router transport rules once more. ".repeat(200);
    serde_json::to_vec(&serde_json::json!({
        "model": "gpt-x",
        "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": text}]}],
        "stream": true,
    }))
    .unwrap()
}

fn is_zstd(h: &HeaderMap) -> bool {
    h.get("content-encoding").and_then(|v| v.to_str().ok()) == Some("zstd")
}

/// An upstream that decodes zstd bodies and answers 200 to readable JSON.
fn decoding() -> Mode {
    Mode::Reply(Arc::new(|h: &HeaderMap, body: &[u8]| {
        let plain = if is_zstd(h) {
            match zstd::stream::decode_all(body) {
                Ok(p) => p,
                Err(_) => return (400, BAD_JSON),
            }
        } else {
            body.to_vec()
        };
        match serde_json::from_slice::<serde_json::Value>(&plain) {
            Ok(_) => (200, OK),
            Err(_) => (400, BAD_JSON),
        }
    }))
}

/// An upstream that never decodes: zstd bodies get `status`.
fn refusing(status: u16, body: &'static str) -> Mode {
    Mode::Reply(Arc::new(move |h: &HeaderMap, _: &[u8]| {
        if is_zstd(h) {
            (status, body)
        } else {
            (200, OK)
        }
    }))
}

fn zstd_line(id: &str, up: &Upstream, zstd: Option<bool>) -> LineConfig {
    let mut l = line(id, up.base("/backend-api/codex"));
    l.zstd = zstd;
    l
}

async fn post(r: &Router, path: &str, body: Vec<u8>, encoding: Option<&str>) -> reqwest::Response {
    let mut rb = r
        .http
        .post(r.url(path))
        .bearer_auth(ROUTER_TOKEN)
        .header("content-type", "application/json")
        .header("session-id", "transport-session")
        .body(body);
    if let Some(e) = encoding {
        rb = rb.header("content-encoding", e);
    }
    rb.send().await.unwrap()
}

const RESPONSES: &str = "/backend-api/codex/responses";

#[tokio::test]
async fn bodies_are_zstd_compressed_for_lines_that_accept_it() {
    let up = upstream(decoding()).await;
    let r = start(test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![zstd_line("a", &up, Some(true))],
    )]))
    .await;
    let body = big_body();
    let resp = post(&r, RESPONSES, body.clone(), None).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), OK);
    let seen = up.seen();
    assert_eq!(seen.len(), 1);
    assert!(is_zstd(&seen[0].headers));
    assert!(
        seen[0].body.len() < body.len() / 5,
        "{} bytes",
        seen[0].body.len()
    );
    assert_eq!(zstd::stream::decode_all(&seen[0].body[..]).unwrap(), body);
    assert_eq!(seen[0].headers["content-type"], "application/json");
    let st = r.status().await;
    assert!(st.members[0].lines[0].compress_requests);
}

#[tokio::test]
async fn unknown_lines_small_bodies_and_other_paths_pass_uncompressed() {
    let up = upstream(decoding()).await;
    let other = upstream(decoding()).await;
    let r = start(test_config(vec![
        member(
            "krill",
            MemberKind::Krill,
            false,
            vec![zstd_line("a", &up, Some(true))],
        ),
        member(
            "site",
            MemberKind::Compatible,
            false,
            vec![zstd_line("b", &other, None)],
        ),
    ]))
    .await;
    // Small body, compaction and other paths: as they are.
    post(
        &r,
        RESPONSES,
        br#"{"model":"gpt-x","input":"hi"}"#.to_vec(),
        None,
    )
    .await;
    post(&r, "/backend-api/codex/responses/compact", big_body(), None).await;
    post(
        &r,
        "/backend-api/codex/memories/trace_summarize",
        big_body(),
        None,
    )
    .await;
    let seen = up.seen();
    assert_eq!(seen.len(), 3);
    for s in &seen {
        assert!(!is_zstd(&s.headers), "{}", s.path);
        assert!(serde_json::from_slice::<serde_json::Value>(&s.body).is_ok());
    }
    // A line whose zstd support is unknown never gets a compressed body.
    let st = r.status().await;
    assert!(st.members[0].lines[0].compress_requests);
    assert!(!st.members[1].lines[0].compress_requests);
    let r2 = start(test_config(vec![member(
        "site",
        MemberKind::Compatible,
        false,
        vec![zstd_line("b", &other, None)],
    )]))
    .await;
    post(&r2, RESPONSES, big_body(), None).await;
    let seen = other.seen();
    assert_eq!(seen.len(), 1);
    assert!(!is_zstd(&seen[0].headers));
    assert_eq!(&seen[0].body[..], &big_body()[..]);
}

#[tokio::test]
async fn codex_zstd_bodies_pass_through_unchanged() {
    let up = upstream(decoding()).await;
    let r = start(test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![zstd_line("a", &up, Some(true))],
    )]))
    .await;
    let plain = big_body();
    let encoded = zstd::stream::encode_all(&plain[..], 19).unwrap();
    let resp = post(&r, RESPONSES, encoded.clone(), Some("zstd")).await;
    assert_eq!(resp.status(), 200);
    let seen = up.seen();
    assert_eq!(seen.len(), 1);
    assert!(is_zstd(&seen[0].headers));
    assert_eq!(&seen[0].body[..], &encoded[..], "never re-compressed");
}

#[tokio::test]
async fn a_refused_zstd_body_is_sent_again_plain_and_the_line_remembered() {
    for (status, body) in [(415, "{}"), (400, BAD_JSON), (422, BAD_JSON)] {
        let up = upstream(refusing(status, body)).await;
        let r = start(test_config(vec![member(
            "krill",
            MemberKind::Krill,
            false,
            vec![zstd_line("a", &up, Some(true))],
        )]))
        .await;
        let resp = post(&r, RESPONSES, big_body(), None).await;
        assert_eq!(
            resp.status(),
            200,
            "Codex never sees the refusal ({status})"
        );
        assert_eq!(resp.text().await.unwrap(), OK);
        let seen = up.seen();
        assert_eq!(seen.len(), 2, "{status}");
        assert!(is_zstd(&seen[0].headers));
        assert!(!is_zstd(&seen[1].headers));
        assert_eq!(&seen[1].body[..], &big_body()[..]);
        // Remembered: the next request goes plain at once.
        post(&r, RESPONSES, big_body(), None).await;
        let seen = up.seen();
        assert_eq!(seen.len(), 3);
        assert!(!is_zstd(&seen[2].headers));
        let st = r.status().await;
        assert!(!st.members[0].lines[0].compress_requests, "{status}");
        assert_eq!(st.totals.failovers, 0, "not a failover");
    }
}

#[tokio::test]
async fn a_genuine_400_is_passed_on_and_not_held_against_the_line() {
    // The plain body is rejected too: the request itself is bad, not zstd.
    let up = upstream(Mode::Json(400, BAD_PARAM)).await;
    let r = start(test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![zstd_line("a", &up, Some(true))],
    )]))
    .await;
    let resp = post(&r, RESPONSES, big_body(), None).await;
    assert_eq!(resp.status(), 400);
    assert_eq!(resp.text().await.unwrap(), BAD_PARAM);
    assert_eq!(up.hits(), 2);
    up.set(decoding());
    post(&r, RESPONSES, big_body(), None).await;
    let seen = up.seen();
    assert_eq!(seen.len(), 3);
    assert!(is_zstd(&seen[2].headers), "still compressed for");
    assert!(r.status().await.members[0].lines[0].compress_requests);
}

#[tokio::test]
async fn other_errors_of_a_zstd_body_are_passed_on_once() {
    // 401 / 429 / 500 say nothing about zstd: no second request.
    for (status, body) in [
        (401, r#"{"error":{"message":"bad key"}}"#),
        (429, r#"{"error":{"message":"slow down"}}"#),
        (500, r#"{"error":{"message":"oops"}}"#),
    ] {
        let up = upstream(Mode::Json(status, body)).await;
        let r = start(test_config(vec![member(
            "krill",
            MemberKind::Krill,
            false,
            vec![zstd_line("a", &up, Some(true))],
        )]))
        .await;
        let resp = post(&r, RESPONSES, big_body(), None).await;
        assert_eq!(resp.status(), status);
        assert_eq!(up.hits(), 1, "{status}");
        assert!(r.status().await.members[0].lines[0].compress_requests);
    }
}

fn ws_request(r: &Router) -> http::Request<()> {
    let mut req = format!("ws://{}/backend-api/codex/responses", r.addr)
        .into_client_request()
        .unwrap();
    let h = req.headers_mut();
    h.insert(
        "authorization",
        format!("Bearer {ROUTER_TOKEN}").parse().unwrap(),
    );
    h.insert("session-id", "ws-transport".parse().unwrap());
    req
}

async fn ws_status(r: &Router) -> u16 {
    match tokio_tungstenite::connect_async(ws_request(r)).await {
        Ok((mut ws, resp)) => {
            let _ = ws.close(None).await;
            resp.status().as_u16()
        }
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => resp.status().as_u16(),
        Err(e) => panic!("unexpected handshake error {e:?}"),
    }
}

#[tokio::test]
async fn websocket_skips_lines_whose_probe_got_426() {
    let refused = upstream(Mode::Json(426, r#"{"error":{"message":"no ws"}}"#)).await;
    let ok = upstream(Mode::WsEcho).await;
    let mut a = line("a", refused.base("/backend-api/codex"));
    a.websocket = Some(false);
    let b = line("b", ok.base("/backend-api/codex"));
    let r = start(test_config(vec![member(
        "krill",
        MemberKind::Krill,
        true,
        vec![a.clone(), b],
    )]))
    .await;
    assert_eq!(ws_status(&r).await, 101);
    assert_eq!(refused.hits(), 0, "never dialled");
    assert_eq!(ok.hits(), 1);
    let st = r.status().await;
    assert!(!st.members[0].lines[0].offers_websocket);
    assert!(st.members[0].lines[1].offers_websocket);

    // Only refusing lines: 426 from the router itself, nothing dialled.
    let r = start(test_config(vec![member(
        "krill",
        MemberKind::Krill,
        true,
        vec![a],
    )]))
    .await;
    assert_eq!(ws_status(&r).await, 426);
    assert_eq!(refused.hits(), 0);
    assert_eq!(r.status().await.totals.ws_refused, 1);
}

#[tokio::test]
async fn a_426_from_a_line_is_passed_on_and_remembered() {
    let refused = upstream(Mode::Json(426, r#"{"error":{"message":"no ws"}}"#)).await;
    let r = start(test_config(vec![member(
        "krill",
        MemberKind::Krill,
        true,
        vec![line("a", refused.base("/backend-api/codex"))],
    )]))
    .await;
    assert_eq!(ws_status(&r).await, 426);
    assert_eq!(refused.hits(), 1);
    // Remembered: the next session gets the router's own 426 at once.
    assert_eq!(ws_status(&r).await, 426);
    assert_eq!(refused.hits(), 1);
    let st = r.status().await;
    assert!(!st.members[0].lines[0].offers_websocket);
    assert_eq!(st.totals.ws_refused, 1);
    // HTTP requests of the session still use the line.
    refused.set(Mode::Json(200, OK));
    let resp = r.post(Some("ws-transport"), r#"{"model":"gpt-x"}"#).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(refused.hits(), 2);
}

#[tokio::test]
async fn websocket_frames_pass_through_a_line_with_websocket() {
    let ok = upstream(Mode::WsEcho).await;
    let mut l = line("a", ok.base("/backend-api/codex"));
    l.websocket = Some(true);
    l.zstd = Some(true);
    let r = start(test_config(vec![member(
        "krill",
        MemberKind::Krill,
        true,
        vec![l],
    )]))
    .await;
    use futures_util::{SinkExt, StreamExt};
    let (mut ws, resp) = tokio_tungstenite::connect_async(ws_request(&r))
        .await
        .unwrap();
    assert_eq!(resp.status(), 101);
    let text = "x".repeat(4096);
    ws.send(tokio_tungstenite::tungstenite::Message::text(text.clone()))
        .await
        .unwrap();
    let reply = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    // Frames are never zstd-compressed: the bytes arrive as sent.
    assert_eq!(reply.into_text().unwrap().as_str(), format!("echo:{text}"));
    ws.close(None).await.unwrap();
}

#[test]
fn member_websocket_follows_the_line_hints() {
    let l = |ws: Option<bool>| LineConfig {
        id: "a".into(),
        name: "A".into(),
        base_url: url::Url::parse("https://h.example/backend-api/codex").unwrap(),
        probe_url: None,
        zstd: None,
        websocket: ws,
    };
    let m = |kind, lines: Vec<LineConfig>| {
        let mut m = MemberConfig {
            id: "m".into(),
            name: "M".into(),
            kind,
            lines,
            websocket: false,
            model_map: Default::default(),
            compaction: Default::default(),
        };
        m.apply_websocket_hints();
        m.websocket
    };
    use MemberKind::{Compatible, Krill};
    assert!(m(Krill, vec![l(None)]));
    assert!(m(Krill, vec![l(Some(false)), l(None)]));
    assert!(!m(Krill, vec![l(Some(false)), l(Some(false))]));
    assert!(!m(Compatible, vec![l(None)]));
    assert!(!m(Compatible, vec![l(Some(false))]));
    assert!(m(Compatible, vec![l(None), l(Some(true))]));
    // Hints round-trip through config.json and stay out of it when unknown.
    let json = serde_json::to_value(l(None)).unwrap();
    assert!(json.get("zstd").is_none() && json.get("websocket").is_none());
    let mut with = l(Some(true));
    with.zstd = Some(true);
    let back: LineConfig = serde_json::from_value(serde_json::to_value(&with).unwrap()).unwrap();
    assert_eq!(back, with);
}

/// A router recording to files in a temp data dir.
async fn start_recording(
    cfg: superba_router::RouterConfig,
) -> (Router, superba_router::RouterPaths, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let paths = superba_router::RouterPaths::new(&superba_core::DataDir::new(dir.path()));
    let mut cfg = cfg;
    cfg.stats = Some(superba_router::StatsConfig::default());
    let secrets = secrets_for(&cfg);
    cfg.save(&paths).unwrap();
    secrets.save(&paths).unwrap();
    let server = superba_router::RouterServer::bind_from_files(paths.clone())
        .await
        .unwrap();
    let router = Router {
        addr: server.local_addr(),
        admin_token: server.admin_token().expose().to_owned(),
        handle: server.shutdown_handle(),
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
    };
    tokio::spawn(server.serve());
    (router, paths, dir)
}

/// The recorded attempts of the `n` newest requests, oldest first.
async fn recorded(
    paths: &superba_router::RouterPaths,
    n: usize,
) -> Vec<superba_core::RequestDetail> {
    let reader = superba_router::Reader::open(paths).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let page = reader
            .query(&superba_core::RequestQuery::default())
            .unwrap();
        if page.rows.len() >= n || std::time::Instant::now() > deadline {
            let mut out: Vec<_> = page
                .rows
                .iter()
                .map(|r| reader.detail(r.id).unwrap())
                .collect();
            out.sort_by_key(|d| d.row.id);
            return out;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_request_log_records_how_each_body_went_upstream() {
    use superba_core::RequestCompression as C;
    let up = upstream(decoding()).await;
    let (r, paths, _dir) = start_recording(test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![zstd_line("a", &up, Some(true))],
    )]))
    .await;
    // Compressed by the router; Codex's own zstd passed through; too small.
    post(&r, RESPONSES, big_body(), None).await;
    let encoded = zstd::stream::encode_all(&big_body()[..], 3).unwrap();
    post(&r, RESPONSES, encoded, Some("zstd")).await;
    post(
        &r,
        RESPONSES,
        br#"{"model":"gpt-x","input":"hi"}"#.to_vec(),
        None,
    )
    .await;
    // The line stops decoding zstd: refused, sent again plain, remembered.
    up.set(refusing(415, "{}"));
    post(&r, RESPONSES, big_body(), None).await;
    post(&r, RESPONSES, big_body(), None).await;
    let details = recorded(&paths, 5).await;
    assert_eq!(details.len(), 5);
    let used = |d: &superba_core::RequestDetail| d.attempts.last().and_then(|a| a.compression);
    assert_eq!(used(&details[0]), Some(C::RouterZstd));
    assert_eq!(used(&details[1]), Some(C::ClientZstd));
    assert_eq!(used(&details[2]), Some(C::None));
    assert_eq!(used(&details[3]), Some(C::ZstdRefused));
    let refused = &details[3].attempts;
    assert_eq!(refused.len(), 2, "{refused:?}");
    assert_eq!(refused[0].status, Some(415));
    assert_eq!(refused[0].compression, Some(C::RouterZstd));
    assert_eq!(details[3].row.failovers, 0, "not a failover");
    assert_eq!(used(&details[4]), Some(C::None));
    // Both legs used HTTP: the router never changes the transport.
    for d in &details {
        assert_eq!(d.row.transport, superba_core::RequestTransport::Http);
    }
    r.handle.shutdown();
}

/// Answers 426 to a WebSocket handshake and 200 to HTTP (an upstream without
/// WebSocket support, like the e2e test relay).
fn http_only() -> Mode {
    Mode::Reply(Arc::new(|h: &HeaderMap, _: &[u8]| {
        if h.get("upgrade").is_some() {
            (426, r#"{"error":{"message":"no ws"}}"#)
        } else {
            (200, OK)
        }
    }))
}

async fn ws_status_for(r: &Router, session: &str) -> u16 {
    let mut req = ws_request(r);
    req.headers_mut()
        .insert("session-id", session.parse().unwrap());
    match tokio_tungstenite::connect_async(req).await {
        Ok((mut ws, resp)) => {
            let _ = ws.close(None).await;
            resp.status().as_u16()
        }
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => resp.status().as_u16(),
        Err(e) => panic!("unexpected handshake error {e:?}"),
    }
}

#[tokio::test]
async fn a_refused_websocket_handshake_does_not_pin_the_session_to_that_line() {
    // e2e s13: line A refused a WebSocket handshake once (remembered), so the
    // next new conversation's handshake went to line B, which refused it too;
    // the 426 pinned that conversation to B and its HTTP turns ran on B while A
    // was healthy. A refused handshake is not a route for the session.
    let up_a = upstream(http_only()).await;
    let up_b = upstream(http_only()).await;
    let r = start(test_config(vec![member(
        "krill",
        MemberKind::Krill,
        true,
        vec![
            line("a", up_a.base("/backend-api/codex")),
            line("b", up_b.base("/backend-api/codex")),
        ],
    )]))
    .await;
    assert_eq!(ws_status_for(&r, "first").await, 426);
    assert_eq!((up_a.hits(), up_b.hits()), (1, 0), "A tried first");
    assert_eq!(ws_status_for(&r, "second").await, 426);
    assert_eq!(
        (up_a.hits(), up_b.hits()),
        (1, 1),
        "A skipped for WebSocket, B refused too"
    );
    let resp = r.post(Some("second"), r#"{"model":"gpt-x"}"#).await;
    assert_eq!(resp.status(), 200);
    let posts = |up: &Upstream| up.seen().iter().filter(|s| s.method == "POST").count();
    assert_eq!(
        (posts(&up_a), posts(&up_b)),
        (1, 0),
        "the HTTP turn uses the first healthy line"
    );
}
