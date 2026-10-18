//! A local estimate of a Messages request's input tokens, for
//! `/v1/messages/count_tokens` when the upstream cannot count (an
//! Anthropic-compatible endpoint without the route).
//!
//! Rough on purpose: about four characters per token for ASCII text, one per
//! token for other scripts (CJK), a fixed cost per image and document, plus
//! the tool definitions as JSON. Claude Code uses the number for context
//! bookkeeping, where a close estimate beats an error.

use serde_json::Value;

/// Tokens counted for one image.
const IMAGE_TOKENS: u64 = 1_600;
/// Tokens counted for one document.
const DOCUMENT_TOKENS: u64 = 3_000;
/// Overhead per message.
const MESSAGE_TOKENS: u64 = 4;

/// Estimated tokens of `text`.
pub fn text_tokens(text: &str) -> u64 {
    let (mut ascii, mut other) = (0u64, 0u64);
    for c in text.chars() {
        if c.is_ascii() {
            ascii += 1;
        } else {
            other += 1;
        }
    }
    ascii.div_ceil(4) + other
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn content_tokens(content: Option<&Value>) -> u64 {
    let blocks = match content {
        Some(Value::String(s)) => return text_tokens(s),
        Some(Value::Array(blocks)) => blocks,
        _ => return 0,
    };
    blocks
        .iter()
        .map(|b| match str_of(b, "type") {
            "text" => text_tokens(str_of(b, "text")),
            "thinking" => text_tokens(str_of(b, "thinking")),
            "image" => IMAGE_TOKENS,
            "document" => DOCUMENT_TOKENS,
            "tool_use" => {
                text_tokens(str_of(b, "name"))
                    + b.get("input")
                        .map(|i| text_tokens(&i.to_string()))
                        .unwrap_or(0)
            }
            "tool_result" => content_tokens(b.get("content")),
            _ => 0,
        })
        .sum()
}

/// Estimated input tokens of a Messages request (`count_tokens` body).
pub fn input_tokens(req: &Value) -> u64 {
    let mut n = content_tokens(req.get("system"));
    for m in req
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        n += MESSAGE_TOKENS + content_tokens(m.get("content"));
    }
    for t in req
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        n += text_tokens(&t.to_string());
    }
    n.max(1)
}
