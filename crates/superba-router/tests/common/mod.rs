//! Fake upstreams (loopback hyper servers) and a router fixture.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http::{HeaderMap, Request, Response, StatusCode};
use http_body::{Body, Frame};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use superba_core::Secret;
use superba_router::{
    LineConfig, MemberConfig, MemberKind, RouterConfig, RouterSecrets, RouterServer, ShutdownHandle,
};
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::sync::{Notify, mpsc};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;
use url::Url;

pub mod anthropic;
pub mod ciphers;
pub mod claude;
pub mod openai;
pub mod responses;

pub const ROUTER_TOKEN: &str = "sbr_test_router_token_0123456789abcdef";

type FakeBody = BoxBody<Bytes, std::io::Error>;

/// One request an upstream received (health probes, which carry no
/// `Authorization`, are answered 401 and not recorded).
#[derive(Clone, Debug)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub headers: HeaderMap,
    pub body: Bytes,
}

/// How an upstream answers.
#[derive(Clone)]
pub enum Mode {
    /// Status + JSON body.
    Json(u16, &'static str),
    /// Status + JSON body + one extra header.
    JsonHeader(u16, &'static str, &'static str, &'static str),
    /// Status + JSON with several response headers.
    JsonHeaders(u16, &'static str, Vec<(&'static str, &'static str)>),
    /// SSE: `first`, then (after `gate` is notified, if any) `rest`; with
    /// `break_mid` the stream is broken after `first` instead.
    Sse {
        first: &'static str,
        rest: &'static str,
        gate: Option<Arc<Notify>>,
        break_mid: bool,
    },
    /// WebSocket upgrade, echoing `echo:<text>`.
    WsEcho,
    /// SSE in these chunks (a short pause between them).
    Chunks(Arc<Vec<Bytes>>),
    /// WebSocket upgrade answering each text frame with the frames the script
    /// returns; `extensions` is put on the 101 when the client offered any.
    WsScript(WsScript),
    /// WebSocket upgrade answering the first text frame with these raw bytes
    /// (hand-made frames, e.g. a header announcing a huge payload).
    WsRaw(Arc<Vec<u8>>),
    /// Status + JSON body chosen from the request headers and body.
    Reply(Reply),
    /// Path-aware JSON response for layered model catalogs.
    PathReply(PathReply),
    /// 200 + JSON body after a delay (a slow answer holding both sockets).
    Slow(Duration, &'static str),
    /// A WebSocket upgrade answered like [`Mode::WsScript`], any other request
    /// like [`Mode::PathReply`] (a member with a catalog and WebSocket).
    WsOrPath(PathReply, WsScript),
}

/// Answer of [`Mode::Reply`].
pub type Reply = Arc<dyn Fn(&HeaderMap, &[u8]) -> (u16, &'static str) + Send + Sync>;
pub type PathReply = Arc<dyn Fn(&str, &HeaderMap, &[u8]) -> (u16, serde_json::Value) + Send + Sync>;

/// Frames to answer a client text frame with.
pub type WsScript = Arc<dyn Fn(&str) -> Vec<String> + Send + Sync>;

#[derive(Clone)]
pub struct Upstream {
    pub addr: SocketAddr,
    pub seen: Arc<Mutex<Vec<Seen>>>,
    pub mode: Arc<Mutex<Mode>>,
    /// Text frames received over WebSocket.
    pub frames: Arc<Mutex<Vec<String>>>,
    /// Close codes received over WebSocket (`None`: a close frame without one).
    pub closes: Arc<Mutex<Vec<Option<u16>>>>,
    pub probes: Arc<std::sync::atomic::AtomicUsize>,
}

impl Upstream {
    pub fn base(&self, path: &str) -> Url {
        Url::parse(&format!("http://{}{path}", self.addr)).unwrap()
    }
    pub fn set(&self, mode: Mode) {
        *self.mode.lock().unwrap() = mode;
    }
    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
    pub fn hits(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
    pub fn frames(&self) -> Vec<String> {
        self.frames.lock().unwrap().clone()
    }
    pub fn closes(&self) -> Vec<Option<u16>> {
        self.closes.lock().unwrap().clone()
    }
}

pub async fn upstream(mode: Mode) -> Upstream {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let up = Upstream {
        addr: listener.local_addr().unwrap(),
        seen: Arc::default(),
        mode: Arc::new(Mutex::new(mode)),
        frames: Arc::default(),
        closes: Arc::default(),
        probes: Arc::default(),
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
                    .with_upgrades()
                    .await;
            });
        }
    });
    up
}

fn full(status: u16, body: &'static str) -> Response<FakeBody> {
    let mut r = Response::new(
        Full::new(Bytes::from_static(body.as_bytes()))
            .map_err(|n| match n {})
            .boxed(),
    );
    *r.status_mut() = StatusCode::from_u16(status).unwrap();
    r.headers_mut()
        .insert("content-type", "application/json".parse().unwrap());
    r
}

pub struct ChanBody(pub mpsc::Receiver<Result<Bytes, std::io::Error>>);

impl Body for ChanBody {
    type Data = Bytes;
    type Error = std::io::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, std::io::Error>>> {
        self.0.poll_recv(cx).map(|o| o.map(|r| r.map(Frame::data)))
    }
}

async fn handle(state: Upstream, mut req: Request<Incoming>) -> Response<FakeBody> {
    if !req.headers().contains_key("authorization")
        && !req.headers().contains_key("x-api-key")
        && !req.headers().contains_key("x-goog-api-key")
    {
        state
            .probes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return full(401, r#"{"error":{"message":"probe"}}"#);
    }
    let mode = state.mode.lock().unwrap().clone();
    let is_ws = req
        .headers()
        .get("upgrade")
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"));
    let mode = match mode {
        Mode::WsOrPath(_, script) if is_ws => Mode::WsScript(script),
        Mode::WsOrPath(reply, _) => Mode::PathReply(reply),
        mode => mode,
    };
    let on_upgrade = is_ws.then(|| hyper::upgrade::on(&mut req));
    let (parts, body) = req.into_parts();
    let body = body
        .collect()
        .await
        .map(|c| c.to_bytes())
        .unwrap_or_default();
    state.seen.lock().unwrap().push(Seen {
        method: parts.method.to_string(),
        path: parts
            .uri
            .path_and_query()
            .map(|p| p.to_string())
            .unwrap_or_default(),
        headers: parts.headers.clone(),
        body: body.clone(),
    });
    match mode {
        Mode::Json(status, body) => full(status, body),
        Mode::JsonHeaders(status, body, headers) => {
            let mut response = full(status, body);
            for (name, value) in headers {
                response.headers_mut().insert(name, value.parse().unwrap());
            }
            response
        }
        Mode::Slow(delay, body) => {
            tokio::time::sleep(delay).await;
            full(200, body)
        }
        Mode::Reply(reply) => {
            let (status, json) = reply(&parts.headers, &body);
            full(status, json)
        }
        Mode::PathReply(reply) => {
            let (status, value) = reply(parts.uri.path(), &parts.headers, &body);
            let mut response = Response::new(
                Full::new(Bytes::from(serde_json::to_vec(&value).unwrap()))
                    .map_err(|never| match never {})
                    .boxed(),
            );
            *response.status_mut() = StatusCode::from_u16(status).unwrap();
            response
                .headers_mut()
                .insert("content-type", "application/json".parse().unwrap());
            response
        }
        Mode::WsOrPath(..) => unreachable!("chosen above"),
        Mode::JsonHeader(status, body, name, value) => {
            let mut r = full(status, body);
            r.headers_mut()
                .insert(http::HeaderName::from_static(name), value.parse().unwrap());
            r
        }
        Mode::Sse {
            first,
            rest,
            gate,
            break_mid,
        } => {
            let (tx, rx) = mpsc::channel(4);
            tokio::spawn(async move {
                let _ = tx.send(Ok(Bytes::from_static(first.as_bytes()))).await;
                if break_mid {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    let _ = tx.send(Err(std::io::Error::other("upstream broke"))).await;
                    return;
                }
                if let Some(g) = gate {
                    g.notified().await;
                }
                let _ = tx.send(Ok(Bytes::from_static(rest.as_bytes()))).await;
            });
            let mut r = Response::new(ChanBody(rx).boxed());
            r.headers_mut()
                .insert("content-type", "text/event-stream".parse().unwrap());
            r
        }
        Mode::Chunks(chunks) => {
            let (tx, rx) = mpsc::channel(4);
            tokio::spawn(async move {
                for c in chunks.iter() {
                    if tx.send(Ok(c.clone())).await.is_err() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            });
            let mut r = Response::new(ChanBody(rx).boxed());
            r.headers_mut().insert(
                "content-type",
                "text/event-stream; charset=utf-8".parse().unwrap(),
            );
            r
        }
        Mode::WsScript(script) => {
            let Some(on_upgrade) = on_upgrade else {
                return full(400, r#"{"error":{"message":"expected websocket"}}"#);
            };
            let key = parts
                .headers
                .get("sec-websocket-key")
                .map(|k| derive_accept_key(k.as_bytes()))
                .unwrap_or_default();
            let offered = parts.headers.contains_key("sec-websocket-extensions");
            let (frames, closes) = (state.frames.clone(), state.closes.clone());
            tokio::spawn(async move {
                let Ok(io) = on_upgrade.await else { return };
                let mut ws =
                    WebSocketStream::from_raw_socket(TokioIo::new(io), Role::Server, None).await;
                while let Some(Ok(msg)) = ws.next().await {
                    match msg {
                        Message::Text(t) => {
                            frames.lock().unwrap().push(t.as_str().to_owned());
                            for reply in script(t.as_str()) {
                                if ws.send(Message::text(reply)).await.is_err() {
                                    return;
                                }
                            }
                        }
                        Message::Close(f) => {
                            closes.lock().unwrap().push(f.map(|f| f.code.into()));
                            return;
                        }
                        _ => {}
                    }
                }
            });
            let mut r = Response::new(Full::new(Bytes::new()).map_err(|n| match n {}).boxed());
            *r.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
            let h = r.headers_mut();
            h.insert("upgrade", "websocket".parse().unwrap());
            h.insert("connection", "Upgrade".parse().unwrap());
            h.insert("sec-websocket-accept", key.parse().unwrap());
            if offered {
                // A real server would agree to deflate; the router must never
                // offer it while it relays frames.
                h.insert(
                    "sec-websocket-extensions",
                    "permessage-deflate".parse().unwrap(),
                );
            }
            r
        }
        Mode::WsRaw(raw) => {
            let Some(on_upgrade) = on_upgrade else {
                return full(400, r#"{"error":{"message":"expected websocket"}}"#);
            };
            let key = parts
                .headers
                .get("sec-websocket-key")
                .map(|k| derive_accept_key(k.as_bytes()))
                .unwrap_or_default();
            let (frames, closes) = (state.frames.clone(), state.closes.clone());
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let Ok(io) = on_upgrade.await else { return };
                let mut ws =
                    WebSocketStream::from_raw_socket(TokioIo::new(io), Role::Server, None).await;
                let mut answered = false;
                while let Some(Ok(msg)) = ws.next().await {
                    match msg {
                        Message::Text(t) => {
                            frames.lock().unwrap().push(t.as_str().to_owned());
                            if !answered {
                                answered = true;
                                let io = ws.get_mut();
                                if io.write_all(&raw).await.is_err() || io.flush().await.is_err() {
                                    return;
                                }
                            }
                        }
                        Message::Close(f) => {
                            closes.lock().unwrap().push(f.map(|f| f.code.into()));
                            return;
                        }
                        _ => {}
                    }
                }
            });
            let mut r = Response::new(Full::new(Bytes::new()).map_err(|n| match n {}).boxed());
            *r.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
            let h = r.headers_mut();
            h.insert("upgrade", "websocket".parse().unwrap());
            h.insert("connection", "Upgrade".parse().unwrap());
            h.insert("sec-websocket-accept", key.parse().unwrap());
            r
        }
        Mode::WsEcho => {
            let Some(on_upgrade) = on_upgrade else {
                return full(400, r#"{"error":{"message":"expected websocket"}}"#);
            };
            let key = parts
                .headers
                .get("sec-websocket-key")
                .map(|k| derive_accept_key(k.as_bytes()))
                .unwrap_or_default();
            tokio::spawn(async move {
                let Ok(io) = on_upgrade.await else { return };
                let mut ws =
                    WebSocketStream::from_raw_socket(TokioIo::new(io), Role::Server, None).await;
                while let Some(Ok(msg)) = ws.next().await {
                    match msg {
                        Message::Text(t) => {
                            let reply = format!("echo:{}", t.as_str());
                            if ws.send(Message::text(reply)).await.is_err() {
                                return;
                            }
                        }
                        Message::Close(_) => return,
                        _ => {}
                    }
                }
            });
            let mut r = Response::new(Full::new(Bytes::new()).map_err(|n| match n {}).boxed());
            *r.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
            let h = r.headers_mut();
            h.insert("upgrade", "websocket".parse().unwrap());
            h.insert("connection", "Upgrade".parse().unwrap());
            h.insert("sec-websocket-accept", key.parse().unwrap());
            r
        }
    }
}

/// A reserved port nothing listens on (refused or a connect timeout).
pub fn dead_port() -> u16 {
    // Reserve without listening so another parallel test cannot reuse it.
    // Linux refuses immediately; macOS and Windows may wait for SYN retries.
    static RESERVED: std::sync::OnceLock<std::sync::Mutex<Vec<tokio::net::TcpSocket>>> =
        std::sync::OnceLock::new();
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let port = socket.local_addr().unwrap().port();
    RESERVED
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .push(socket);
    port
}

pub fn dead_base() -> Url {
    Url::parse(&format!("http://127.0.0.1:{}/up", dead_port())).unwrap()
}

/// A server that reads the request and closes the connection without answering.
pub async fn resetter() -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf).await;
            drop(s);
        }
    });
    Url::parse(&format!("http://{addr}/up")).unwrap()
}

