//! Gemini v1beta ingress. Authentication happens in the server before model
//! discovery, admission or body parsing. Query API keys are never forwarded.

mod tap;

use crate::claude::route::{Target, attempts};
use crate::forward::{
    Observed, describe_error, failover_status, response_headers, set_route, up_response,
};
use crate::limits::{Admission, Place, file_limit_detail, file_limit_in};
use crate::proxy::{note_line_down, on_answer};
use crate::state::{GaugeGuard, RespBody, Shared, lock};
use crate::stats::{
    body::{Mode, TapBody},
    hub::{End, Tracker},
    meta::RequestMeta,
};
use crate::translate::{self, ir::Protocol, routing};
use http::{Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, Limited};
use hyper::body::Incoming;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;
use superba_core::{RequestHarness, RequestProtocol, RequestStatus};

/// Gemini ingress prefix including the API version.
pub const PREFIX: &str = "/gemini/v1beta";

/// Base for Gemini SDKs/CLI which append `/v1beta` themselves.
pub fn base_url(listen: std::net::SocketAddr) -> String {
    format!("http://{listen}/gemini")
}

pub(crate) fn ingress_rest(path: &str) -> Option<String> {
    let rest = path.strip_prefix(PREFIX)?;
    (rest.is_empty() || rest.starts_with('/')).then(|| rest.to_owned())
}

pub(crate) fn error(status: StatusCode, message: &str) -> Response<RespBody> {
    translate::body::error(Protocol::Gemini, status, &translate::Invalid::new(message))
}

fn refusal(t: Option<Tracker>, status: u16, message: &str) {
    if let Some(t) = t {
        t.finish(End {
            status: Some(RequestStatus::Http {
                status,
                code: Some("router_error".into()),
            }),
            error: Some(message.into()),
            ..End::default()
        });
    }
}

