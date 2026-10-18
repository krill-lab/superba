//! The reply stats line on the Claude Code ingress (multi-harness §3).
//!
//! - **Who**: Claude Code (by token or `User-Agent`) while its harness switch
//!   is on, the main conversation only (never subagents, titles, compaction
//!   or other background calls), never structured output.
//! - **Where**: the final assistant message of a turn (`stop_reason`
//!   `end_turn`, never `tool_use`): appended to its last text block (a new
//!   text block when the answer ends without one). Thinking,
//!   redacted_thinking and tool_use blocks are never touched.
//! - **Strip**: every later request has the line removed from its assistant
//!   text blocks (the same marker as Codex: `⚡` + U+2060), before any
//!   translation; a block holding nothing else is removed.

use serde_json::{Value, json};
use superba_core::{AgentKind, RequestHarness};

use crate::stats::StatsLineConfig;
use crate::stats::meta::RequestMeta;
use crate::stats::strip::{may_contain_line, strip_text};

/// The line applies to this request.
pub fn allowed(line: &StatsLineConfig, meta: &RequestMeta) -> bool {
    meta.harness == RequestHarness::ClaudeCode
        && line.harnesses.contains(&RequestHarness::ClaudeCode)
        && meta.agent_kind == Some(AgentKind::Main)
        && !meta.structured_output
}

/// Remove earlier stats lines from the assistant text blocks of a Messages
/// request; returns how many were removed.
pub fn strip_request(req: &mut Value) -> u32 {
    let Some(messages) = req.get_mut("messages").and_then(Value::as_array_mut) else {
        return 0;
    };
    let mut removed = 0;
    for m in messages.iter_mut() {
        if m.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(content) = m.get_mut("content") else {
            continue;
        };
        match content {
            Value::String(s) => {
                if may_contain_line(s.as_bytes())
                    && let Some(kept) = strip_text(s)
                {
                    *s = kept.to_owned();
                    removed += 1;
                }
            }
            Value::Array(blocks) => {
                let before = blocks.len();
                let mut changed = 0;
                blocks.retain_mut(|b| {
                    if b.get("type").and_then(Value::as_str) != Some("text") {
                        return true;
                    }
                    let Some(Value::String(text)) = b.get_mut("text") else {
                        return true;
                    };
                    if !may_contain_line(text.as_bytes()) {
                        return true;
                    }
                    match strip_text(text) {
                        Some(kept) => {
                            changed += 1;
                            let kept = kept.to_owned();
                            if kept.trim().is_empty() {
                                return false;
                            }
                            *text = kept;
                            true
                        }
                        None => true,
                    }
                });
                if blocks.is_empty() && before > 0 {
                    blocks.push(json!({"type": "text", "text": "."}));
                }
                removed += changed;
            }
            _ => {}
        }
    }
    removed
}

/// Append `suffix` to the final answer of a non-streamed Messages answer
/// (its last text block, or a new one). Only for `end_turn`.
pub fn inject_json(answer: &mut Value, suffix: &str) -> bool {
    if answer.get("stop_reason").and_then(Value::as_str) != Some("end_turn") {
        return false;
    }
    let Some(content) = answer.get_mut("content").and_then(Value::as_array_mut) else {
        return false;
    };
    if let Some(Value::Object(last)) = content.last_mut()
        && last.get("type").and_then(Value::as_str) == Some("text")
        && let Some(Value::String(text)) = last.get_mut("text")
    {
        text.push_str(suffix);
        return true;
    }
    content.push(json!({"type": "text", "text": suffix}));
    true
}