pub fn line(id: &str, base_url: Url) -> LineConfig {
    LineConfig {
        id: id.into(),
        name: id.to_uppercase(),
        base_url,
        probe_url: None,
        zstd: None,
        websocket: None,
    }
}

pub fn member(id: &str, kind: MemberKind, websocket: bool, lines: Vec<LineConfig>) -> MemberConfig {
    MemberConfig {
        id: id.into(),
        name: id.to_uppercase(),
        kind,
        lines,
        websocket,
        model_map: BTreeMap::new(),
        compaction: Default::default(),
    }
}

pub fn key_of(member: &str) -> String {
    format!("nb_key_of_{member}_0123456789")
}

pub fn test_config(members: Vec<MemberConfig>) -> RouterConfig {
    let mut cfg = RouterConfig::new(0, members);
    cfg.use_system_proxy = false;
    cfg.tuning.connect_timeout_ms = 1_000;
    cfg.tuning.active_probes = false;
    cfg
}

pub fn secrets_for(cfg: &RouterConfig) -> RouterSecrets {
    RouterSecrets {
        router_token: Secret::new(ROUTER_TOKEN),
        member_keys: cfg
            .members
            .iter()
            .map(|m| (m.id.clone(), Secret::new(key_of(&m.id))))
            .collect(),
        official_tokens: Default::default(),
    }
}

