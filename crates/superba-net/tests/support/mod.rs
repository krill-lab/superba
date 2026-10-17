//! Loopback HTTP fixtures shared by the integration test binaries.

// Each binary uses a different subset of these fixtures.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use superba_core::{
    AuthMode, CodexPath, Line, LineCaps, LineId, LineSource, LocalizedText, ManualClock, Origin,
    ResolvedEndpoints, Secret, Site, SiteId, Timestamp,
};
use superba_net::{HttpLineProber, ProbeConfig, new_compatible_site};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};
use url::Url;

pub const KEY: &str = "sk-superba-test-0123456789abcdef";
pub const MODELS: &str = "/v1/models";
pub const WHOAMI: &str = "/v1/user-auth-credential/whoami";
pub const WHAM: &str = "/backend-api/wham/accounts/check";
pub const BACKEND_MODELS: &str = "/backend-api/codex/models";
pub const CODEX_V1_MODELS: &str = "/codex/v1/models";
pub const BILLING_SUBSCRIPTION: &str = "/v1/dashboard/billing/subscription";
pub const BILLING_SUBSCRIPTION_NO_V1: &str = "/dashboard/billing/subscription";
pub const BILLING_USAGE: &str = "/v1/dashboard/billing/usage";
pub const BILLING_USAGE_NO_V1: &str = "/dashboard/billing/usage";
pub const USAGE_COMPLETIONS: &str = "/v1/organization/usage/completions";
pub const COSTS: &str = "/v1/organization/costs";
pub const RESPONSES: &str = "/backend-api/codex/responses";
pub const COMPACT: &str = "/backend-api/codex/responses/compact";
pub const COMPLETED_SSE: &str =
    "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{}}\n\n";
pub const CHECKED_AT: Timestamp = Timestamp::from_millis(1_793_491_200_000);

#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
    /// Allows tests to verify that warm-up and samples share a TCP connection.
    pub peer_addr: SocketAddr,
    /// When the request was fully read (lets tests prove overlap without
    /// wall-clock thresholds).
    pub received_at: Instant,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

#[derive(Clone, Debug)]
pub struct MockResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub delay: Option<Duration>,
    /// Announce one byte more than the body and keep the connection open after
    /// writing it (a stream that never finishes).
    pub hold_open: bool,
}

impl MockResponse {
    pub fn empty(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
            delay: None,
            hold_open: false,
        }
    }

    pub fn held_open(mut self) -> Self {
        self.hold_open = true;
        self
    }

    pub fn json(status: u16, value: Value) -> Self {
        Self {
            body: serde_json::to_vec(&value).unwrap(),
            ..Self::empty(status)
        }
        .with_header("Content-Type", "application/json")
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        let name = name.into();
        self.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(&name));
        self.headers.push((name, value.into()));
        self
    }

    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }

    pub fn sse(body: impl AsRef<[u8]>) -> Self {
        Self {
            body: body.as_ref().to_vec(),
            ..Self::empty(200)
        }
        .with_header("Content-Type", "text/event-stream")
    }
}

pub struct MockServer {
    address: SocketAddr,
    pub requests: Arc<Mutex<Vec<RecordedRequest>>>,
    task: JoinHandle<()>,
}

impl MockServer {
    pub async fn start<F>(handler: F) -> Self
    where
        F: Fn(&RecordedRequest) -> MockResponse + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&requests);
        let handler = Arc::new(handler);
        let task = tokio::spawn(async move {
            // Dropping this set also aborts connections waiting on a delayed reply.
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (socket, peer_addr) = accepted.unwrap();
                        let log = Arc::clone(&log);
                        let handler = Arc::clone(&handler);
                        connections.spawn(async move {
                            // Timeouts can close a client while the server is writing.
                            let _ = serve(socket, peer_addr, log, handler).await;
                        });
                    }
                    finished = connections.join_next(), if !connections.is_empty() => {
                        finished.unwrap().expect("mock connection task panicked");
                    }
                }
            }
        });
        Self {
            address,
            requests,
            task,
        }
    }

    pub fn origin(&self) -> String {
        format!("http://{}", self.address)
    }

    pub fn count(&self, path: &str) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.path == path)
            .count()
    }

    pub fn recorded(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve<F>(
    socket: TcpStream,
    peer_addr: SocketAddr,
    log: Arc<Mutex<Vec<RecordedRequest>>>,
    handler: Arc<F>,
) -> io::Result<()>
where
    F: Fn(&RecordedRequest) -> MockResponse + Send + Sync,
{
    let (read, mut write) = socket.into_split();
    let mut read = BufReader::new(read);
    loop {
        let mut line = String::new();
        if read.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let mut parts = line.split_whitespace();
        let method = parts.next().ok_or_else(bad_request)?.to_owned();
        let target = parts.next().ok_or_else(bad_request)?;
        let (path, query) = match target.split_once('?') {
            Some((path, query)) => (path.to_owned(), Some(query.to_owned())),
            None => (target.to_owned(), None),
        };
        let mut headers = BTreeMap::new();
        loop {
            line.clear();
            if read.read_line(&mut line).await? == 0 {
                return Err(bad_request());
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
            let (name, value) = line.split_once(':').ok_or_else(bad_request)?;
            headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
        }
        let length = headers
            .get("content-length")
            .map(|s| s.parse::<usize>())
            .transpose()
            .map_err(|_| bad_request())?
            .unwrap_or(0);
        let mut body = vec![0; length];
        read.read_exact(&mut body).await?;
        let close = headers.get("connection").is_some_and(|value| {
            value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("close"))
        });
        let request = RecordedRequest {
            method,
            path,
            query,
            headers,
            body,
            peer_addr,
            received_at: Instant::now(),
        };
        log.lock().unwrap().push(request.clone());
        let response = handler(&request);
        if let Some(delay) = response.delay {
            tokio::time::sleep(delay).await;
        }
        let announced = response.body.len() + usize::from(response.hold_open);
        let mut head = format!(
            "HTTP/1.1 {} Mock\r\nContent-Length: {announced}\r\n",
            response.status
        );
        for (name, value) in response.headers {
            // Framing and connection lifetime belong to the server.
            if !name.eq_ignore_ascii_case("content-length")
                && !name.eq_ignore_ascii_case("connection")
            {
                head.push_str(&format!("{name}: {value}\r\n"));
            }
        }
        if close {
            head.push_str("Connection: close\r\n");
        }
        head.push_str("\r\n");
        let mut bytes = head.into_bytes();
        bytes.extend_from_slice(&response.body);
        write.write_all(&bytes).await?;
        if response.hold_open {
            write.flush().await?;
            // Never finish the body; the client must stop on its own.
            tokio::time::sleep(Duration::from_secs(30)).await;
            return Ok(());
        }
        if close {
            return Ok(());
        }
    }
}

fn bad_request() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "malformed mock HTTP request")
}

