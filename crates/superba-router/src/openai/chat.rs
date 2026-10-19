//! `POST /openai/v1/chat/completions`: pick the members that serve Chat
//! Completions ([`super::route`]), try their lines in order (failover before
//! the first byte only), and pass the answer through, streamed or not, with
//! the stats line when it applies.
//!
//! Body (re-serialised only when something changes): earlier stats lines and
//! the private `dsh_*` fields removed, the member's model name
//! (`model_map`), the member's `drop_fields` and the extra reasoning fields
//! it refused before left out. A 400 / 422 naming one of those fields
//! ([`super::shape::rejected_field`]) sends the request once more to the same
//! line without it, and later requests leave it out for that member.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use bytes::Bytes;
use http::header::{ACCEPT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE};
use http::request::Parts;
use http::{HeaderMap, Response, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use superba_core::RequestStatus;

use super::config::OpenAiProtocol;
use super::shape::{REASONING_EXTRAS, drop_private, rejected_field};
use super::tap::{ChatSse, chat_usage};
use super::{line, meta, route};
use crate::claude::route::{Target, attempts};
use crate::config::join_path;
use crate::forward::{
    Observed, UpResponse, describe_error, failover_status, full, request_headers, response_headers,
    router_error, set_route, up_response,
};
use crate::limits::{Admission, LOCAL_BUSY_CODE, Place, file_limit_detail, file_limit_in};
use crate::proxy::{note_line_down, on_answer, read_small};
use crate::select::Candidate;
use crate::state::{GaugeGuard, Generation, RespBody, Shared, lock};
use crate::stats::body::{Mode, TapBody};
use crate::stats::hub::{End, Tracker};
use crate::stats::tap::{LineSetup, Outcome, TapSummary, render_line};
use crate::status::RouteEventKind;
use crate::translate::{self, ir::Protocol, routing};

/// Largest non-streamed answer read whole for the stats line.
const MAX_COLLECT: usize = 64 * 1024 * 1024;

/// The body for `target` on `member`: `(bytes, notes, extras sent)`, the
/// client's bytes when nothing changes.
fn prepare(
    shared: &Shared,
    generation: &Generation,
    req: &Value,
    original: Option<&Bytes>,
    target: &Target,
) -> (Bytes, Vec<String>, Vec<String>) {
    let member = &generation.members[target.member];
    let cfg = &generation.openai.members[target.member].cfg;
    let refused = shared.openai.refused_fields(&member.cfg.id);
    let mut v = req.clone();
    let mut notes = Vec::new();
    let mut changed = false;
    let Some(obj) = v.as_object_mut() else {
        return (original.cloned().unwrap_or_default(), notes, Vec::new());
    };
    if let Some(m) = obj.get("model").and_then(Value::as_str)
        && let Some(to) = member.cfg.model_map.get(m).filter(|to| to.as_str() != m)
    {
        obj.insert("model".into(), Value::String(to.clone()));
        changed = true;
    }
    for f in cfg.drop_fields.iter().chain(refused.iter()) {
        if obj.remove(f).is_some() {
            changed = true;
            notes.push(format!(
                "{}: field {f} left out for this provider",
                member.cfg.id
            ));
        }
    }
    let extras: Vec<String> = REASONING_EXTRAS
        .iter()
        .filter(|f| obj.contains_key(**f))
        .map(|f| (*f).to_owned())
        .collect();
    let bytes = match (changed, original) {
        (false, Some(o)) => o.clone(),
        _ => Bytes::from(serde_json::to_vec(&v).unwrap_or_default()),
    };
    (bytes, notes, extras)
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

/// The request headers for `member`: the router's rules, without
/// `Accept-Encoding` (the answer is read for the request log).
fn headers_for(src: &HeaderMap, generation: &Generation, member: usize) -> HeaderMap {
    let mut h = request_headers(src, false, false, &generation.members[member].auth);
    h.remove(ACCEPT_ENCODING);
    h
}

/// Serve one Chat Completions request.
pub(super) async fn chat(
    shared: Arc<Shared>,
    mut parts: Parts,
    raw: Bytes,
    admission: Admission,
    t0: Instant,
    started_ms: i64,
) -> Response<RespBody> {
    let generation = shared.current();
    shared.totals.requests.fetch_add(1, Ordering::Relaxed);
    let active = GaugeGuard::new(shared.clone(), |t| &t.active_requests);
    let limit = generation.config.tuning.max_body_bytes;
    let raw = match super::body::decode(&mut parts.headers, raw, limit) {
        Ok(b) => b,
        Err(resp) => return *resp,
    };
    let mut req = match super::parse_body(&raw) {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let stripped = line::strip_request(&mut req);
    let private = drop_private(&mut req);
    let mut meta = meta::of_request(
        &parts.headers,
        &meta::peek_value(&req),
        OpenAiProtocol::Chat,
    );
    meta.stripped = stripped;
    let original = (stripped == 0 && private.is_empty()).then_some(&raw);
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
    route::note_excluded(&generation, OpenAiProtocol::Chat, tracker.as_mut());
    let Some(model) = meta.model.clone() else {
        let message = "model: a model name is required";
        record_refusal(tracker, 400, "invalid_request_error", message);
        return router_error(StatusCode::BAD_REQUEST, "invalid_request_error", message);
    };
    let targets =
        match route::resolve(&shared, &generation, OpenAiProtocol::Chat, Some(&model)).await {
            Ok(t) => t,
            Err((status, code, message)) => {
                record_refusal(tracker, status, code, &message);
                return router_error(
                    StatusCode::from_u16(status).unwrap_or(StatusCode::NOT_FOUND),
                    code,
                    &message,
                );
            }
        };
    let key = route::sticky_key(&meta);
    let now = Instant::now();
    let pin = key
        .as_deref()
        .and_then(|k| lock(&generation.sticky).get(k, now));
    let order = attempts(&generation, &targets, pin, now);
    let mut prepared: HashMap<usize, (Bytes, Vec<String>, Vec<String>)> = HashMap::new();
    let mut failures: Vec<String> = Vec::new();
    let total = order.len();
    for (i, a) in order.into_iter().enumerate() {
        let target = &targets[a.target];
        let member = &generation.members[a.c.member];
        let line = &member.lines[a.c.line];
        let label = generation.label(a.c.member, a.c.line);
        let Some(upstream) = generation
            .config
            .upstream_protocol(&member.cfg.id, Protocol::Chat)
        else {
            continue;
        };
        let Some(base) = routing::base(&generation.config, a.c.member, a.c.line, upstream) else {
            continue;
        };
        let upstream_model = member
            .cfg
            .model_map
            .get(&target.model)
            .unwrap_or(&target.model);
        let url = if upstream == Protocol::Chat {
            join_path(&base, OpenAiProtocol::Chat.path(), parts.uri.query())
        } else {
            routing::url(
                &base,
                upstream,
                upstream_model,
                req["stream"].as_bool().unwrap_or(false),
            )
        };
        if let Some(t) = &tracker {
            t.trying(a.c);
        }
        let mut resent = false;
        let (result, started) = loop {
            let fresh = !prepared.contains_key(&a.c.member);
            let (body, notes, extras) = prepared
                .entry(a.c.member)
                .or_insert_with(|| prepare(&shared, &generation, &req, original, target))
                .clone();
            if fresh && let Some(t) = &mut tracker {
                for n in notes {
                    t.note(n);
                }
            }
            let body = if upstream == Protocol::Chat {
                body
            } else {
                match translate::body::request(Protocol::Chat, upstream, &req, upstream_model) {
                    Ok(body) => body,
                    Err(e) => {
                        record_refusal(tracker, 400, "protocol_conversion_error", &e.to_string());
                        return translate::body::error(Protocol::Chat, StatusCode::BAD_REQUEST, &e);
                    }
                }
            };
            let started = Instant::now();
            let result = generation
                .http
                .post(url.clone())
                .headers(if upstream == Protocol::Chat {
                    headers_for(&parts.headers, &generation, a.c.member)
                } else {
                    routing::headers(&parts.headers, &generation, a.c.member, upstream)
                })
                .body(routing::wire_body(upstream, body))
                .send()
                .await
                .map(up_response);
            // A refusal of an extra field arrives as a small JSON error before
            // any byte reached the tool: read it, then decide.
            let resp = match result {
                Ok(r)
                    if !resent
                        && !extras.is_empty()
                        && matches!(r.status().as_u16(), 400 | 422) =>
                {
                    r
                }
                other => break (other, started),
            };
            let (resp, whole) = read_small(resp).await;
            let candidates: Vec<&str> = extras.iter().map(String::as_str).collect();
            let Some(field) = whole
                .as_deref()
                .and_then(|b| rejected_field(b, &candidates))
            else {
                break (Ok(resp), started);
            };
            let status = resp.status().as_u16();
            shared.openai.refuse_field(&member.cfg.id, &field);
            if let Some(t) = &mut tracker {
                t.attempt_failed(
                    a.c,
                    Some(status),
                    &format!("refused field {field}"),
                    started.elapsed(),
                );
                t.note(format!(
                    "{}: refused field {field}; sent again without it",
                    member.cfg.id
                ));
            }
            tracing::info!(
                member = %member.cfg.id,
                %field,
                "Chat Completions field refused; left out for this member from now on"
            );
            resent = true;
            prepared.remove(&a.c.member);
        };
        let (reason, status) = match result {
            Err(e) if file_limit_in(&e).is_some() => {
                let message =
                    shared.local_file_limit(Place::Upstream(label.clone()), &file_limit_detail(&e));
                record_refusal(tracker, 503, LOCAL_BUSY_CODE, &message);
                return crate::forward::local_busy(&message);
            }
            Err(e) => {
                let reason = describe_error(&e);
                let opened = lock(&line.health).on_hard_failure(Instant::now(), reason.clone());
                note_line_down(&shared, &generation, a.c, opened, &reason);
                (reason, None)
            }
            Ok(resp) => match failover_status(resp.status(), resp.headers()) {
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
                        "chat completions proxied"
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
                    let resp = if upstream != Protocol::Chat {
                        match translate::body::response(resp, upstream, Protocol::Chat).await {
                            Ok(resp) => resp,
                            Err(e) => {
                                record_refusal(
                                    tracker,
                                    502,
                                    "protocol_conversion_error",
                                    &e.to_string(),
                                );
                                return translate::body::error(
                                    Protocol::Chat,
                                    StatusCode::BAD_GATEWAY,
                                    &e,
                                );
                            }
                        }
                    } else {
                        resp
                    };
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
    router_error(
        StatusCode::BAD_GATEWAY,
        "router_unavailable",
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
        let body = resp.into_body();
        if line.is_some() && !sse {
            // A non-streamed answer that gets the line: read it whole.
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
                Mode::Events(Box::new(ChatSse::new(self.t0, line)))
            } else {
                Mode::json(status.as_u16())
            };
            (mode, t)
        });
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
        let finish = msg
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
            .map(str::to_owned);
        // A whole answer has no first token of its own: the line shows its
        // tokens and cost, not a speed.
        let mut summary = TapSummary {
            end: Some(self.t0.elapsed()),
            usage: msg.get("usage").and_then(chat_usage),
            response_model: msg
                .get("model")
                .and_then(Value::as_str)
                .map(|s| s.chars().take(64).collect()),
            response_id: msg
                .get("id")
                .and_then(Value::as_str)
                .map(crate::stats::tap::clip_id),
            outcome: Some(match finish.as_deref() {
                Some(r @ ("length" | "content_filter")) => Outcome::Incomplete {
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
        router_error(
            StatusCode::BAD_GATEWAY,
            "router_unavailable",
            &format!("the upstream answer broke off ({detail})"),
        )
    }
}
