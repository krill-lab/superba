//! ChatGPT backend calls the router answers itself (PAT router mode).
//!
//! In PAT router mode Codex's `chatgpt_base_url` is the router
//! (`https://127.0.0.1:<port>/backend-api/`), so Codex and the ChatGPT app send
//! it everything they would send chatgpt.com besides model traffic: the plugin
//! service (`/ps/plugins/*`) and its hosted apps MCP server (`/ps/mcp`),
//! `/plugins/featured`, usage and analytics (`/wham/usage*`,
//! `/wham/analytics/*`), telemetry (`…/analytics-events/events`), the profile
//! and settings pages (`/me`, `/profiles/*`, `/settings/*`), managed
//! configuration (`/wham/config/bundle`) and more. None of them is a member's
//! business: a relay answers them with errors or rate limits (429), and its
//! pooled accounts' data would not describe this computer anyway. They are
//! answered here and never forwarded:
//!
//! - endpoints with a known shape get minimal valid JSON: empty lists, zeroed
//!   or absent usage, and the router's own identity ([`crate::account`]);
//! - `/ps/mcp` is an MCP server without tools;
//! - everything else gets ChatGPT's `404 {"detail":"Not Found"}`, which the
//!   app and Codex read as "not available".
//!
//! Below `/backend-api/codex` only model traffic goes to members
//! ([`upstream`]). Local answers need no router token (they hold nothing, and
//! Codex's MCP client sends none), carry `x-superba-route: local`, are counted
//! per route in the admin status (`totals.local_answers`, `local_answers`) and
//! the first answer of each route is logged. Identity endpoints stay in
//! [`crate::account`] and keep requiring the token.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use bytes::Bytes;
use http::header::{ALLOW, CACHE_CONTROL, CONTENT_TYPE};
use http::{HeaderValue, Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, Limited};
use hyper::body::Incoming;
use serde_json::{Value, json};
use superba_core::Timestamp;

use crate::account::{ACCOUNT, DISPLAY_NAME};
use crate::admin::VERSION;
use crate::forward::{empty, full, set_route};
use crate::state::{RespBody, Shared, lock};

/// Value of the route header on a locally answered call.
pub(crate) const LOCAL_ROUTE: &str = "local";

/// Codex's background notes hint for a new thread context. The notes and
/// history tools the model calls during a turn go upstream.
const THREAD_HINT: &str = "/alpha/notes/v2/thread_hint";

/// Largest MCP message read from `/ps/mcp`.
const MCP_BODY_LIMIT: usize = 1 << 20;

/// MCP versions answered as asked; any other request gets the newest.
const MCP_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// First segments of ChatGPT backend paths also answered without the
/// `/backend-api` prefix.
const BARE: &[&str] = &[
    "wham",
    "ps",
    "me",
    "profiles",
    "settings",
    "accounts",
    "plugins",
    "connectors",
];

/// Counter labels of unknown paths, by first segment ("other" otherwise).
const NAMESPACES: &[&str] = &[
    "wham/*",
    "ps/*",
    "plugins/*",
    "accounts/*",
    "profiles/*",
    "settings/*",
    "connectors/*",
    "files/*",
    "public/*",
    "aip/*",
    "subscriptions/*",
    "payments/*",
    "conversations/*",
    "sentinel/*",
];

/// How a local route answers.
#[derive(Clone, Copy, Debug)]
enum Answer {
    /// 200 with this JSON (given the query string).
    Json(fn(Option<&str>) -> Value),
    /// The hosted apps MCP server, without tools.
    Mcp,
    /// ChatGPT's `404 {"detail":"Not Found"}`.
    NotFound,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verb {
    Get,
    Post,
    Any,
}

impl Verb {
    fn allows(self, method: &Method) -> bool {
        match self {
            Verb::Get => method == Method::GET,
            Verb::Post => method == Method::POST,
            Verb::Any => true,
        }
    }
}

/// A call answered locally.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Route {
    /// Static name for counters and logs (never an id or a query).
    pub label: &'static str,
    answer: Answer,
}

