//! The OpenAI ingress for the other coding tools (dsh, opencode, pi, ZCode,
//! WorkBuddy and other OpenAI-compatible clients): OpenAI Responses and Chat
//! Completions under `/openai/v1` on the router's own port
//! (`baseURL = http://127.0.0.1:<port>/openai/v1`).
//!
//! | Path | |
//! |---|---|
//! | `POST /openai/v1/responses` | streamed or not, through the router's Responses path ([`crate::proxy`]): failover, stickiness, health, limits, encrypted-content handling, request log and the Responses stats line |
//! | `POST /openai/v1/chat/completions` | streamed or not ([`chat`]), with its own stats line ([`tap`]) |
//! | `GET /openai/v1/models[/<id>]` | the members' models, OpenAI shape ([`models`]) |
//!
//! Native protocols pass through unchanged. Enabled IR conversions let members
//! serve additional client protocols ([`config`], [`route`]); unavailable routes
//! carry [`superba_core::OpenAiIneligible`]. A Responses
//! WebSocket is not served here (no tool needs one: they all default to HTTP
//! streaming) and is answered 426, so a client falls back to HTTP.
//!
//! Auth: the router token, or the tool's own harness token
//! ([`crate::token::harness_token`]), as a bearer or as `x-api-key` (WorkBuddy
//! sends both); neither ever reaches a member. The token decides which tool a
//! request-log row belongs to; the `User-Agent` is the fallback ([`meta`]).
//!
//! Request shaping ([`shape`]): the private `dsh_*` fields are left out,
//! `store: false` is added to Responses requests that do not say, and
//! fields a member refuses are left out for it.

pub mod config;
pub mod line;
pub mod meta;
pub mod shape;
pub mod tap;

mod body;
mod chat;
mod models;
pub(crate) mod route;
pub(crate) mod rt;

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use bytes::Bytes;
use http::header::CONTENT_ENCODING;
use http::{HeaderMap, Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, Limited};
use hyper::body::Incoming;

pub use config::{OpenAiConfig, OpenAiEndpoint, OpenAiMember, OpenAiModel, OpenAiProtocol};

use crate::forward::router_error;
use crate::limits::Admission;
use crate::state::{RespBody, Shared, lock};
use crate::stats::meta::RequestMeta;

/// The ingress prefix.
pub const PREFIX: &str = "/openai/v1";

/// The base URL a tool is given for a router listening on `listen` (it
/// appends `/responses`, `/chat/completions` or `/models`).
pub fn base_url(listen: std::net::SocketAddr) -> String {
    format!("http://{listen}{PREFIX}")
}

/// Ingress state kept across reloads.
#[derive(Debug, Default)]
pub(crate) struct OpenAiState {
    /// Fetched model lists.
    pub catalog: crate::claude::catalog::Catalog,
    /// Extra fields each member refused on Chat Completions, by member id.
    refused: Mutex<HashMap<String, BTreeSet<String>>>,
}

impl OpenAiState {
    /// The fields member `id` refused.
    pub fn refused_fields(&self, id: &str) -> BTreeSet<String> {
        lock(&self.refused).get(id).cloned().unwrap_or_default()
    }

    /// Member `id` refused `field`: leave it out from now on.
    pub fn refuse_field(&self, id: &str, field: &str) {
        lock(&self.refused)
            .entry(id.to_owned())
            .or_default()
            .insert(field.to_owned());
    }
}

/// The path below [`PREFIX`] (`/responses`), when `path` is on the ingress.
pub(crate) fn ingress_rest(path: &str) -> Option<String> {
    let rest = path.strip_prefix(PREFIX)?;
    (rest.is_empty() || rest.starts_with('/')).then(|| rest.to_owned())
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
    let path = rest.trim_end_matches('/').to_owned();
    let method = req.method().clone();
    if path == "/responses" && crate::ws::is_upgrade(&req) {
        shared
            .totals
            .ws_refused
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return router_error(
            StatusCode::UPGRADE_REQUIRED,
            "websocket_unavailable",
            &format!("{PREFIX}/responses is served over HTTP only; send the request over HTTP"),
        );
    }
    match (&method, path.as_str()) {
        (&Method::GET, "/models") => {
            models::list(&shared, crate::forward::bound_member(req.headers())).await
        }
        (&Method::GET, p) if p.starts_with("/models/") => {
            models::one(
                &shared,
                &p["/models/".len()..],
                crate::forward::bound_member(req.headers()),
            )
            .await
        }
        (&Method::POST, "/responses" | "/responses/compact") => {
            crate::proxy::proxy_http(
                shared,
                req,
                path.clone(),
                admission,
                crate::proxy::Ingress::OpenAi,
            )
            .await
        }
        (&Method::POST, "/chat/completions") => {
            let limit = shared.current().config.tuning.max_body_bytes;
            let (parts, body) = req.into_parts();
            let body = match read_body(body, limit).await {
                Ok(b) => b,
                Err(resp) => return *resp,
            };
            chat::chat(shared, parts, body, admission, t0, started_ms).await
        }
        _ => router_error(
            StatusCode::NOT_FOUND,
            "not_found",
            &format!(
                "unknown path {PREFIX}{path}; this ingress serves {PREFIX}/responses, {PREFIX}/chat/completions and {PREFIX}/models"
            ),
        ),
    }
}

