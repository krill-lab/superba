//! WebSocket passthrough for `GET …/responses` upgrades.
//!
//! The upstream handshake is completed before upgrading the client. Both
//! WebSocket legs terminate here so top-level private request fields are
//! removed even when statistics are disabled. Per-message deflate is not
//! negotiated; other frames retain their contents. A socket stays pinned
//! to its line for its lifetime. Refused or unavailable WebSocket routes
//! return 426 so Codex can retry over HTTP.
//!
//! With a `stats` section, [`crate::ws_relay`] also tracks requests, strips
//! earlier stats lines and adds the configured line to responses. Under the
//! merged catalog the model in each frame chooses the member
//! ([`crate::ws_catalog`]).

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use http::header::{CONNECTION, SEC_WEBSOCKET_EXTENSIONS, UPGRADE};
use http::request::Parts;
use http::{HeaderMap, HeaderValue, Request, Response, StatusCode};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tracing::Instrument;

use crate::config::join_path;
use crate::forward::{
    Observed, describe_error, empty, local_busy, request_headers, response_headers, router_error,
    set_route,
};
use crate::limits::{Admission, ConnSlot, Place, file_limit_detail, file_limit_in};
use crate::proxy::{note_line_down, on_answer};
use crate::select::{Candidate, candidates};
use crate::state::{GaugeGuard, Generation, RespBody, Shared, View, lock};
use crate::status::RouteEventKind;
use crate::sticky::{Pin, session_key};
use crate::transport;

/// True for a WebSocket upgrade request.
pub(crate) fn is_upgrade<B>(req: &Request<B>) -> bool {
    let has_token = |name: http::HeaderName, token: &str| {
        req.headers().get_all(name).iter().any(|v| {
            v.to_str()
                .map(|s| s.split(',').any(|t| t.trim().eq_ignore_ascii_case(token)))
                .unwrap_or(false)
        })
    };
    req.method() == http::Method::GET
        && has_token(UPGRADE, "websocket")
        && has_token(CONNECTION, "upgrade")
}

/// The 101 for a handshake the router answers itself (the member is dialed
/// later, [`crate::ws_catalog`], or reached over HTTP, [`crate::translate::ws`]),
/// or the 400 for a malformed one.
pub(crate) fn accept(headers: &HeaderMap) -> Result<Response<RespBody>, Box<Response<RespBody>>> {
    let Some(key) = headers
        .get("sec-websocket-key")
        .and_then(|v| v.to_str().ok())
    else {
        return Err(Box::new(router_error(
            StatusCode::BAD_REQUEST,
            "invalid_websocket",
            "missing WebSocket key",
        )));
    };
    if headers
        .get("sec-websocket-version")
        .is_none_or(|v| v != "13")
    {
        return Err(Box::new(router_error(
            StatusCode::BAD_REQUEST,
            "invalid_websocket",
            "WebSocket version 13 is required",
        )));
    }
    let Ok(accept) = HeaderValue::from_str(&derive_accept_key(key.as_bytes())) else {
        return Err(Box::new(router_error(
            StatusCode::BAD_REQUEST,
            "invalid_websocket",
            "invalid WebSocket key",
        )));
    };
    let mut response = Response::new(empty());
    *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    let h = response.headers_mut();
    h.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
    h.insert(UPGRADE, HeaderValue::from_static("websocket"));
    h.insert("sec-websocket-accept", accept);
    Ok(response)
}

