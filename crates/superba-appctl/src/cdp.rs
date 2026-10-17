//! Minimal Chrome DevTools Protocol client for the desktop app's main renderer.
//!
//! Scope (plan 12.1): loopback only, `GET /json/list` + `/json/version`, one
//! WebSocket per evaluation, `Runtime.evaluate` with `awaitPromise` and
//! `returnByValue`. No domains are enabled and nothing is injected persistently;
//! the connection is closed right after the call.
//!
//! The WebSocket client sends no `Origin` header, which Chromium accepts even when
//! `--remote-allow-origins` is not given (launches with `--remote-debugging-port=0`
//! cannot name the port in advance).

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use superba_core::codex::is_main_renderer_url;
use superba_core::{Error, Result};
use tokio_tungstenite::tungstenite::Message;
use url::Url;

/// Default timeout for one CDP HTTP request or evaluation.
pub const DEFAULT_CDP_TIMEOUT: Duration = Duration::from_secs(5);

/// One entry of `/json/list`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct CdpTarget {
    /// Target id.
    #[serde(default)]
    pub id: String,
    /// `page`, `service_worker`, `iframe`, ...
    #[serde(rename = "type", default)]
    pub kind: String,
    /// Page URL.
    #[serde(default)]
    pub url: String,
    /// Title.
    #[serde(default)]
    pub title: String,
    /// WebSocket debugger URL (absent when another client is attached exclusively).
    #[serde(rename = "webSocketDebuggerUrl", default)]
    pub ws_url: Option<String>,
}

/// True if `ws_url` is a `ws://` URL on a loopback host with exactly `port`.
pub fn validate_ws_url(ws_url: &str, port: u16) -> bool {
    let Ok(url) = Url::parse(ws_url) else {
        return false;
    };
    if url.scheme() != "ws" || url.port() != Some(port) {
        return false;
    }
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

/// Pick the main renderer page (`app://-/index.html`, excluding avatar-overlay and
/// quick-chat-prewarm targets) whose WebSocket URL is on loopback and `port`.
/// An exact `app://-/index.html` URL wins over variants with a query or hash.
pub fn pick_main_target(targets: &[CdpTarget], port: u16) -> Option<&CdpTarget> {
    let usable = |t: &&CdpTarget| {
        t.kind == "page"
            && is_main_renderer_url(&t.url)
            && t.ws_url
                .as_deref()
                .is_some_and(|w| validate_ws_url(w, port))
    };
    targets
        .iter()
        .filter(usable)
        .find(|t| t.url == superba_core::codex::RENDERER_URL_PREFIX)
        .or_else(|| targets.iter().find(usable))
}

fn cdp_err(message: impl Into<String>) -> Error {
    Error::Cdp {
        message: message.into(),
    }
}

fn http_client(timeout: Duration) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(timeout)
        .connect_timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| cdp_err(format!("http client: {e}")))
}

