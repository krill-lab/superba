//! HTTP plumbing: clients, bounded reads, key scoping and redaction.
//!
//! Every request goes through [`Http::send`], which
//! - never follows redirects (a redirect is reported, not followed, so a key never
//!   reaches a second origin),
//! - refuses to attach a key to anything but https or a loopback http URL,
//! - reads at most a bounded number of body bytes,
//! - and returns a [`Transport`] failure whose message never contains the key.

use std::error::Error as StdError;
use std::time::{Duration, Instant};

use reqwest::header::{
    ACCEPT, ACCESS_CONTROL_ALLOW_ORIGIN, AUTHORIZATION, CONTENT_ENCODING, CONTENT_TYPE, ETAG,
    HeaderMap, HeaderValue, IF_NONE_MATCH, LOCATION, RETRY_AFTER,
};
use superba_core::codex::EDGE_BLOCK_HEADER;
use superba_core::{Error, Result, Secret};
use url::Url;

use crate::ProbeConfig;

/// Accept header for JSON endpoints.
pub(crate) const ACCEPT_JSON: &str = "application/json";
/// Accept header for streamed Responses answers.
pub(crate) const ACCEPT_SSE: &str = "text/event-stream";

/// Placeholder that replaces a key if it ever shows up in a message.
const REDACTED: &str = "[redacted]";

/// What came back from one request, reduced to what the probes need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Answer {
    /// HTTP status code.
    pub status: u16,
    /// Value of the Krill edge interception header (`l-block`), if present.
    pub edge_header: Option<String>,
    /// `Content-Type`, if present.
    pub content_type: Option<String>,
    /// `Access-Control-Allow-Origin` was present.
    pub cors: bool,
    /// `Location` of a redirect (never followed).
    pub location: Option<String>,
    /// Body bytes (at most the request's limit).
    pub body: Vec<u8>,
    /// The body was longer than the limit and was cut.
    pub truncated: bool,
    /// Time from sending the request to the end of the (bounded) body.
    pub elapsed: Duration,
    /// `Retry-After`, if present (rate-limited answers).
    pub retry_after: Option<String>,
    /// `ETag`, if present (model lists).
    pub etag: Option<String>,
}

impl Answer {
    /// True for 2xx statuses.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// True for 3xx statuses.
    pub fn is_redirect(&self) -> bool {
        (300..400).contains(&self.status)
    }

    /// Elapsed time in whole milliseconds (saturating).
    pub fn elapsed_ms(&self) -> u32 {
        duration_ms(self.elapsed)
    }
}

/// Saturating conversion of a duration to whole milliseconds.
pub(crate) fn duration_ms(d: Duration) -> u32 {
    u32::try_from(d.as_millis()).unwrap_or(u32::MAX)
}

/// Kind of a transport failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportKind {
    /// The request did not finish within its timeout.
    Timeout,
    /// DNS, TCP or TLS connection failure.
    Connect,
    /// Anything else (protocol errors, body read errors, client setup).
    Other,
}

/// A request that produced no HTTP answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Transport {
    /// Failure kind.
    pub kind: TransportKind,
    /// Short redacted message (no key, no URL).
    pub message: String,
}

impl Transport {
    /// Convert to the shared error type for `url` (which never carries a secret).
    pub fn into_error(self, url: &Url) -> Error {
        match self.kind {
            TransportKind::Timeout => Error::Timeout {
                what: format!("{}: {}", display_url(url), self.message),
            },
            _ => Error::Network {
                url: display_url(url),
                message: self.message,
            },
        }
    }
}

/// URL for messages: scheme, host, port and path only (queries are dropped).
pub(crate) fn display_url(url: &Url) -> String {
    let mut u = url.clone();
    u.set_query(None);
    u.set_fragment(None);
    // Origins never carry credentials (`Origin` refuses them), but be defensive.
    let _ = u.set_username("");
    let _ = u.set_password(None);
    u.to_string()
}

