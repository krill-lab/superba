//! The router's own answers on the Claude Code ingress, in the Anthropic error
//! shape (`{"type":"error","error":{"type","message"}}`), with types Claude
//! Code's retry logic understands: it retries 408, 409, 429, 5xx and 529
//! (`overloaded_error`), after `Retry-After`.

use bytes::Bytes;
use http::header::{CONTENT_TYPE, RETRY_AFTER};
use http::{HeaderValue, Response, StatusCode};
use serde_json::{Value, json};

use crate::forward::full;
use crate::state::RespBody;

/// Anthropic error types the router answers with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// 400.
    InvalidRequest,
    /// 401.
    Authentication,
    /// 404.
    NotFound,
    /// 413.
    RequestTooLarge,
    /// 5xx.
    Api,
    /// 529.
    Overloaded,
}

impl Kind {
    /// The `error.type` value.
    pub fn name(self) -> &'static str {
        match self {
            Kind::InvalidRequest => "invalid_request_error",
            Kind::Authentication => "authentication_error",
            Kind::NotFound => "not_found_error",
            Kind::RequestTooLarge => "request_too_large",
            Kind::Api => "api_error",
            Kind::Overloaded => "overloaded_error",
        }
    }
}

/// The error body.
pub fn body(kind: Kind, message: &str) -> Value {
    json!({"type": "error", "error": {"type": kind.name(), "message": message}})
}

/// A JSON answer with `status`.
pub(crate) fn json_response(status: StatusCode, body: &Value) -> Response<RespBody> {
    let mut resp = Response::new(full(Bytes::from(body.to_string())));
    *resp.status_mut() = status;
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    resp
}

/// The router's own refusal.
pub(crate) fn local(status: StatusCode, kind: Kind, message: &str) -> Response<RespBody> {
    json_response(
        status,
        &body(kind, &format!("Superba local router: {message}")),
    )
}

/// The answer to a request that got no admission (or met "too many open
/// files"): 529 `overloaded_error`, which Claude Code retries after
/// `Retry-After`; `Connection: close` gives the descriptor back at once.
pub(crate) fn local_busy(message: &str) -> Response<RespBody> {
    let mut resp = local(
        StatusCode::from_u16(529).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
        Kind::Overloaded,
        message,
    );
    let h = resp.headers_mut();
    h.insert(http::header::CONNECTION, HeaderValue::from_static("close"));
    h.insert(
        RETRY_AFTER,
        HeaderValue::from(crate::limits::RETRY_AFTER_SECS),
    );
    resp
}
