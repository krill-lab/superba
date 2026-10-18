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
use superba_core::{
    AgentKind, RequestClient, RequestHarness, RequestKind, RequestProtocol, RequestTransport,
};

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
    /// The Codex host the router authenticated (`wsl:<distro>`; `None` = this
    /// computer), from [`crate::forward::CODEX_HOST_HEADER`].
    pub codex_host: Option<String>,
    /// Compaction trigger and compaction items of the input
    /// ([`crate::compaction`]).
    pub compaction: crate::compaction::CompactionInput,
    /// The tool that sent the request ([`crate::forward::HARNESS_HEADER`], else
    /// the `User-Agent`, else the ingress's own harness).
    pub harness: RequestHarness,
    /// The tool's version (`User-Agent`).
    pub harness_version: Option<String>,
    /// The protocol the tool spoke.
    pub protocol: RequestProtocol,
    /// The model the tool asked for (for Codex the same as `model`).
    pub requested_model: Option<String>,
    /// The tool's session id.
    pub session_id: Option<String>,
    /// Main conversation, subagent or background work.
    pub agent_kind: Option<AgentKind>,
}

/// The harness of a request and its version: the harness the router token
/// named (internal header set by the server), else the one its `User-Agent`
/// names, else `ingress` (the harness the ingress serves). The version comes
/// from the `User-Agent` when it names the same harness.
pub fn attribution(
    headers: &HeaderMap,
    ingress: RequestHarness,
) -> (RequestHarness, Option<String>) {
    let by_token =
        header(headers, crate::forward::HARNESS_HEADER).and_then(RequestHarness::from_id);
    let by_agent = header(headers, "user-agent").and_then(RequestHarness::from_user_agent);
    match (by_token, by_agent) {
        (Some(h), Some((a, version))) if a == h => (h, version),
        (Some(h), _) => (h, None),
        (None, Some((a, version))) => (a, version),
        (None, None) => (ingress, None),
    }
}

/// Codex's agent kind from its turn metadata.
fn codex_agent_kind(kind: Option<RequestKind>, source: Option<&str>) -> Option<AgentKind> {
    match kind {
        Some(RequestKind::Compaction | RequestKind::Memory) => return Some(AgentKind::Background),
        None => return None,
        _ => {}
    }
    Some(match source {
        None | Some("user") => AgentKind::Main,
        Some(s) if s.contains("subagent") => AgentKind::Subagent,
        Some(_) => AgentKind::Background,
    })
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
    let (harness, harness_version) = attribution(headers, RequestHarness::Codex);
    let request_kind = turn
        .as_ref()
        .and_then(|t| t.request_kind.as_deref().map(request_kind));
    let thread_source = turn
        .as_ref()
        .and_then(|t| short(t.thread_source.as_ref().and_then(serde_json::Value::as_str)));
    let session_id = turn
        .as_ref()
        .and_then(|t| short(t.session_id.as_deref()))
        .or_else(|| short(header(headers, "session-id")))
        .or_else(|| thread_id.clone());
    let model = short(peek.model.as_deref());
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
        agent_kind: codex_agent_kind(request_kind, thread_source.as_deref()),
        request_kind,
        thread_source,
        requested_model: model.clone(),
        model,
        harness,
        harness_version,
        protocol: RequestProtocol::Responses,
        session_id,
        service_tier: short(peek.service_tier.as_deref()),
        prompt_cache_key: short(peek.prompt_cache_key.as_deref()),
        continuation: peek.previous_response_id.is_some_and(|p| !p.is_empty()),
        prewarm: peek.generate == Some(false),
        structured_output,
        transport,
        stripped: 0,
        codex_host: short(header(headers, crate::forward::CODEX_HOST_HEADER)),
        compaction: crate::compaction::inspect(body),
    }
}
