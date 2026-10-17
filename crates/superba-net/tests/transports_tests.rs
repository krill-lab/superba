//! Transport probe (`LineProber::probe_transports`) against loopback mock
//! relays: WebSocket 101 vs 426, zstd decoded vs 415 vs ignored, key errors.

mod support;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use pretty_assertions::assert_eq;
use superba_core::{AuthMode, CodexPath, LineProber, Origin, ResolvedEndpoints, TransportSupport};
use support::{KEY, key, prober};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

const RESPONSES: &str = "/backend-api/codex/responses";
const MISSING_MODEL: &str = r#"{"error":{"message":"Missing required parameter: 'model'.","type":"invalid_request_error","param":"model","code":"missing_required_parameter"}}"#;
const BAD_JSON: &str = r#"{"error":{"message":"We could not parse the JSON body of your request.","type":"invalid_request_error","param":null,"code":null}}"#;

/// How the mock relay treats WebSocket handshakes and zstd bodies.
#[derive(Clone, Copy)]
struct Relay {
    websocket: Ws,
    zstd: Zstd,
    status: Option<u16>,
}

#[derive(Clone, Copy)]
enum Ws {
    Upgrade,
    Refuse426,
}

#[derive(Clone, Copy)]
enum Zstd {
    Decode,
    Refuse415,
    Ignore,
}

#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
    /// Bytes the client sent on an upgraded connection.
    after_upgrade: Vec<u8>,
}

/// Decode the raw-block zstd frames the probe sends (`28 b5 2f fd 20 <len>`
/// + a three-byte raw block header); anything else is not zstd.
fn unzstd(body: &[u8]) -> Option<Vec<u8>> {
    let (head, rest) = body.split_at_checked(9)?;
    if head[..5] != [0x28, 0xb5, 0x2f, 0xfd, 0x20] || head[6] & 0x07 != 0x01 {
        return None;
    }
    (rest.len() == usize::from(head[5])).then(|| rest.to_vec())
}