/// One request.
#[derive(Debug)]
pub(crate) struct Request<'a> {
    /// Target.
    pub url: Url,
    /// Bearer key, if the request is authenticated.
    pub key: Option<&'a Secret>,
    /// JSON body; `Some` makes it a POST.
    pub json_body: Option<Vec<u8>>,
    /// `Accept` header value.
    pub accept: &'static str,
    /// Whole-request timeout (connect + headers + body).
    pub timeout: Duration,
    /// Maximum number of body bytes kept.
    pub limit: usize,
    /// Stop reading once this returns true for the body read so far (streams that
    /// stay open after their final event).
    pub done: Option<fn(&[u8]) -> bool>,
    /// `If-None-Match` (a cached answer's `ETag`).
    pub if_none_match: Option<String>,
    /// `Content-Encoding` of the JSON body (the body is sent as given).
    pub content_encoding: Option<&'static str>,
    /// An Anthropic endpoint: the key also as `x-api-key`, plus
    /// `anthropic-version`.
    pub anthropic: bool,
    /// Send the key only as `x-api-key` (no `Authorization`).
    pub x_api_key_only: bool,
    /// Send no `x-api-key` with an Anthropic request.
    pub bearer_only: bool,
}

impl<'a> Request<'a> {
    /// Unauthenticated JSON GET.
    pub fn get(url: Url, timeout: Duration, limit: usize) -> Self {
        Self {
            url,
            key: None,
            json_body: None,
            accept: ACCEPT_JSON,
            timeout,
            limit,
            done: None,
            if_none_match: None,
            content_encoding: None,
            anthropic: false,
            x_api_key_only: false,
            bearer_only: false,
        }
    }

    /// Talk to an Anthropic endpoint: the key also as `x-api-key`, and
    /// `anthropic-version`.
    pub fn anthropic(mut self) -> Self {
        self.anthropic = true;
        self
    }

    /// Talk to an Anthropic endpoint the way Claude Code does with an
    /// `ANTHROPIC_AUTH_TOKEN`: the key only as `Authorization: Bearer`.
    pub fn anthropic_bearer(mut self) -> Self {
        self.anthropic = true;
        self.bearer_only = true;
        self
    }

    /// Talk to an Anthropic endpoint the way Claude Code does with an
    /// `ANTHROPIC_API_KEY`: the key only as `x-api-key`.
    pub fn anthropic_api_key(mut self) -> Self {
        self.anthropic = true;
        self.x_api_key_only = true;
        self
    }

    /// Declare the JSON body as already encoded (`zstd`); the bytes are sent as
    /// given.
    pub fn with_content_encoding(mut self, encoding: &'static str) -> Self {
        self.content_encoding = Some(encoding);
        self
    }

    /// Ask for `304 Not Modified` when the answer still has this `ETag`.
    pub fn if_none_match(mut self, etag: Option<String>) -> Self {
        self.if_none_match = etag;
        self
    }

    /// Stop reading the body as soon as `done` says it is complete.
    pub fn until(mut self, done: fn(&[u8]) -> bool) -> Self {
        self.done = Some(done);
        self
    }

    /// Attach a bearer key.
    pub fn with_key(mut self, key: &'a Secret) -> Self {
        self.key = Some(key);
        self
    }

    /// Make it a POST with a JSON body.
    pub fn with_json(mut self, body: Vec<u8>, accept: &'static str) -> Self {
        self.json_body = Some(body);
        self.accept = accept;
        self
    }
}

