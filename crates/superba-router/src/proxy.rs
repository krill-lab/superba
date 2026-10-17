//! HTTP proxying with per-request failover.
//!
//! The request body is buffered (bounded) so it can be sent again. Candidates are
//! tried in order (see [`crate::select`]), one attempt each, and only while no
//! byte has reached Codex: a transport failure before response headers, a
//! 502/503/504 or an edge interception moves on to the next candidate. Any other
//! answer — 401/403/429 account errors, every other 4xx and 500 included — is
//! passed through verbatim. There is no time-to-first-byte timeout. Once headers
//! are passed on, the body streams through; an upstream break aborts the
//! downstream response (Codex re-sends with the completed items) and marks the
//! line suspect. The router never retries a started response itself.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use bytes::Bytes;
use http::header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE};
use http::request::Parts;
use http::{HeaderMap, Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, Limited};
use hyper::body::Incoming;
use superba_core::RequestCompression;

use crate::config::join_path;
use crate::forward::{
    Observed, describe_error, empty_responses_answer, failover_status, local_busy, request_headers,
    response_headers, router_error, set_route,
};
use crate::limits::{Admission, LOCAL_BUSY_CODE, Place, file_limit_detail, file_limit_in};
use crate::model_map::map_model;
use crate::select::{Candidate, candidates};
use crate::state::{GaugeGuard, Generation, RespBody, Shared, View, lock};
use crate::stats::body::{Mode, TapBody};
use crate::stats::hub::{End, Tracker, now_ms};
use crate::stats::tap::{ResponseTap, SseTap};
use crate::stats::{meta, strip};
use crate::status::RouteEventKind;
use crate::sticky::{Pin, session_key};
use crate::transport;

/// True for the Responses endpoints whose requests are recorded (`POST
/// …/responses`, `POST …/responses/compact`).
pub(crate) fn is_responses_path(method: &Method, rest: &str) -> bool {
    let p = rest.trim_end_matches('/');
    method == Method::POST && (p.ends_with("/responses") || p.ends_with("/responses/compact"))
}

fn content_encoding(headers: &HeaderMap) -> Option<String> {
    headers
        .get(CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty() && v != "identity")
}

/// Peek and strip a Responses request body: `(body to send, request facts)`.
fn prepare(headers: &HeaderMap, body: Bytes, limit: usize) -> (Bytes, meta::RequestMeta) {
    match content_encoding(headers).as_deref() {
        None => {
            let mut m = meta::peek(headers, &body);
            match strip::strip(&body) {
                Some((new, n)) => {
                    m.stripped = n;
                    (Bytes::from(new), m)
                }
                None => (body, m),
            }
        }
        Some("zstd") => match strip::decode_zstd(&body, limit) {
            Some(plain) => {
                let mut m = meta::peek(headers, &plain);
                match strip::strip(&plain).and_then(|(new, n)| {
                    Some((zstd::stream::encode_all(new.as_slice(), 3).ok()?, n))
                }) {
                    Some((encoded, n)) => {
                        m.stripped = n;
                        (Bytes::from(encoded), m)
                    }
                    None => (body, m),
                }
            }
            None => (body, meta::from_headers(headers)),
        },
        Some(_) => (body, meta::from_headers(headers)),
    }
}

