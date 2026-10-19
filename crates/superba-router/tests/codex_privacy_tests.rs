//! Private uploads never reach native Codex HTTP or WebSocket upstreams,
//! independently of request recording and transport encoding.
mod common;

use std::io::Write;
use std::time::Duration;

use common::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use superba_router::{MemberKind, StatsConfig};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

const PATHS: [&str; 2] = ["/v1/responses", "/backend-api/codex/responses"];

async fn router(up: &Upstream, recording: Option<bool>) -> Router {
    let mut cfg = test_config(vec![member(
        "native",
        MemberKind::Compatible,
        true,
        vec![line("a", up.base("/v1"))],
    )]);
    cfg.stats = recording.map(|record| StatsConfig {
        record,
        line: None,
        ..Default::default()
    });
    start(cfg).await
}

fn encode(body: &[u8], encoding: &str) -> Vec<u8> {
    match encoding {
        "gzip" => {
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            e.write_all(body).unwrap();
            e.finish().unwrap()
        }
        "deflate" => {
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
            e.write_all(body).unwrap();
            e.finish().unwrap()
        }
        "br" => {
            let mut out = Vec::new();
            {
                let mut e = brotli::CompressorWriter::new(&mut out, 4096, 3, 22);
                e.write_all(body).unwrap();
            }
            out
        }
        "zstd" => zstd::stream::encode_all(body, 3).unwrap(),
        _ => body.to_vec(),
    }
}

fn request() -> Value {
    json!({"model":"m","input":[{"role":"user","content":"keep dsh_text in history"}],
        "metadata":{"dsh_nested":"keep"}, "store":true,
        "dsh_session_log":{"events":["private"]},"dsh_plugin_packages":["private"],
        "dsh_future_field":"private"})
}

fn assert_public(actual: &Value, original: &Value) {
    assert!(
        actual
            .as_object()
            .unwrap()
            .keys()
            .all(|k| !k.starts_with("dsh_"))
    );
    let mut expected = original.clone();
    expected
        .as_object_mut()
        .unwrap()
        .retain(|k, _| !k.starts_with("dsh_"));
    assert_eq!(
        actual, &expected,
        "public fields and nested keys are preserved"
    );
}

async fn http_private(recording: Option<bool>) {
    let up = upstream(Mode::Json(200, r#"{"id":"r","output":[]}"#)).await;
    let r = router(&up, recording).await;
    let body = request();
    for path in PATHS {
        for suffix in ["", "/compact"] {
            for encoding in ["identity", "gzip", "deflate", "br", "zstd"] {
                let response = r
                    .http
                    .post(r.url(&format!("{path}{suffix}")))
                    .bearer_auth(ROUTER_TOKEN)
                    .header("content-encoding", encoding)
                    .body(encode(&serde_json::to_vec(&body).unwrap(), encoding))
                    .send()
                    .await
                    .unwrap();
                assert_eq!(response.status(), 200, "{path}{suffix} {encoding}");
                response.bytes().await.unwrap();
                let seen = up.seen();
                let sent = seen.last().unwrap();
                assert!(!sent.headers.contains_key("content-encoding"));
                assert_eq!(sent.headers["content-length"], sent.body.len().to_string());
                assert_public(&serde_json::from_slice(&sent.body).unwrap(), &body);
            }
        }
    }
}

#[tokio::test]
async fn http_private_uploads_removed_with_recording() {
    http_private(Some(true)).await;
}
#[tokio::test]
async fn http_private_uploads_removed_with_recording_disabled() {
    http_private(Some(false)).await;
}
#[tokio::test]
async fn http_private_uploads_removed_without_stats() {
    http_private(None).await;
}

async fn ws_private(recording: Option<bool>) {
    let up = upstream(Mode::WsEcho).await;
    let r = router(&up, recording).await;
    for path in PATHS {
        let mut req = format!("ws://{}{path}", r.addr)
            .into_client_request()
            .unwrap();
        req.headers_mut().insert(
            "authorization",
            format!("Bearer {ROUTER_TOKEN}").parse().unwrap(),
        );
        req.headers_mut().insert(
            "sec-websocket-extensions",
            "permessage-deflate".parse().unwrap(),
        );
        let (mut ws, response) = tokio_tungstenite::connect_async(req).await.unwrap();
        assert!(!response.headers().contains_key("sec-websocket-extensions"));
        assert!(
            !up.seen()
                .last()
                .unwrap()
                .headers
                .contains_key("sec-websocket-extensions")
        );
        for continuation in [false, true] {
            let mut body = request();
            body["type"] = json!("response.create");
            if continuation {
                body["previous_response_id"] = json!("r");
            }
            ws.send(Message::Text(body.to_string().into()))
                .await
                .unwrap();
            let reply = tokio::time::timeout(Duration::from_secs(5), ws.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let text = reply.into_text().unwrap();
            assert_public(
                &serde_json::from_str(text.strip_prefix("echo:").unwrap()).unwrap(),
                &body,
            );
        }
        ws.close(None).await.unwrap();
    }
}

#[tokio::test]
async fn ws_private_uploads_removed_with_recording() {
    ws_private(Some(true)).await;
}
#[tokio::test]
async fn ws_private_uploads_removed_with_recording_disabled() {
    ws_private(Some(false)).await;
}
#[tokio::test]
async fn ws_private_uploads_removed_without_stats() {
    ws_private(None).await;
}

#[tokio::test]
async fn codex_unreadable_bodies_cannot_bypass_privacy_shaping() {
    let up = upstream(Mode::Json(200, "{}")).await;
    let r = router(&up, None).await;
    for path in PATHS {
        for (encoding, body, status) in [
            ("unknown", "{}", 415),
            ("gzip", "broken", 400),
            ("identity", "{broken", 400),
        ] {
            let response = r
                .http
                .post(r.url(path))
                .bearer_auth(ROUTER_TOKEN)
                .header("content-encoding", encoding)
                .body(body)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), status);
        }
    }
    assert!(up.seen().is_empty());
}