/// True if `url` points at a loopback host (127.0.0.0/8, ::1, `localhost`).
pub(crate) fn is_loopback_url(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

/// Key scoping rule: a key may only be sent to https, or to plain http on a
/// loopback host (tests and local forwarders), and never inside the URL (no
/// userinfo, and the key must not appear in the URL, percent-encoded or not).
pub(crate) fn ensure_key_target(url: &Url, key: &Secret) -> Result<()> {
    if url_contains_key(url, key) {
        return Err(Error::invalid(
            "refusing to send a key to a URL that contains the key",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::invalid(format!(
            "refusing to send a key to a URL with credentials: {}",
            display_url(url)
        )));
    }
    match url.scheme() {
        "https" if url.host().is_some() => Ok(()),
        "http" if is_loopback_url(url) => Ok(()),
        _ => Err(Error::invalid(format!(
            "refusing to send a key over a non-https URL: {}",
            display_url(url)
        ))),
    }
}

/// True if the key appears in the URL as written or after percent-decoding.
fn url_contains_key(url: &Url, key: &Secret) -> bool {
    let k = key.expose();
    if k.len() < 4 {
        return false;
    }
    let raw = url.as_str();
    raw.contains(k) || percent_decode(raw).contains(k)
}

/// Lossy percent-decoding (`%41` -> `A`; malformed escapes are kept as is).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (
                b.get(i + 1).and_then(|c| (*c as char).to_digit(16)),
                b.get(i + 2).and_then(|c| (*c as char).to_digit(16)),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Replace every occurrence of the key in `text` (defence in depth: servers should
/// never echo keys, and reqwest messages never contain headers).
pub(crate) fn redact(text: &str, key: Option<&Secret>) -> String {
    match key {
        Some(k) if k.expose().len() >= 4 && text.contains(k.expose()) => {
            text.replace(k.expose(), REDACTED)
        }
        _ => text.to_owned(),
    }
}

/// The reqwest clients (system proxy for real lines, direct for loopback; the
/// WebSocket pair speaks HTTP/1.1 only, since an upgrade needs it).
#[derive(Debug, Clone)]
pub(crate) struct Http {
    proxied: std::result::Result<reqwest::Client, String>,
    direct: std::result::Result<reqwest::Client, String>,
    ws_proxied: std::result::Result<reqwest::Client, String>,
    ws_direct: std::result::Result<reqwest::Client, String>,
}

/// The answer to a WebSocket handshake ([`Http::websocket_handshake`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Handshake {
    /// HTTP status (101 = upgraded; the socket was closed right away).
    pub status: u16,
    /// The Krill edge interception header was present.
    pub edge_blocked: bool,
    /// Body excerpt of a refusal (never of an upgraded connection).
    pub body: Vec<u8>,
}

/// A masked client close frame with status 1000 (RFC 6455 §5.5.1); the mask is
/// fixed, which RFC 6455 allows a client that sends nothing else to choose.
const CLOSE_FRAME: [u8; 8] = [0x88, 0x82, 0x5a, 0x17, 0x3c, 0x61, 0x03 ^ 0x5a, 0xe8 ^ 0x17];

impl Http {
    /// Build the clients. Failures are kept and reported per request, so
    /// construction (and `Default`) never panics.
    pub fn new(config: &ProbeConfig) -> Self {
        let build = |proxy: bool, http1_only: bool| {
            let mut b = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(config.connect_timeout)
                .user_agent(config.user_agent.clone())
                .pool_idle_timeout(Duration::from_secs(30));
            if !proxy {
                b = b.no_proxy();
            }
            if http1_only {
                // An upgrade needs HTTP/1.1; ALPN must not pick h2. Upgraded
                // connections never return to the pool.
                b = b.http1_only().pool_max_idle_per_host(0);
            }
            b.build().map_err(|e| error_chain(&e))
        };
        Self {
            proxied: build(config.use_system_proxy, false),
            direct: build(false, false),
            ws_proxied: build(config.use_system_proxy, true),
            ws_direct: build(false, true),
        }
    }

    fn client_for(&self, url: &Url) -> std::result::Result<&reqwest::Client, Transport> {
        self.pick(url, false)
    }

    fn pick(&self, url: &Url, ws: bool) -> std::result::Result<&reqwest::Client, Transport> {
        let c = match (is_loopback_url(url), ws) {
            (true, false) => &self.direct,
            (false, false) => &self.proxied,
            (true, true) => &self.ws_direct,
            (false, true) => &self.ws_proxied,
        };
        c.as_ref().map_err(|e| Transport {
            kind: TransportKind::Other,
            message: format!("HTTP client unavailable: {e}"),
        })
    }

    /// Send one request. Key scoping is enforced here as well as by callers.
    pub async fn send(&self, req: Request<'_>) -> std::result::Result<Answer, Transport> {
        if let Some(key) = req.key {
            ensure_key_target(&req.url, key).map_err(|e| Transport {
                kind: TransportKind::Other,
                message: e.to_string(),
            })?;
        }
        let client = self.client_for(&req.url)?;
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static(req.accept));
        if let Some(key) = req.key {
            let mut v =
                HeaderValue::from_str(&format!("Bearer {}", key.expose())).map_err(|_| {
                    Transport {
                        kind: TransportKind::Other,
                        message: "the key contains characters that cannot be sent in a header"
                            .into(),
                    }
                })?;
            v.set_sensitive(true);
            if !req.x_api_key_only {
                headers.insert(AUTHORIZATION, v);
            }
            if req.anthropic
                && !req.bearer_only
                && let Ok(mut k) = HeaderValue::from_str(key.expose())
            {
                k.set_sensitive(true);
                headers.insert("x-api-key", k);
            }
        }
        if req.anthropic {
            headers.insert(
                "anthropic-version",
                HeaderValue::from_static(superba_core::claude::ANTHROPIC_VERSION),
            );
        }
        if let Some(tag) = req.if_none_match.as_deref()
            && let Ok(v) = HeaderValue::from_str(tag)
        {
            headers.insert(IF_NONE_MATCH, v);
        }
        let builder = match req.json_body {
            Some(body) => {
                headers.insert(CONTENT_TYPE, HeaderValue::from_static(ACCEPT_JSON));
                if let Some(encoding) = req.content_encoding {
                    headers.insert(CONTENT_ENCODING, HeaderValue::from_static(encoding));
                }
                client.post(req.url.clone()).body(body)
            }
            None => client.get(req.url.clone()),
        };
        let started = Instant::now();
        let mut resp = builder
            .headers(headers)
            .timeout(req.timeout)
            .send()
            .await
            .map_err(|e| transport(&e, req.key))?;
        let status = resp.status().as_u16();
        let h = resp.headers();
        let header_str = |name: &str| {
            h.get(name)
                .map(|v| String::from_utf8_lossy(v.as_bytes()).trim().to_owned())
        };
        let edge_header = header_str(EDGE_BLOCK_HEADER);
        let content_type = header_str(CONTENT_TYPE.as_str());
        let cors = h.contains_key(ACCESS_CONTROL_ALLOW_ORIGIN);
        let location = header_str(LOCATION.as_str());
        let retry_after = header_str(RETRY_AFTER.as_str());
        let etag = header_str(ETAG.as_str());
        let mut body = Vec::new();
        let mut truncated = false;
        loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    let room = req.limit.saturating_sub(body.len());
                    if chunk.len() > room {
                        body.extend_from_slice(&chunk[..room]);
                        truncated = true;
                        break;
                    }
                    body.extend_from_slice(&chunk);
                    if req.done.is_some_and(|done| done(&body)) {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => return Err(transport(&e, req.key)),
            }
        }
        let elapsed = started.elapsed();
        // A server that echoes the key must not get it into any diagnostic built
        // from this answer: scrub it at the source.
        let location = location.map(|l| redact(&l, req.key));
        if let Some(key) = req.key {
            scrub(&mut body, key.expose().as_bytes(), truncated);
        }
        Ok(Answer {
            status,
            edge_header,
            content_type,
            cors,
            location,
            body,
            truncated,
            elapsed,
            retry_after,
            etag,
        })
    }
}