pub(crate) async fn handle(
    shared: Arc<Shared>,
    req: Request<Incoming>,
    rest: String,
    admission: Admission,
) -> Response<RespBody> {
    let generation = shared.current();
    let bound = crate::forward::bound_member(req.headers()).map(str::to_owned);
    if req.method() == Method::GET && rest.trim_end_matches('/') == "/models" {
        let mut models = Vec::new();
        for (index, member) in generation.members.iter().enumerate() {
            if bound.as_deref().is_some_and(|id| member.cfg.id != id)
                || generation
                    .config
                    .upstream_protocol(&member.cfg.id, Protocol::Gemini)
                    .is_none()
            {
                continue;
            }
            if let Some(list) = crate::openai::route::list_of(&shared, &generation, index).await {
                for m in list.iter() {
                    let name = format!("models/{}", m.id);
                    if !models.iter().any(|v: &Value| v["name"] == name) {
                        models.push(json!({"name":name,"displayName":m.display_name.as_deref().unwrap_or(&m.id),"supportedGenerationMethods":["generateContent","streamGenerateContent"]}));
                    }
                }
            }
        }
        return crate::claude::error::json_response(StatusCode::OK, &json!({"models":models}));
    }
    let Some((model, operation)) = rest
        .strip_prefix("/models/")
        .and_then(|r| r.rsplit_once(':'))
    else {
        return error(StatusCode::NOT_FOUND, "unknown Gemini endpoint");
    };
    let stream = match operation {
        "generateContent" => false,
        "streamGenerateContent" => true,
        _ => return error(StatusCode::NOT_FOUND, "unknown Gemini operation"),
    };
    if req.method() != Method::POST {
        return error(
            StatusCode::METHOD_NOT_ALLOWED,
            "generateContent requires POST",
        );
    }
    if stream
        && !req.uri().query().is_some_and(|q| {
            url::form_urlencoded::parse(q.as_bytes()).any(|(k, v)| k == "alt" && v == "sse")
        })
    {
        return error(
            StatusCode::BAD_REQUEST,
            "streamGenerateContent requires alt=sse",
        );
    }
    // URL path segments may be percent encoded; decoding through a URL keeps
    // model punctuation intact without treating it as another query/path.
    let model =
        url::form_urlencoded::parse(format!("model={}", model.replace('+', "%2B")).as_bytes())
            .next()
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default();
    if model.is_empty() {
        return error(StatusCode::BAD_REQUEST, "model is required");
    }
    let t0 = Instant::now();
    let started = crate::stats::hub::now_ms();
    shared.totals.requests.fetch_add(1, Ordering::Relaxed);
    let active = GaugeGuard::new(shared.clone(), |t| &t.active_requests);
    let (parts, body) = req.into_parts();
    let raw = match Limited::new(body, generation.config.tuning.max_body_bytes)
        .collect()
        .await
    {
        Ok(c) => c.to_bytes(),
        Err(_) => {
            return error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request body exceeds router limit",
            );
        }
    };
    let mut value: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(_) => return error(StatusCode::BAD_REQUEST, "request is not JSON"),
    };
    if !value.is_object() {
        return error(StatusCode::BAD_REQUEST, "request must be an object");
    }
    value["model"] = model.clone().into();
    value["stream"] = stream.into();
    let (harness, harness_version) =
        crate::stats::meta::attribution(&parts.headers, RequestHarness::Gemini);
    let session_id = parts
        .headers
        .get("x-gemini-session-id")
        .or_else(|| parts.headers.get("x-session-id"))
        .and_then(|v| v.to_str().ok())
        .filter(|s| s.len() <= 256)
        .map(str::to_owned);
    let meta = RequestMeta {
        harness,
        harness_version,
        protocol: RequestProtocol::Gemini,
        model: Some(model.clone()),
        requested_model: Some(model.clone()),
        session_id: session_id.clone(),
        thread_id: session_id,
        codex_host: parts
            .headers
            .get(crate::forward::CODEX_HOST_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        ..RequestMeta::default()
    };
    let mut tracker = generation.config.stats.is_some().then(|| {
        shared
            .stats
            .track(generation.clone(), meta.clone(), t0, started)
    });
    let mut order: Vec<usize> = generation
        .config
        .gemini
        .as_ref()
        .into_iter()
        .flat_map(|g| g.order.iter())
        .filter_map(|id| generation.members.iter().position(|m| &m.cfg.id == id))
        .collect();
    for i in 0..generation.members.len() {
        if !order.contains(&i) {
            order.push(i);
        }
    }
    let targets: Vec<Target> = order
        .into_iter()
        .filter(|i| {
            bound
                .as_deref()
                .is_none_or(|id| generation.members[*i].cfg.id == id)
                && generation
                    .config
                    .upstream_protocol(&generation.members[*i].cfg.id, Protocol::Gemini)
                    .is_some()
        })
        .map(|member| Target {
            member,
            model: generation.members[member]
                .cfg
                .model_map
                .get(&model)
                .cloned()
                .unwrap_or_else(|| model.clone()),
        })
        .collect();
    if targets.is_empty() {
        refusal(
            tracker,
            404,
            "no provider has an enabled Gemini protocol route",
        );
        return error(
            StatusCode::NOT_FOUND,
            "no provider has an enabled Gemini protocol route",
        );
    }
    let key = meta
        .session_id
        .as_deref()
        .map(|s| format!("gemini:{s}:{model}"));
    let now = Instant::now();
    let pin = key
        .as_deref()
        .and_then(|k| lock(&generation.sticky).get(k, now));
    for a in attempts(&generation, &targets, pin, now) {
        let member = &generation.members[a.c.member];
        let line = &member.lines[a.c.line];
        let target = &targets[a.target];
        let Some(protocol) = generation
            .config
            .upstream_protocol(&member.cfg.id, Protocol::Gemini)
        else {
            continue;
        };
        let Some(base) = routing::base(&generation.config, a.c.member, a.c.line, protocol) else {
            continue;
        };
        let body = if protocol == Protocol::Gemini {
            raw.clone()
        } else {
            match translate::body::request(Protocol::Gemini, protocol, &value, &target.model) {
                Ok(b) => b,
                Err(e) => {
                    refusal(tracker, 400, &e.to_string());
                    return translate::body::error(Protocol::Gemini, StatusCode::BAD_REQUEST, &e);
                }
            }
        };
        if let Some(t) = &mut tracker {
            t.trying(a.c);
            t.set_upstream_model(Some(target.model.clone()));
            if protocol != Protocol::Gemini {
                t.note(format!("protocol conversion: gemini -> {protocol:?}"));
            }
        }
        let begun = Instant::now();
        let label = generation.label(a.c.member, a.c.line);
        let upstream = generation
            .http
            .post(routing::url(&base, protocol, &target.model, stream))
            .headers(routing::headers(
                &parts.headers,
                &generation,
                a.c.member,
                protocol,
            ))
            .body(body)
            .send()
            .await;
        let resp = match upstream {
            Err(e) if file_limit_in(&e).is_some() => {
                let message =
                    shared.local_file_limit(Place::Upstream(label), &file_limit_detail(&e));
                refusal(tracker, 503, &message);
                return error(StatusCode::SERVICE_UNAVAILABLE, &message);
            }
            Err(e) => {
                let reason = describe_error(&e);
                let opened = lock(&line.health).on_hard_failure(Instant::now(), reason.clone());
                note_line_down(&shared, &generation, a.c, opened, &reason);
                if let Some(t) = &mut tracker {
                    t.attempt_failed(a.c, None, &reason, begun.elapsed());
                }
                continue;
            }
            Ok(resp) => resp,
        };
        if let Some(reason) = failover_status(resp.status(), resp.headers()) {
            let opened = lock(&line.health).on_soft_failure(Instant::now(), reason.clone());
            note_line_down(&shared, &generation, a.c, opened, &reason);
            if let Some(t) = &mut tracker {
                t.attempt_failed(a.c, Some(resp.status().as_u16()), &reason, begun.elapsed());
            }
            continue;
        }
        on_answer(
            &shared,
            &generation,
            a.c,
            pin,
            key.as_deref(),
            resp.status(),
            begun.elapsed(),
        );
        if let Some(t) = &mut tracker {
            t.answered(
                a.c,
                resp.status().as_u16(),
                resp.headers(),
                begun,
                Instant::now(),
            );
        }
        let mut resp = up_response(resp);
        if protocol != Protocol::Gemini {
            resp = match translate::body::response(resp, protocol, Protocol::Gemini).await {
                Ok(r) => r,
                Err(e) => {
                    refusal(tracker, 502, &e.to_string());
                    return translate::body::error(Protocol::Gemini, StatusCode::BAD_GATEWAY, &e);
                }
            };
        }
        let status = resp.status();
        let mut headers = response_headers(resp.headers(), false);
        set_route(&mut headers, &label);
        let sse = headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"));
        let body = match tracker {
            Some(t) => {
                let mode = if sse {
                    Mode::Events(Box::new(tap::Tap::new(t0)))
                } else {
                    Mode::json(status.as_u16())
                };
                Observed::new(
                    TapBody::new(resp.into_body(), mode, t),
                    shared,
                    generation,
                    a.c.member,
                    a.c.line,
                    active,
                )
                .holding(admission)
                .boxed_unsync()
            }
            None => Observed::new(
                resp.into_body(),
                shared,
                generation,
                a.c.member,
                a.c.line,
                active,
            )
            .holding(admission)
            .boxed_unsync(),
        };
        let mut response = Response::new(body);
        *response.status_mut() = status;
        *response.headers_mut() = headers;
        return response;
    }
    refusal(tracker, 502, "no Gemini route reachable");
    error(StatusCode::BAD_GATEWAY, "no Gemini route reachable")
}
