//! The router against local fake upstreams: forwarding, auth swap, failover
//! rules, stickiness, streaming, WebSocket, model mapping and the admin API.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use futures_util::{SinkExt, StreamExt};
use superba_router::{LineState, MemberKind, RouteEventKind};
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const OK: &str = r#"{"ok":true}"#;
const BODY: &str = r#"{"model":"gpt-x","input":"hello"}"#;

#[tokio::test]
async fn forwards_with_the_member_key_and_maps_the_path() {
    let up = upstream(Mode::Json(200, OK)).await;
    let r = start(test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", up.base("/backend-api/codex"))],
    )]))
    .await;
    let resp = r
        .http
        .post(r.url("/backend-api/codex/responses?trace=1"))
        .bearer_auth(ROUTER_TOKEN)
        .header("session-id", "s1")
        .header("chatgpt-account-id", "acct")
        .header("x-codex-turn-metadata", "{}")
        .body(BODY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(route(&resp), "krill/a");
    assert_eq!(resp.text().await.unwrap(), OK);
    let seen = up.seen();
    assert_eq!(seen.len(), 1);
    let s = &seen[0];
    assert_eq!(s.method, "POST");
    assert_eq!(s.path, "/backend-api/codex/responses?trace=1");
    assert_eq!(
        s.headers["authorization"],
        format!("Bearer {}", key_of("krill")).as_str()
    );
    assert!(!s.headers.contains_key("chatgpt-account-id"));
    assert_eq!(s.headers["session-id"], "s1");
    assert_eq!(s.headers["x-codex-turn-metadata"], "{}");
    assert_eq!(s.headers["host"], up.addr.to_string().as_str());
    assert_eq!(&s.body[..], BODY.as_bytes());
}

#[tokio::test]
async fn v1_prefix_maps_like_the_codex_prefix() {
    let up = upstream(Mode::Json(200, OK)).await;
    let r = start(test_config(vec![member(
        "site",
        MemberKind::Compatible,
        false,
        vec![line("a", up.base("/v1"))],
    )]))
    .await;
    let resp = r
        .http
        .get(r.url("/v1/models"))
        .bearer_auth(ROUTER_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(up.seen()[0].path, "/v1/models");
}

#[tokio::test]
async fn refuses_a_missing_or_wrong_router_token() {
    let up = upstream(Mode::Json(200, OK)).await;
    let r = start(test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", up.base("/x"))],
    )]))
    .await;
    for token in [None, Some("wrong"), Some(key_of("krill").as_str())] {
        let mut rb = r
            .http
            .post(r.url("/backend-api/codex/responses"))
            .body(BODY);
        if let Some(t) = token {
            rb = rb.bearer_auth(t);
        }
        let resp = rb.send().await.unwrap();
        assert_eq!(resp.status(), 401);
        let v: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(v["error"]["type"], "router_auth");
    }
    let unknown = r.http.get(r.url("/elsewhere")).send().await.unwrap();
    assert_eq!(unknown.status(), 404);
    assert_eq!(up.hits(), 0);
    assert_eq!(r.status().await.totals.rejected, 3);
}

#[tokio::test]
async fn fails_over_on_connection_refused_and_opens_the_breaker() {
    let good = upstream(Mode::Json(200, OK)).await;
    let r = start(test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", dead_base()), line("b", good.base("/up"))],
    )]))
    .await;
    let resp = r.post(Some("s1"), BODY).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(route(&resp), "krill/b");
    let st = r.status().await;
    let a = &st.members[0].lines[0];
    assert_eq!(a.state, LineState::Open);
    assert!(
        a.last_error
            .as_deref()
            .unwrap()
            .starts_with("connect failed")
    );
    assert_eq!(a.failovers_away, 1);
    assert_eq!(st.totals.failovers, 1);
    assert!(st.recent.iter().any(|e| e.kind == RouteEventKind::Failover));
    // The open line is skipped: a new conversation goes straight to b.
    let resp = r.post(Some("s2"), BODY).await;
    assert_eq!(route(&resp), "krill/b");
    assert_eq!(r.status().await.totals.failovers, 1);
}

