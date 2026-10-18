//! The Claude Code ingress: Anthropic Messages under `/claude` on the
//! router's own port (`ANTHROPIC_BASE_URL = http://127.0.0.1:<port>/claude`).
//!
//! | Path | |
//! |---|---|
//! | `POST /claude/v1/messages` | routed per model ([`route`]) to members and passed through to their Anthropic endpoints ([`passthrough`]), streamed or not |
//! | `POST /claude/v1/messages/count_tokens` | the upstream's count when it has one, else a local estimate |
//! | `GET /claude/v1/models[/<id>]` | the union of the routes' models and the members' model lists, Anthropic shape |
//!
//! Auth: the router token (or a harness token, [`crate::token`]) as a bearer
//! (`ANTHROPIC_AUTH_TOKEN`) or as `x-api-key`. Failover, stickiness, health,
//! limits and the request log are the router's: one attempt per line before
//! the first byte, on connect errors, 502/503/504/529 and edge interceptions.
//! Codex's routes are not affected.
//!
//! Only members with an Anthropic Messages endpoint are eligible
//! ([`superba_core::ClaudeIneligible`]); there is no protocol conversion.

pub mod catalog;
pub mod config;
pub(crate) mod error;
pub mod line;
pub mod meta;
pub mod tap;

mod count;
pub mod estimate;
pub(crate) mod events;
mod messages;
mod models;
pub(crate) mod passthrough;
pub(crate) mod route;
pub(crate) mod rt;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use bytes::Bytes;
use http::{Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, Limited};
use hyper::body::Incoming;

pub use config::{
    AnthropicAuth, BetaPolicy, ClaudeConfig, ClaudeMember, ClaudeModel, ClaudeRoute, ClaudeTarget,
    Endpoint, ModelAlias,
};

use crate::limits::Admission;
use crate::state::{RespBody, Shared};

/// The ingress prefix.
pub const PREFIX: &str = "/claude";

/// Claude Code's `ANTHROPIC_API_KEY` header (the router token may come here).
pub(crate) const API_KEY_HEADER: &str = "x-api-key";

/// Ingress state kept across reloads.
#[derive(Debug, Default)]
pub(crate) struct ClaudeState {
    /// Fetched model lists.
    pub catalog: catalog::Catalog,
    /// Members whose Anthropic endpoint has no `count_tokens`, until when.
    pub no_count: Mutex<HashMap<String, Instant>>,
}

/// The path below [`PREFIX`] (`/v1/messages`), when `path` is on the ingress.
pub(crate) fn ingress_rest(path: &str) -> Option<String> {
    let rest = path.strip_prefix(PREFIX)?;
    (rest.is_empty() || rest.starts_with('/')).then(|| rest.to_owned())
}

/// The `ANTHROPIC_BASE_URL` of a router listening on `listen`.
pub fn base_url(listen: std::net::SocketAddr) -> String {
    format!("http://{listen}{PREFIX}")
}

/// Serve one authenticated request on the ingress.
pub(crate) async fn handle(
    shared: Arc<Shared>,
    req: Request<Incoming>,
    rest: String,
    admission: Admission,
) -> Response<RespBody> {
    let t0 = Instant::now();
    let started_ms = crate::stats::hub::now_ms();
    let path = rest.trim_end_matches('/');
    let method = req.method().clone();
    match (&method, path) {
        (&Method::GET, "/v1/models") => models::list(&shared, req.uri().query()).await,
        (&Method::GET, p) if p.starts_with("/v1/models/") => {
            models::one(&shared, &p["/v1/models/".len()..]).await
        }
        (&Method::POST, "/v1/messages" | "/v1/messages/count_tokens") => {
            let limit = shared.current().config.tuning.max_body_bytes;
            let (parts, body) = req.into_parts();
            let body = match read_body(body, limit).await {
                Ok(b) => b,
                Err(resp) => return *resp,
            };
            if path.ends_with("count_tokens") {
                drop(admission);
                count::count(&shared, &parts, body).await
            } else {
                messages::messages(shared, parts, body, admission, t0, started_ms).await
            }
        }
        _ => error::local(
            StatusCode::NOT_FOUND,
            error::Kind::NotFound,
            &format!(
                "unknown path {PREFIX}{path}; the Claude Code ingress serves /v1/messages, /v1/messages/count_tokens and /v1/models"
            ),
        ),
    }
}

/// Read a request body of at most `limit` bytes.
async fn read_body(body: Incoming, limit: usize) -> Result<Bytes, Box<Response<RespBody>>> {
    match Limited::new(body, limit).collect().await {
        Ok(c) => Ok(c.to_bytes()),
        Err(e) if e.is::<http_body_util::LengthLimitError>() => Err(Box::new(error::local(
            StatusCode::PAYLOAD_TOO_LARGE,
            error::Kind::RequestTooLarge,
            &format!("the request body exceeds {limit} bytes"),
        ))),
        Err(_) => Err(Box::new(error::local(
            StatusCode::BAD_REQUEST,
            error::Kind::InvalidRequest,
            "the request body could not be read",
        ))),
    }
}

/// Parse a JSON request body (zstd-encoded bodies decoded).
pub(crate) fn parse_body(
    headers: &http::HeaderMap,
    body: &[u8],
    limit: usize,
) -> Result<serde_json::Value, Box<Response<RespBody>>> {
    let encoding = headers
        .get(http::header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty() && v != "identity");
    let decoded;
    let plain: &[u8] = match encoding.as_deref() {
        None => body,
        Some("zstd") => match crate::stats::strip::decode_zstd(body, limit) {
            Some(d) => {
                decoded = d;
                &decoded
            }
            None => {
                return Err(Box::new(error::local(
                    StatusCode::BAD_REQUEST,
                    error::Kind::InvalidRequest,
                    "the zstd request body could not be decoded",
                )));
            }
        },
        Some(other) => {
            return Err(Box::new(error::local(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                error::Kind::InvalidRequest,
                &format!("request bodies encoded as {other} are not supported"),
            )));
        }
    };
    serde_json::from_slice(plain).map_err(|_| {
        Box::new(error::local(
            StatusCode::BAD_REQUEST,
            error::Kind::InvalidRequest,
            "the request body is not valid JSON",
        ))
    })
}