impl Http {
    /// A Responses WebSocket handshake (`GET` with `Upgrade: websocket`, the
    /// key and `extra` headers) to `url` (`https://…/responses`). A 101 is
    /// closed at once with a close frame; nothing else is sent on the socket.
    /// Other answers are read (bounded) and reported. Same key scoping as
    /// [`Http::send`].
    pub async fn websocket_handshake(
        &self,
        url: &Url,
        key: &Secret,
        extra: HeaderMap,
        timeout: Duration,
        limit: usize,
    ) -> std::result::Result<Handshake, Transport> {
        use reqwest::header::{CONNECTION, SEC_WEBSOCKET_KEY, SEC_WEBSOCKET_VERSION, UPGRADE};
        ensure_key_target(url, key).map_err(|e| Transport {
            kind: TransportKind::Other,
            message: e.to_string(),
        })?;
        let client = self.pick(url, true)?;
        let mut headers = extra;
        let mut auth =
            HeaderValue::from_str(&format!("Bearer {}", key.expose())).map_err(|_| Transport {
                kind: TransportKind::Other,
                message: "the key contains characters that cannot be sent in a header".into(),
            })?;
        auth.set_sensitive(true);
        headers.insert(AUTHORIZATION, auth);
        headers.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
        headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
        headers.insert(SEC_WEBSOCKET_VERSION, HeaderValue::from_static("13"));
        if let Ok(v) = HeaderValue::from_str(&websocket_key()) {
            headers.insert(SEC_WEBSOCKET_KEY, v);
        }
        let attempt = async {
            let resp = client
                .get(url.clone())
                .headers(headers)
                .send()
                .await
                .map_err(|e| transport(&e, Some(key)))?;
            let status = resp.status().as_u16();
            let edge_blocked = resp.headers().contains_key(EDGE_BLOCK_HEADER);
            if status == 101 {
                if let Ok(mut io) = resp.upgrade().await {
                    use tokio::io::AsyncWriteExt;
                    // Best effort: the socket is dropped either way.
                    let _ = io.write_all(&CLOSE_FRAME).await;
                    let _ = io.shutdown().await;
                }
                return Ok(Handshake {
                    status,
                    edge_blocked,
                    body: Vec::new(),
                });
            }
            let mut resp = resp;
            let mut body = Vec::new();
            while let Ok(Some(chunk)) = resp.chunk().await {
                let room = limit.saturating_sub(body.len());
                body.extend_from_slice(&chunk[..chunk.len().min(room)]);
                if body.len() >= limit {
                    break;
                }
            }
            let cut = body.len() >= limit;
            scrub(&mut body, key.expose().as_bytes(), cut);
            Ok(Handshake {
                status,
                edge_blocked,
                body,
            })
        };
        tokio::time::timeout(timeout, attempt)
            .await
            .unwrap_or_else(|_| {
                Err(Transport {
                    kind: TransportKind::Timeout,
                    message: "timed out".into(),
                })
            })
    }
}

