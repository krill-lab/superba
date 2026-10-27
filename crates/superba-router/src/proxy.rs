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

use crate::cipher::{self, Plan, Removed};
use crate::compaction::{self, CompactionInput, Route};
use crate::config::join_path;
use crate::forward::{
    Observed, UpBody, UpResponse, describe_error, empty_responses_answer, local_busy,
    request_headers, response_headers, router_error, set_route, up_response,
};
use crate::limits::{Admission, LOCAL_BUSY_CODE, Place, file_limit_detail, file_limit_in};
use crate::model_map::map_model;
use crate::select::{Candidate, candidates};
use crate::state::{GaugeGuard, Generation, MemberRt, RespBody, Shared, View, lock};
use crate::stats::body::{Mode, TapBody};
use crate::stats::hub::{End, Tracker, now_ms};
use crate::stats::tap::{ResponseTap, SseTap};
use crate::stats::{meta, strip};
use crate::status::RouteEventKind;
use crate::sticky::{Pin, session_key};
use crate::translate::{self, ir::Protocol, routing};
use crate::transport;

/// The ingress a Responses request came in on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ingress {
    /// Codex's own paths (`/backend-api/codex`, `/v1`): every member at its
    /// line base URL, Codex's request facts, stickiness and stats line.
    Codex,
    /// The OpenAI ingress (`/openai/v1/responses`, the other coding tools):
    /// the members that serve Responses at their Responses endpoint, the
    /// tools' facts and shaping ([`crate::openai`]); bodies are never
    /// compressed for them and the answer is asked for unencoded.
    OpenAi,
}

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

/// Proxy one HTTP request whose path below the router prefix is `rest`,
/// received on `ingress`.
pub(crate) async fn proxy_http(
    shared: Arc<Shared>,
    req: Request<Incoming>,
    rest: String,
    admission: Admission,
    ingress: Ingress,
) -> Response<RespBody> {
    let req = req.map(ProxyBody::Incoming);
    proxy_request(shared, req, rest, admission, ingress, None).await
}

enum ProxyBody {
    Incoming(Incoming),
    Buffered(Bytes),
}

/// Only the authenticated catalog test constructs this route. It bypasses
/// publication, never request preparation, conversion or response handling.
struct ProbeRoute {
    generation: Arc<Generation>,
    target: crate::merged_catalog::Target,
    line: usize,
}

pub(crate) fn probe_http(
    shared: Arc<Shared>,
    generation: Arc<Generation>,
    member: usize,
    line: usize,
    model: &str,
    standard: bool,
    body: Bytes,
) -> futures_util::future::BoxFuture<'static, Response<RespBody>> {
    let model = model.to_owned();
    Box::pin(async move {
        let admission = match shared.limits.admit(std::time::Duration::ZERO).await {
            Ok(admission) => admission,
            Err(_) => return local_busy("the router is busy; retry the model test"),
        };
        let req = Request::builder()
            .method(Method::POST)
            .uri("/backend-api/codex/responses")
            .header(CONTENT_TYPE, "application/json")
            .body(ProxyBody::Buffered(body))
            .expect("probe request");
        proxy_request(
            shared,
            req,
            "/responses".into(),
            admission,
            Ingress::Codex,
            Some(ProbeRoute {
                generation,
                target: crate::merged_catalog::Target {
                    member,
                    upstream: model,
                    standard,
                },
                line,
            }),
        )
        .await
    })
}