#[tokio::test]
async fn fails_over_on_reset_before_headers() {
    let good = upstream(Mode::Json(200, OK)).await;
    let r = start(test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", resetter().await), line("b", good.base("/up"))],
    )]))
    .await;
    let resp = r.post(None, BODY).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(route(&resp), "krill/b");
    assert_eq!(good.seen()[0].body, BODY.as_bytes());
}

#[tokio::test]
async fn fails_over_on_502_503_504_and_edge_interception() {
    for mode in [
        Mode::Json(502, r#"{"error":"bad gateway"}"#),
        Mode::Json(503, r#"{"error":"unavailable"}"#),
        Mode::Json(504, r#"{"error":"timeout"}"#),
        Mode::JsonHeader(403, r#"{"error":"blocked"}"#, "l-block", "1"),
    ] {
        let bad = upstream(mode).await;
        let good = upstream(Mode::Json(200, OK)).await;
        let r = start(test_config(vec![member(
            "krill",
            MemberKind::Krill,
            false,
            vec![line("a", bad.base("/up")), line("b", good.base("/up"))],
        )]))
        .await;
        let resp = r.post(Some("s"), BODY).await;
        assert_eq!(resp.status(), 200);
        assert_eq!(route(&resp), "krill/b");
        assert_eq!(bad.hits(), 1);
        assert_eq!(good.hits(), 1);
    }
}

#[tokio::test]
async fn an_empty_2xx_to_responses_is_an_edge_interception() {
    let edge = upstream(Mode::Json(200, "")).await;
    let good = upstream(Mode::Json(200, OK)).await;
    let r = start(test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", edge.base("/up")), line("b", good.base("/up"))],
    )]))
    .await;
    let resp = r.post(Some("s"), BODY).await;
    assert_eq!(route(&resp), "krill/b");
    // Other paths may answer an empty 2xx legitimately: passed through. (One
    // soft failure does not open line a's breaker, so it is tried first.)
    let resp = r
        .http
        .get(r.url("/backend-api/codex/models"))
        .bearer_auth(ROUTER_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(route(&resp), "krill/a");
    assert_eq!(edge.hits(), 2);
    assert_eq!(good.hits(), 1);
}

#[tokio::test]
async fn never_fails_over_on_account_errors_or_500() {
    for (status, body) in [
        (401, r#"{"error":{"message":"invalid key"}}"#),
        (403, r#"{"error":{"message":"forbidden"}}"#),
        (429, r#"{"error":{"message":"slow down"}}"#),
        (400, r#"{"error":{"message":"bad request"}}"#),
        (500, r#"{"error":{"message":"internal"}}"#),
    ] {
        let first = upstream(Mode::Json(status, body)).await;
        let second = upstream(Mode::Json(200, OK)).await;
        let r = start(test_config(vec![
            member(
                "krill",
                MemberKind::Krill,
                false,
                vec![line("a", first.base("/up")), line("b", second.base("/up"))],
            ),
            member(
                "site",
                MemberKind::Compatible,
                false,
                vec![line("c", second.base("/v1"))],
            ),
        ]))
        .await;
        let resp = r.post(Some("s"), BODY).await;
        assert_eq!(resp.status().as_u16(), status);
        assert_eq!(route(&resp), "krill/a");
        assert_eq!(resp.text().await.unwrap(), body);
        assert_eq!(second.hits(), 0, "HTTP {status} must not fail over");
        let st = r.status().await;
        assert_eq!(st.totals.failovers, 0);
        assert_eq!(st.members[0].lines[0].state, LineState::Healthy);
    }
}

/// The e2e router scenario's account-error check: a line whose breaker opened is
/// skipped (whatever it answers by then) until its backoff ends; once half-open,
/// its 401/403/429 is passed back like a healthy line's, never failed over, and
/// closes the breaker (the line answered).
#[tokio::test]
async fn a_recovering_line_passes_account_errors_back_without_failover() {
    for (status, body) in [
        (401, r#"{"error":{"message":"invalid key"}}"#),
        (403, r#"{"error":{"message":"forbidden"}}"#),
        (429, r#"{"error":{"message":"slow down"}}"#),
    ] {
        let first = upstream(Mode::Json(503, "{}")).await;
        let second = upstream(Mode::Json(200, OK)).await;
        let mut cfg = test_config(vec![member(
            "krill",
            MemberKind::Krill,
            false,
            vec![line("a", first.base("/up")), line("b", second.base("/up"))],
        )]);
        cfg.tuning.breaker_threshold = 1;
        cfg.tuning.breaker_base_ms = 300;
        let r = start(cfg).await;
        let resp = r.post(None, BODY).await;
        assert_eq!(route(&resp), "krill/b");
        assert_eq!(r.status().await.members[0].lines[0].state, LineState::Open);

        // Open: skipped, even though it would answer now.
        first.set(Mode::Json(status, body));
        let resp = r.post(None, BODY).await;
        assert_eq!(route(&resp), "krill/b");
        assert_eq!(first.hits(), 1);

        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(
            r.status().await.members[0].lines[0].state,
            LineState::HalfOpen
        );
        let resp = r.post(None, BODY).await;
        assert_eq!(resp.status().as_u16(), status);
        assert_eq!(route(&resp), "krill/a");
        assert_eq!(resp.text().await.unwrap(), body);
        assert_eq!(second.hits(), 2, "HTTP {status} must not fail over");
        let st = r.status().await;
        assert_eq!(st.totals.failovers, 1, "only the 503 failed over");
        assert_eq!(st.members[0].lines[0].state, LineState::Healthy);
    }
}

#[tokio::test]
async fn all_lines_down_answers_router_unavailable() {
    let bad = upstream(Mode::Json(503, "{}")).await;
    let r = start(test_config(vec![
        member(
            "krill",
            MemberKind::Krill,
            false,
            vec![line("a", dead_base())],
        ),
        member(
            "site",
            MemberKind::Compatible,
            false,
            vec![line("c", bad.base("/v1"))],
        ),
    ]))
    .await;
    let resp = r.post(Some("s"), BODY).await;
    assert_eq!(resp.status(), 502);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["error"]["type"], "router_unavailable");
    let msg = v["error"]["message"].as_str().unwrap();
    assert!(msg.starts_with("Superba local router: no line reachable"));
    assert!(msg.contains("krill/a: connect failed"), "{msg}");
    assert!(msg.contains("site/c: HTTP 503"), "{msg}");
    let st = r.status().await;
    assert_eq!(st.totals.unavailable, 1);
    // Every line is open now, but a request still gets one attempt (the line
    // whose backoff ends first) instead of being refused untried.
    bad.set(Mode::Json(200, OK));
    let resp = r.post(Some("s"), BODY).await;
    assert_eq!(resp.status(), 200);
    assert_eq!(route(&resp), "site/c");
}

#[tokio::test]
async fn a_conversation_stays_on_its_member_after_failover() {
    let x = upstream(Mode::Json(200, OK)).await;
    let y = upstream(Mode::Json(200, OK)).await;
    let r = start(test_config(vec![
        member(
            "x",
            MemberKind::Krill,
            false,
            vec![line("x1", x.base("/up"))],
        ),
        member(
            "y",
            MemberKind::Compatible,
            false,
            vec![line("y1", y.base("/v1"))],
        ),
    ]))
    .await;
    assert_eq!(route(&r.post(Some("s1"), BODY).await), "x/x1");
    x.set(Mode::Json(502, "{}"));
    assert_eq!(route(&r.post(Some("s1"), BODY).await), "y/y1");
    // x recovers; s1 stays on y (no flapping back), a new conversation uses x.
    x.set(Mode::Json(200, OK));
    assert_eq!(route(&r.post(Some("s1"), BODY).await), "y/y1");
    assert_eq!(route(&r.post(Some("s2"), BODY).await), "x/x1");
    assert_eq!(route(&r.post(Some("s1"), BODY).await), "y/y1");
    let st = r.status().await;
    assert_eq!(st.sticky_sessions, 2);
    assert!(
        st.recent
            .iter()
            .any(|e| e.kind == RouteEventKind::MemberChanged)
    );
}

#[tokio::test]
async fn the_pinned_members_other_lines_come_before_other_members() {
    let x1 = upstream(Mode::Json(200, OK)).await;
    let x2 = upstream(Mode::Json(200, OK)).await;
    let y = upstream(Mode::Json(200, OK)).await;
    let r = start(test_config(vec![
        member(
            "x",
            MemberKind::Krill,
            false,
            vec![line("x1", x1.base("/up")), line("x2", x2.base("/up"))],
        ),
        member(
            "y",
            MemberKind::Compatible,
            false,
            vec![line("y1", y.base("/v1"))],
        ),
    ]))
    .await;
    assert_eq!(route(&r.post(Some("s"), BODY).await), "x/x1");
    x1.set(Mode::Json(503, "{}"));
    assert_eq!(route(&r.post(Some("s"), BODY).await), "x/x2");
    assert_eq!(y.hits(), 0);
}

#[tokio::test]
async fn turn_state_is_dropped_only_when_the_member_changes() {
    let x = upstream(Mode::Json(200, OK)).await;
    let y = upstream(Mode::Json(200, OK)).await;
    let r = start(test_config(vec![
        member(
            "x",
            MemberKind::Krill,
            false,
            vec![line("x1", x.base("/up"))],
        ),
        member(
            "y",
            MemberKind::Krill,
            false,
            vec![line("y1", y.base("/up"))],
        ),
    ]))
    .await;
    let send = |r: &Router| {
        r.http
            .post(r.url("/backend-api/codex/responses"))
            .bearer_auth(ROUTER_TOKEN)
            .header("session-id", "s")
            .header("x-codex-turn-state", "opaque")
            .body(BODY)
            .send()
    };
    send(&r).await.unwrap();
    send(&r).await.unwrap();
    assert!(
        x.seen()
            .iter()
            .all(|s| s.headers["x-codex-turn-state"] == "opaque")
    );
    x.set(Mode::Json(502, "{}"));
    let resp = send(&r).await.unwrap();
    assert_eq!(route(&resp), "y/y1");
    assert!(!y.seen()[0].headers.contains_key("x-codex-turn-state"));
}

#[tokio::test]
async fn sse_streams_without_buffering() {
    let gate = Arc::new(Notify::new());
    let up = upstream(Mode::Sse {
        first: "data: one\n\n",
        rest: "data: two\n\n",
        gate: Some(gate.clone()),
        break_mid: false,
    })
    .await;
    let r = start(test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", up.base("/up"))],
    )]))
    .await;
    let mut resp = r.post(Some("s"), BODY).await;
    assert_eq!(resp.headers()["content-type"], "text/event-stream");
    // The first event arrives while the upstream still holds the second one.
    let first = tokio::time::timeout(Duration::from_secs(5), resp.chunk())
        .await
        .expect("first event was buffered")
        .unwrap()
        .unwrap();
    assert_eq!(&first[..], b"data: one\n\n");
    gate.notify_one();
    let mut rest = Vec::new();
    while let Some(c) = tokio::time::timeout(Duration::from_secs(5), resp.chunk())
        .await
        .unwrap()
        .unwrap()
    {
        rest.extend_from_slice(&c);
    }
    assert_eq!(rest, b"data: two\n\n");
}

#[tokio::test]
async fn a_broken_stream_aborts_downstream_and_marks_the_line() {
    let flaky = upstream(Mode::Sse {
        first: "data: partial\n\n",
        rest: "",
        gate: None,
        break_mid: true,
    })
    .await;
    let good = upstream(Mode::Json(200, OK)).await;
    let r = start(test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", flaky.base("/up")), line("b", good.base("/up"))],
    )]))
    .await;
    let mut resp = r.post(Some("s"), BODY).await;
    assert_eq!(route(&resp), "krill/a");
    let first = resp.chunk().await.unwrap().unwrap();
    assert_eq!(&first[..], b"data: partial\n\n");
    let mut broke = false;
    for _ in 0..4 {
        match resp.chunk().await {
            Err(_) => {
                broke = true;
                break;
            }
            Ok(None) => break,
            Ok(Some(_)) => {}
        }
    }
    assert!(
        broke,
        "the downstream response must be aborted, not completed"
    );
    let st = r.status().await;
    let a = &st.members[0].lines[0];
    assert_eq!(a.broken_streams, 1);
    assert_eq!(a.state, LineState::Open);
    // Codex re-sends at once; the router picks the healthy line.
    assert_eq!(route(&r.post(Some("s"), BODY).await), "krill/b");
}

#[tokio::test]
async fn zstd_bodies_pass_through_byte_for_byte() {
    let up = upstream(Mode::Json(200, OK)).await;
    let r = start(test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", up.base("/up"))],
    )]))
    .await;
    let compressed = zstd::stream::encode_all(BODY.as_bytes(), 3).unwrap();
    let resp = r
        .http
        .post(r.url("/backend-api/codex/responses"))
        .bearer_auth(ROUTER_TOKEN)
        .header("content-encoding", "zstd")
        .body(compressed.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let s = &up.seen()[0];
    assert_eq!(s.headers["content-encoding"], "zstd");
    assert_eq!(s.body.to_vec(), compressed);
}

#[tokio::test]
async fn model_names_are_mapped_per_member() {
    let up = upstream(Mode::Json(200, OK)).await;
    let mut m = member(
        "site",
        MemberKind::Compatible,
        false,
        vec![line("a", up.base("/v1"))],
    );
    m.model_map.insert("gpt-x".into(), "vendor-x".into());
    let r = start(test_config(vec![m])).await;
    r.post(None, BODY).await;
    let compressed = zstd::stream::encode_all(BODY.as_bytes(), 3).unwrap();
    r.http
        .post(r.url("/backend-api/codex/responses"))
        .bearer_auth(ROUTER_TOKEN)
        .header("content-encoding", "zstd")
        .body(compressed)
        .send()
        .await
        .unwrap();
    r.post(None, r#"{"model":"other","input":"x"}"#).await;
    let seen = up.seen();
    let model = |b: &[u8]| -> String {
        let v: serde_json::Value = serde_json::from_slice(b).unwrap();
        v["model"].as_str().unwrap().to_owned()
    };
    assert_eq!(model(&seen[0].body), "vendor-x");
    let plain = zstd::stream::decode_all(&seen[1].body[..]).unwrap();
    assert_eq!(model(&plain), "vendor-x");
    assert_eq!(&seen[2].body[..], br#"{"model":"other","input":"x"}"#);
}

#[tokio::test]
async fn oversized_bodies_are_refused() {
    let up = upstream(Mode::Json(200, OK)).await;
    let mut cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", up.base("/up"))],
    )]);
    cfg.tuning.max_body_bytes = 8;
    let r = start(cfg).await;
    let resp = r.post(None, BODY).await;
    assert_eq!(resp.status(), 413);
    assert_eq!(up.hits(), 0);
}