/// Proxy one WebSocket handshake whose path below the router prefix is `rest`.
pub(crate) async fn proxy_ws(
    shared: Arc<Shared>,
    mut req: Request<Incoming>,
    rest: String,
    admission: Admission,
) -> Response<RespBody> {
    let generation = shared.current();
    // The model is in the frames: the catalog routes each of them.
    if generation.config.catalog.is_some() {
        return crate::ws_catalog::upgrade(shared, generation, req, rest, admission);
    }
    if !generation.config.synthetic_models.is_empty() || shared.compaction_proxy.used() {
        return refuse(
            &shared,
            "router compaction requires complete history over HTTP",
        );
    }
    let on_upgrade = hyper::upgrade::on(&mut req);
    let (parts, _body) = req.into_parts();
    // The inbound connection's place: an upgraded socket keeps it.
    let conn_slot = parts.extensions.get::<ConnSlot>().cloned();
    let key = session_key(&parts.headers);
    let now = Instant::now();
    let pin = key.as_deref().and_then(|k| generation.pin(k, now));

    // The preferred member decides whether this session uses WebSocket at all.
    let preferred = candidates(
        &View {
            generation: &generation,
            websocket_only: false,
            route: &crate::compaction::Route::Any,
        },
        pin,
        now,
    );
    let preferred_member = preferred.first().map(|c| c.member);
    if let Some(c) = preferred.first().copied()
        && generation
            .config
            .upstream_protocol(
                &generation.members[c.member].cfg.id,
                crate::translate::ir::Protocol::Responses,
            )
            .is_some_and(|p| p != crate::translate::ir::Protocol::Responses)
    {
        return crate::translate::ws::upgrade(shared, generation, parts, on_upgrade, admission, c);
    }
    let ws_capable = preferred_member.is_some_and(|m| generation.members[m].cfg.websocket);
    if !ws_capable {
        return refuse(&shared, "the preferred member does not serve WebSocket");
    }
    // Lines that refused the handshake (probe or a 426 met here) are skipped;
    // when that leaves none of the preferred member's lines, the session stays
    // on its member over HTTP rather than moving for the transport.
    let order: Vec<Candidate> = candidates(
        &View {
            generation: &generation,
            websocket_only: true,
            route: &crate::compaction::Route::Any,
        },
        pin,
        now,
    )
    .into_iter()
    .filter(|c| {
        let m = &generation.members[c.member];
        !generation.config.tuning.failover_enabled
            || transport::offers_websocket(&shared.transports, &m.cfg, &m.lines[c.line].cfg, now)
    })
    .collect();
    if order.first().map(|c| c.member) != preferred_member {
        return refuse(
            &shared,
            "the preferred member's lines refused WebSocket (HTTP 426)",
        );
    }

    let (io, c, headers, failovers) = match dial(
        &shared,
        &generation,
        &parts,
        &rest,
        order,
        pin,
        key.as_deref(),
    )
    .await
    {
        Dialed::Open {
            io,
            c,
            headers,
            failovers,
        } => (io, c, headers, failovers),
        Dialed::Answered { resp, c } => {
            return answered(&shared, &generation, &parts, resp, c, admission);
        }
        Dialed::FileLimit(message) => return local_busy(&message),
        Dialed::Unreachable(failures) => {
            return refuse(
                &shared,
                &format!("no WebSocket line reachable ({})", failures.join("; ")),
            );
        }
    };
    let tunnel_shared = shared.clone();
    let handshake = parts.headers.clone();
    let span = tracing::info_span!("ws", route = %generation.label(c.member, c.line));
    let limit = generation.config.tuning.max_body_bytes;
    tokio::spawn(
        async move {
            // Both sockets stay open for the session: it keeps its
            // admission and the inbound connection's place to the end.
            let _held = (admission, conn_slot);
            let _gauge = GaugeGuard::new(tunnel_shared.clone(), |t| &t.active_websockets);
            match on_upgrade.await {
                Ok(downstream) => {
                    let down = crate::ws_relay::downstream(TokioIo::new(downstream), limit).await;
                    let ctx = crate::ws_relay::RelayCtx {
                        shared: tunnel_shared,
                        generation,
                        handshake,
                        answered: c,
                        failovers: std::sync::atomic::AtomicU32::new(failovers),
                    };
                    tunnel(down, io, ctx, None).await;
                }
                Err(e) => tracing::debug!(error = %e, "downstream upgrade failed"),
            }
        }
        .instrument(span),
    );
    let mut out = Response::new(empty());
    *out.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    *out.headers_mut() = headers;
    out
}

/// What dialing the member's lines came to.
pub(crate) enum Dialed {
    /// A line upgraded: its socket and the 101's headers for Codex.
    Open {
        io: reqwest::Upgraded,
        c: Candidate,
        headers: HeaderMap,
        /// Lines failed over before this one.
        failovers: u32,
    },
    /// A line answered without upgrading (426: no WebSocket for this
    /// account, 401/403, ...).
    Answered {
        resp: reqwest::Response,
        c: Candidate,
    },
    /// Out of descriptors here: the local 503's message.
    FileLimit(String),
    /// No line was reachable (one reason per line).
    Unreachable(Vec<String>),
}