fn reply(relay: Relay, req: &Seen) -> Result<(u16, String), ()> {
    if let Some(s) = relay.status {
        return Ok((s, r#"{"error":{"message":"denied"}}"#.into()));
    }
    if req.method == "GET" {
        return match relay.websocket {
            Ws::Upgrade => Err(()),
            Ws::Refuse426 => Ok((426, r#"{"error":{"message":"Upgrade Required"}}"#.into())),
        };
    }
    let zstd = req.headers.get("content-encoding").map(String::as_str) == Some("zstd");
    let body = match (zstd, relay.zstd) {
        (true, Zstd::Refuse415) => return Ok((415, String::new())),
        (true, Zstd::Decode) => unzstd(&req.body).unwrap_or_default(),
        _ => req.body.clone(),
    };
    Ok(match serde_json::from_slice::<serde_json::Value>(&body) {
        Ok(v) if v["model"].as_str().is_none_or(str::is_empty) => (400, MISSING_MODEL.into()),
        Ok(_) => (200, "{}".into()),
        Err(_) => (400, BAD_JSON.into()),
    })
}

/// A one-request-per-connection HTTP/1.1 mock that can also upgrade.
async fn start(relay: Relay) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            let log = log.clone();
            tokio::spawn(async move {
                let (read, mut write) = socket.into_split();
                let mut read = BufReader::new(read);
                let mut line = String::new();
                if read.read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
                let mut parts = line.split_whitespace();
                let method = parts.next().unwrap_or_default().to_owned();
                let path = parts.next().unwrap_or_default().to_owned();
                let mut headers = BTreeMap::new();
                loop {
                    line.clear();
                    if read.read_line(&mut line).await.unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some((n, v)) = line.split_once(':') {
                        headers.insert(n.to_ascii_lowercase(), v.trim().to_owned());
                    }
                }
                let len = headers
                    .get("content-length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let mut body = vec![0; len];
                let _ = read.read_exact(&mut body).await;
                let mut req = Seen {
                    method,
                    path,
                    headers,
                    body,
                    after_upgrade: Vec::new(),
                };
                match reply(relay, &req) {
                    Ok((status, body)) => {
                        let head = format!(
                            "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = write.write_all(head.as_bytes()).await;
                        let _ = write.write_all(body.as_bytes()).await;
                        log.lock().unwrap().push(req);
                    }
                    Err(()) => {
                        let head = "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
                                    Connection: Upgrade\r\nSec-WebSocket-Accept: mock\r\n\r\n";
                        let _ = write.write_all(head.as_bytes()).await;
                        let mut rest = Vec::new();
                        let _ = tokio::time::timeout(
                            std::time::Duration::from_secs(2),
                            read.read_to_end(&mut rest),
                        )
                        .await;
                        req.after_upgrade = rest;
                        log.lock().unwrap().push(req);
                    }
                }
            });
        }
    });
    (origin, seen)
}

fn endpoints(origin: &str) -> ResolvedEndpoints {
    ResolvedEndpoints::for_line(
        &Origin::parse(origin).unwrap(),
        CodexPath::BackendApiCodex,
        AuthMode::ApiKey,
    )
    .unwrap()
}

/// Requests seen once every connection has finished (the upgraded one waits for
/// the client to close).
async fn settled(seen: &Arc<Mutex<Vec<Seen>>>, n: usize) -> Vec<Seen> {
    for _ in 0..200 {
        if seen.lock().unwrap().len() >= n {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    seen.lock().unwrap().clone()
}

#[tokio::test]
async fn websocket_101_and_decoded_zstd_are_supported() {
    let (origin, seen) = start(Relay {
        websocket: Ws::Upgrade,
        zstd: Zstd::Decode,
        status: None,
    })
    .await;
    let p = prober()
        .probe_transports(&endpoints(&origin), &key())
        .await
        .unwrap();
    assert_eq!(p.websocket.support, TransportSupport::Supported);
    assert_eq!(p.websocket.status, Some(101));
    assert_eq!(p.zstd.support, TransportSupport::Supported, "{:?}", p.zstd);
    assert_eq!(p.zstd.status, Some(400));

    let seen = settled(&seen, 4).await;
    assert_eq!(seen.len(), 4);
    let bearer = format!("Bearer {KEY}");
    for r in &seen {
        assert_eq!(r.path, RESPONSES);
        assert_eq!(r.headers.get("authorization"), Some(&bearer));
    }
    let ws = seen.iter().find(|r| r.method == "GET").unwrap();
    assert_eq!(ws.headers["upgrade"], "websocket");
    assert_eq!(ws.headers["sec-websocket-version"], "13");
    assert_eq!(ws.headers["openai-beta"], "responses_websockets=2026-02-06");
    assert_eq!(ws.headers["sec-websocket-key"].len(), 24);
    // Closed at once: one masked close frame (1000), nothing else.
    assert_eq!(ws.after_upgrade.len(), 8, "{:?}", ws.after_upgrade);
    assert_eq!(ws.after_upgrade[..2], [0x88, 0x82]);
    let posts: Vec<_> = seen.iter().filter(|r| r.method == "POST").collect();
    assert_eq!(posts.len(), 3);
    let zstd: Vec<_> = posts
        .iter()
        .filter(|r| r.headers.get("content-encoding").map(String::as_str) == Some("zstd"))
        .collect();
    assert_eq!(zstd.len(), 1);
    let probe: serde_json::Value = serde_json::from_slice(&unzstd(&zstd[0].body).unwrap()).unwrap();
    assert_eq!(probe["model"], "");
    assert_eq!(zstd[0].headers["content-type"], "application/json");
    // No probe body names a model, has input or allows output: nothing can
    // start a turn. The plain one is the decoded zstd one.
    let plain = posts
        .iter()
        .find(|r| serde_json::from_slice::<serde_json::Value>(&r.body).is_ok())
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&plain.body).unwrap();
    assert_eq!(
        v,
        serde_json::json!({"model": "", "input": [], "max_output_tokens": 0})
    );
    assert_eq!(unzstd(&zstd[0].body).unwrap(), plain.body);
}

#[tokio::test]
async fn websocket_426_and_zstd_415_are_not_supported() {
    let (origin, _) = start(Relay {
        websocket: Ws::Refuse426,
        zstd: Zstd::Refuse415,
        status: None,
    })
    .await;
    let p = prober()
        .probe_transports(&endpoints(&origin), &key())
        .await
        .unwrap();
    assert_eq!(p.websocket.support, TransportSupport::Unsupported);
    assert_eq!(p.websocket.status, Some(426));
    assert_eq!(p.zstd.support, TransportSupport::Unsupported);
    assert_eq!(p.zstd.status, Some(415));
}

#[tokio::test]
async fn a_server_that_ignores_content_encoding_does_not_support_zstd() {
    let (origin, _) = start(Relay {
        websocket: Ws::Refuse426,
        zstd: Zstd::Ignore,
        status: None,
    })
    .await;
    let p = prober()
        .probe_transports(&endpoints(&origin), &key())
        .await
        .unwrap();
    assert_eq!(
        p.zstd.support,
        TransportSupport::Unsupported,
        "{:?}",
        p.zstd
    );
    assert_eq!(p.zstd.status, Some(400));
    assert!(p.zstd.detail.unwrap().contains("not decoded"));
}

#[tokio::test]
async fn key_errors_leave_both_unknown_and_never_show_the_key() {
    for status in [401, 403, 429] {
        let (origin, _) = start(Relay {
            websocket: Ws::Upgrade,
            zstd: Zstd::Decode,
            status: Some(status),
        })
        .await;
        let p = prober()
            .probe_transports(&endpoints(&origin), &key())
            .await
            .unwrap();
        assert_eq!(p.websocket.support, TransportSupport::Unknown, "{status}");
        assert_eq!(p.zstd.support, TransportSupport::Unknown, "{status}");
        assert_eq!(p.websocket.status, Some(status));
        let text = format!("{p:?}");
        assert!(!text.contains(KEY));
    }
}

#[tokio::test]
async fn unreachable_lines_are_unknown_and_foreign_targets_refused() {
    let origin = support::closed_origin().await;
    let p = prober()
        .probe_transports(&endpoints(&origin), &key())
        .await
        .unwrap();
    assert_eq!(p.websocket.support, TransportSupport::Unknown);
    assert_eq!(p.websocket.status, None);
    assert_eq!(p.zstd.support, TransportSupport::Unknown);
    // A key never goes over plain http to a non-loopback host, nor to a base
    // URL outside the endpoints' origin.
    let mut plain = endpoints(&origin);
    plain.openai_base_url = url::Url::parse("http://relay.example/backend-api/codex").unwrap();
    assert!(prober().probe_transports(&plain, &key()).await.is_err());
    let mut foreign = endpoints(&origin);
    foreign.openai_base_url = url::Url::parse("https://relay.example/backend-api/codex").unwrap();
    assert!(prober().probe_transports(&foreign, &key()).await.is_err());
}