/// Proxy one HTTP request whose path below the router prefix is `rest`.
pub(crate) async fn proxy_http(
    shared: Arc<Shared>,
    req: Request<Incoming>,
    rest: String,
    admission: Admission,
) -> Response<RespBody> {
    let t0 = Instant::now();
    let started_ms = now_ms();
    let generation = shared.current();
    shared.totals.requests.fetch_add(1, Ordering::Relaxed);
    let active = GaugeGuard::new(shared.clone(), |t| &t.active_requests);
    let (parts, body) = req.into_parts();
    let limit = generation.config.tuning.max_body_bytes;
    let tracked = generation.config.stats.is_some() && is_responses_path(&parts.method, &rest);
    let body = match Limited::new(body, limit).collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(e) if e.is::<http_body_util::LengthLimitError>() => {
            if tracked {
                let t = shared.stats.track(
                    generation.clone(),
                    meta::from_headers(&parts.headers),
                    t0,
                    started_ms,
                );
                t.finish(End {
                    status: Some(superba_core::RequestStatus::Http {
                        status: 413,
                        code: Some("request_too_large".into()),
                    }),
                    ..End::default()
                });
            }
            return router_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request_too_large",
                &format!("the request body exceeds {limit} bytes"),
            );
        }
        Err(_) => {
            return router_error(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "the request body could not be read",
            );
        }
    };
    let (body, mut tracker) = if tracked {
        let (body, m) = prepare(&parts.headers, body, limit);
        let t = shared.stats.track(generation.clone(), m, t0, started_ms);
        (body, Some(t))
    } else {
        (body, None)
    };

    let key = session_key(&parts.headers);
    let now = Instant::now();
    let pin = key
        .as_deref()
        .and_then(|k| lock(&generation.sticky).get(k, now));
    let order = candidates(
        &View {
            generation: &generation,
            websocket_only: false,
        },
        pin,
        now,
    );

    let mut mapped: HashMap<usize, Bytes> = HashMap::new();
    // Lines that accept zstd get Responses bodies Codex sent uncompressed
    // compressed (one compression per member body).
    let client_zstd = content_encoding(&parts.headers).as_deref() == Some("zstd");
    let compressible = transport::compressible_path(&parts.method, &rest)
        && content_encoding(&parts.headers).is_none()
        && !body.is_empty();
    let mut compressed: HashMap<usize, Option<Bytes>> = HashMap::new();
    let mut failures: Vec<String> = Vec::new();
    let total = order.len();
    for (i, c) in order.into_iter().enumerate() {
        let member = &generation.members[c.member];
        let line = &member.lines[c.line];
        let label = generation.label(c.member, c.line);
        let send_body = mapped
            .entry(c.member)
            .or_insert_with(|| {
                map_model(&body, &parts.headers, &member.cfg.model_map, limit)
                    .unwrap_or_else(|| body.clone())
            })
            .clone();
        let drop_turn_state = pin.is_some_and(|p| p.member != c.member);
        if let Some(t) = &tracker {
            t.trying(c);
        }
        let zstd_body = if compressible
            && transport::compresses(&shared.transports, &member.cfg, &line.cfg, Instant::now())
        {
            match compressed.get(&c.member) {
                Some(z) => z.clone(),
                None => {
                    let z = transport::compress_body(send_body.clone()).await;
                    compressed.insert(c.member, z.clone());
                    z
                }
            }
        } else {
            None
        };
        let started = Instant::now();
        let (result, compression) = match zstd_body {
            Some(z) => {
                send_zstd(
                    &shared,
                    &generation,
                    &parts,
                    &rest,
                    c,
                    (z, send_body),
                    drop_turn_state,
                    tracker.as_mut(),
                )
                .await
            }
            None => (
                send(
                    &generation,
                    &parts,
                    &rest,
                    c,
                    send_body,
                    drop_turn_state,
                    false,
                )
                .await,
                if client_zstd {
                    RequestCompression::ClientZstd
                } else {
                    RequestCompression::None
                },
            ),
        };
        if let Some(t) = &mut tracker {
            t.set_compression(compression);
        }
        let (reason, status) = match result {
            // This computer is out of descriptors: no other line would do
            // better, and the line is not to blame.
            Err(e) if file_limit_in(&e).is_some() => {
                let message =
                    shared.local_file_limit(Place::Upstream(label.clone()), &file_limit_detail(&e));
                if let Some(t) = tracker {
                    t.finish(End {
                        status: Some(superba_core::RequestStatus::Http {
                            status: 503,
                            code: Some(LOCAL_BUSY_CODE.into()),
                        }),
                        error: Some(message.chars().take(300).collect()),
                        ..End::default()
                    });
                }
                return local_busy(&message);
            }
            Err(e) => {
                let reason = describe_error(&e);
                let opened = lock(&line.health).on_hard_failure(Instant::now(), reason.clone());
                note_line_down(&shared, &generation, c, opened, &reason);
                (reason, None)
            }
            Ok(resp) => match failover_status(resp.status(), resp.headers()).or_else(|| {
                empty_responses_answer(&rest, resp.status(), resp.headers()).then(|| {
                    format!(
                        "intercepted by the line's edge (HTTP {} with an empty body)",
                        resp.status().as_u16()
                    )
                })
            }) {
                Some(reason) => {
                    let opened = lock(&line.health).on_soft_failure(Instant::now(), reason.clone());
                    note_line_down(&shared, &generation, c, opened, &reason);
                    (reason, Some(resp.status().as_u16()))
                }
                None => {
                    let ttfb = started.elapsed();
                    on_answer(
                        &shared,
                        &generation,
                        c,
                        pin,
                        key.as_deref(),
                        resp.status(),
                        ttfb,
                    );
                    tracing::debug!(
                        method = %parts.method,
                        route = %label,
                        status = resp.status().as_u16(),
                        ttfb_ms = u64::try_from(ttfb.as_millis()).unwrap_or(u64::MAX),
                        "proxied"
                    );
                    if let Some(t) = &mut tracker {
                        let now = Instant::now();
                        t.answered(c, resp.status().as_u16(), resp.headers(), started, now);
                    }
                    return downstream(
                        shared.clone(),
                        generation.clone(),
                        c,
                        &label,
                        resp,
                        (active, admission),
                        tracker,
                    );
                }
            },
        };
        if let Some(t) = &mut tracker {
            t.attempt_failed(c, status, &reason, started.elapsed());
        }
        line.counters.failovers_away.fetch_add(1, Ordering::Relaxed);
        failures.push(format!("{label}: {reason}"));
        if i + 1 < total {
            shared.totals.failovers.fetch_add(1, Ordering::Relaxed);
            shared.event(
                RouteEventKind::Failover,
                Some(&member.cfg.id),
                Some(&line.cfg.id),
                Some(reason),
            );
        }
    }
    shared.totals.unavailable.fetch_add(1, Ordering::Relaxed);
    shared.event(
        RouteEventKind::Unavailable,
        None,
        None,
        Some(failures.join("; ")),
    );
    if let Some(t) = tracker {
        t.finish(End {
            status: Some(superba_core::RequestStatus::Http {
                status: 502,
                code: Some("router_unavailable".into()),
            }),
            error: Some(failures.join("; ").chars().take(300).collect()),
            ..End::default()
        });
    }
    router_error(
        StatusCode::BAD_GATEWAY,
        "router_unavailable",
        &format!("no line reachable ({})", failures.join("; ")),
    )
}