/// Open a member socket on the first line of `order` that upgrades, failing
/// over across the others when enabled by the generation.
pub(crate) async fn dial(
    shared: &Arc<Shared>,
    generation: &Arc<Generation>,
    parts: &Parts,
    rest: &str,
    order: Vec<Candidate>,
    pin: Option<Pin>,
    key: Option<&str>,
) -> Dialed {
    let mut failures = Vec::new();
    for c in order {
        let member = &generation.members[c.member];
        let line = &member.lines[c.line];
        let label = generation.label(c.member, c.line);
        let url = join_path(&line.cfg.base_url, rest, parts.uri.query());
        let drop_turn_state = pin.is_some_and(|p| p.member != c.member);
        let official = if member.cfg.kind == crate::MemberKind::Official {
            let client = parts.extensions.get::<crate::official::OfficialClient>();
            match crate::official::resolve(member, client, &generation.http).await {
                Ok(a) => Some(a),
                Err(reason) => {
                    failures.push(format!("{label}: {reason}"));
                    continue;
                }
            }
        } else {
            None
        };
        let mut headers = request_headers(
            &parts.headers,
            true,
            drop_turn_state,
            official.as_ref().map_or(&member.auth, |a| &a.auth),
        );
        if let Some(a) = &official
            && let Ok(v) = http::HeaderValue::from_str(&a.account_id)
        {
            headers.insert(crate::official::ACCOUNT_HEADER, v);
        }
        // Privacy shaping requires readable frames on every connection.
        headers.remove(SEC_WEBSOCKET_EXTENSIONS);
        let started = Instant::now();
        let result = generation.ws.get(url).headers(headers).send().await;
        let resp = match result {
            // Out of descriptors here: not the line's fault, and no other
            // line would do better.
            Err(e) if file_limit_in(&e).is_some() => {
                return Dialed::FileLimit(
                    shared.local_file_limit(Place::Upstream(label.clone()), &file_limit_detail(&e)),
                );
            }
            Err(e) => {
                let reason = describe_error(&e);
                let opened = lock(&line.health).on_hard_failure(Instant::now(), reason.clone());
                note_line_down(shared, generation, c, opened, &reason);
                failed_over(
                    shared,
                    generation,
                    member,
                    line,
                    &mut failures,
                    &label,
                    reason,
                );
                continue;
            }
            Ok(resp) => resp,
        };
        if let Some(reason) = generation.failover_reason(resp.status(), resp.headers()) {
            let opened = lock(&line.health).on_soft_failure(Instant::now(), reason.clone());
            note_line_down(shared, generation, c, opened, &reason);
            failed_over(
                shared,
                generation,
                member,
                line,
                &mut failures,
                &label,
                reason,
            );
            continue;
        }
        let status = resp.status();
        // A refused handshake (426) is no route for the session: its HTTP turns
        // keep the normal line order instead of sticking to the line that only
        // happened to be asked for WebSocket.
        let sticky_key = (status != StatusCode::UPGRADE_REQUIRED)
            .then_some(key)
            .flatten();
        on_answer(
            shared,
            generation,
            c,
            pin,
            sticky_key,
            status,
            started.elapsed(),
        );
        if status == StatusCode::UPGRADE_REQUIRED {
            shared
                .transports
                .note_ws_refused(&member.cfg, &line.cfg, Instant::now());
        } else if status == StatusCode::SWITCHING_PROTOCOLS {
            shared.transports.note_ws_ok(&member.cfg, &line.cfg);
        }
        if status != StatusCode::SWITCHING_PROTOCOLS {
            return Dialed::Answered { resp, c };
        }
        let mut headers = response_headers(resp.headers(), true);
        headers.remove(SEC_WEBSOCKET_EXTENSIONS);
        set_route(&mut headers, &label);
        let io = match resp.upgrade().await {
            Ok(io) => io,
            Err(e) => {
                let reason = format!("upgrade failed: {}", e.without_url());
                let opened = lock(&line.health).on_hard_failure(Instant::now(), reason.clone());
                note_line_down(shared, generation, c, opened, &reason);
                failed_over(
                    shared,
                    generation,
                    member,
                    line,
                    &mut failures,
                    &label,
                    reason,
                );
                continue;
            }
        };
        shared.totals.websockets.fetch_add(1, Ordering::Relaxed);
        line.counters.websockets.fetch_add(1, Ordering::Relaxed);
        return Dialed::Open {
            io,
            c,
            headers,
            failovers: u32::try_from(failures.len()).unwrap_or(u32::MAX),
        };
    }
    Dialed::Unreachable(failures)
}