fn ws_request(r: &Router, token: &str) -> http::Request<()> {
    let mut req = format!("ws://{}/backend-api/codex/responses", r.addr)
        .into_client_request()
        .unwrap();
    let h = req.headers_mut();
    h.insert("authorization", format!("Bearer {token}").parse().unwrap());
    h.insert("session-id", "ws-session".parse().unwrap());
    h.insert(
        "openai-beta",
        "responses_websockets=2026-02-06".parse().unwrap(),
    );
    req
}

#[tokio::test]
async fn websocket_passes_through_with_the_member_key() {
    let up = upstream(Mode::WsEcho).await;
    let r = start(test_config(vec![member(
        "krill",
        MemberKind::Krill,
        true,
        vec![
            line("a", dead_base()),
            line("b", up.base("/backend-api/codex")),
        ],
    )]))
    .await;
    let (mut ws, resp) = tokio_tungstenite::connect_async(ws_request(&r, ROUTER_TOKEN))
        .await
        .unwrap();
    assert_eq!(resp.status(), 101);
    assert_eq!(
        resp.headers()[superba_router::ROUTE_HEADER],
        "krill/b",
        "dial failures fail over at the handshake"
    );
    ws.send(Message::text("hi")).await.unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(reply, Message::text("echo:hi"));
    let s = &up.seen()[0];
    assert_eq!(s.path, "/backend-api/codex/responses");
    assert_eq!(
        s.headers["authorization"],
        format!("Bearer {}", key_of("krill")).as_str()
    );
    assert_eq!(s.headers["openai-beta"], "responses_websockets=2026-02-06");
    assert_eq!(s.headers["session-id"], "ws-session");
    let st = r.status().await;
    assert_eq!(st.totals.websockets, 1);
    assert_eq!(st.totals.active_websockets, 1);
    ws.close(None).await.unwrap();
}