pub struct Router {
    pub addr: SocketAddr,
    pub admin_token: String,
    pub handle: ShutdownHandle,
    pub http: reqwest::Client,
}

impl Router {
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// POST `/backend-api/codex/responses` with the router token and a session.
    pub async fn post(&self, session: Option<&str>, body: &str) -> reqwest::Response {
        let mut rb = self
            .http
            .post(self.url("/backend-api/codex/responses"))
            .bearer_auth(ROUTER_TOKEN)
            .header("content-type", "application/json")
            .body(body.to_owned());
        if let Some(s) = session {
            rb = rb.header("session-id", s);
        }
        rb.send().await.unwrap()
    }

    pub async fn status(&self) -> superba_router::RouterStatus {
        self.http
            .get(self.url("/_superba/v1/status"))
            .bearer_auth(&self.admin_token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
}

pub async fn start(cfg: RouterConfig) -> Router {
    let secrets = secrets_for(&cfg);
    let server = RouterServer::bind(cfg, secrets).await.unwrap();
    let router = Router {
        addr: server.local_addr(),
        admin_token: server.admin_token().expose().to_owned(),
        handle: server.shutdown_handle(),
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
    };
    tokio::spawn(server.serve());
    router
}

pub fn route(resp: &reqwest::Response) -> String {
    resp.headers()
        .get(superba_router::ROUTE_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}
