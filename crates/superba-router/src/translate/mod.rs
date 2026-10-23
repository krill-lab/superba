//! An IR hub: each protocol has one codec, with no pairwise translators.
//! Native requests bypass this module entirely.

pub mod anthropic;
pub(crate) mod body;
pub mod chat;
pub mod gemini;
mod gemini_args;
pub mod ir;
pub mod responses;
pub mod routing;
pub mod stream;
pub(crate) mod ws;

use ir::{Protocol, Request, Response};
use serde_json::Value;

/// Invalid or unrepresentable input. Messages contain field names, never
/// request content, credentials, or raw upstream responses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invalid(pub String);

impl Invalid {
    /// Create an error using a description that contains no request content.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for Invalid {}

/// Decode and resolve function call identities in a native request.
pub fn decode_request(protocol: Protocol, body: &Value) -> Result<Request, Invalid> {
    let mut request = match protocol {
        Protocol::Messages => anthropic::decode_request(body),
        Protocol::Responses => responses::decode_request(body),
        Protocol::Chat => chat::decode_request(body),
        Protocol::Gemini => gemini::decode_request(body),
    }?;
    request.resolve_tools()?;
    Ok(request)
}

/// Encode the shared request in the selected protocol.
pub fn encode_request(protocol: Protocol, request: &Request) -> Result<Value, Invalid> {
    match protocol {
        Protocol::Messages => anthropic::encode_request(request),
        Protocol::Responses => responses::encode_request(request),
        Protocol::Chat => chat::encode_request(request),
        Protocol::Gemini => gemini::encode_request(request),
    }
}

/// Decode a completed native response.
pub fn decode_response(protocol: Protocol, body: &Value) -> Result<Response, Invalid> {
    match protocol {
        Protocol::Messages => anthropic::decode_response(body),
        Protocol::Responses => responses::decode_response(body),
        Protocol::Chat => chat::decode_response(body),
        Protocol::Gemini => gemini::decode_response(body),
    }
}

/// Encode a completed response in the selected protocol.
pub fn encode_response(protocol: Protocol, response: &Response) -> Result<Value, Invalid> {
    match protocol {
        Protocol::Messages => anthropic::encode_response(response),
        Protocol::Responses => responses::encode_response(response),
        Protocol::Chat => chat::encode_response(response),
        Protocol::Gemini => gemini::encode_response(response),
    }
}

pub(crate) fn string(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}
pub(crate) fn kind(v: &Value) -> &str {
    v.get("type").and_then(Value::as_str).unwrap_or("")
}
pub(crate) fn array<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}
pub(crate) fn number(v: &Value, key: &str) -> Option<serde_json::Number> {
    v.get(key).and_then(Value::as_number).cloned()
}
pub(crate) fn optional_string(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}
pub(crate) fn strings(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => vec![],
    }
}
pub(crate) fn role(s: &str) -> Result<ir::Role, Invalid> {
    match s {
        "user" | "tool" => Ok(ir::Role::User),
        "assistant" | "model" => Ok(ir::Role::Assistant),
        "system" | "developer" => Ok(ir::Role::System),
        _ => Err(Invalid::new("unknown message role")),
    }
}
pub(crate) fn role_name(role: ir::Role) -> &'static str {
    match role {
        ir::Role::User => "user",
        ir::Role::Assistant => "assistant",
        ir::Role::System => "system",
    }
}
pub(crate) fn json_arguments(s: &str) -> Result<Value, Invalid> {
    serde_json::from_str(if s.is_empty() { "{}" } else { s })
        .map_err(|_| Invalid::new("tool arguments are not valid JSON"))
}
pub(crate) fn text_content(blocks: &[ir::Block]) -> String {
    blocks
        .iter()
        .filter_map(|b| match &b.content {
            ir::Content::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Normalize a native error envelope with its HTTP status.
pub fn decode_error(status: u16, value: &Value) -> ir::ApiError {
    let e = value.get("error").unwrap_or(value);
    let code = e
        .get("code")
        .and_then(Value::as_str)
        .or_else(|| e.get("type").and_then(Value::as_str))
        .or_else(|| e.get("status").and_then(Value::as_str))
        .unwrap_or("api_error");
    ir::ApiError {
        status,
        code: code.to_owned(),
        message: e
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Upstream request failed")
            .to_owned(),
    }
}

/// Encode a normalized error in the client's protocol.
pub fn encode_error(protocol: Protocol, e: &ir::ApiError) -> Value {
    use serde_json::json;
    match protocol {
        Protocol::Messages => {
            let kind = match e.status {
                400 | 422 => "invalid_request_error",
                401 => "authentication_error",
                403 => "permission_error",
                404 => "not_found_error",
                413 => "request_too_large",
                429 => "rate_limit_error",
                503 | 529 => "overloaded_error",
                _ => "api_error",
            };
            json!({"type":"error","error":{"type":kind,"message":e.message}})
        }
        Protocol::Gemini => {
            let status = match e.status {
                400 | 422 => "INVALID_ARGUMENT",
                401 => "UNAUTHENTICATED",
                403 => "PERMISSION_DENIED",
                404 => "NOT_FOUND",
                429 => "RESOURCE_EXHAUSTED",
                503 | 529 => "UNAVAILABLE",
                _ => "INTERNAL",
            };
            json!({"error":{"code":e.status,"status":status,"message":e.message}})
        }
        _ => json!({"error":{"type":e.code,"code":e.code,"message":e.message}}),
    }
}

/// Convert normalized IR usage to the router's statistics representation.
pub(crate) fn stats_usage(u: ir::Usage) -> Option<crate::stats::usage::Usage> {
    if u.input.is_none() && u.output.is_none() {
        return None;
    }
    Some(crate::stats::usage::Usage {
        input: u.input.unwrap_or(0),
        output: u.output.unwrap_or(0),
        cached: u.cache_read,
        cache_write: u.cache_write,
        cache_write_1h: u.cache_write_1h,
        reasoning: u.reasoning,
    })
}