async fn proxy_request(
    shared: Arc<Shared>,
    req: Request<ProxyBody>,
    rest: String,
    admission: Admission,
    ingress: Ingress,
    probe: Option<ProbeRoute>,
) -> Response<RespBody> {
    let openai = ingress == Ingress::OpenAi;
    let t0 = Instant::now();
    let started_ms = now_ms();
    let generation = probe
        .as_ref()
        .map(|p| p.generation.clone())
        .unwrap_or_else(|| shared.current());
    shared.totals.requests.fetch_add(1, Ordering::Relaxed);
    let active = GaugeGuard::new(shared.clone(), |t| &t.active_requests);
    let (mut parts, body) = req.into_parts();
    let limit = generation.config.tuning.max_body_bytes;
    let tracked = generation.config.stats.is_some() && is_responses_path(&parts.method, &rest);
    let collected = match body {
        ProxyBody::Incoming(body) => Limited::new(body, limit)
            .collect()
            .await
            .map(|c| c.to_bytes()),
        ProxyBody::Buffered(body) => Ok(body),
    };
    let body = match collected {
        Ok(body) => body,
        Err(e) if e.is::<http_body_util::LengthLimitError>() => {
            if tracked {
                let m = match openai {
                    true => crate::openai::meta::from_headers(
                        &parts.headers,
                        crate::openai::OpenAiProtocol::Responses,
                    ),
                    false => meta::from_headers(&parts.headers),
                };
                let t = shared.stats.track(generation.clone(), m, t0, started_ms);
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
    let body = if !openai && is_responses_path(&parts.method, &rest) {
        match crate::openai::strip_private_request(&mut parts.headers, body, limit) {
            Ok(body) => body,
            Err(response) => return *response,
        }
    } else {
        body
    };
    let (body, catalog_selection) = if let Some(probe) = &probe {
        (
            body,
            Some(crate::merged_catalog::Selection {
                targets: vec![probe.target.clone()],
            }),
        )
    } else if !openai
        && generation.config.catalog.is_some()
        && is_responses_path(&parts.method, &rest)
    {
        match crate::merged_catalog::select(&generation, &mut parts.headers, body, limit).await {
            Ok(selected) => selected,
            Err(response) => return *response,
        }
    } else {
        (body, None)
    };
    // The merged catalog is authoritative. The explicit seam is only used
    // without it, never to replace or filter a mixed native/L2 selection.
    let (body, catalog_selection) = if probe.is_none()
        && generation.config.catalog.is_none()
        && !generation.config.synthetic_models.is_empty()
        && is_responses_path(&parts.method, &rest)
    {
        let plain = match crate::openai::body::decode(&mut parts.headers, body, limit) {
            Ok(body) => body,
            Err(response) => return *response,
        };
        let model = meta::peek(&parts.headers, &plain).model;
        let targets: Vec<_> = generation
            .config
            .synthetic_models
            .iter()
            .filter(|t| Some(t.slug.as_str()) == model.as_deref())
            .filter_map(|t| {
                let member = generation
                    .members
                    .iter()
                    .position(|m| m.cfg.id == t.member)?;
                Some(crate::merged_catalog::Target {
                    member,
                    upstream: t.model.clone(),
                    standard: true,
                })
            })
            .collect();
        let selection =
            (!targets.is_empty()).then_some(crate::merged_catalog::Selection { targets });
        (plain, selection)
    } else {
        (body, catalog_selection)
    };
    if let Some(selection) = &catalog_selection {
        parts.extensions.insert(selection.clone());
    }
    // The OpenAI ingress always reads the request (its facts route it, and
    // it is shaped whether or not it is recorded).
    let mut openai_meta = None;
    let (body, mut tracker, mut compaction) = if openai {
        let (body, m, notes) =
            match crate::openai::prepare_responses(&mut parts.headers, body, limit) {
                Ok(prepared) => prepared,
                Err(resp) => return *resp,
            };
        let compaction = m.compaction.clone();
        let mut t = tracked.then(|| {
            shared
                .stats
                .track(generation.clone(), m.clone(), t0, started_ms)
        });
        if let Some(t) = &mut t {
            for n in notes {
                t.note(n);
            }
        }
        crate::openai::route::note_excluded(
            &generation,
            crate::openai::OpenAiProtocol::Responses,
            t.as_mut(),
        );
        openai_meta = Some(m);
        (body, t, compaction)
    } else if tracked {
        let (body, m) = prepare(&parts.headers, body, limit);
        let compaction = m.compaction.clone();
        let t = shared.stats.track(generation.clone(), m, t0, started_ms);
        (body, Some(t), compaction)
    } else if is_responses_path(&parts.method, &rest) {
        let compaction = inspect_encoded(&parts.headers, &body, limit);
        (body, None, compaction)
    } else {
        (body, None, CompactionInput::default())
    };

    if rest.trim_end_matches('/').ends_with("/responses/compact") {
        compaction.trigger = true;
    }
    let key = match &openai_meta {
        Some(m) => crate::openai::route::sticky_key(m),
        None => session_key(&parts.headers),
    };
    let now = Instant::now();
    let pin = key.as_deref().and_then(|k| generation.pin(k, now));
    // Remote compaction: a compacted conversation stays with the member that
    // made its compaction item; compaction requests skip members that cannot
    // compact (crate::compaction).
    let mut facts = generation.member_facts();
    if let Some(selection) = &catalog_selection {
        for target in &selection.targets {
            if target.standard {
                // Only this selected L2 target can delegate compaction. Other
                // targets for the same slug retain their native capabilities.
                facts[target.member].compaction = superba_core::CompactionSupport::Native;
            }
        }
    }
    let route = compaction::route(&facts, &compaction, &shared.stats.compactions);
    if let Route::Refuse {
        status,
        code,
        message,
    } = &route
    {
        shared.event(
            RouteEventKind::Compaction,
            None,
            None,
            Some(message.clone()),
        );
        if let Some(t) = tracker {
            t.finish(End {
                status: Some(superba_core::RequestStatus::Http {
                    status: *status,
                    code: Some((*code).into()),
                }),
                error: Some(message.chars().take(300).collect()),
                ..End::default()
            });
        }
        return router_error(
            StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_REQUEST),
            code,
            message,
        );
    }
    let mut order = match &openai_meta {
        None if catalog_selection.is_some() => catalog_selection
            .as_ref()
            .expect("selection")
            .candidates(&generation, &route, pin, now),
        None => candidates(
            &View {
                generation: &generation,
                websocket_only: false,
                route: &route,
            },
            pin,
            now,
        ),
        Some(m) => {
            match crate::openai::route::resolve(
                &shared,
                &generation,
                crate::openai::OpenAiProtocol::Responses,
                m.model.as_deref(),
                crate::forward::bound_member(&parts.headers),
            )
            .await
            {
                Ok(targets) => {
                    let allowed: Vec<_> = targets
                        .into_iter()
                        .filter(|t| {
                            route.allows(t.member)
                                && catalog_selection.as_ref().is_none_or(|selection| {
                                    selection.targets.iter().any(|s| s.member == t.member)
                                })
                        })
                        .collect();
                    crate::claude::route::attempts(&generation, &allowed, pin, now)
                        .into_iter()
                        .map(|a| a.c)
                        .collect()
                }
                Err((status, code, message)) => {
                    if let Some(t) = tracker {
                        t.finish(End {
                            status: Some(superba_core::RequestStatus::Http {
                                status,
                                code: Some(code.into()),
                            }),
                            error: Some(message.chars().take(300).collect()),
                            ..End::default()
                        });
                    }
                    return router_error(
                        StatusCode::from_u16(status).unwrap_or(StatusCode::NOT_FOUND),
                        code,
                        &message,
                    );
                }
            }
        }
    };
    if let Some(probe) = &probe {
        order.retain(|c| c.member == probe.target.member && c.line == probe.line);
    }
    note_compaction_route(&shared, &generation, &route, pin, now, order.first());

    let mut preparation_error = None;
    let mut failures: Vec<String> = Vec::new();
    let total = order.len();
    for (i, c) in order.into_iter().enumerate() {
        let mut parts = parts.clone();
        let synthetic = catalog_selection
            .as_ref()
            .is_some_and(|s| s.standard(c.member));
        let mut attempt_body = body.clone();
        if let Some(target) = catalog_selection
            .as_ref()
            .and_then(|s| s.targets.iter().find(|t| t.member == c.member))
            && let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&attempt_body)
            && value["model"].as_str() != Some(target.upstream.as_str())
        {
            value["model"] = serde_json::json!(target.upstream);
            attempt_body = Bytes::from(serde_json::to_vec(&value).expect("request JSON"));
        }
        let proxy_compaction = if is_responses_path(&parts.method, &rest) {
            match crate::compaction_proxy::prepare(
                &shared,
                &generation,
                &mut parts,
                attempt_body,
                synthetic,
                limit,
            )
            .await
            {
                Ok(prepared) => prepared,
                Err(response) => {
                    // A native target may still accept ciphertext that an L2
                    // target cannot read. Never send a partially prepared body.
                    preparation_error = Some(response);
                    continue;
                }
            }
        } else {
            crate::compaction_proxy::Prepared::unchanged(attempt_body)
        };
        let body = proxy_compaction.body.clone();
        let attempt_rest = if proxy_compaction.transaction.is_some() {
            rest.trim_end_matches('/')
                .strip_suffix("/compact")
                .unwrap_or(&rest)
        } else {
            &rest
        };
        // Ciphertexts another provider made (GitHub issue 11): what a member
        // refused before is left out of its body, and a refusal now leaves out
        // what it names and goes to the same line once more ([`crate::cipher`]).
        let ciphers = if is_responses_path(&parts.method, &rest) {
            CipherBody::read(&parts.headers, &body, limit)
        } else {
            None
        };
        let mut mapped: HashMap<usize, Bytes> = HashMap::new();
        // Lines that accept zstd get Responses bodies Codex sent uncompressed
        // compressed (one compression per member body).
        let client_zstd = content_encoding(&parts.headers).as_deref() == Some("zstd");
        let compressible = !openai
            && catalog_selection.is_none()
            && transport::compressible_path(&parts.method, &rest)
            && content_encoding(&parts.headers).is_none()
            && !body.is_empty();
        let mut compressed: HashMap<usize, Option<Bytes>> = HashMap::new();
        let member = &generation.members[c.member];
        let line = &member.lines[c.line];
        let label = generation.label(c.member, c.line);
        let drop_turn_state = pin.is_some_and(|p| p.member != c.member);
        if let Some(t) = &tracker {
            t.trying(c);
        }
        let upstream = generation
            .config
            .upstream_protocol(&member.cfg.id, Protocol::Responses)
            .unwrap_or(Protocol::Responses);
        let converted = upstream != Protocol::Responses && is_responses_path(&parts.method, &rest);
        let mut response_tools = Vec::new();
        let mut resent = false;
        parts.extensions.remove::<crate::official::ResolvedAuth>();
        if member.cfg.kind == crate::MemberKind::Official {
            let client = parts
                .extensions
                .get::<crate::official::OfficialClient>()
                .cloned();
            match crate::official::resolve(member, client.as_ref(), &generation.http).await {
                Ok(a) => {
                    parts.extensions.insert(a);
                }
                Err(reason) => {
                    if let Some(t) = &mut tracker {
                        t.attempt_failed(c, None, &reason, std::time::Duration::ZERO);
                    }
                    failures.push(format!("{label}: {reason}"));
                    continue;
                }
            }
        }
        let (result, compression, started) = loop {
            let send_body = match mapped.get(&c.member) {
                Some(b) => b.clone(),
                None => {
                    let (mut b, left_out) = member_body(
                        &shared,
                        &body,
                        ciphers.as_ref(),
                        member,
                        &parts.headers,
                        limit,
                    );
                    if let (Some(t), Some(r), false) = (&mut tracker, left_out, resent) {
                        t.note(format!(
                            "{}: left out {} it refused before",
                            member.cfg.id,
                            r.describe()
                        ));
                    }
                    if (openai
                        || catalog_selection
                            .as_ref()
                            .is_some_and(|s| s.standard(c.member)))
                        && let Some((dropped, fields)) = crate::openai::member_responses_body(
                            &generation,
                            c.member,
                            &b,
                            &parts.headers,
                        )
                    {
                        b = dropped;
                        if let (Some(t), false) = (&mut tracker, resent) {
                            for f in fields {
                                t.note(format!(
                                    "{}: field {f} left out for this provider",
                                    member.cfg.id
                                ));
                            }
                        }
                    }
                    mapped.insert(c.member, b.clone());
                    b
                }
            };
            let send_body = if converted {
                let plain = if client_zstd {
                    match strip::decode_zstd(&send_body, limit) {
                        Some(b) => Bytes::from(b),
                        None => {
                            return translate::body::error(
                                Protocol::Responses,
                                StatusCode::BAD_REQUEST,
                                &translate::Invalid::new("invalid zstd request"),
                            );
                        }
                    }
                } else {
                    send_body
                };
                let value = match serde_json::from_slice::<serde_json::Value>(&plain) {
                    Ok(v) => v,
                    Err(_) => {
                        return translate::body::error(
                            Protocol::Responses,
                            StatusCode::BAD_REQUEST,
                            &translate::Invalid::new("request is not JSON"),
                        );
                    }
                };
                let model = value["model"].as_str().unwrap_or_default();
                match translate::body::request_with_tools(
                    Protocol::Responses,
                    upstream,
                    &value,
                    model,
                ) {
                    Ok((b, tools)) => {
                        response_tools = tools;
                        b
                    }
                    Err(e) => {
                        return translate::body::error(
                            Protocol::Responses,
                            StatusCode::BAD_REQUEST,
                            &e,
                        );
                    }
                }
            } else {
                send_body
            };
            let zstd_body = if !converted
                && compressible
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
                        (attempt_rest, ingress),
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
                        (attempt_rest, ingress),
                        c,
                        send_body,
                        drop_turn_state,
                        false,
                    )
                    .await,
                    if client_zstd && !converted {
                        RequestCompression::ClientZstd
                    } else {
                        RequestCompression::None
                    },
                ),
            };
            let result = result.map(up_response);
            if converted {
                break (result, compression, started);
            }
            // A refusal of a ciphertext arrives as a small JSON error before
            // any byte reached Codex: read it, then decide.
            let (Some(cb), false) = (ciphers.as_ref(), resent) else {
                break (result, compression, started);
            };
            let resp = match result {
                Ok(r)
                    if cipher::rejection::may_be(
                        r.status().as_u16(),
                        content_type_of(r.headers()),
                    ) =>
                {
                    r
                }
                other => break (other, compression, started),
            };
            let (resp, whole) = read_small(resp).await;
            let Some(rejection) = whole.as_deref().and_then(cipher::rejection::from_body) else {
                break (Ok(resp), compression, started);
            };
            let id = &member.cfg.id;
            let status = resp.status().as_u16();
            match cipher::refused(
                &shared.stats.ciphers,
                id,
                &cb.sent(&shared.stats.ciphers, id),
                &rejection,
            ) {
                Plan::Neutralise(set) => {
                    let removed = cb.removed_by(&set);
                    if let Some(t) = &mut tracker {
                        t.cipher_refused(c, status, started.elapsed(), &removed.describe());
                    }
                    shared.event(
                        RouteEventKind::Ciphertext,
                        Some(id),
                        Some(&line.cfg.id),
                        Some(cipher::event_detail(&member.cfg.name, &removed, false)),
                    );
                    resent = true;
                    mapped.remove(&c.member);
                    compressed.remove(&c.member);
                }
                Plan::TaskUnreadable => {
                    let message = cipher::task_unreadable_message(&member.cfg.name);
                    shared.event(
                        RouteEventKind::Ciphertext,
                        Some(id),
                        Some(&line.cfg.id),
                        Some(message.clone()),
                    );
                    if let Some(t) = tracker {
                        t.finish(End {
                            status: Some(superba_core::RequestStatus::Http {
                                status: 400,
                                code: Some(cipher::TASK_UNREADABLE_CODE.into()),
                            }),
                            error: Some(message.chars().take(300).collect()),
                            ..End::default()
                        });
                    }
                    return router_error(
                        StatusCode::BAD_REQUEST,
                        cipher::TASK_UNREADABLE_CODE,
                        &message,
                    );
                }
                Plan::PassThrough => break (Ok(resp), compression, started),
            }
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
                let opened = generation.hard_failure(c, reason.clone());
                note_line_down(&shared, &generation, c, opened, &reason);
                (reason, None)
            }
            Ok(resp) => match generation
                .failover_reason(resp.status(), resp.headers())
                .or_else(|| {
                    // The official plan's own window is spent (or its login
                    // was refused): another member may still serve.
                    (generation.config.tuning.failover_enabled
                        && member.cfg.kind == crate::MemberKind::Official
                        && i + 1 < total
                        && (crate::official::plan_limit(resp.status())
                            || resp.status() == StatusCode::UNAUTHORIZED))
                        .then(|| {
                            format!(
                                "official ChatGPT {} (HTTP {})",
                                if resp.status() == StatusCode::UNAUTHORIZED {
                                    "login refused"
                                } else {
                                    "plan limit reached"
                                },
                                resp.status().as_u16()
                            )
                        })
                })
                .or_else(|| {
                    (generation.config.tuning.failover_enabled
                        && empty_responses_answer(&rest, resp.status(), resp.headers()))
                    .then(|| {
                        format!(
                            "intercepted by the line's edge (HTTP {} with an empty body)",
                            resp.status().as_u16()
                        )
                    })
                }) {
                Some(reason) => {
                    if resp.status() == StatusCode::UNAUTHORIZED
                        && let (Some(v), Some(t)) = (
                            member.vault.as_ref(),
                            parts
                                .extensions
                                .get::<crate::official::ResolvedAuth>()
                                .and_then(|a| a.stored.as_ref()),
                        )
                    {
                        v.note_refused(t.expose());
                    }
                    let opened = generation.soft_failure(c, reason.clone());
                    note_line_down(&shared, &generation, c, opened, &reason);
                    (reason, Some(resp.status().as_u16()))
                }
                None => {
                    let resp = if converted {
                        match translate::body::response_with_tools(
                            resp,
                            upstream,
                            Protocol::Responses,
                            &response_tools,
                        )
                        .await
                        {
                            Ok(resp) => resp,
                            Err(e) => {
                                return translate::body::error(
                                    Protocol::Responses,
                                    StatusCode::BAD_GATEWAY,
                                    &e,
                                );
                            }
                        }
                    } else {
                        resp
                    };
                    let resp = match crate::compaction_proxy::finish(
                        &proxy_compaction,
                        &generation,
                        c.member,
                        resp,
                    )
                    .await
                    {
                        Ok(response) => response,
                        Err(failure) => {
                            shared.event(
                                RouteEventKind::Compaction,
                                Some(&member.cfg.id),
                                Some(&line.cfg.id),
                                Some(failure.message.clone()),
                            );
                            if let Some(t) = tracker {
                                t.finish(End {
                                    status: Some(superba_core::RequestStatus::Http {
                                        status: failure.status,
                                        code: Some(failure.code.into()),
                                    }),
                                    error: Some(failure.message.clone()),
                                    ..End::default()
                                });
                            }
                            return crate::compaction_proxy::error(failure);
                        }
                    };
                    if let (Some(cb), Some(t)) = (&ciphers, &mut tracker) {
                        let sent = cb.sent(&shared.stats.ciphers, &member.cfg.id);
                        t.ciphers_sent(sent.ciphers.into_iter().map(|c| c.fp).collect());
                    }
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
                    // A member's 401 says nothing about the user's ChatGPT
                    // login: never let Codex refresh it for that.
                    let relay_auth_error = resp.status() == StatusCode::UNAUTHORIZED
                        && member.cfg.kind != crate::MemberKind::Official
                        && parts
                            .extensions
                            .get::<crate::official::OfficialClient>()
                            .is_some();
                    let mut out = downstream(
                        shared.clone(),
                        generation.clone(),
                        c,
                        &label,
                        resp,
                        (active, admission),
                        (tracker, ingress),
                    );
                    if relay_auth_error {
                        *out.status_mut() = StatusCode::BAD_GATEWAY;
                    }
                    return out;
                }
            },
        };
        if let Some(t) = &mut tracker {
            t.attempt_failed(c, status, &reason, started.elapsed());
        }
        if generation.config.tuning.failover_enabled {
            line.counters.failovers_away.fetch_add(1, Ordering::Relaxed);
        }
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
    if let Some(response) = preparation_error {
        if let Some(t) = tracker {
            t.finish(End {
                status: Some(superba_core::RequestStatus::Http {
                    status: response.status().as_u16(),
                    code: Some("compaction_prepare_failed".into()),
                }),
                ..End::default()
            });
        }
        return *response;
    }
    // A compacted conversation never fails over to a member that cannot
    // read it: say so instead of a bare "no line reachable".
    let (status, code, message) = match &route {
        Route::Members {
            kept_on: Some(o), ..
        } => (
            StatusCode::SERVICE_UNAVAILABLE,
            compaction::OWNER_UNAVAILABLE_CODE,
            compaction::owner_unavailable_message(
                &generation.members[*o].cfg.name,
                &failures.join("; "),
            ),
        ),
        _ => (
            StatusCode::BAD_GATEWAY,
            "router_unavailable",
            format!("no line reachable ({})", failures.join("; ")),
        ),
    };
    if let Some(t) = tracker {
        t.finish(End {
            status: Some(superba_core::RequestStatus::Http {
                status: status.as_u16(),
                code: Some(code.into()),
            }),
            error: Some(failures.join("; ").chars().take(300).collect()),
            ..End::default()
        });
    }
    router_error(status, code, &message)
}