/// Send `zstd` (the compressed form of `plain`) to a line that accepts zstd.
/// A 400 / 415 / 422 answer may be a refusal to read the body: the plain body
/// goes again to the same line (nothing was processed), and a 415, or a plain
/// retry that is not refused the same way, marks the line as refusing zstd
/// ([`transport::TransportMemory`]). A genuine 400 comes back from the plain
/// retry too and is passed on as such.
#[allow(clippy::too_many_arguments)]
async fn send_zstd(
    shared: &Shared,
    generation: &Generation,
    parts: &Parts,
    rest: &str,
    c: Candidate,
    (zstd, plain): (Bytes, Bytes),
    drop_turn_state: bool,
    tracker: Option<&mut Tracker>,
) -> (
    Result<reqwest::Response, reqwest::Error>,
    RequestCompression,
) {
    let started = Instant::now();
    let first = send(generation, parts, rest, c, zstd, drop_turn_state, true).await;
    let first_elapsed = started.elapsed();
    let refused = match &first {
        Ok(r) if transport::may_refuse_zstd(r.status()) => r.status(),
        _ => return (first, RequestCompression::RouterZstd),
    };
    // The refusal's body is not needed (and not passed on).
    drop(first);
    let retry = send(generation, parts, rest, c, plain, drop_turn_state, false).await;
    let blamed = refused == StatusCode::UNSUPPORTED_MEDIA_TYPE
        || matches!(&retry, Ok(r) if r.status() != refused);
    let member = &generation.members[c.member];
    let line = &member.lines[c.line];
    let label = generation.label(c.member, c.line);
    if blamed {
        shared
            .transports
            .note_zstd_refused(&member.cfg, &line.cfg, Instant::now());
        tracing::info!(
            route = %label,
            status = refused.as_u16(),
            "line refused a zstd request body; sending its bodies uncompressed"
        );
    }
    if let Some(t) = tracker {
        t.zstd_refused(c, refused.as_u16(), first_elapsed);
    }
    let compression = if blamed {
        RequestCompression::ZstdRefused
    } else {
        RequestCompression::None
    };
    (retry, compression)
}