/// A member's answer to the handshake that did not upgrade (426 for this
/// account, 401/403, ...), passed through to Codex.
fn answered(
    shared: &Arc<Shared>,
    generation: &Arc<Generation>,
    parts: &Parts,
    resp: reqwest::Response,
    c: Candidate,
    admission: Admission,
) -> Response<RespBody> {
    let status = resp.status();
    let mut headers = response_headers(resp.headers(), false);
    set_route(&mut headers, &generation.label(c.member, c.line));
    let active = GaugeGuard::new(shared.clone(), |t| &t.active_requests);
    let upstream: http::Response<reqwest::Body> = resp.into();
    let body = Observed::new(
        upstream.into_body(),
        shared.clone(),
        generation.clone(),
        c.member,
        c.line,
        active,
    );
    let mut out = Response::new(http_body_util::BodyExt::boxed_unsync(
        body.holding(admission),
    ));
    *out.status_mut() = answered_status(generation, parts, status, c);
    *out.headers_mut() = headers;
    out
}

/// The status Codex gets for a member's `status`: a member's 401 is not the
/// official login's (mixed login), so Codex must never refresh that login for
/// it.
pub(crate) fn answered_status(
    generation: &Generation,
    parts: &Parts,
    status: StatusCode,
    c: Candidate,
) -> StatusCode {
    if status == StatusCode::UNAUTHORIZED
        && generation.members[c.member].cfg.kind != crate::MemberKind::Official
        && parts
            .extensions
            .get::<crate::official::OfficialClient>()
            .is_some()
    {
        StatusCode::BAD_GATEWAY
    } else {
        status
    }
}

/// Relay Codex's socket and the member's for the session, counted on the
/// member's line.
pub(crate) async fn tunnel<D>(
    down: tokio_tungstenite::WebSocketStream<D>,
    up: reqwest::Upgraded,
    ctx: crate::ws_relay::RelayCtx,
    first: Option<tokio_tungstenite::tungstenite::Utf8Bytes>,
) where
    D: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    let generation = ctx.generation.clone();
    let c = ctx.answered;
    let counters = &generation.members[c.member].lines[c.line].counters;
    counters.active_websockets.fetch_add(1, Ordering::Relaxed);
    crate::ws_relay::relay(down, up, ctx, first).await;
    counters.active_websockets.fetch_sub(1, Ordering::Relaxed);
}

fn failed_over(
    shared: &Shared,
    generation: &Generation,
    member: &crate::state::MemberRt,
    line: &crate::state::LineRt,
    failures: &mut Vec<String>,
    label: &str,
    reason: String,
) {
    if !generation.config.tuning.failover_enabled {
        failures.push(format!("{label}: {reason}"));
        return;
    }
    line.counters.failovers_away.fetch_add(1, Ordering::Relaxed);
    shared.totals.failovers.fetch_add(1, Ordering::Relaxed);
    shared.event(
        RouteEventKind::Failover,
        Some(&member.cfg.id),
        Some(&line.cfg.id),
        Some(format!("websocket: {reason}")),
    );
    failures.push(format!("{label}: {reason}"));
}

/// 426 from the router itself: Codex falls back to HTTP for this session.
fn refuse(shared: &Shared, why: &str) -> Response<RespBody> {
    refuse_as(shared, "websocket_unavailable", why)
}

/// [`refuse`] with its own error code.
pub(crate) fn refuse_as(shared: &Shared, code: &str, why: &str) -> Response<RespBody> {
    shared.totals.ws_refused.fetch_add(1, Ordering::Relaxed);
    tracing::debug!(reason = why, "websocket refused locally");
    router_error(StatusCode::UPGRADE_REQUIRED, code, why)
}
