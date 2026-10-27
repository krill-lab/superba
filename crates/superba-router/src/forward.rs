//! Header rules, failover classification, router error answers and the observed
//! response body shared by the HTTP and WebSocket paths.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll};

use bytes::Bytes;
use http::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HOST};
use http::{HeaderMap, HeaderName, HeaderValue, Response, StatusCode};
use http_body::{Body, Frame, SizeHint};
use http_body_util::{BodyExt, Full};
use superba_core::codex::EDGE_BLOCK_HEADER;

use crate::limits::Admission;
use crate::state::{BoxError, GaugeGuard, Generation, RespBody, Shared};
use crate::status::RouteEventKind;

/// Response header naming the member and line that served a request.
pub const ROUTE_HEADER: &str = "x-superba-route";

/// Hop-by-hop headers (RFC 9110 7.6.1) plus the legacy proxy ones.
pub(crate) const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// The Codex host a request came from (`wsl:<distro>`; absent for this
/// computer), set by the router after checking the token (user bug 45) and
/// read into the request log; never forwarded.
pub const CODEX_HOST_HEADER: &str = "x-superba-codex-host";

/// The harness a request's router token named (`claude-code`; absent for the
/// plain router token), set by the router after checking the token and read
/// into the request log; never forwarded.
pub const HARNESS_HEADER: &str = "x-superba-harness";
/// The provider authenticated by a bound tool token; never accepted from a client header.
pub const MEMBER_HEADER: &str = "x-superba-harness-member";

/// The verified provider binding set by the listener.
pub(crate) fn bound_member(headers: &HeaderMap) -> Option<&str> {
    headers.get(MEMBER_HEADER).and_then(|v| v.to_str().ok())
}

/// Request headers never forwarded to a member: the client's credential
/// (`Authorization`, and `x-api-key`, which WorkBuddy and ZCode also send) is
/// replaced by the member's own key, and ChatGPT account routing belongs to
/// PAT mode.
const DROPPED_REQUEST: &[&str] = &[
    "chatgpt-account-id",
    "x-api-key",
    "x-goog-api-key",
    CODEX_HOST_HEADER,
    HARNESS_HEADER,
    MEMBER_HEADER,
];

/// Turn routing token issued by one provider; meaningless to another.
pub const TURN_STATE_HEADER: &str = "x-codex-turn-state";