/// A fresh `Sec-WebSocket-Key`: base64 of 16 bytes derived from the clock and a
/// counter (RFC 6455 asks for a nonce; the server only echoes a digest of it).
fn websocket_key() -> String {
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let digest = Sha256::digest(format!("{nanos}:{n}:{}", std::process::id()).as_bytes());
    base64(&digest[..16])
}

/// Standard base64 with padding.
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Minimum key length that is scrubbed from bodies (shorter strings would match
/// ordinary text; such keys are refused by every real site anyway).
const MIN_SCRUB_LEN: usize = 8;

/// Replace every occurrence of `key` in `body` with [`REDACTED`]; when the body was
/// cut at the read limit, also drop a trailing partial copy of the key.
fn scrub(body: &mut Vec<u8>, key: &[u8], truncated: bool) {
    if key.len() < MIN_SCRUB_LEN {
        return;
    }
    if body.windows(key.len()).any(|w| w == key) {
        let mut out = Vec::with_capacity(body.len());
        let mut i = 0;
        while i < body.len() {
            if body[i..].starts_with(key) {
                out.extend_from_slice(REDACTED.as_bytes());
                i += key.len();
            } else {
                out.push(body[i]);
                i += 1;
            }
        }
        *body = out;
    }
    if truncated
        && let Some(n) = (MIN_SCRUB_LEN / 2..key.len())
            .rev()
            .find(|&n| body.ends_with(&key[..n]))
    {
        body.truncate(body.len() - n);
    }
}

/// Classify and describe a reqwest error without the URL or any secret.
fn transport(e: &reqwest::Error, key: Option<&Secret>) -> Transport {
    let kind = if e.is_timeout() {
        TransportKind::Timeout
    } else if e.is_connect() {
        TransportKind::Connect
    } else {
        TransportKind::Other
    };
    let message = match kind {
        TransportKind::Timeout => "timed out".to_owned(),
        _ => error_chain(e),
    };
    Transport {
        kind,
        message: redact(&message, key),
    }
}

/// The innermost useful messages of an error chain ("connection refused",
/// "dns error: ...", "invalid peer certificate: ..."), without reqwest's
/// "error sending request for url (...)" wrapper.
fn error_chain(e: &(dyn StdError + 'static)) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut cur: Option<&(dyn StdError + 'static)> = Some(e);
    while let Some(err) = cur {
        let msg = err.to_string();
        if !msg.starts_with("error sending request")
            && !msg.starts_with("builder error")
            && !msg.starts_with("client error")
            && !parts.iter().any(|p| p == &msg)
        {
            parts.push(msg);
        }
        cur = err.source();
    }
    if parts.is_empty() {
        e.to_string()
    } else {
        parts.join(": ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_targets_refuse_urls_that_carry_the_key() {
        let key = Secret::new("sk-test-1234567890");
        let ok = |s: &str| ensure_key_target(&Url::parse(s).unwrap(), &key).is_ok();
        assert!(ok("https://api.example.com/v1"));
        assert!(!ok("https://api.example.com/v1?api_key=sk-test-1234567890"));
        assert!(!ok("https://api.example.com/sk-test-1234567890/v1"));
        assert!(!ok("https://api.example.com/v1?k=sk%2Dtest%2D1234567890"));
        let e = ensure_key_target(
            &Url::parse("http://h.example/?k=sk-test-1234567890").unwrap(),
            &key,
        )
        .unwrap_err();
        assert!(!e.to_string().contains("sk-test-1234567890"));
        assert_eq!(percent_decode("a%41%zz%4"), "aA%zz%4");
    }

    #[test]
    fn key_targets_require_https_or_loopback_http() {
        let key = Secret::new("sk-test-1234567890");
        let ok = |s: &str| ensure_key_target(&Url::parse(s).unwrap(), &key).is_ok();
        assert!(ok("https://api.example.com/v1/models"));
        assert!(ok("http://127.0.0.1:8080/v1/models"));
        assert!(ok("http://127.8.9.10/x"));
        assert!(ok("http://[::1]:9/x"));
        assert!(ok("http://localhost:9/x"));
        assert!(!ok("http://api.example.com/v1/models"));
        assert!(!ok("http://127.attacker.example/v1/models"));
        assert!(!ok("https://user:pw@api.example.com/"));
        assert!(!ok("ftp://api.example.com/"));
    }

    #[test]
    fn redact_replaces_every_occurrence_of_the_key() {
        let key = Secret::new("sk-test-1234567890");
        let text = "bad key sk-test-1234567890 and again sk-test-1234567890";
        let out = redact(text, Some(&key));
        assert!(!out.contains("sk-test-1234567890"));
        assert_eq!(out.matches(REDACTED).count(), 2);
        assert_eq!(redact("nothing", Some(&key)), "nothing");
        assert_eq!(redact("nothing", None), "nothing");
    }

    #[test]
    fn scrub_removes_echoed_keys_and_cut_off_prefixes() {
        let key = b"sk-test-1234567890";
        let mut body = br#"{"error":{"message":"invalid token sk-test-1234567890!"}}"#.to_vec();
        scrub(&mut body, key, false);
        assert_eq!(
            String::from_utf8(body).unwrap(),
            r#"{"error":{"message":"invalid token [redacted]!"}}"#
        );
        let mut twice = b"sk-test-1234567890sk-test-1234567890".to_vec();
        scrub(&mut twice, key, false);
        assert_eq!(twice, b"[redacted][redacted]");
        let mut cut = b"{\"m\":\"sk-test-123".to_vec();
        scrub(&mut cut, key, true);
        assert_eq!(cut, b"{\"m\":\"");
        let mut not_cut = b"{\"m\":\"sk-test-123".to_vec();
        scrub(&mut not_cut, key, false);
        assert_eq!(
            not_cut, b"{\"m\":\"sk-test-123",
            "complete bodies keep text"
        );
        let mut short = b"abc abc".to_vec();
        scrub(&mut short, b"abc", false);
        assert_eq!(short, b"abc abc", "too short to scrub safely");
    }

    #[test]
    fn websocket_keys_are_base64_nonces() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"the sample nonce"), "dGhlIHNhbXBsZSBub25jZQ==");
        let (a, b) = (websocket_key(), websocket_key());
        assert_eq!(a.len(), 24);
        assert_ne!(a, b);
        // The close frame is masked, carries status 1000 and nothing else.
        assert_eq!(CLOSE_FRAME[0], 0x88);
        assert_eq!(CLOSE_FRAME[1], 0x80 | 2);
        let code = [
            CLOSE_FRAME[6] ^ CLOSE_FRAME[2],
            CLOSE_FRAME[7] ^ CLOSE_FRAME[3],
        ];
        assert_eq!(u16::from_be_bytes(code), 1000);
    }

    #[test]
    fn display_url_drops_query_and_credentials() {
        let u = Url::parse("https://a:b@h.example/p?client_version=1#f").unwrap();
        assert_eq!(display_url(&u), "https://h.example/p");
    }
}