/// Read a request body of at most `limit` bytes.
async fn read_body(body: Incoming, limit: usize) -> Result<Bytes, Box<Response<RespBody>>> {
    match Limited::new(body, limit).collect().await {
        Ok(c) => Ok(c.to_bytes()),
        Err(e) if e.is::<http_body_util::LengthLimitError>() => Err(Box::new(router_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request_too_large",
            &format!("the request body exceeds {limit} bytes"),
        ))),
        Err(_) => Err(Box::new(router_error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "the request body could not be read",
        ))),
    }
}

fn invalid_json() -> Box<Response<RespBody>> {
    Box::new(router_error(
        StatusCode::BAD_REQUEST,
        "bad_request",
        "the request body must be a valid JSON object",
    ))
}

/// Inspect every Codex Responses body, even without statistics. Keep its
/// original bytes/encoding when no private fields occur; otherwise forward
/// the shaped identity body. Unreadable encodings fail closed.
pub(crate) fn strip_private_request(
    headers: &mut HeaderMap,
    body: Bytes,
    limit: usize,
) -> Result<Bytes, Box<Response<RespBody>>> {
    let mut decoded_headers = headers.clone();
    let plain = body::decode(&mut decoded_headers, body.clone(), limit)?;
    crate::stats::strip::split_object(&plain).ok_or_else(invalid_json)?;
    if let Some(shaped) = shape::shape_top(&plain, &shape::is_private_field, false) {
        *headers = decoded_headers;
        Ok(Bytes::from(shaped.body))
    } else {
        Ok(body)
    }
}

/// Parse an already decoded JSON object.
fn parse_body(body: &[u8]) -> Result<serde_json::Value, Box<Response<RespBody>>> {
    let value: serde_json::Value = serde_json::from_slice(body).map_err(|_| invalid_json())?;
    if !value.is_object() {
        return Err(invalid_json());
    }
    Ok(value)
}

/// A Responses request on the ingress, as the Responses path sends it on:
/// earlier stats lines stripped, the private fields left out, `store: false`
/// added when the tool did not say ([`shape`]), and its facts. Supported
/// compressed bodies are decoded and sent as identity; an unreadable body
/// is refused locally so private fields can never bypass shaping.
pub(crate) fn prepare_responses(
    headers: &mut HeaderMap,
    body: Bytes,
    limit: usize,
) -> Result<(Bytes, RequestMeta, Vec<String>), Box<Response<RespBody>>> {
    let plain = body::decode(headers, body, limit)?;
    // Validate without reserializing the remaining values, including large
    // numbers and duplicate keys. Malformed JSON must never pass unshaped.
    crate::stats::strip::split_object(&plain).ok_or_else(invalid_json)?;
    let peek = meta::peek(&plain);
    let mut m = meta::of_request(headers, &peek, OpenAiProtocol::Responses);
    m.compaction = crate::compaction::inspect(&plain);
    let add_store = peek.store.is_none() && !m.continuation;
    drop(peek);
    let mut notes = Vec::new();
    let mut out = plain;
    if let Some((new, n)) = crate::stats::strip::strip(&out) {
        m.stripped = n;
        out = Bytes::from(new);
    }
    if let Some(shaped) = shape::shape_top(&out, &shape::is_private_field, add_store) {
        if !shaped.removed.is_empty() {
            notes.push(format!(
                "{} left out (never sent upstream)",
                shaped.removed.join(", ")
            ));
        }
        if shaped.store_added {
            notes.push("store: false added (the tool did not say)".to_owned());
        }
        out = Bytes::from(shaped.body);
    }
    Ok((out, m, notes))
}

/// The body for member `member` on the Responses path: without the fields it
/// is configured to refuse (identity bodies; `None`: unchanged).
pub(crate) fn member_responses_body(
    generation: &crate::state::Generation,
    member: usize,
    body: &Bytes,
    headers: &HeaderMap,
) -> Option<(Bytes, Vec<String>)> {
    let fields = &generation.openai.members[member].cfg.drop_fields;
    if fields.is_empty() || headers.contains_key(CONTENT_ENCODING) {
        return None;
    }
    let shaped = shape::shape_top(body, &|k| fields.iter().any(|f| f == k), false)?;
    Some((Bytes::from(shaped.body), shaped.removed))
}