async fn send(
    generation: &Generation,
    parts: &Parts,
    rest: &str,
    c: Candidate,
    body: Bytes,
    drop_turn_state: bool,
    zstd: bool,
) -> Result<reqwest::Response, reqwest::Error> {
    let member = &generation.members[c.member];
    let line = &member.lines[c.line];
    let url = join_path(&line.cfg.base_url, rest, parts.uri.query());
    let mut headers = request_headers(&parts.headers, false, drop_turn_state, &member.auth);
    if zstd {
        headers.insert(CONTENT_ENCODING, http::HeaderValue::from_static("zstd"));
    }
    let mut rb = generation
        .http
        .request(parts.method.clone(), url)
        .headers(headers);
    if !body.is_empty() {
        rb = rb.body(body);
    }
    rb.send().await
}

/// Bookkeeping for an answer that is passed on.
pub(crate) fn on_answer(
    shared: &Shared,
    generation: &Generation,
    c: Candidate,
    pin: Option<Pin>,
    key: Option<&str>,
    status: StatusCode,
    ttfb: std::time::Duration,
) {
    let member = &generation.members[c.member];
    let line = &member.lines[c.line];
    line.counters.requests.fetch_add(1, Ordering::Relaxed);
    let reopened = {
        let mut h = lock(&line.health);
        let was_open = h.open_until().is_some();
        h.on_success(Instant::now(), Some(ttfb));
        was_open
    };
    if reopened {
        shared.event(
            RouteEventKind::LineUp,
            Some(&member.cfg.id),
            Some(&line.cfg.id),
            Some(format!("HTTP {}", status.as_u16())),
        );
    }
    if let Some(k) = key {
        let new_pin = Pin {
            member: c.member,
            line: c.line,
        };
        lock(&generation.sticky).set(k, new_pin, Instant::now());
        if let Some(old) = pin
            && old.member != c.member
        {
            shared.event(
                RouteEventKind::MemberChanged,
                Some(&member.cfg.id),
                Some(&line.cfg.id),
                Some(format!("from {}", generation.members[old.member].cfg.id)),
            );
        }
    }
}

pub(crate) fn note_line_down(
    shared: &Shared,
    generation: &Generation,
    c: Candidate,
    opened: bool,
    reason: &str,
) {
    if opened {
        let member = &generation.members[c.member];
        shared.event(
            RouteEventKind::LineDown,
            Some(&member.cfg.id),
            Some(&member.lines[c.line].cfg.id),
            Some(reason.to_owned()),
        );
    }
}

fn downstream(
    shared: Arc<Shared>,
    generation: Arc<Generation>,
    c: Candidate,
    label: &str,
    resp: reqwest::Response,
    (active, admission): (GaugeGuard, Admission),
    tracker: Option<Tracker>,
) -> Response<RespBody> {
    let status = resp.status();
    let mut headers = response_headers(resp.headers(), false);
    set_route(&mut headers, label);
    let content_type = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let encoded = content_encoding(resp.headers()).is_some();
    let upstream: http::Response<reqwest::Body> = resp.into();
    let inner = upstream.into_body();
    let body = match tracker {
        None => Observed::new(inner, shared, generation, c.member, c.line, active)
            .holding(admission)
            .boxed_unsync(),
        Some(t) => {
            let ct = content_type
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let mode =
                if status == StatusCode::OK && ct.starts_with("text/event-stream") && !encoded {
                    let line = t.line_setup();
                    if line.is_some() {
                        // The stats line changes the length.
                        headers.remove(CONTENT_LENGTH);
                    }
                    Mode::Sse(Box::new(SseTap::new(ResponseTap::new(t.t0(), line))))
                } else if !encoded && ct.contains("json") {
                    Mode::json(status.as_u16())
                } else {
                    Mode::Plain {
                        status: status.as_u16(),
                    }
                };
            let tapped = TapBody::new(inner, mode, t);
            Observed::new(tapped, shared, generation, c.member, c.line, active)
                .holding(admission)
                .boxed_unsync()
        }
    };
    let mut out = Response::new(body);
    *out.status_mut() = status;
    *out.headers_mut() = headers;
    out
}