pub fn relay_401() -> MockResponse {
    MockResponse {
        body: br#"{"type":"error","error":{"type":"unauthorized_error","message":"missing Authorization"}}"#.to_vec(),
        ..MockResponse::empty(401)
    }
    .with_header("Content-Type", "application/json")
    .with_header("Access-Control-Allow-Origin", "*")
}

pub fn edge_block() -> MockResponse {
    MockResponse::empty(200).with_header("l-block", "Filter|Site-1")
}

pub fn whoami_body() -> Value {
    json!({
        "email": "a@example.com",
        "chatgpt_user_id": "user-1",
        "chatgpt_account_id": "acc",
        "chatgpt_plan_type": "pro",
        "chatgpt_account_is_fedramp": false,
    })
}

pub fn healthy_authenticated(request: &RecordedRequest) -> MockResponse {
    match request.path.as_str() {
        WHOAMI => MockResponse::json(200, whoami_body()),
        WHAM => MockResponse::json(200, json!({"accounts": [{"id": "acc"}]})),
        BACKEND_MODELS => MockResponse::json(200, json!({"models": [{"slug": "gpt-x"}]})),
        CODEX_V1_MODELS => {
            MockResponse::json(200, json!({"object": "list", "data": [{"id": "gpt-x"}]}))
        }
        RESPONSES => MockResponse::sse(COMPLETED_SSE),
        COMPACT => MockResponse::json(200, json!({"output": []})),
        _ => MockResponse::empty(404),
    }
}

pub fn probe_config() -> ProbeConfig {
    ProbeConfig {
        request_timeout: Duration::from_secs(1),
        latency_timeout: Duration::from_millis(500),
        connect_timeout: Duration::from_millis(500),
        responses_timeout: Duration::from_secs(2),
        latency_samples: 2,
        client_version: "0.162.0-superba-test".into(),
        use_system_proxy: false,
        // Mock servers speak plain http; the https rule has its own test.
        pat_requires_https: false,
        ..ProbeConfig::default()
    }
}

pub fn prober() -> HttpLineProber {
    HttpLineProber::new(probe_config()).with_clock(Arc::new(ManualClock::new(CHECKED_AT)))
}

pub fn key() -> Secret {
    Secret::new(KEY)
}

pub fn site_with_origins(origins: &[&str]) -> Site {
    let mut site = superba_core::catalog::krill_site();
    site.lines = origins
        .iter()
        .enumerate()
        .map(|(i, origin)| Line {
            id: LineId::new(format!("loopback-{i}")),
            name: LocalizedText::new("", format!("Loopback {i}")),
            origin: Origin::parse(origin).unwrap(),
            source: LineSource::Test,
        })
        .collect();
    site.default_line = site.lines.first().map(|line| line.id.clone());
    site
}

pub fn site(server: &MockServer) -> Site {
    site_with_origins(&[&server.origin()])
}

pub fn compatible_site(server: &MockServer) -> Site {
    let base = Url::parse(&format!("{}/v1", server.origin())).unwrap();
    let mut site =
        new_compatible_site(SiteId::new("compatible-test"), "Compatible test", &base).unwrap();
    site.lines[0].source = LineSource::Test;
    site
}

pub async fn closed_origin() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    origin
}

pub fn endpoints(server: &MockServer) -> ResolvedEndpoints {
    ResolvedEndpoints::for_line(
        &Origin::parse(&server.origin()).unwrap(),
        CodexPath::BackendApiCodex,
        AuthMode::ApiKey,
    )
    .unwrap()
}

pub fn assert_detail(caps: &LineCaps, key: &str, args: &[(&str, &str)]) {
    let detail = caps.detail.as_ref().expect("probe summary");
    assert_eq!(detail.key, key);
    assert_eq!(
        detail
            .args
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect::<Vec<_>>(),
        args
    );
    assert!(detail.nested.is_empty());
}

pub fn assert_authenticated(server: &MockServer) {
    let requests = server.recorded();
    assert!(!requests.is_empty());
    let expected = format!("Bearer {KEY}");
    for request in requests {
        assert_eq!(request.header("authorization"), Some(expected.as_str()));
    }
}

pub fn assert_client_version(request: &RecordedRequest, version: &str) {
    let query = request.query.as_deref().expect("client_version query");
    let pairs = url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect::<Vec<_>>();
    assert_eq!(
        pairs,
        vec![("client_version".to_owned(), version.to_owned())]
    );
}