pub(crate) fn connection_tokens(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(http::header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|t| t.trim().to_ascii_lowercase())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Headers of a request as forwarded upstream. `websocket` keeps the upgrade
/// handshake (`Connection: Upgrade`, `Upgrade`, `Sec-WebSocket-*`).
pub(crate) fn request_headers(
    src: &HeaderMap,
    websocket: bool,
    drop_turn_state: bool,
    auth: &HeaderValue,
) -> HeaderMap {
    let listed = connection_tokens(src);
    let mut out = HeaderMap::with_capacity(src.len());
    for (name, value) in src {
        let n = name.as_str();
        let keep_for_ws = websocket && (n == "connection" || n == "upgrade");
        if !keep_for_ws && (HOP_BY_HOP.contains(&n) || listed.iter().any(|t| t == n)) {
            continue;
        }
        if name == HOST
            || name == CONTENT_LENGTH
            || name == AUTHORIZATION
            || DROPPED_REQUEST.contains(&n)
            || (drop_turn_state && n == TURN_STATE_HEADER)
        {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    out.insert(AUTHORIZATION, auth.clone());
    out
}

/// Headers of an upstream response as passed back to Codex.
pub(crate) fn response_headers(src: &HeaderMap, upgrade: bool) -> HeaderMap {
    let listed = connection_tokens(src);
    let mut out = HeaderMap::with_capacity(src.len() + 1);
    for (name, value) in src {
        let n = name.as_str();
        let keep_for_ws = upgrade && (n == "connection" || n == "upgrade");
        if !keep_for_ws && (HOP_BY_HOP.contains(&n) || listed.iter().any(|t| t == n)) {
            continue;
        }
        if upgrade && name == CONTENT_LENGTH {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    out
}

/// Why an answer that arrived is still a failover reason: 502/503/504, or the
/// line's edge intercepted the request (Krill CDN `l-block` header). Every other
/// status is passed through, 401/403/429 and 500 included.
pub(crate) fn failover_status(status: StatusCode, headers: &HeaderMap) -> Option<String> {
    failover_status_in(crate::config::DEFAULT_FAILOVER_STATUSES, status, headers)
}

/// [`failover_status`] with the configured statuses that fail over
/// ([`crate::config::Tuning::failover_statuses`]); an edge interception
/// always does.
pub(crate) fn failover_status_in(
    statuses: &[u16],
    status: StatusCode,
    headers: &HeaderMap,
) -> Option<String> {
    if headers.contains_key(EDGE_BLOCK_HEADER) {
        return Some(format!(
            "intercepted by the line's edge (HTTP {})",
            status.as_u16()
        ));
    }
    statuses
        .contains(&status.as_u16())
        .then(|| format!("HTTP {}", status.as_u16()))
}

/// A 2xx answer to a Responses request with an empty body comes from something
/// in front of the relay (the relay always streams events), like the Krill
/// edge's interceptions without an `l-block` header.
pub(crate) fn empty_responses_answer(rest: &str, status: StatusCode, headers: &HeaderMap) -> bool {
    status.is_success()
        && rest.trim_end_matches('/').ends_with("/responses")
        && headers
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim() == "0")
}

/// Short description of a transport failure before response headers. Never
/// contains the URL's query, a header or a body.
pub(crate) fn describe_error(e: &reqwest::Error) -> String {
    let kind = if e.is_timeout() {
        "connect timeout"
    } else if e.is_connect() {
        "connect failed"
    } else {
        "no response"
    };
    let mut chain = Vec::new();
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        chain.push(s.to_string());
        source = s.source();
    }
    let mut detail = chain.join(": ");
    if detail.len() > 200 {
        let cut = (0..=200)
            .rev()
            .find(|i| detail.is_char_boundary(*i))
            .unwrap_or(0);
        detail.truncate(cut);
        detail.push('…');
    }
    if detail.is_empty() {
        kind.to_owned()
    } else {
        format!("{kind}: {detail}")
    }
}

/// A JSON error answered by the router itself, in the OpenAI error shape Codex
/// extracts `error.message` from.
pub(crate) fn router_error(status: StatusCode, kind: &str, message: &str) -> Response<RespBody> {
    let body = serde_json::json!({
        "error": {
            "message": format!("Superba local router: {message}"),
            "type": kind,
            "code": kind,
        }
    });
    let mut resp = Response::new(full(Bytes::from(body.to_string())));
    *resp.status_mut() = status;
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    resp
}

/// The router's 503 for a local shortage ([`crate::limits`]): every admission
/// in use, or too many open files on this computer. `Connection: close` gives
/// the descriptor back at once; Codex retries after `Retry-After`.
pub(crate) fn local_busy(message: &str) -> Response<RespBody> {
    let mut resp = router_error(
        StatusCode::SERVICE_UNAVAILABLE,
        crate::limits::LOCAL_BUSY_CODE,
        message,
    );
    let headers = resp.headers_mut();
    headers.insert(http::header::CONNECTION, HeaderValue::from_static("close"));
    headers.insert(
        http::header::RETRY_AFTER,
        HeaderValue::from(crate::limits::RETRY_AFTER_SECS),
    );
    resp
}

/// A complete body.
pub(crate) fn full(bytes: Bytes) -> RespBody {
    Full::new(bytes)
        .map_err(|never| match never {})
        .boxed_unsync()
}

/// Empty body.
pub(crate) fn empty() -> RespBody {
    full(Bytes::new())
}

/// Insert the route header.
pub(crate) fn set_route(headers: &mut HeaderMap, label: &str) {
    if let Ok(v) = HeaderValue::from_str(label) {
        headers.insert(HeaderName::from_static(ROUTE_HEADER), v);
    }
}

/// Tracks a streamed upstream body: counts the request as active until the body
/// ends or is dropped, and marks the line suspect when the upstream breaks the
/// stream (not when Codex goes away: then the body is just dropped).
pub(crate) struct Observed<B> {
    inner: B,
    shared: Arc<Shared>,
    generation: Arc<Generation>,
    member: usize,
    line: usize,
    _active: GaugeGuard,
    /// The request's admission: its place is held until the body ends.
    _admission: Option<Admission>,
}

impl<B> Observed<B> {
    pub(crate) fn new(
        inner: B,
        shared: Arc<Shared>,
        generation: Arc<Generation>,
        member: usize,
        line: usize,
        active: GaugeGuard,
    ) -> Self {
        Self {
            inner,
            shared,
            generation,
            member,
            line,
            _active: active,
            _admission: None,
        }
    }

    /// Keep `admission` until the body ends or is dropped.
    pub(crate) fn holding(mut self, admission: Admission) -> Self {
        self._admission = Some(admission);
        self
    }

    fn broken(&self, detail: String) {
        let m = &self.generation.members[self.member];
        let l = &m.lines[self.line];
        l.counters.broken_streams.fetch_add(1, Ordering::Relaxed);
        let opened = self.generation.hard_failure(
            crate::select::Candidate {
                member: self.member,
                line: self.line,
            },
            detail.clone(),
        );
        self.shared.event(
            RouteEventKind::StreamBroken,
            Some(&m.cfg.id),
            Some(&l.cfg.id),
            Some(detail),
        );
        if opened {
            self.shared.event(
                RouteEventKind::LineDown,
                Some(&m.cfg.id),
                Some(&l.cfg.id),
                Some("stream broken".into()),
            );
        }
    }
}

impl<B> Body for Observed<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: Into<BoxError> + std::fmt::Display,
{
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        match Pin::new(&mut self.inner).poll_frame(cx) {
            Poll::Ready(Some(Err(e))) => {
                let detail = format!("stream broken: {e}");
                self.broken(detail);
                // Propagating the error makes hyper abort the downstream response
                // without a terminating chunk; Codex re-sends at once.
                Poll::Ready(Some(Err(e.into())))
            }
            other => other.map(|o| o.map(|r| r.map_err(Into::into))),
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// An upstream response body, possibly with its first bytes already read
/// (a small error answer the router inspected before passing it on).
pub(crate) struct UpBody {
    prefix: Option<Bytes>,
    rest: Option<RespBody>,
}

impl UpBody {
    /// The body as it arrives.
    pub(crate) fn live(body: reqwest::Body) -> Self {
        Self {
            prefix: None,
            rest: Some(body.map_err(|e| -> BoxError { Box::new(e) }).boxed_unsync()),
        }
    }

    /// A body transformed by a protocol codec, retaining streaming backpressure.
    pub(crate) fn translated(body: RespBody) -> Self {
        Self {
            prefix: None,
            rest: Some(body),
        }
    }

    /// `bytes`, the whole body (read already).
    pub(crate) fn replay(bytes: Bytes) -> Self {
        Self {
            prefix: (!bytes.is_empty()).then_some(bytes),
            rest: None,
        }
    }

    /// This body (its own first bytes already taken) with `prefix` before
    /// what it still holds.
    pub(crate) fn prepend(mut self, prefix: Bytes) -> Self {
        if !prefix.is_empty() {
            self.prefix = Some(prefix);
        }
        self
    }
}

impl Body for UpBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        if let Some(p) = self.prefix.take() {
            return Poll::Ready(Some(Ok(Frame::data(p))));
        }
        match self.rest.as_mut() {
            Some(rest) => Pin::new(rest).poll_frame(cx),
            None => Poll::Ready(None),
        }
    }

    fn is_end_stream(&self) -> bool {
        self.prefix.is_none() && self.rest.as_ref().is_none_or(Body::is_end_stream)
    }

    fn size_hint(&self) -> SizeHint {
        let mut hint = self
            .rest
            .as_ref()
            .map(Body::size_hint)
            .unwrap_or_else(|| SizeHint::with_exact(0));
        if let Some(p) = &self.prefix {
            // The upper bound first: `set_lower` refuses a value above the
            // current upper bound (a replayed body has an exact 0 behind it).
            let n = p.len() as u64;
            if let Some(u) = hint.upper() {
                hint.set_upper(u + n);
            }
            hint.set_lower(hint.lower() + n);
        }
        hint
    }
}

/// An upstream answer as passed on: status, headers and [`UpBody`].
pub(crate) type UpResponse = http::Response<UpBody>;

/// A `reqwest` answer as an [`UpResponse`].
pub(crate) fn up_response(resp: reqwest::Response) -> UpResponse {
    let resp: http::Response<reqwest::Body> = resp.into();
    resp.map(UpBody::live)
}
