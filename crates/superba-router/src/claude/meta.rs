//! Request facts of a Claude Code request for the request log, stickiness and
//! the stats line.
//!
//! | Fact | Source |
//! |---|---|
//! | harness, version | the router token's harness, else the `User-Agent` (`claude-cli/<version>`), else unknown |
//! | session | `X-Claude-Code-Session-Id`, else `metadata.user_id` (`session_id` in its JSON, or the older `…_session_<id>`) |
//! | turn | `x-claude-code-prompt-id` (sent with `CLAUDE_CODE_GATEWAY_HINT_HEADERS=1`) |
//! | agent | `x-claude-code-request-class` (`main`, `subagent`, `workflow`, `compaction`, `auxiliary`, `side_reply`), else `x-claude-code-agent-id` (subagent), `x-app: cli-bg` (background), else main when the request offers tools |

use http::HeaderMap;
use serde_json::Value;
use superba_core::{AgentKind, RequestClient, RequestHarness, RequestKind, RequestProtocol};

use crate::stats::meta::{RequestMeta, attribution};

/// Claude Code's session header.
pub const SESSION_HEADER: &str = "x-claude-code-session-id";
/// Claude Code's prompt (turn) header.
pub const PROMPT_HEADER: &str = "x-claude-code-prompt-id";
/// Claude Code's request class header.
pub const REQUEST_CLASS_HEADER: &str = "x-claude-code-request-class";
/// Claude Code's subagent header.
pub const AGENT_HEADER: &str = "x-claude-code-agent-id";

/// Longest id kept.
const MAX_ID_LEN: usize = 256;

fn header<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty() && v.len() <= MAX_ID_LEN)
}

/// The session id in `metadata.user_id`.
fn session_of_user_id(body: &Value) -> Option<String> {
    let user = body.get("metadata")?.get("user_id")?.as_str()?;
    if let Ok(v) = serde_json::from_str::<Value>(user)
        && let Some(s) = v.get("session_id").and_then(Value::as_str)
    {
        return Some(s.to_owned()).filter(|s| !s.is_empty() && s.len() <= MAX_ID_LEN);
    }
    let (_, s) = user.rsplit_once("_session_")?;
    Some(s.to_owned()).filter(|s| !s.is_empty() && s.len() <= MAX_ID_LEN)
}

/// The request asks for structured output (`output_config.format` or the
/// older `output_format` with a JSON schema).
fn structured(body: &Value) -> bool {
    body.get("output_config")
        .and_then(|c| c.get("format"))
        .or_else(|| body.get("output_format"))
        .and_then(|f| f.get("type"))
        .and_then(Value::as_str)
        == Some("json_schema")
}

/// The facts of a Messages request asking for `requested`, served as `model`.
pub fn claude_meta(headers: &HeaderMap, body: &Value, requested: &str, model: &str) -> RequestMeta {
    let (harness, harness_version) = attribution(headers, RequestHarness::Unknown);
    let session_id = header(headers, SESSION_HEADER)
        .map(str::to_owned)
        .or_else(|| session_of_user_id(body));
    let class = header(headers, REQUEST_CLASS_HEADER).map(str::to_ascii_lowercase);
    let has_tools = body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|t| !t.is_empty());
    let agent_kind = match class.as_deref() {
        Some("main") => AgentKind::Main,
        Some("subagent" | "workflow") => AgentKind::Subagent,
        Some(_) => AgentKind::Background,
        None if header(headers, AGENT_HEADER).is_some() => AgentKind::Subagent,
        None if header(headers, "x-app") == Some("cli-bg") => AgentKind::Background,
        None if has_tools => AgentKind::Main,
        None => AgentKind::Background,
    };
    let request_kind = match (class.as_deref(), agent_kind) {
        (Some("compaction"), _) => RequestKind::Compaction,
        (_, AgentKind::Main | AgentKind::Subagent) => RequestKind::Turn,
        _ => RequestKind::Other,
    };
    RequestMeta {
        originator: None,
        client: RequestClient::Other,
        thread_id: session_id.clone(),
        turn_id: header(headers, PROMPT_HEADER).map(str::to_owned),
        request_kind: Some(request_kind),
        thread_source: class,
        model: Some(model.to_owned()),
        service_tier: None,
        prompt_cache_key: None,
        continuation: false,
        prewarm: false,
        structured_output: structured(body),
        transport: superba_core::RequestTransport::Http,
        stripped: 0,
        codex_host: header(headers, crate::forward::CODEX_HOST_HEADER).map(str::to_owned),
        compaction: Default::default(),
        harness,
        harness_version,
        protocol: RequestProtocol::Messages,
        requested_model: Some(requested.to_owned()),
        session_id,
        agent_kind: Some(agent_kind),
    }
}
