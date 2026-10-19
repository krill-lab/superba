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
//! earlier stats lines and adds the configured line to responses.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use http::header::{CONNECTION, SEC_WEBSOCKET_EXTENSIONS, UPGRADE};
use http::{Request, Response, StatusCode};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use tracing::Instrument;

use crate::config::join_path;
use crate::forward::{
    Observed, describe_error, empty, failover_status, local_busy, request_headers,
    response_headers, router_error, set_route,
};
use crate::limits::{Admission, ConnSlot, Place, file_limit_detail, file_limit_in};
use crate::proxy::{note_line_down, on_answer};
use crate::select::{Candidate, candidates};
use crate::state::{GaugeGuard, RespBody, Shared, View, lock};
use crate::status::RouteEventKind;
use crate::sticky::session_key;
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

/// Proxy one WebSocket handshake whose path below the router prefix is `rest`.
pub(crate) async fn proxy_ws(
    shared: Arc<Shared>,
    mut req: Request<Incoming>,
    rest: String,
    admission: Admission,
) -> Response<RespBody> {
    let generation = shared.current();
    let on_upgrade = hyper::upgrade::on(&mut req);
    let (parts, _body) = req.into_parts();
    // The inbound connection's place: an upgraded socket keeps it.
    let conn_slot = parts.extensions.get::<ConnSlot>().cloned();
    let key = session_key(&parts.headers);
    let now = Instant::now();
    let pin = key
        .as_deref()
        .and_then(|k| lock(&generation.sticky).get(k, now));

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
        transport::offers_websocket(&shared.transports, &m.cfg, &m.lines[c.line].cfg, now)
    })
    .collect();
    if order.first().map(|c| c.member) != preferred_member {
        return refuse(
            &shared,
            "the preferred member's lines refused WebSocket (HTTP 426)",
        );
    }

    let mut failures = Vec::new();
    for c in order {
        let member = &generation.members[c.member];
        let line = &member.lines[c.line];
        let label = generation.label(c.member, c.line);
        let url = join_path(&line.cfg.base_url, &rest, parts.uri.query());
        let drop_turn_state = pin.is_some_and(|p| p.member != c.member);
        let mut headers = request_headers(&parts.headers, true, drop_turn_state, &member.auth);
        // Privacy shaping requires readable frames on every connection.
        headers.remove(SEC_WEBSOCKET_EXTENSIONS);
        let started = Instant::now();
        let result = generation.ws.get(url).headers(headers).send().await;
        let resp = match result {
            // Out of descriptors here: not the line's fault, and no other
            // line would do better.
            Err(e) if file_limit_in(&e).is_some() => {
                let message =
                    shared.local_file_limit(Place::Upstream(label.clone()), &file_limit_detail(&e));
                return local_busy(&message);
            }
            Err(e) => {
                let reason = describe_error(&e);
                let opened = lock(&line.health).on_hard_failure(Instant::now(), reason.clone());
                note_line_down(&shared, &generation, c, opened, &reason);
                failed_over(&shared, member, line, &mut failures, &label, reason);
                continue;
            }
            Ok(resp) => resp,
        };
        if let Some(reason) = failover_status(resp.status(), resp.headers()) {
            let opened = lock(&line.health).on_soft_failure(Instant::now(), reason.clone());
            note_line_down(&shared, &generation, c, opened, &reason);
            failed_over(&shared, member, line, &mut failures, &label, reason);
            continue;
        }
        let status = resp.status();
        // A refused handshake (426) is no route for the session: its HTTP turns
        // keep the normal line order instead of sticking to the line that only
        // happened to be asked for WebSocket.
        let sticky_key = (status != StatusCode::UPGRADE_REQUIRED)
            .then_some(key.as_deref())
            .flatten();
        on_answer(
            &shared,
            &generation,
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
            // 426 (no WebSocket for this account), 401/403, ...: pass through.
            let mut headers = response_headers(resp.headers(), false);
            set_route(&mut headers, &label);
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
            *out.status_mut() = status;
            *out.headers_mut() = headers;
            return out;
        }
        let mut headers = response_headers(resp.headers(), true);
        headers.remove(SEC_WEBSOCKET_EXTENSIONS);
        set_route(&mut headers, &label);
        let upstream_io = match resp.upgrade().await {
            Ok(io) => io,
            Err(e) => {
                let reason = format!("upgrade failed: {}", e.without_url());
                let opened = lock(&line.health).on_hard_failure(Instant::now(), reason.clone());
                note_line_down(&shared, &generation, c, opened, &reason);
                failed_over(&shared, member, line, &mut failures, &label, reason);
                continue;
            }
        };
        shared.totals.websockets.fetch_add(1, Ordering::Relaxed);
        line.counters.websockets.fetch_add(1, Ordering::Relaxed);
        let tunnel_shared = shared.clone();
        let tunnel_gen = generation.clone();
        let (m, l) = (c.member, c.line);
        let handshake = parts.headers.clone();
        let failovers = u32::try_from(failures.len()).unwrap_or(u32::MAX);
        let span = tracing::info_span!("ws", route = %label);
        tokio::spawn(
            async move {
                // Both sockets stay open for the session: it keeps its
                // admission and the inbound connection's place to the end.
                let _held = (admission, conn_slot);
                let _gauge = GaugeGuard::new(tunnel_shared.clone(), |t| &t.active_websockets);
                let counters = &tunnel_gen.members[m].lines[l].counters;
                counters.active_websockets.fetch_add(1, Ordering::Relaxed);
                match on_upgrade.await {
                    Ok(downstream) => {
                        let ctx = crate::ws_relay::RelayCtx {
                            shared: tunnel_shared.clone(),
                            generation: tunnel_gen.clone(),
                            handshake,
                            answered: Candidate { member: m, line: l },
                            failovers: std::sync::atomic::AtomicU32::new(failovers),
                        };
                        crate::ws_relay::relay(TokioIo::new(downstream), upstream_io, ctx).await;
                    }
                    Err(e) => tracing::debug!(error = %e, "downstream upgrade failed"),
                }
                counters.active_websockets.fetch_sub(1, Ordering::Relaxed);
            }
            .instrument(span),
        );
        let mut out = Response::new(empty());
        *out.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
        *out.headers_mut() = headers;
        return out;
    }
    refuse(
        &shared,
        &format!("no WebSocket line reachable ({})", failures.join("; ")),
    )
}

fn failed_over(
    shared: &Shared,
    member: &crate::state::MemberRt,
    line: &crate::state::LineRt,
    failures: &mut Vec<String>,
    label: &str,
    reason: String,
) {
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
    shared.totals.ws_refused.fetch_add(1, Ordering::Relaxed);
    tracing::debug!(reason = why, "websocket refused locally");
    router_error(StatusCode::UPGRADE_REQUIRED, "websocket_unavailable", why)
}
