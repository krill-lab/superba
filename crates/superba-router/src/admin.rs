//! Local admin API under `/_superba/v1/`, used by the GUI, `spb` and the
//! watchdog. Everything except `health` needs `Authorization: Bearer <admin
//! token>` (per instance, published in the 0600 `runtime.json`).
//!
//! | Method + path | Answer |
//! |---|---|
//! | `GET health` | [`HealthReply`] (no auth; no secret inside) |
//! | `GET status` | [`RouterStatus`] |
//! | `POST reload` | re-read `config.json` + `secrets.json`; `{"generation": n}` |
//! | `POST probe` | probe every line now, then [`RouterStatus`] |
//! | `POST shutdown` | stop gracefully |
//! | `GET requests?after_id=&limit=` | recent request rows (live ring, oldest first) |
//! | `GET requests/active` | requests in flight |
//! | `POST requests/clear` | delete recorded rows ([`superba_core::ClearRequest`] body) |

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use bytes::Bytes;
use http::header::{AUTHORIZATION, CONTENT_TYPE};
use http::{HeaderValue, Method, Request, Response, StatusCode};
use http_body_util::BodyExt;
use superba_core::{ClearRequest, Result};

use crate::config::{ROUTER_BASE_PATH, RouterConfig, RouterSecrets};
use crate::forward::{full, router_error};
use crate::health::LineState;
use crate::limits::{Budget, Place};
use crate::probe::probe_all;
use crate::state::{Generation, RespBody, Shared, lock};
use crate::stats::hub::LIVE_RING;
use crate::status::{
    HealthReply, LineStatus, MemberStatus, RouteEventKind, RouterStatus, SERVICE_NAME, Totals,
};
use crate::token::{bearer, constant_time_eq};

/// Path prefix of the admin API.
pub const ADMIN_PREFIX: &str = "/_superba/v1/";