/// The compaction facts of a Responses body Codex sent as it is or
/// zstd-compressed (other encodings are not read).
fn inspect_encoded(headers: &HeaderMap, body: &Bytes, limit: usize) -> CompactionInput {
    match content_encoding(headers).as_deref() {
        None => compaction::inspect(body),
        Some("zstd") => strip::decode_zstd(body, limit)
            .map(|plain| compaction::inspect(&plain))
            .unwrap_or_default(),
        Some(_) => CompactionInput::default(),
    }
}

/// A routing event when compaction keeps a request off the member it would
/// otherwise have gone to (its conversation's pin, or the preferred member).
pub(crate) fn note_compaction_route(
    shared: &Shared,
    generation: &Generation,
    route: &Route,
    pin: Option<Pin>,
    now: Instant,
    chosen: Option<&Candidate>,
) {
    if matches!(route, Route::Any) {
        return;
    }
    let usual = pin.map(|p| p.member).or_else(|| {
        candidates(
            &View {
                generation,
                websocket_only: false,
                route: &Route::Any,
            },
            None,
            now,
        )
        .first()
        .map(|c| c.member)
    });
    let Some(usual) = usual.filter(|u| !route.allows(*u)) else {
        return;
    };
    let from = &generation.members[usual].cfg.name;
    let detail = match (route, chosen) {
        (
            Route::Members {
                kept_on: Some(o), ..
            },
            _,
        ) => format!(
            "compacted conversation kept on “{}” (made its compaction item) instead of “{from}”",
            generation.members[*o].cfg.name
        ),
        (_, Some(c)) => format!(
            "compaction went to “{}”: “{from}” does not support remote compaction",
            generation.members[c.member].cfg.name
        ),
        _ => return,
    };
    let member = chosen.map(|c| generation.members[c.member].cfg.id.as_str());
    shared.event(RouteEventKind::Compaction, member, None, Some(detail));
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
    rest: (&str, Ingress),
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
    (rest, ingress): (&str, Ingress),
    c: Candidate,
    body: Bytes,
    drop_turn_state: bool,
    zstd: bool,
) -> Result<reqwest::Response, reqwest::Error> {
    let member = &generation.members[c.member];
    let line = &member.lines[c.line];
    let base = match ingress {
        Ingress::Codex => &line.cfg.base_url,
        Ingress::OpenAi => generation.openai.members[c.member]
            .bases(crate::openai::OpenAiProtocol::Responses)
            .and_then(|b| b.get(c.line))
            .unwrap_or(&line.cfg.base_url),
    };
    let selected_base = parts
        .extensions
        .get::<crate::merged_catalog::Selection>()
        .and_then(|s| s.base(generation, c));
    let base = selected_base.as_ref().unwrap_or(base);
    let upstream = generation
        .config
        .upstream_protocol(&member.cfg.id, Protocol::Responses)
        .unwrap_or(Protocol::Responses);
    let converted = upstream != Protocol::Responses && is_responses_path(&parts.method, rest);
    let (url, mut headers) = if converted {
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        let model = v["model"].as_str().unwrap_or_default();
        let base = routing::base(&generation.config, c.member, c.line, upstream)
            .unwrap_or_else(|| base.clone());
        (
            routing::url(
                &base,
                upstream,
                model,
                v["stream"].as_bool().unwrap_or(true),
            ),
            routing::headers(&parts.headers, generation, c.member, upstream),
        )
    } else {
        let official = parts.extensions.get::<crate::official::ResolvedAuth>();
        let mut h = request_headers(
            &parts.headers,
            false,
            drop_turn_state,
            official.map_or(&member.auth, |a| &a.auth),
        );
        if let Some(a) = official
            && let Ok(v) = http::HeaderValue::from_str(&a.account_id)
        {
            h.insert(crate::official::ACCOUNT_HEADER, v);
        }
        (join_path(base, rest, parts.uri.query()), h)
    };
    if let Some(route) = parts.extensions.get::<crate::catalog::ValidatorRoute>()
        && route.0.to_str().ok() != Some(generation.label(c.member, c.line).as_str())
    {
        headers.remove(http::header::IF_NONE_MATCH);
    }
    if ingress == Ingress::OpenAi {
        headers.remove(http::header::ACCEPT_ENCODING);
    }
    if zstd {
        headers.insert(CONTENT_ENCODING, http::HeaderValue::from_static("zstd"));
    }
    let mut rb = generation
        .http
        .request(parts.method.clone(), url)
        .headers(headers);
    if !body.is_empty() {
        rb = rb.body(if converted {
            routing::wire_body(upstream, body)
        } else {
            body
        });
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
    *lock(&member.last_route) = Some(superba_core::RouterLineRoute {
        line_id: line.cfg.id.clone(),
        failed_lines: member.lines[..c.line]
            .iter()
            .filter(|_| generation.config.tuning.failover_enabled)
            .filter(|l| lock(&l.health).last_error().is_some())
            .map(|l| l.cfg.id.clone())
            .collect(),
    });
    line.counters.requests.fetch_add(1, Ordering::Relaxed);
    if !generation.config.tuning.failover_enabled {
        return;
    }
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
    resp: UpResponse,
    (active, admission): (GaugeGuard, Admission),
    (tracker, ingress): (Option<Tracker>, Ingress),
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
    let inner = resp.into_body();
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
                    let line = match ingress {
                        Ingress::Codex => t.line_setup(),
                        Ingress::OpenAi => t.line_setup_if(crate::openai::line::allowed),
                    };
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

// ---------------------------------------------------------------------------
// Ciphertexts another provider made (GitHub issue 11)
// ---------------------------------------------------------------------------

/// Largest error answer read before it is passed on.
const REFUSAL_READ_LIMIT: usize = 64 * 1024;
/// How long reading such an answer may take.
const REFUSAL_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn content_type_of(headers: &HeaderMap) -> Option<&str> {
    headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok())
}

/// A Responses body that carries ciphertexts, decoded when Codex sent it
/// zstd-compressed.
struct CipherBody {
    plain: Bytes,
    zstd: bool,
    scan: cipher::Scan,
}

impl CipherBody {
    /// `None` when the body carries no ciphertext (or an encoding the router
    /// does not read).
    fn read(headers: &HeaderMap, body: &Bytes, limit: usize) -> Option<Self> {
        let (plain, zstd) = match content_encoding(headers).as_deref() {
            None => (body.clone(), false),
            Some("zstd") => (Bytes::from(strip::decode_zstd(body, limit)?), true),
            Some(_) => return None,
        };
        let scan = cipher::scan(&plain);
        (!scan.is_empty()).then_some(Self { plain, zstd, scan })
    }

    /// The body for `member` without what it refused before.
    fn for_member(&self, ledger: &cipher::Ledger, member: &str) -> Option<(Bytes, Removed)> {
        let (new, removed) = cipher::neutralise(&self.plain, &|fp| ledger.refuses(member, fp))?;
        let bytes = if self.zstd {
            Bytes::from(zstd::stream::encode_all(new.as_slice(), 3).ok()?)
        } else {
            Bytes::from(new)
        };
        Some((bytes, removed))
    }

    /// The ciphertexts that go to `member` (those it did not refuse before).
    fn sent(&self, ledger: &cipher::Ledger, member: &str) -> cipher::Scan {
        cipher::Scan {
            ciphers: self
                .scan
                .ciphers
                .iter()
                .filter(|c| !ledger.refuses(member, &c.fp))
                .cloned()
                .collect(),
            task_item: self.scan.task_item,
        }
    }

    /// What leaving out `set` changes.
    fn removed_by(&self, set: &std::collections::BTreeSet<compaction::Fingerprint>) -> Removed {
        cipher::neutralise(&self.plain, &|fp| set.contains(fp))
            .map(|(_, r)| r)
            .unwrap_or_default()
    }
}

/// The body a member gets: without the ciphertexts it refused before
/// (what was left out), with its model names mapped.
fn member_body(
    shared: &Shared,
    body: &Bytes,
    ciphers: Option<&CipherBody>,
    member: &MemberRt,
    headers: &HeaderMap,
    limit: usize,
) -> (Bytes, Option<Removed>) {
    let (base, left_out) =
        match ciphers.and_then(|cb| cb.for_member(&shared.stats.ciphers, &member.cfg.id)) {
            Some((b, r)) => (b, Some(r)),
            None => (body.clone(), None),
        };
    let mapped = map_model(&base, headers, &member.cfg.model_map, limit).unwrap_or(base);
    (mapped, left_out)
}

/// Read an answer of at most [`REFUSAL_READ_LIMIT`] bytes; the answer comes
/// back whole either way, with its bytes when it ended within the limit.
pub(crate) async fn read_small(resp: UpResponse) -> (UpResponse, Option<Bytes>) {
    let (head, mut body) = resp.into_parts();
    let mut buf = bytes::BytesMut::new();
    let deadline = tokio::time::Instant::now() + REFUSAL_READ_TIMEOUT;
    loop {
        if buf.len() > REFUSAL_READ_LIMIT {
            break;
        }
        match tokio::time::timeout_at(deadline, body.frame()).await {
            Ok(Some(Ok(frame))) => {
                if let Ok(data) = frame.into_data() {
                    buf.extend_from_slice(&data);
                }
            }
            Ok(None) => {
                let bytes = buf.freeze();
                let whole = bytes.clone();
                return (
                    http::Response::from_parts(head, UpBody::replay(bytes)),
                    Some(whole),
                );
            }
            // A broken or slow answer is passed on as it is.
            Ok(Some(Err(_))) | Err(_) => break,
        }
    }
    (
        http::Response::from_parts(head, body.prepend(buf.freeze())),
        None,
    )
}