/// `GET http://127.0.0.1:<port><path>` and parse JSON. Loopback only by construction.
pub async fn get_json(port: u16, path: &str, timeout: Duration) -> Result<Value> {
    let url = format!("http://127.0.0.1:{port}{path}");
    let resp = http_client(timeout)?
        .get(&url)
        .send()
        .await
        .map_err(|e| cdp_err(format!("GET {url}: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(cdp_err(format!("GET {url}: HTTP {}", status.as_u16())));
    }
    resp.json::<Value>()
        .await
        .map_err(|e| cdp_err(format!("GET {url}: invalid JSON: {e}")))
}

/// `/json/list`.
pub async fn list_targets(port: u16, timeout: Duration) -> Result<Vec<CdpTarget>> {
    let v = get_json(port, "/json/list", timeout).await?;
    serde_json::from_value(v).map_err(|e| cdp_err(format!("/json/list: {e}")))
}

/// `/json/version` (`Browser`, `User-Agent`, `webSocketDebuggerUrl`, ...).
pub async fn browser_version(port: u16, timeout: Duration) -> Result<Value> {
    get_json(port, "/json/version", timeout).await
}

/// Build the `Runtime.evaluate` request for `expression` with message id `id`.
pub fn evaluate_request(id: u64, expression: &str) -> Value {
    json!({
        "id": id,
        "method": "Runtime.evaluate",
        "params": {
            "expression": expression,
            "awaitPromise": true,
            "returnByValue": true,
            "silent": true,
        }
    })
}

/// Interpret the CDP response to [`evaluate_request`]: the returned value, or an
/// error for protocol errors and JavaScript exceptions.
pub fn evaluate_response(msg: &Value) -> Result<Value> {
    if let Some(err) = msg.get("error") {
        let text = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown protocol error");
        return Err(cdp_err(format!("Runtime.evaluate: {text}")));
    }
    let result = msg
        .get("result")
        .ok_or_else(|| cdp_err("Runtime.evaluate: response without result"))?;
    if let Some(exc) = result.get("exceptionDetails") {
        let text = exc
            .pointer("/exception/description")
            .and_then(Value::as_str)
            .or_else(|| exc.get("text").and_then(Value::as_str))
            .unwrap_or("exception");
        let first_line = text.lines().next().unwrap_or(text);
        return Err(cdp_err(format!("JavaScript exception: {first_line}")));
    }
    Ok(result
        .pointer("/result/value")
        .cloned()
        .unwrap_or(Value::Null))
}

/// Evaluate `expression` in the page behind `ws_url` and return its value
/// (`returnByValue`). The socket is closed afterwards.
pub async fn evaluate(ws_url: &str, expression: &str, timeout: Duration) -> Result<Value> {
    let fut = async {
        let (mut ws, _) = tokio_tungstenite::connect_async(ws_url)
            .await
            .map_err(|e| cdp_err(format!("connect {ws_url}: {e}")))?;
        let request = evaluate_request(1, expression);
        ws.send(Message::text(request.to_string()))
            .await
            .map_err(|e| cdp_err(format!("send: {e}")))?;
        let outcome = loop {
            let Some(msg) = ws.next().await else {
                break Err(cdp_err("connection closed before the response"));
            };
            let msg = msg.map_err(|e| cdp_err(format!("receive: {e}")))?;
            let text = match msg {
                Message::Text(t) => t.to_string(),
                Message::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
                Message::Close(_) => break Err(cdp_err("connection closed by the browser")),
                _ => continue,
            };
            let Ok(v) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if v.get("id").and_then(Value::as_u64) == Some(1) {
                break evaluate_response(&v);
            }
        };
        let _ = ws.close(None).await;
        outcome
    };
    match tokio::time::timeout(timeout, fut).await {
        Ok(r) => r,
        Err(_) => Err(Error::Timeout {
            what: "CDP Runtime.evaluate".into(),
        }),
    }
}

/// Find the main renderer target on `port` and evaluate `expression` there.
/// Returns the target URL with the value.
pub async fn evaluate_in_main(
    port: u16,
    expression: &str,
    timeout: Duration,
) -> Result<(String, Value)> {
    let targets = list_targets(port, timeout).await?;
    let target = pick_main_target(&targets, port)
        .ok_or_else(|| cdp_err("no app://-/index.html page target"))?;
    let ws = target
        .ws_url
        .as_deref()
        .ok_or_else(|| cdp_err("main target has no WebSocket URL"))?;
    let value = evaluate(ws, expression, timeout).await?;
    Ok((target.url.clone(), value))
}

/// A free loopback TCP port chosen by the OS (there is a small race until the app
/// binds it; callers verify the listener afterwards).
pub fn free_loopback_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(|e| Error::Process {
        message: format!("cannot reserve a loopback port: {e}"),
    })?;
    listener
        .local_addr()
        .map(|a| a.port())
        .map_err(|e| Error::Process {
            message: format!("cannot read the reserved port: {e}"),
        })
}

/// Parse a Chromium `DevToolsActivePort` file (`<port>\n<browser ws path>`).
pub fn parse_devtools_active_port(text: &str) -> Option<u16> {
    text.lines()
        .next()
        .and_then(|l| l.trim().parse::<u16>().ok())
        .filter(|p| *p != 0)
}