/// Router version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Handle an admin request (`path` starts with [`ADMIN_PREFIX`]).
pub(crate) async fn handle<B>(shared: Arc<Shared>, req: Request<B>) -> Response<RespBody>
where
    B: http_body::Body + Send,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let endpoint = req
        .uri()
        .path()
        .strip_prefix(ADMIN_PREFIX)
        .unwrap_or_default()
        .to_owned();
    if endpoint == "health" && req.method() == Method::GET {
        return json(
            StatusCode::OK,
            &HealthReply {
                service: SERVICE_NAME.into(),
                version: VERSION.into(),
                instance_id: shared.instance_id.clone(),
            },
        );
    }
    let authorized = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| bearer(v.as_bytes()))
        .is_some_and(|t| constant_time_eq(t, shared.admin_token.expose().as_bytes()));
    if !authorized {
        return router_error(
            StatusCode::UNAUTHORIZED,
            "admin_auth",
            "missing or wrong admin token",
        );
    }
    let method = req.method().clone();
    match (&method, endpoint.as_str()) {
        (&Method::GET, "catalog") => json(
            StatusCode::OK,
            &crate::merged_catalog::report(&shared.current()).await,
        ),
        (&Method::POST, "catalog/test") => {
            let body = match http_body_util::Limited::new(req.into_body(), 4096)
                .collect()
                .await
            {
                Ok(body) => body.to_bytes(),
                Err(_) => {
                    return router_error(
                        StatusCode::BAD_REQUEST,
                        "invalid_model",
                        "unreadable model test request",
                    );
                }
            };
            let model = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v["model"].as_str().map(str::to_owned))
                .filter(|m| !m.is_empty() && m.len() <= 512);
            match model {
                Some(model) => json(
                    StatusCode::OK,
                    &crate::merged_catalog::test(&shared, &model).await,
                ),
                None => router_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_model",
                    "expected a model id",
                ),
            }
        }
        (&Method::GET, "status") => json(StatusCode::OK, &status(&shared)),
        (&Method::POST, "probe") => {
            probe_all(&shared, &shared.current()).await;
            json(StatusCode::OK, &status(&shared))
        }
        (&Method::POST, "reload") => match reload(&shared) {
            Ok(n) => json(StatusCode::OK, &serde_json::json!({ "generation": n })),
            Err(e) => router_error(StatusCode::BAD_REQUEST, "reload_failed", &e.to_string()),
        },
        (&Method::POST, "shutdown") => {
            shared.request_shutdown();
            json(StatusCode::OK, &serde_json::json!({ "stopping": true }))
        }
        (&Method::GET, "requests") => {
            let q = req.uri().query().unwrap_or_default();
            let after_id = query_param(q, "after_id").and_then(|v| v.parse::<i64>().ok());
            let limit = query_param(q, "limit")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(LIVE_RING)
                .min(LIVE_RING);
            json(StatusCode::OK, &shared.stats.live_rows(after_id, limit))
        }
        (&Method::GET, "requests/active") => json(StatusCode::OK, &shared.stats.inflight()),
        (&Method::POST, "requests/clear") => {
            let body = match http_body_util::Limited::new(req.into_body(), 64 * 1024)
                .collect()
                .await
            {
                Ok(b) => b.to_bytes(),
                Err(_) => {
                    return router_error(StatusCode::BAD_REQUEST, "bad_request", "unreadable body");
                }
            };
            let request: ClearRequest = if body.is_empty() {
                ClearRequest::default()
            } else {
                match serde_json::from_slice(&body) {
                    Ok(r) => r,
                    Err(_) => {
                        return router_error(
                            StatusCode::BAD_REQUEST,
                            "bad_request",
                            "expected a clear request",
                        );
                    }
                }
            };
            let hub = shared.stats.clone();
            match tokio::task::spawn_blocking(move || hub.clear(&request)).await {
                Ok(Ok(report)) => json(StatusCode::OK, &report),
                Ok(Err(e)) => router_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "clear_failed",
                    &e.to_string(),
                ),
                Err(_) => router_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "clear_failed",
                    "interrupted",
                ),
            }
        }
        _ => router_error(StatusCode::NOT_FOUND, "not_found", "unknown admin endpoint"),
    }
}

/// Re-read the configuration and secrets and swap in a new generation.
pub(crate) fn reload(shared: &Shared) -> Result<u64> {
    let paths = shared.paths.as_ref().ok_or_else(|| {
        superba_core::Error::invalid("this router was started without a data dir")
    })?;
    let config = RouterConfig::load(paths).inspect_err(|e| note_file_limit(shared, e))?;
    if config.listen_port != 0 && config.listen_port != shared.listen.port() {
        return Err(superba_core::Error::invalid(format!(
            "the listen port changed ({} -> {}); restart the router",
            shared.listen.port(),
            config.listen_port
        )));
    }
    let secrets = RouterSecrets::load(paths).inspect_err(|e| note_file_limit(shared, e))?;
    let number = shared.current().number + 1;
    // The admission and connection caps stay as started; the pool follows the
    // new settings.
    let budget = Budget::new(shared.limits.budget.open_files_limit, &config.tuning);
    let generation = Generation::build(number, config, &secrets, &budget)?;
    crate::merged_catalog::inherit_probes(&shared.current(), &generation);
    shared.install(generation);
    shared.event(
        RouteEventKind::Reloaded,
        None,
        None,
        Some(format!("generation {number}")),
    );
    Ok(number)
}

/// A configuration file that could not be read for want of descriptors.
fn note_file_limit(shared: &Shared, e: &superba_core::Error) {
    if crate::limits::is_file_limit_error(e) {
        shared.local_file_limit(Place::Config, &e.detail());
    }
}

