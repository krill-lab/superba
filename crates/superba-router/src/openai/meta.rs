//! Request facts of a request on the OpenAI ingress for the request log,
//! stickiness and the stats line (multi-harness §2; the harness research).
//!
//! The harness is the one the router token names, else the `User-Agent`'s,
//! else unknown: pi's `session_id` header has the name Codex uses, so headers
//! never decide the tool.
//!
//! | Harness | Session | Turn | Agent kind |
//! |---|---|---|---|
//! | dsh | `session_id` (Responses; = `x-client-request-id`, `prompt_cache_key`) | — | no tools → background (its title request) |
//! | opencode | `x-opencode-session-id` (= `x-session-affinity`, `X-Session-Id`) | — | `x-opencode-parent-session-id` → subagent; no tools → background |
//! | pi | `session_id` (Responses; Chat with session affinity on) | — | no tools → background |
//! | ZCode | `x-session-id` | `x-zcode-trace-id` | `x-zcode-session-type` (`main`, `subagent`, else background); `x-zcode-query-source` `compact` → compaction |
//! | WorkBuddy | `X-Conversation-ID` | `X-Conversation-Request-ID` (else `X-Root-Request-ID`) | `X-Agent-Type` (`main`, else subagent) |
//! | other | the first of those headers present | — | no tools → background |
//!
//! Without a session header the body's `prompt_cache_key` stands in.

use std::borrow::Cow;

use http::HeaderMap;
use serde::Deserialize;
use serde::de::IgnoredAny;
use superba_core::{AgentKind, RequestClient, RequestHarness, RequestKind, RequestTransport};

use super::config::OpenAiProtocol;
use crate::stats::meta::{RequestMeta, attribution};

/// Longest id kept.
const MAX_ID_LEN: usize = 256;

/// Session headers of each tool, in order of preference.
fn session_headers(harness: RequestHarness) -> &'static [&'static str] {
    const ALL: &[&str] = &[
        "x-session-id",
        "x-opencode-session-id",
        "x-conversation-id",
        "session_id",
        "session-id",
        "x-session-affinity",
        "x-client-request-id",
    ];
    match harness {
        RequestHarness::Dsh | RequestHarness::Pi => &["session_id", "x-client-request-id"],
        RequestHarness::OpenCode => &[
            "x-opencode-session-id",
            "x-session-id",
            "x-session-affinity",
        ],
        RequestHarness::ZCode => &["x-session-id"],
        RequestHarness::WorkBuddy => &["x-conversation-id"],
        _ => ALL,
    }
}

fn header<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty() && v.len() <= MAX_ID_LEN)
}

fn short(v: Option<&str>) -> Option<String> {
    v.map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= MAX_ID_LEN)
        .map(str::to_owned)
}

/// The parts of a request body the facts need (both protocols).
#[derive(Deserialize, Default)]
#[serde(default)]
pub struct BodyPeek<'a> {
    /// `model`.
    #[serde(borrow)]
    pub model: Option<Cow<'a, str>>,
    /// `prompt_cache_key`.
    #[serde(borrow)]
    pub prompt_cache_key: Option<Cow<'a, str>>,
    /// `previous_response_id` (Responses).
    #[serde(borrow)]
    pub previous_response_id: Option<Cow<'a, str>>,
    /// `store`.
    pub store: Option<bool>,
    /// `tools`.
    pub tools: Option<Vec<IgnoredAny>>,
    /// Responses `text.format`.
    #[serde(borrow)]
    text: Option<TextPeek<'a>>,
    /// Chat `response_format`.
    #[serde(borrow)]
    response_format: Option<FormatPeek<'a>>,
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

impl BodyPeek<'_> {
    /// The request offers tools.
    pub fn has_tools(&self) -> bool {
        self.tools.as_ref().is_some_and(|t| !t.is_empty())
    }

    /// It asks for structured output (`json_schema` / `json_object`).
    pub fn structured(&self) -> bool {
        self.text
            .as_ref()
            .and_then(|t| t.format.as_ref())
            .or(self.response_format.as_ref())
            .and_then(|f| f.kind.as_deref())
            .is_some_and(|k| matches!(k, "json_schema" | "json_object"))
    }
}