impl Route {
    /// True when the answer is a 404.
    #[cfg(test)]
    fn not_found(&self) -> bool {
        matches!(self.answer, Answer::NotFound)
    }
}

use Answer::{Json, Mcp, NotFound};
use Verb::{Any, Get, Post};

/// Paths below `/backend-api` (trailing `/` removed) answered with a shape.
const EXACT: &[(Verb, &str, Answer)] = &[
    // Usage page and Codex's rate limits.
    (Get, "/wham/usage", Json(usage)),
    (
        Get,
        "/wham/usage/daily-token-usage-breakdown",
        Json(fresh_data),
    ),
    (
        Post,
        "/wham/usage/thread_usage/query_v2",
        Json(thread_usage_v2),
    ),
    (Post, "/wham/usage/thread_usage/query", Json(threads)),
    (Get, "/wham/usage/credit-usage-events", Json(data)),
    (
        Get,
        "/wham/usage/daily-workspace-user-token-usage-breakdown",
        Json(fresh_data),
    ),
    (
        Get,
        "/wham/usage/daily-workspace-user-credit-usage",
        Json(credit_usage),
    ),
    (Get, "/wham/usage/plan_limit_history", NotFound),
    (Post, "/wham/usage/credits/estimate", NotFound),
    (Get, "/wham/rate-limit-reset-credits", NotFound),
    // Analytics (a 404 on code review metrics makes the app retry many times a second).
    (
        Get,
        "/wham/analytics/daily-workspace-usage-counts",
        Json(data),
    ),
    (
        Get,
        "/wham/analytics/daily-plugin-usage-metrics",
        Json(data),
    ),
    (Get, "/wham/analytics/daily-skill-usage-metrics", Json(data)),
    (Get, "/wham/analytics/daily-code-review-metrics", Json(data)),
    (Post, "/wham/analytics-events/events", Json(accepted)),
    (Post, "/wham/statsig/bootstrap", NotFound),
    // Managed configuration, workspace messages, account and profile pages.
    (Get, "/wham/config/bundle", Json(config_bundle)),
    (Get, "/wham/workspace-messages", Json(workspace_messages)),
    (Get, "/wham/profiles/me", Json(wham_profile)),
    (Get, "/me", Json(me)),
    (Get, "/settings/is_adult", NotFound),
    (Get, "/profiles/me", Json(profile_page)),
    (Get, "/profiles/me/page", Json(profile_page)),
    (Get, "/profiles/me/page/pin_options", Json(pin_options)),
    // Plugin service, hosted apps MCP server, connector directory.
    (Get, "/ps/plugins/installed", Json(plugin_list)),
    (Get, "/ps/plugins/list", Json(plugin_list)),
    (Get, "/ps/plugins/search", Json(plugin_list)),
    (Get, "/ps/plugins/workspace/shared", Json(plugin_list)),
    (Get, "/ps/plugins/workspace/created", Json(plugin_list)),
    (Any, "/ps/mcp", Mcp),
    (Get, "/plugins/featured", Json(featured)),
    (Get, "/connectors/directory/list", Json(directory)),
    (Get, "/connectors/directory/list_workspace", Json(directory)),
];

/// Path prefixes below `/backend-api` (after [`EXACT`]), with their label.
const PREFIXED: &[(Verb, &str, &str, Answer)] = &[
    (
        Get,
        "/ps/plugins/suggested",
        "ps/plugins/suggested/*",
        Json(suggested_plugins),
    ),
    (
        Get,
        "/accounts/check",
        "accounts/check/*",
        Json(accounts_check_v4),
    ),
    (Any, "/ps/plugins", "ps/plugins/{id}", NotFound),
];

