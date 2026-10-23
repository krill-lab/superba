//! `POST /claude/v1/messages`: resolve the model, try the targets' lines in
//! order (failover before the first byte only), and pass the answer through,
//! streamed or not, with the stats line when it applies.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use bytes::Bytes;
use http::header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE};
use http::request::Parts;
use http::{HeaderMap, Response, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use superba_core::RequestStatus;

use super::error::{self, Kind};
use super::route::{self, Target};
use super::rt::MemberClaude;
use super::tap::{AnthropicSse, AnthropicUsage};
use super::{line, meta, passthrough};
use crate::config::join_path;
use crate::forward::{
    Observed, UpResponse, describe_error, failover_status_in, full, response_headers, set_route,
    up_response,
};
use crate::limits::{Admission, LOCAL_BUSY_CODE, Place, file_limit_detail, file_limit_in};
use crate::proxy::{note_line_down, on_answer};
use crate::select::Candidate;
use crate::state::{GaugeGuard, Generation, RespBody, Shared, lock};
use crate::stats::body::{Mode, TapBody};
use crate::stats::hub::{End, Tracker};
use crate::stats::tap::{LineSetup, Outcome, TapSummary, render_line};
use crate::status::RouteEventKind;
use crate::translate::{self, ir::Protocol, routing};

/// Largest non-streamed answer read whole for the stats line.
const MAX_COLLECT: usize = 64 * 1024 * 1024;

/// The request body for `target` (the client's bytes when nothing changes).
fn prepare(
    req: &Value,
    original: Option<&Bytes>,
    target: &Target,
    c: &MemberClaude,
) -> (Bytes, Vec<String>) {
    let (body, notes) = passthrough::body(req, c, &target.model);
    let body = match (body, original) {
        (Some(b), _) => b,
        (None, Some(o)) => o.clone(),
        (None, None) => Bytes::from(serde_json::to_vec(req).unwrap_or_default()),
    };
    (body, notes)
}

/// Why an answer is a failover reason (502-504, 529, an edge interception).
fn failover_reason(statuses: &[u16], resp: &reqwest::Response) -> Option<String> {
    failover_status_in(statuses, resp.status(), resp.headers())
        .or_else(|| (resp.status().as_u16() == 529).then(|| "HTTP 529".to_owned()))
}

fn record_refusal(tracker: Option<Tracker>, status: u16, code: &str, message: &str) {
    if let Some(t) = tracker {
        t.finish(End {
            status: Some(RequestStatus::Http {
                status,
                code: Some(code.to_owned()),
            }),
            error: Some(message.chars().take(300).collect()),
            ..End::default()
        });
    }
}

/// Serve one Messages request.
pub(super) async fn messages(
    shared: Arc<Shared>,
    parts: Parts,
    raw: Bytes,
    admission: Admission,
    t0: Instant,
    started_ms: i64,
) -> Response<RespBody> {
    let generation = shared.current();
    shared.totals.requests.fetch_add(1, Ordering::Relaxed);
    let active = GaugeGuard::new(shared.clone(), |t| &t.active_requests);
    let limit = generation.config.tuning.max_body_bytes;
    let mut req = match super::parse_body(&parts.headers, &raw, limit) {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let Some(requested) = req
        .get("model")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
        .map(str::to_owned)
    else {
        return error::local(
            StatusCode::BAD_REQUEST,
            Kind::InvalidRequest,
            "model: a model name is required",
        );
    };
    let stripped = line::strip_request(&mut req);
    // dsh's session-log upload and plugin list (its DeepSeek route) never
    // leave the router ([`crate::openai::shape`]).
    let private = crate::openai::shape::drop_private(&mut req);
    let identity = !parts.headers.contains_key(CONTENT_ENCODING);
    let original = (stripped == 0 && private.is_empty() && identity).then_some(&raw);
    let served = if crate::forward::bound_member(&parts.headers).is_some() {
        requested.clone()
    } else {
        route::alias(&generation, &requested)
    };
    let mut meta = meta::claude_meta(&parts.headers, &req, &requested, &served);
    meta.stripped = stripped;
    let mut tracker = generation.config.stats.is_some().then(|| {
        shared
            .stats
            .track(generation.clone(), meta.clone(), t0, started_ms)
    });
    if !private.is_empty()
        && let Some(t) = &mut tracker
    {
        t.note(format!(
            "{} left out (never sent upstream)",
            private.join(", ")
        ));
    }
    let resolution = match route::resolve(
        &shared,
        &generation,
        &requested,
        crate::forward::bound_member(&parts.headers),
    )
    .await
    {
        Ok(r) => r,
        Err((status, kind, message)) => {
            record_refusal(tracker, status, kind.name(), &message);
            return error::local(
                StatusCode::from_u16(status).unwrap_or(StatusCode::NOT_FOUND),
                kind,
                &message,
            );
        }
    };
    let key = meta
        .session_id
        .as_deref()
        .map(|s| format!("claude:{s}:{}", resolution.model));
    let now = Instant::now();
    let pin = key.as_deref().and_then(|k| generation.pin(k, now));
    let order = route::attempts(&generation, &resolution.targets, pin, now);
    let mut prepared: HashMap<usize, (Bytes, Vec<String>)> = HashMap::new();
    let mut failures: Vec<String> = Vec::new();
    let total = order.len();
    for (i, a) in order.into_iter().enumerate() {
        let target = &resolution.targets[a.target];
        let member = &generation.members[a.c.member];
        let line = &member.lines[a.c.line];
        let label = generation.label(a.c.member, a.c.line);
        let Some(mc) = generation
            .claude
            .as_ref()
            .and_then(|c| c.member(a.c.member))
        else {
            continue;
        };
        let (body, notes) = if mc.protocol == Protocol::Messages {
            prepared
                .entry(a.target)
                .or_insert_with(|| prepare(&req, original, target, mc))
                .clone()
        } else {
            match translate::body::request(Protocol::Messages, mc.protocol, &req, &target.model) {
                Ok(body) => (
                    body,
                    vec![format!(
                        "protocol conversion: messages -> {:?}",
                        mc.protocol
                    )],
                ),
                Err(e) => {
                    record_refusal(tracker, 400, "protocol_conversion_error", &e.to_string());
                    return translate::body::error(Protocol::Messages, StatusCode::BAD_REQUEST, &e);
                }
            }
        };
        if let Some(t) = &mut tracker {
            t.trying(a.c);
            t.set_upstream_model(Some(target.model.clone()));
            for n in notes {
                t.note(n);
            }
        }
        let started = Instant::now();
        let url = if mc.protocol == Protocol::Messages {
            join_path(&mc.base[a.c.line], "/v1/messages", parts.uri.query())
        } else {
            routing::url(
                &mc.base[a.c.line],
                mc.protocol,
                &target.model,
                req["stream"].as_bool().unwrap_or(false),
            )
        };
        let headers = if mc.protocol == Protocol::Messages {
            passthrough::headers(&parts.headers, mc)
        } else {
            routing::headers(&parts.headers, &generation, a.c.member, mc.protocol)
        };
        let result = generation
            .http
            .post(url)
            .headers(headers)
            .body(routing::wire_body(mc.protocol, body))
            .send()
            .await;
        let (reason, status) = match result {
            Err(e) if file_limit_in(&e).is_some() => {
                let message =
                    shared.local_file_limit(Place::Upstream(label.clone()), &file_limit_detail(&e));
                record_refusal(tracker, 529, LOCAL_BUSY_CODE, &message);
                return error::local_busy(&message);
            }
            Err(e) => {
                let reason = describe_error(&e);
                let opened = lock(&line.health).on_hard_failure(Instant::now(), reason.clone());
                note_line_down(&shared, &generation, a.c, opened, &reason);
                (reason, None)
            }
            Ok(resp) => match failover_reason(&generation.config.tuning.failover_statuses, &resp) {
                Some(reason) => {
                    let opened = lock(&line.health).on_soft_failure(Instant::now(), reason.clone());
                    note_line_down(&shared, &generation, a.c, opened, &reason);
                    (reason, Some(resp.status().as_u16()))
                }
                None => {
                    let ttfb = started.elapsed();
                    on_answer(
                        &shared,
                        &generation,
                        a.c,
                        pin,
                        key.as_deref(),
                        resp.status(),
                        ttfb,
                    );
                    tracing::debug!(
                        route = %label,
                        status = resp.status().as_u16(),
                        ttfb_ms = u64::try_from(ttfb.as_millis()).unwrap_or(u64::MAX),
                        "claude proxied"
                    );
                    if let Some(t) = &mut tracker {
                        t.answered(
                            a.c,
                            resp.status().as_u16(),
                            resp.headers(),
                            started,
                            Instant::now(),
                        );
                    }
                    let answer = Answer {
                        shared: shared.clone(),
                        generation: generation.clone(),
                        c: a.c,
                        label,
                        t0,
                    };
                    let mut resp = up_response(resp);
                    if mc.protocol != Protocol::Messages {
                        resp =
                            match translate::body::response(resp, mc.protocol, Protocol::Messages)
                                .await
                            {
                                Ok(resp) => resp,
                                Err(e) => {
                                    record_refusal(
                                        tracker,
                                        502,
                                        "protocol_conversion_error",
                                        &e.to_string(),
                                    );
                                    return translate::body::error(
                                        Protocol::Messages,
                                        StatusCode::BAD_GATEWAY,
                                        &e,
                                    );
                                }
                            };
                    }
                    return answer.downstream(resp, tracker, active, admission).await;
                }
            },
        };
        if let Some(t) = &mut tracker {
            t.attempt_failed(a.c, status, &reason, started.elapsed());
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
    let detail = failures.join("; ");
    shared.event(
        RouteEventKind::Unavailable,
        None,
        None,
        Some(detail.clone()),
    );
    record_refusal(tracker, 502, "router_unavailable", &detail);
    error::local(
        StatusCode::BAD_GATEWAY,
        Kind::Api,
        &format!("no line reachable ({detail})"),
    )
}

/// The line that answered.
struct Answer {
    shared: Arc<Shared>,
    generation: Arc<Generation>,
    c: Candidate,
    label: String,
    t0: Instant,
}

fn is_sse(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("text/event-stream"))
}

impl Answer {
    /// A response streaming `body` with `headers` (through the tap when the
    /// request is tracked).
    fn respond<B>(
        &self,
        status: StatusCode,
        mut headers: HeaderMap,
        body: B,
        tap: Option<(Mode, Tracker)>,
        (active, admission): (GaugeGuard, Admission),
    ) -> Response<RespBody>
    where
        B: http_body::Body<Data = Bytes> + Unpin + Send + 'static,
        B::Error: Into<crate::state::BoxError> + std::fmt::Display,
    {
        set_route(&mut headers, &self.label);
        let (shared, generation) = (self.shared.clone(), self.generation.clone());
        let (m, l) = (self.c.member, self.c.line);
        let body = match tap {
            Some((mode, t)) => Observed::new(
                TapBody::new(body, mode, t),
                shared,
                generation,
                m,
                l,
                active,
            )
            .holding(admission)
            .boxed_unsync(),
            None => Observed::new(body, shared, generation, m, l, active)
                .holding(admission)
                .boxed_unsync(),
        };
        let mut out = Response::new(body);
        *out.status_mut() = status;
        *out.headers_mut() = headers;
        out
    }

    async fn downstream(
        self,
        resp: UpResponse,
        tracker: Option<Tracker>,
        active: GaugeGuard,
        admission: Admission,
    ) -> Response<RespBody> {
        let status = resp.status();
        let sse = is_sse(resp.headers());
        let line: Option<LineSetup> = tracker
            .as_ref()
            .and_then(|t| t.line_setup_if(line::allowed))
            .filter(|_| status.is_success());
        let mut headers = response_headers(resp.headers(), false);
        if line.is_some() && !sse {
            // A non-streamed answer that gets the line: read it whole.
            let body = resp.into_body();
            let bytes = match http_body_util::Limited::new(body, MAX_COLLECT)
                .collect()
                .await
            {
                Ok(c) => c.to_bytes(),
                Err(e) => {
                    let detail: String = format!("stream broken: {e}").chars().take(200).collect();
                    return self.broken(tracker, &detail);
                }
            };
            headers.remove(CONTENT_LENGTH);
            return match serde_json::from_slice::<Value>(&bytes) {
                Ok(msg) => {
                    self.json_answer(status, headers, msg, line, tracker, (active, admission))
                }
                Err(_) => {
                    let tap = tracker.map(|t| (Mode::json(status.as_u16()), t));
                    self.respond(status, headers, full(bytes), tap, (active, admission))
                }
            };
        }
        if line.is_some() {
            headers.remove(CONTENT_LENGTH);
        }
        let tap = tracker.map(|t| {
            let mode = if sse {
                Mode::Events(Box::new(AnthropicSse::new(self.t0, line)))
            } else {
                Mode::json(status.as_u16())
            };
            (mode, t)
        });
        let body = resp.into_body();
        self.respond(status, headers, body, tap, (active, admission))
    }

    /// A complete answer with the stats line; the request's row is finished
    /// now.
    fn json_answer(
        &self,
        status: StatusCode,
        headers: HeaderMap,
        mut msg: Value,
        line: Option<LineSetup>,
        tracker: Option<Tracker>,
        guards: (GaugeGuard, Admission),
    ) -> Response<RespBody> {
        let mut usage = AnthropicUsage::default();
        usage.merge(msg.get("usage"));
        // A whole answer has no first token of its own: the line shows its
        // tokens and cost, not a speed.
        let mut summary = TapSummary {
            end: Some(self.t0.elapsed()),
            usage: usage.normalised(),
            response_model: msg
                .get("model")
                .and_then(Value::as_str)
                .map(|s| s.chars().take(64).collect()),
            response_id: msg
                .get("id")
                .and_then(Value::as_str)
                .map(crate::stats::tap::clip_id),
            outcome: Some(match msg.get("stop_reason").and_then(Value::as_str) {
                Some(r @ ("max_tokens" | "refusal")) => Outcome::Incomplete {
                    reason: Some(r.to_owned()),
                },
                _ => Outcome::Completed,
            }),
            ..TapSummary::default()
        };
        if let Some(l) = &line {
            let suffix = render_line(l, &summary, summary.usage, None);
            summary.injected = line::inject_json(&mut msg, &suffix);
        }
        if let Some(t) = tracker {
            t.finish(End {
                summary: Some(summary),
                ..End::default()
            });
        }
        self.respond(
            status,
            headers,
            full(Bytes::from(msg.to_string())),
            None,
            guards,
        )
    }

    /// The upstream broke while its answer was read whole.
    fn broken(&self, tracker: Option<Tracker>, detail: &str) -> Response<RespBody> {
        let m = &self.generation.members[self.c.member];
        let l = &m.lines[self.c.line];
        let opened = lock(&l.health).on_hard_failure(Instant::now(), detail.to_owned());
        note_line_down(&self.shared, &self.generation, self.c, opened, detail);
        if let Some(t) = tracker {
            t.finish(End {
                status: Some(RequestStatus::Broken),
                error: Some(detail.to_owned()),
                ..End::default()
            });
        }
        error::local(
            StatusCode::BAD_GATEWAY,
            Kind::Api,
            &format!("the upstream answer broke off ({detail})"),
        )
    }
}
