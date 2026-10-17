//! Test helpers: a fake CDP endpoint (HTTP `/json/list` + WebSocket
//! `Runtime.evaluate`) served by the test process itself, so ownership checks see
//! the test pid as the listener.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

/// Decides the value returned for an evaluated expression.
pub type Responder = Arc<dyn Fn(&str) -> Value + Send + Sync>;

/// A running fake endpoint.
pub struct FakeCdp {
    /// Port.
    pub port: u16,
    /// Every expression evaluated, in order.
    pub evaluated: Arc<Mutex<Vec<String>>>,
}

/// Serve targets with `url`s; `/devtools/page/main` evaluates with `respond`.
pub async fn serve(urls: Vec<&'static str>, respond: Responder) -> FakeCdp {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let evaluated = Arc::new(Mutex::new(Vec::new()));
    let ev = evaluated.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let respond = respond.clone();
            let ev = ev.clone();
            let urls = urls.clone();
            tokio::spawn(async move {
                let mut peek = [0u8; 2048];
                let n = stream.peek(&mut peek).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&peek[..n]).to_ascii_lowercase();
                if head.contains("upgrade: websocket") {
                    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                        return;
                    };
                    while let Some(Ok(msg)) = ws.next().await {
                        let Message::Text(t) = msg else { continue };
                        let req: Value = serde_json::from_str(t.as_str()).unwrap();
                        let expr = req["params"]["expression"]
                            .as_str()
                            .unwrap_or("")
                            .to_owned();
                        ev.lock().unwrap().push(expr.clone());
                        let value = respond(&expr);
                        let resp = if value == json!("__throw__") {
                            json!({"id": req["id"], "result": {"result": {"type": "object"},
                                "exceptionDetails": {"text": "Uncaught", "exception": {"description": "Error: boom\n at x"}}}})
                        } else {
                            json!({"id": req["id"], "result": {"result": {"type": "object", "value": value}}})
                        };
                        let _ = ws.send(Message::text(resp.to_string())).await;
                    }
                    return;
                }
                let mut stream = stream;
                let mut buf = vec![0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let targets: Vec<Value> = urls
                    .iter()
                    .enumerate()
                    .map(|(i, u)| {
                        let id = if i == 0 { "main".to_owned() } else { format!("t{i}") };
                        json!({"id": id, "type": "page", "url": u, "title": "",
                               "webSocketDebuggerUrl": format!("ws://127.0.0.1:{port}/devtools/page/{id}")})
                    })
                    .collect();
                let body = serde_json::to_string(&targets).unwrap();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    FakeCdp { port, evaluated }
}
