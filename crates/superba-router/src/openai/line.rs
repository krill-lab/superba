//! The reply stats line on the OpenAI ingress (multi-harness §3).
//!
//! - **Who**: a tool (by token or `User-Agent`) whose harness switch is on —
//!   off by default for dsh, opencode, pi, ZCode and WorkBuddy
//!   ([`crate::stats::default_line_harnesses`]); the main conversation only
//!   (never subagents, titles, compaction or other background calls), never
//!   structured output. Codex and Claude Code get it on their own ingresses.
//! - **Responses**: the Responses tap's own injection (final answer message,
//!   streamed answers).
//! - **Chat Completions**: appended to the final `delta.content` before
//!   `finish_reason: "stop"` ([`super::tap`]), never when the answer calls
//!   tools; a whole answer gets it at the end of its assistant message.
//! - **Strip**: every later request has the line removed from its assistant
//!   messages (Responses `output_text`, Chat `content` strings and text
//!   parts; the same marker as Codex: `⚡` + U+2060), on and off alike.

use serde_json::Value;
use superba_core::{AgentKind, RequestHarness};

use crate::stats::StatsLineConfig;
use crate::stats::meta::RequestMeta;

/// The line applies to this request.
pub fn allowed(line: &StatsLineConfig, meta: &RequestMeta) -> bool {
    !matches!(
        meta.harness,
        RequestHarness::Codex | RequestHarness::ClaudeCode
    ) && line.harnesses.contains(&meta.harness)
        && meta.agent_kind == Some(AgentKind::Main)
        && !meta.structured_output
}

/// Remove earlier stats lines from the assistant messages of a Chat
/// Completions request (`content` as a string or as text parts); returns how
/// many were removed. The message shapes are those of Anthropic Messages.
pub fn strip_request(req: &mut Value) -> u32 {
    crate::claude::line::strip_request(req)
}

/// Append `suffix` to the answer of a whole Chat Completions answer: one
/// choice, `finish_reason: "stop"`, no tool calls, text content.
pub fn inject_json(answer: &mut Value, suffix: &str) -> bool {
    let Some([choice]) = answer
        .get_mut("choices")
        .and_then(Value::as_array_mut)
        .map(Vec::as_mut_slice)
    else {
        return false;
    };
    if choice.get("finish_reason").and_then(Value::as_str) != Some("stop") {
        return false;
    }
    let Some(message) = choice.get_mut("message") else {
        return false;
    };
    let calls = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|c| !c.is_empty())
        || message.get("function_call").is_some_and(|f| !f.is_null());
    if calls {
        return false;
    }
    match message.get_mut("content") {
        Some(Value::String(text)) => {
            text.push_str(suffix);
            true
        }
        _ => false,
    }
}