/// Peek a JSON body (`None`-filled when it is not an object).
pub fn peek(body: &[u8]) -> BodyPeek<'_> {
    serde_json::from_slice(body).unwrap_or_default()
}

/// Peek a parsed body.
pub fn peek_value(body: &serde_json::Value) -> BodyPeek<'_> {
    BodyPeek::deserialize(body).unwrap_or_default()
}

/// Agent kind, request kind and the label the row keeps as its source.
fn agent_of(
    harness: RequestHarness,
    headers: &HeaderMap,
    has_tools: bool,
) -> (AgentKind, RequestKind, Option<String>) {
    let by_tools = if has_tools {
        AgentKind::Main
    } else {
        AgentKind::Background
    };
    let kind_of = |a: AgentKind| match a {
        AgentKind::Background => RequestKind::Other,
        _ => RequestKind::Turn,
    };
    match harness {
        RequestHarness::ZCode => {
            let source = header(headers, "x-zcode-query-source").map(str::to_ascii_lowercase);
            let session = header(headers, "x-zcode-session-type").map(str::to_ascii_lowercase);
            let agent = match session.as_deref() {
                Some("main") => AgentKind::Main,
                Some("subagent") => AgentKind::Subagent,
                Some(_) => AgentKind::Background,
                None => by_tools,
            };
            let (agent, kind) = match source.as_deref() {
                Some("compact") => (AgentKind::Background, RequestKind::Compaction),
                Some("main_turn") | None => (agent, kind_of(agent)),
                Some(_) => (AgentKind::Background, RequestKind::Other),
            };
            (agent, kind, source.or(session))
        }
        RequestHarness::WorkBuddy => {
            let t = header(headers, "x-agent-type").map(str::to_ascii_lowercase);
            let agent = match t.as_deref() {
                Some("main") => AgentKind::Main,
                Some(_) => AgentKind::Subagent,
                None => by_tools,
            };
            (agent, kind_of(agent), t)
        }
        _ => {
            let parent = header(headers, "x-opencode-parent-session-id")
                .or_else(|| header(headers, "x-parent-session-id"));
            let agent = if parent.is_some() {
                AgentKind::Subagent
            } else {
                by_tools
            };
            (agent, kind_of(agent), None)
        }
    }
}

/// The facts of a request on the ingress speaking `protocol`, whose body is
/// `body` (peeked).
pub fn of_request(
    headers: &HeaderMap,
    body: &BodyPeek<'_>,
    protocol: OpenAiProtocol,
) -> RequestMeta {
    let (harness, harness_version) = attribution(headers, RequestHarness::Unknown);
    let prompt_cache_key = short(body.prompt_cache_key.as_deref());
    let session_id = session_headers(harness)
        .iter()
        .find_map(|h| header(headers, h))
        .map(str::to_owned)
        .or_else(|| prompt_cache_key.clone());
    let turn_id = match harness {
        RequestHarness::ZCode => header(headers, "x-zcode-trace-id"),
        RequestHarness::WorkBuddy => header(headers, "x-conversation-request-id")
            .or_else(|| header(headers, "x-root-request-id")),
        _ => None,
    }
    .map(str::to_owned);
    let (agent_kind, request_kind, source) = agent_of(harness, headers, body.has_tools());
    let model = short(body.model.as_deref());
    RequestMeta {
        originator: None,
        client: RequestClient::Other,
        thread_id: session_id.clone(),
        turn_id,
        request_kind: Some(request_kind),
        thread_source: source,
        requested_model: model.clone(),
        model,
        service_tier: None,
        prompt_cache_key,
        continuation: body
            .previous_response_id
            .as_deref()
            .is_some_and(|p| !p.is_empty()),
        prewarm: false,
        structured_output: body.structured(),
        transport: RequestTransport::Http,
        stripped: 0,
        codex_host: header(headers, crate::forward::CODEX_HOST_HEADER).map(str::to_owned),
        compaction: Default::default(),
        harness,
        harness_version,
        protocol: protocol.request_protocol(),
        session_id,
        agent_kind: Some(agent_kind),
    }
}

/// The facts of the headers alone (a body that could not be read).
pub fn from_headers(headers: &HeaderMap, protocol: OpenAiProtocol) -> RequestMeta {
    of_request(headers, &BodyPeek::default(), protocol)
}