#[tokio::test]
async fn websocket_426_from_a_member_is_passed_through() {
    let up = upstream(Mode::Json(
        426,
        r#"{"error":{"message":"websocket not enabled"}}"#,
    ))
    .await;
    let other = upstream(Mode::WsEcho).await;
    let r = start(test_config(vec![member(
        "krill",
        MemberKind::Krill,
        true,
        vec![line("a", up.base("/up")), line("b", other.base("/up"))],
    )]))
    .await;
    let err = tokio_tungstenite::connect_async(ws_request(&r, ROUTER_TOKEN))
        .await
        .unwrap_err();
    let tokio_tungstenite::tungstenite::Error::Http(resp) = err else {
        panic!("expected an HTTP answer, got {err:?}");
    };
    assert_eq!(resp.status(), 426);
    assert_eq!(other.hits(), 0);
}

#[tokio::test]
async fn websocket_is_refused_locally_for_members_without_it() {
    let up = upstream(Mode::WsEcho).await;
    let r = start(test_config(vec![member(
        "site",
        MemberKind::Compatible,
        false,
        vec![line("a", up.base("/v1"))],
    )]))
    .await;
    let err = tokio_tungstenite::connect_async(ws_request(&r, ROUTER_TOKEN))
        .await
        .unwrap_err();
    let tokio_tungstenite::tungstenite::Error::Http(resp) = err else {
        panic!("expected an HTTP answer, got {err:?}");
    };
    assert_eq!(resp.status(), 426);
    assert_eq!(up.hits(), 0);
    assert_eq!(r.status().await.totals.ws_refused, 1);
    // A wrong token is refused before any of that.
    let err = tokio_tungstenite::connect_async(ws_request(&r, "nope"))
        .await
        .unwrap_err();
    let tokio_tungstenite::tungstenite::Error::Http(resp) = err else {
        panic!("expected an HTTP answer");
    };
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn admin_api_needs_the_admin_token() {
    let up = upstream(Mode::Json(200, OK)).await;
    let r = start(test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![line("a", up.base("/up"))],
    )]))
    .await;
    let health: superba_router::HealthReply = r
        .http
        .get(r.url("/_superba/v1/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health.service, "superba-router");
    for token in [None, Some(ROUTER_TOKEN)] {
        let mut rb = r.http.get(r.url("/_superba/v1/status"));
        if let Some(t) = token {
            rb = rb.bearer_auth(t);
        }
        assert_eq!(rb.send().await.unwrap().status(), 401);
    }
    let st = r.status().await;
    assert_eq!(st.instance_id, health.instance_id);
    assert_eq!(st.base_url, format!("http://{}/backend-api/codex", r.addr));
    assert_eq!(st.members[0].lines[0].base_url, up.base("/up").to_string());
    // Started without files: reload is refused, shutdown works.
    let resp = r
        .http
        .post(r.url("/_superba/v1/reload"))
        .bearer_auth(&r.admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let resp = r
        .http
        .post(r.url("/_superba/v1/shutdown"))
        .bearer_auth(&r.admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn health_probes_open_dead_lines_at_start() {
    let up = upstream(Mode::Json(200, OK)).await;
    let mut cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        false,
        vec![
            line("a", dead_base()),
            line("b", up.base("/up")),
            line("c", resetter().await),
        ],
    )]);
    cfg.tuning.active_probes = true;
    let r = start(cfg).await;
    let mut st = r.status().await;
    for _ in 0..50 {
        if st.members[0].lines[0].state == LineState::Open {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        st = r.status().await;
    }
    assert_eq!(st.members[0].lines[0].state, LineState::Open);
    // Let the other start-up probes settle before judging them.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let st = r.status().await;
    assert_eq!(st.members[0].lines[0].state, LineState::Open);
    assert_eq!(st.members[0].lines[1].state, LineState::Healthy);
    // A peer that accepts and hangs up is reachable: inconclusive, still healthy.
    assert_eq!(st.members[0].lines[2].state, LineState::Healthy);
    assert_eq!(
        st.recent
            .iter()
            .filter(|e| e.kind == RouteEventKind::LineDown)
            .count(),
        1
    );
    // The probe was unauthenticated: the fake upstream recorded nothing.
    assert_eq!(up.hits(), 0);
    assert_eq!(route(&r.post(None, BODY).await), "krill/b");
}