/// True when a request below `/backend-api/codex` (its remainder `rest`) is
/// model traffic for a member: Responses over HTTP and WebSocket, compaction,
/// the model catalog, and the provider endpoints a turn uses (memories,
/// images, realtime, the guardian reviewer, the hosted search, history and
/// notes tools). Everything else there (telemetry, the background notes hint)
/// is answered locally.
pub(crate) fn upstream(rest: &str) -> bool {
    let p = rest.trim_end_matches('/');
    matches!(p, "/responses" | "/responses/compact" | "/guardian")
        || ["/models", "/memories", "/images", "/realtime"]
            .iter()
            .any(|prefix| below(p, prefix).is_some())
        || (below(p, "/alpha").is_some_and(|r| !r.is_empty()) && p != THREAD_HINT)
}

/// The local answer for `path`, or `None` when the router routes it as
/// before (model traffic, its other ingresses, unknown paths).
pub(crate) fn route(method: &Method, path: &str) -> Option<Route> {
    let p = backend_path(path)?;
    if let Some(rest) = below(p, "/codex") {
        if upstream(rest) {
            return None;
        }
        return Some(match rest {
            "/analytics-events/events" if method == Method::POST => Route {
                label: "codex/analytics-events/events",
                answer: Json(accepted),
            },
            THREAD_HINT if method == Method::POST => Route {
                label: "codex/alpha/notes/v2/thread_hint",
                answer: Json(thread_hint),
            },
            _ => Route {
                label: "codex/*",
                answer: NotFound,
            },
        });
    }
    if let Some((_, path, answer)) = EXACT
        .iter()
        .find(|(verb, known, _)| *known == p && verb.allows(method))
    {
        return Some(Route {
            label: &path[1..],
            answer: *answer,
        });
    }
    if let Some((_, _, label, answer)) = PREFIXED
        .iter()
        .find(|(verb, prefix, _, _)| below(p, prefix).is_some() && verb.allows(method))
    {
        return Some(Route {
            label,
            answer: *answer,
        });
    }
    let first = p.trim_start_matches('/').split('/').next().unwrap_or("");
    let label = NAMESPACES
        .iter()
        .find(|n| n.strip_suffix("/*") == Some(first))
        .copied()
        .unwrap_or("other");
    Some(Route {
        label,
        answer: NotFound,
    })
}

/// Answer `req` locally: count it, read or discard its body, reply.
pub(crate) async fn answer(
    shared: &Arc<Shared>,
    req: Request<Incoming>,
    route: Route,
) -> Response<RespBody> {
    note(shared, route.label);
    let limit = shared.current().config.tuning.max_body_bytes;
    let (parts, body) = req.into_parts();
    let mut resp = match route.answer {
        Mcp => mcp(&parts.method, body, limit).await,
        Json(make) => {
            crate::server::drain(body, limit).await;
            json(StatusCode::OK, &make(parts.uri.query()))
        }
        NotFound => {
            crate::server::drain(body, limit).await;
            json(StatusCode::NOT_FOUND, &json!({"detail": "Not Found"}))
        }
    };
    set_route(resp.headers_mut(), LOCAL_ROUTE);
    resp.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

/// Count a local answer; the first of each route is logged at info level.
pub(crate) fn note(shared: &Shared, label: &'static str) {
    shared.totals.local_answers.fetch_add(1, Ordering::Relaxed);
    let first = {
        let mut routes = lock(&shared.local_routes);
        let n = routes.entry(label).or_insert(0);
        *n += 1;
        *n == 1
    };
    if first {
        tracing::info!(
            route = label,
            "answered a ChatGPT backend call locally; these calls never go to a member"
        );
    } else {
        tracing::debug!(route = label, "answered a ChatGPT backend call locally");
    }
}

/// `path` below `/backend-api` (or a bare ChatGPT path), without a trailing
/// `/`; `None` for every other path.
fn backend_path(path: &str) -> Option<&str> {
    let p = match path.strip_prefix("/backend-api") {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => rest,
        Some(_) => return None,
        None => {
            let first = path.trim_start_matches('/').split('/').next()?;
            if !BARE.contains(&first) {
                return None;
            }
            path
        }
    };
    let p = p.trim_end_matches('/');
    Some(if p.is_empty() { "/" } else { p })
}

/// The remainder of `path` below `prefix` (`""` or `/…`), or `None`.
fn below<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = path.strip_prefix(prefix)?;
    (rest.is_empty() || rest.starts_with('/')).then_some(rest)
}