/// Snapshot of the router's state.
pub(crate) fn status(shared: &Shared) -> RouterStatus {
    let generation = shared.current();
    let now = Instant::now();
    let members = generation
        .members
        .iter()
        .map(|m| MemberStatus {
            id: m.cfg.id.clone(),
            last_route: lock(&m.last_route).clone(),
            name: m.cfg.name.clone(),
            kind: m.cfg.kind,
            websocket: m.cfg.websocket,
            model_mappings: m.cfg.model_map.len(),
            lines: m
                .lines
                .iter()
                .map(|l| {
                    let h = lock(&l.health);
                    let state = h.state(now);
                    LineStatus {
                        id: l.cfg.id.clone(),
                        name: l.cfg.name.clone(),
                        base_url: l.cfg.base_url.to_string(),
                        state,
                        consecutive_failures: h.consecutive_failures(),
                        reopens_in_ms: (state == LineState::Open)
                            .then(|| h.open_until())
                            .flatten()
                            .map(|u| ms(u.saturating_duration_since(now))),
                        last_error: h.last_error().map(str::to_owned),
                        last_ok_ms_ago: h.last_ok().map(|t| ms(now.saturating_duration_since(t))),
                        ttfb_ms: h.ttfb_ms(),
                        requests: l.counters.requests.load(Ordering::Relaxed),
                        failovers_away: l.counters.failovers_away.load(Ordering::Relaxed),
                        broken_streams: l.counters.broken_streams.load(Ordering::Relaxed),
                        websockets: l.counters.websockets.load(Ordering::Relaxed),
                        active_websockets: l.counters.active_websockets.load(Ordering::Relaxed),
                        compress_requests: crate::transport::compresses(
                            &shared.transports,
                            &m.cfg,
                            &l.cfg,
                            now,
                        ),
                        offers_websocket: m.cfg.websocket
                            && crate::transport::offers_websocket(
                                &shared.transports,
                                &m.cfg,
                                &l.cfg,
                                now,
                            ),
                    }
                })
                .collect(),
        })
        .collect();
    let t = &shared.totals;
    let sticky_sessions = {
        let mut s = lock(&generation.sticky);
        s.purge(now);
        s.len()
    };
    RouterStatus {
        service: SERVICE_NAME.into(),
        version: VERSION.into(),
        instance_id: shared.instance_id.clone(),
        pid: std::process::id(),
        listen: shared.listen.to_string(),
        base_url: format!("http://{}{ROUTER_BASE_PATH}", shared.listen),
        started_at_ms: shared.started_at_ms,
        uptime_ms: ms(shared.started.elapsed()),
        config_generation: generation.number,
        members,
        totals: Totals {
            requests: t.requests.load(Ordering::Relaxed),
            active_requests: t.active_requests.load(Ordering::Relaxed),
            websockets: t.websockets.load(Ordering::Relaxed),
            active_websockets: t.active_websockets.load(Ordering::Relaxed),
            failovers: t.failovers.load(Ordering::Relaxed),
            unavailable: t.unavailable.load(Ordering::Relaxed),
            rejected: t.rejected.load(Ordering::Relaxed),
            ws_refused: t.ws_refused.load(Ordering::Relaxed),
            local_answers: t.local_answers.load(Ordering::Relaxed),
        },
        sticky_sessions,
        recent: lock(&shared.events).iter().cloned().collect(),
        stats: shared.stats.status(generation.config.stats.as_ref()),
        local: shared.limits.status(),
        wsl_listen: lock(&shared.wsl_bound)
            .iter()
            .map(ToString::to_string)
            .collect(),
        local_answers: lock(&shared.local_routes)
            .iter()
            .map(|(route, n)| ((*route).to_owned(), *n))
            .collect(),
    }
}

/// The value of `name` in a query string (no percent-decoding: numbers only).
fn query_param<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

fn ms(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

fn json<T: serde::Serialize>(status: StatusCode, value: &T) -> Response<RespBody> {
    let bytes = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    let mut resp = Response::new(full(Bytes::from(bytes)));
    *resp.status_mut() = status;
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    resp.headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    resp
}
