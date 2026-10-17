//! What the router learns about a Responses request: one borrowed pass over the
//! headers and the body (or a WebSocket `response.create` frame), skipping the
//! input items.
//!
//! | Field | Source |
//! |---|---|
//! | originator | `originator` header (HTTP request or WebSocket handshake) |
//! | turn metadata | `x-codex-turn-metadata` header, else `client_metadata["x-codex-turn-metadata"]` (a JSON string) |
//! | thread id | turn metadata `thread_id`, else the `thread-id` / `session-id` header |
//! | model, `service_tier`, `prompt_cache_key`, `previous_response_id`, `generate` | body |
//! | structured output | body `text.format.type` is `json_schema` or `json_object` |

use std::borrow::Cow;

use http::HeaderMap;
use serde::Deserialize;
use superba_core::{RequestClient, RequestKind, RequestTransport};

/// Header carrying Codex's turn metadata (also in `client_metadata`).
pub const TURN_METADATA_HEADER: &str = "x-codex-turn-metadata";

/// Longest identifier kept (longer values are dropped, not truncated).
const MAX_ID_LEN: usize = 256;

/// Request facts used for gating, recording and stickiness-independent grouping.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestMeta {
    /// Raw `originator` header.
    pub originator: Option<String>,
    /// Client derived from the originator.
    pub client: RequestClient,
    /// Thread id.
    pub thread_id: Option<String>,
    /// Turn id.
    pub turn_id: Option<String>,
    /// Turn metadata `request_kind` (`None` when there is no turn metadata).
    pub request_kind: Option<RequestKind>,
    /// Turn metadata `thread_source`.
    pub thread_source: Option<String>,
    /// Requested model.
    pub model: Option<String>,
    /// Requested `service_tier`.
    pub service_tier: Option<String>,
    /// `prompt_cache_key`.
    pub prompt_cache_key: Option<String>,
    /// The request continues a stored response (`previous_response_id`).
    pub continuation: bool,
    /// `generate: false` (WebSocket prewarm).
    pub prewarm: bool,
    /// The body asks for structured output (`json_schema` / `json_object`).
    pub structured_output: bool,
    /// Transport.
    pub transport: RequestTransport,
    /// Stats lines removed from the input.
    pub stripped: u32,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct BodyPeek<'a> {
    #[serde(borrow)]
    model: Option<Cow<'a, str>>,
    #[serde(borrow)]
    service_tier: Option<Cow<'a, str>>,
    #[serde(borrow)]
    prompt_cache_key: Option<Cow<'a, str>>,
    #[serde(borrow)]
    previous_response_id: Option<Cow<'a, str>>,
    generate: Option<bool>,
    #[serde(borrow)]
    text: Option<TextPeek<'a>>,
    #[serde(borrow)]
    client_metadata: Option<ClientMetadataPeek<'a>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct TextPeek<'a> {
    #[serde(borrow)]
    format: Option<FormatPeek<'a>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FormatPeek<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ClientMetadataPeek<'a> {
    #[serde(rename = "x-codex-turn-metadata", borrow)]
    turn_metadata: Option<Cow<'a, str>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct TurnMetadata {
    thread_id: Option<String>,
    session_id: Option<String>,
    turn_id: Option<String>,
    request_kind: Option<String>,
    thread_source: Option<serde_json::Value>,
}

fn short(v: Option<&str>) -> Option<String> {
    v.map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= MAX_ID_LEN)
        .map(str::to_owned)
}

fn header<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// Parse a `request_kind` value.
pub fn request_kind(raw: &str) -> RequestKind {
    serde_json::from_value(serde_json::Value::String(raw.to_owned())).unwrap_or(RequestKind::Other)
}

/// Peek an HTTP request: `headers` and the (decoded) JSON `body`. A body that
/// is not a JSON object yields the header facts only.
pub fn peek(headers: &HeaderMap, body: &[u8]) -> RequestMeta {
    peek_with(headers, body, RequestTransport::Http)
}

/// Peek a WebSocket `response.create` frame with the headers of its handshake.
pub fn peek_frame(handshake: &HeaderMap, frame: &[u8]) -> RequestMeta {
    peek_with(handshake, frame, RequestTransport::WebSocket)
}

/// The facts of the headers alone (a body that could not be read).
pub fn from_headers(headers: &HeaderMap) -> RequestMeta {
    peek_with(headers, b"", RequestTransport::Http)
}

fn peek_with(headers: &HeaderMap, body: &[u8], transport: RequestTransport) -> RequestMeta {
    let originator = short(header(headers, "originator"));
    let peek: BodyPeek<'_> = serde_json::from_slice(body).unwrap_or_default();
    let turn_json = header(headers, TURN_METADATA_HEADER)
        .map(Cow::Borrowed)
        .or_else(|| {
            peek.client_metadata
                .as_ref()
                .and_then(|c| c.turn_metadata.clone())
        });
    let turn: Option<TurnMetadata> = turn_json
        .as_deref()
        .and_then(|j| serde_json::from_str(j).ok());
    let thread_id = turn
        .as_ref()
        .and_then(|t| short(t.thread_id.as_deref()).or_else(|| short(t.session_id.as_deref())))
        .or_else(|| short(header(headers, "thread-id")))
        .or_else(|| short(header(headers, "session-id")));
    let structured_output = peek
        .text
        .as_ref()
        .and_then(|t| t.format.as_ref())
        .and_then(|f| f.kind.as_deref())
        .is_some_and(|k| matches!(k, "json_schema" | "json_object"));
    RequestMeta {
        client: RequestClient::from_originator(originator.as_deref()),
        originator,
        thread_id,
        turn_id: turn.as_ref().and_then(|t| short(t.turn_id.as_deref())),
        request_kind: turn
            .as_ref()
            .and_then(|t| t.request_kind.as_deref().map(request_kind)),
        thread_source: turn
            .as_ref()
            .and_then(|t| short(t.thread_source.as_ref().and_then(serde_json::Value::as_str))),
        model: short(peek.model.as_deref()),
        service_tier: short(peek.service_tier.as_deref()),
        prompt_cache_key: short(peek.prompt_cache_key.as_deref()),
        continuation: peek.previous_response_id.is_some_and(|p| !p.is_empty()),
        prewarm: peek.generate == Some(false),
        structured_output,
        transport,
        stripped: 0,
    }
}