fn json(status: StatusCode, value: &Value) -> Response<RespBody> {
    let mut resp = Response::new(full(Bytes::from(value.to_string())));
    *resp.status_mut() = status;
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    resp
}

/// The hosted apps MCP server (`/ps/mcp`, Streamable HTTP) without tools,
/// resources or prompts: it initializes, answers pings and lists nothing.
/// No session id is issued and no server stream is offered (GET and DELETE
/// get 405, as the transport allows).
async fn mcp(method: &Method, body: Incoming, limit: usize) -> Response<RespBody> {
    if method != Method::POST {
        crate::server::drain(body, limit).await;
        let mut resp = Response::new(empty());
        *resp.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
        resp.headers_mut()
            .insert(ALLOW, HeaderValue::from_static("POST"));
        return resp;
    }
    let message = match Limited::new(body, MCP_BODY_LIMIT).collect().await {
        Ok(collected) => serde_json::from_slice::<Value>(&collected.to_bytes()).ok(),
        Err(_) => None,
    };
    let Some(message) = message.filter(Value::is_object) else {
        return json(
            StatusCode::BAD_REQUEST,
            &json!({"jsonrpc": "2.0", "id": null,
                    "error": {"code": -32700, "message": "Parse error"}}),
        );
    };
    match mcp_reply(&message) {
        Some(reply) => json(StatusCode::OK, &reply),
        None => {
            let mut resp = Response::new(empty());
            *resp.status_mut() = StatusCode::ACCEPTED;
            resp
        }
    }
}

/// The JSON-RPC reply to one MCP message; `None` for a notification or a
/// response (accepted without a reply).
fn mcp_reply(message: &Value) -> Option<Value> {
    let method = message.get("method")?.as_str()?;
    let id = message.get("id")?.clone();
    let result = match method {
        "initialize" => {
            let asked = message["params"]["protocolVersion"].as_str();
            let version = asked
                .filter(|v| MCP_VERSIONS.contains(v))
                .unwrap_or(MCP_VERSIONS[0]);
            json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "superba-local-router", "version": VERSION},
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": []}),
        "resources/list" => json!({"resources": []}),
        "resources/templates/list" => json!({"resourceTemplates": []}),
        "prompts/list" => json!({"prompts": []}),
        _ => {
            return Some(json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": "Method not found"}}));
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn now_rfc3339() -> String {
    Timestamp::now().to_rfc3339()
}

/// `GET /wham/usage`: no plan windows, no credits (the router has no plan
/// limits of its own); Codex reads `plan_type` `unknown` like the local
/// identity's.
fn usage(_: Option<&str>) -> Value {
    json!({
        "user_id": ACCOUNT,
        "account_id": ACCOUNT,
        "email": null,
        "plan_type": "unknown",
        "rate_limit": {
            "allowed": true,
            "limit_reached": false,
            "primary_window": null,
            "secondary_window": null
        },
        "credits": null,
        "spend_control": null,
        "additional_rate_limits": null,
        "rate_limit_reached_type": null
    })
}

fn data(_: Option<&str>) -> Value {
    json!({"data": []})
}

fn fresh_data(_: Option<&str>) -> Value {
    json!({"data": [], "data_freshness_ts": now_rfc3339()})
}

fn threads(_: Option<&str>) -> Value {
    json!({"threads": []})
}

fn thread_usage_v2(_: Option<&str>) -> Value {
    json!({"data_as_of": now_rfc3339(), "threads": []})
}

/// `GET /wham/usage/daily-workspace-user-credit-usage`: `unit` must echo the
/// request's `balance_unit` (default `credit`).
fn credit_usage(query: Option<&str>) -> Value {
    let unit = query
        .and_then(|q| {
            url::form_urlencoded::parse(q.as_bytes())
                .find(|(k, _)| k == "balance_unit")
                .map(|(_, v)| v.into_owned())
        })
        .filter(|u| {
            !u.is_empty()
                && u.len() <= 32
                && u.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
        .unwrap_or_else(|| "credit".to_owned());
    json!({"unit": unit, "series": [], "data": [], "data_freshness_ts": now_rfc3339()})
}

fn accepted(_: Option<&str>) -> Value {
    json!({})
}

fn thread_hint(_: Option<&str>) -> Value {
    json!({"text": ""})
}

fn config_bundle(_: Option<&str>) -> Value {
    json!({"config_toml": null, "requirements_toml": null})
}

fn workspace_messages(_: Option<&str>) -> Value {
    json!({"messages": []})
}

fn wham_profile(_: Option<&str>) -> Value {
    json!({
        "profile": {
            "username": "superba",
            "display_name": DISPLAY_NAME,
            "profile_picture_url": null
        },
        "stats": null,
        "metadata": {"stats_error": null}
    })
}

fn me(_: Option<&str>) -> Value {
    json!({
        "id": ACCOUNT,
        "email": null,
        "name": DISPLAY_NAME,
        "first_name": null,
        "phone_number": null,
        "picture": null,
        "email_domain_type": null
    })
}

/// `GET /profiles/me/page`: no statistics (the page shows "Profile stats
/// unavailable") and an empty activity graph.
fn profile_page(_: Option<&str>) -> Value {
    json!({
        "is_self": true,
        "can_edit": false,
        "profile_details": {
            "username": "superba",
            "display_name": DISPLAY_NAME,
            "profile_picture_url": null,
            "avatar_shape": "circle"
        },
        "page": {
            "workspace_id": null,
            "visibility": {"value": "private"},
            "pins": {"items": []},
            "stats": null,
            "insights": null,
            "top_plugins": [],
            "activity_graph": {
                "daily_usage_buckets": [],
                "weekly_usage_buckets": [],
                "cumulative_daily_usage_buckets": []
            }
        }
    })
}

fn pin_options(_: Option<&str>) -> Value {
    json!({"items": [], "next_cursor": null, "pinned_ids": []})
}

fn plugin_list(_: Option<&str>) -> Value {
    json!({"plugins": [], "pagination": {"next_page_token": null}})
}

fn suggested_plugins(_: Option<&str>) -> Value {
    json!({"enabled": false, "plugins": []})
}

fn featured(_: Option<&str>) -> Value {
    json!([])
}

fn directory(_: Option<&str>) -> Value {
    json!({"apps": [], "next_token": null})
}

/// `GET /accounts/check/v4-2023-04-27`: the local identity as a personal
/// account without a subscription.
fn accounts_check_v4(_: Option<&str>) -> Value {
    let entry = json!({
        "account": {
            "account_id": ACCOUNT,
            "account_user_id": format!("{ACCOUNT}__{ACCOUNT}"),
            "account_user_role": "account-owner",
            "structure": "personal",
            "plan_type": "unknown",
            "is_zdr": false,
            "is_openai_internal": false,
            "is_deactivated": false,
            "name": DISPLAY_NAME,
            "profile_picture_url": null,
            "account_residency_region": "no_constraint",
            "is_fedramp_compliant_workspace": false
        },
        "features": [],
        "can_access_with_session": true,
        "entitlement": {
            "has_active_subscription": false,
            "subscription_plan": null,
            "expires_at": null,
            "renews_at": null
        }
    });
    json!({
        "accounts": {ACCOUNT: entry.clone(), "default": entry},
        "account_ordering": [ACCOUNT]
    })
}

#[cfg(test)]
#[path = "local_backend_tests.rs"]
mod tests;
