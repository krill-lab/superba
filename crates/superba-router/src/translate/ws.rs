//! Codex WebSocket ↔ HTTP bridge for members without a native Responses API.
//! One socket has a bounded local continuation; opaque upstream state is never
//! used to emulate remote compaction or a stored server-side conversation.

use super::{Invalid, ir::Protocol, routing};
use crate::forward::{empty, failover_status, set_route, up_response};
use crate::limits::{Admission, ConnSlot};
use crate::select::{Candidate, candidates};
use crate::state::{GaugeGuard, Generation, RespBody, Shared, lock};
use crate::stats::{
    body::{Mode, TapBody},
    hub::End,
    meta, strip,
    tap::{ResponseTap, SseTap},
};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http::{Response, StatusCode, request::Parts};
use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        handshake::derive_accept_key,
        protocol::{Role, WebSocketConfig},
    },
};

/// Upgrade locally, then translate each response.create through HTTP.
pub(crate) fn upgrade(
    shared: Arc<Shared>,
    generation: Arc<Generation>,
    parts: Parts,
    on_upgrade: hyper::upgrade::OnUpgrade,
    admission: Admission,
    preferred: Candidate,
) -> Response<RespBody> {
    let key = match parts
        .headers
        .get("sec-websocket-key")
        .and_then(|v| v.to_str().ok())
    {
        Some(k) => k,
        None => {
            return crate::forward::router_error(
                StatusCode::BAD_REQUEST,
                "invalid_websocket",
                "missing WebSocket key",
            );
        }
    };
    if parts
        .headers
        .get("sec-websocket-version")
        .is_none_or(|v| v != "13")
    {
        return crate::forward::router_error(
            StatusCode::BAD_REQUEST,
            "invalid_websocket",
            "WebSocket version 13 is required",
        );
    }
    let mut response = Response::new(empty());
    *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    response
        .headers_mut()
        .insert("connection", http::HeaderValue::from_static("Upgrade"));
    response
        .headers_mut()
        .insert("upgrade", http::HeaderValue::from_static("websocket"));
    let Ok(accept) = http::HeaderValue::from_str(&derive_accept_key(key.as_bytes())) else {
        return crate::forward::router_error(
            StatusCode::BAD_REQUEST,
            "invalid_websocket",
            "invalid WebSocket key",
        );
    };
    response
        .headers_mut()
        .insert("sec-websocket-accept", accept);
    set_route(
        response.headers_mut(),
        &generation.label(preferred.member, preferred.line),
    );
    let conn_slot = parts.extensions.get::<ConnSlot>().cloned();
    shared.totals.websockets.fetch_add(1, Ordering::Relaxed);
    tokio::spawn(async move {
        let _held = (admission, conn_slot);
        let _active = GaugeGuard::new(shared.clone(), |t| &t.active_websockets);
        let Ok(io) = on_upgrade.await else {
            return;
        };
        let config = WebSocketConfig::default()
            .max_message_size(Some(generation.config.tuning.max_body_bytes))
            .max_frame_size(Some(
                generation
                    .config
                    .tuning
                    .max_body_bytes
                    .min(crate::ws_relay::MAX_FRAME_BYTES),
            ));
        let mut socket =
            WebSocketStream::from_raw_socket(TokioIo::new(io), Role::Server, Some(config)).await;
        let mut history: Option<(String, Vec<Value>)> = None;
        let mut shutdown = shared.shutdown.subscribe();
        loop {
            let frame = tokio::select! {frame=socket.next()=>frame,_=shutdown.changed()=>break};
            match frame {
                Some(Ok(Message::Text(text))) => {
                    let request = serde_json::from_str::<Value>(&text)
                        .map_err(|_| Invalid::new("WebSocket request is not JSON"));
                    let result = match request {
                        Ok(request) => {
                            serve(
                                &shared,
                                &generation,
                                &parts,
                                preferred,
                                request,
                                &mut history,
                                &mut socket,
                            )
                            .await
                        }
                        Err(e) => Err(e),
                    };
                    if let Err(e) = result {
                        let event = json!({"type":"error","status":400,"error":{"type":"invalid_request_error","code":"protocol_conversion_error","message":e.to_string()}});
                        if socket
                            .send(Message::Text(event.to_string().into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
                Some(Ok(Message::Ping(data))) => {
                    if socket.send(Message::Pong(data)).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            }
        }
        let _ = socket.close(None).await;
    });
    response
}

async fn serve<S>(
    shared: &Arc<Shared>,
    generation: &Arc<Generation>,
    parts: &Parts,
    preferred: Candidate,
    mut value: Value,
    history: &mut Option<(String, Vec<Value>)>,
    socket: &mut WebSocketStream<S>,
) -> Result<(), Invalid>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    if value["type"] != "response.create" {
        return Err(Invalid::new(
            "only response.create is supported by the HTTP bridge",
        ));
    }
    if let Some(previous) = value["previous_response_id"].as_str() {
        let Some((id, old)) = history.as_ref().filter(|(id, _)| id == previous) else {
            return Err(Invalid::new(
                "previous response is not in this socket; resend the full input",
            ));
        };
        let _ = id;
        let mut input = old.clone();
        input.extend(value["input"].as_array().cloned().unwrap_or_default());
        value["input"] = input.into();
        value
            .as_object_mut()
            .map(|v| v.remove("previous_response_id"));
    }
    if let Some(o) = value.as_object_mut() {
        o.remove("type");
    }
    let bytes = Bytes::from(value.to_string());
    let bytes = strip::strip(&bytes)
        .map(|(b, _)| Bytes::from(b))
        .unwrap_or(bytes);
    value = serde_json::from_slice(&bytes).map_err(|_| Invalid::new("request is not JSON"))?;
    let compaction = crate::compaction::inspect(&bytes);
    if !compaction.is_empty() {
        let event = json!({"type":"error","status":400,"error":{"type":"invalid_request_error","code":crate::compaction::UNSUPPORTED_CODE,"message":"Remote compaction is unsupported by protocol conversion"}});
        return socket
            .send(Message::Text(event.to_string().into()))
            .await
            .map_err(|_| Invalid::new("WebSocket disconnected"));
    }
    if value["generate"] == false {
        let id = format!("resp_{}", uuid::Uuid::new_v4().simple());
        let response = json!({"id":id,"object":"response","model":value["model"],"status":"completed","output":[],"usage":{"input_tokens":0,"output_tokens":0}});
        for kind in ["response.created", "response.completed"] {
            socket
                .send(Message::Text(
                    json!({"type":kind,"response":response}).to_string().into(),
                ))
                .await
                .map_err(|_| Invalid::new("WebSocket disconnected"))?;
        }
        return Ok(());
    }
    if let Some(o) = value.as_object_mut() {
        o.remove("generate");
    }
    value["stream"] = true.into();
    let t0 = Instant::now();
    let mut facts = meta::peek(&parts.headers, &Bytes::from(value.to_string()));
    facts.transport = superba_core::RequestTransport::WebSocket;
    let mut tracker = generation.config.stats.is_some().then(|| {
        shared
            .stats
            .track(generation.clone(), facts, t0, crate::stats::hub::now_ms())
    });
    let _active = GaugeGuard::new(shared.clone(), |t| &t.active_requests);
    shared.totals.requests.fetch_add(1, Ordering::Relaxed);
    let pin = Some(crate::sticky::Pin {
        member: preferred.member,
        line: preferred.line,
    });
    let view = crate::state::View {
        generation,
        websocket_only: false,
        route: &crate::compaction::Route::Any,
    };
    let model = value["model"].as_str().unwrap_or_default();
    for c in candidates(&view, pin, Instant::now()) {
        let member = &generation.members[c.member];
        let line = &member.lines[c.line];
        let Some(protocol) = generation
            .config
            .upstream_protocol(&member.cfg.id, Protocol::Responses)
        else {
            continue;
        };
        let target = member
            .cfg
            .model_map
            .get(model)
            .map(String::as_str)
            .unwrap_or(model);
        let body = if protocol == Protocol::Responses {
            let mut v = value.clone();
            v["model"] = target.into();
            Bytes::from(v.to_string())
        } else {
            super::body::request(Protocol::Responses, protocol, &value, target)?
        };
        let Some(base) = routing::base(&generation.config, c.member, c.line, protocol) else {
            continue;
        };
        let begun = Instant::now();
        if let Some(t) = &mut tracker {
            t.trying(c);
            t.set_upstream_model(Some(target.into()));
            t.note(format!(
                "WebSocket via HTTP; protocol conversion: responses -> {protocol:?}"
            ));
        }
        let response = generation
            .http
            .post(routing::url(&base, protocol, target, true))
            .headers(routing::headers(
                &parts.headers,
                generation,
                c.member,
                protocol,
            ))
            .body(routing::wire_body(protocol, body))
            .send()
            .await;
        let resp = match response {
            Ok(r) => r,
            Err(e) => {
                let reason = crate::forward::describe_error(&e);
                let opened = lock(&line.health).on_hard_failure(Instant::now(), reason.clone());
                crate::proxy::note_line_down(shared, generation, c, opened, &reason);
                if let Some(t) = &mut tracker {
                    t.attempt_failed(c, None, &reason, begun.elapsed());
                }
                continue;
            }
        };
        if let Some(reason) = failover_status(resp.status(), resp.headers()) {
            let opened = lock(&line.health).on_soft_failure(Instant::now(), reason.clone());
            crate::proxy::note_line_down(shared, generation, c, opened, &reason);
            if let Some(t) = &mut tracker {
                t.attempt_failed(c, Some(resp.status().as_u16()), &reason, begun.elapsed());
            }
            continue;
        }
        let status = resp.status();
        if let Some(t) = &mut tracker {
            t.answered(c, status.as_u16(), resp.headers(), begun, Instant::now());
        }
        crate::proxy::on_answer(shared, generation, c, None, None, status, begun.elapsed());
        let mut resp = up_response(resp);
        if protocol != Protocol::Responses {
            resp = super::body::response(resp, protocol, Protocol::Responses).await?;
        }
        if !status.is_success() {
            let bytes = http_body_util::Limited::new(resp.into_body(), 1024 * 1024)
                .collect()
                .await
                .map_err(|_| Invalid::new("upstream error could not be read"))?
                .to_bytes();
            let v: Value = serde_json::from_slice(&bytes).unwrap_or_default();
            socket
                .send(Message::Text(
                    json!({"type":"error","status":status.as_u16(),"error":v["error"]})
                        .to_string()
                        .into(),
                ))
                .await
                .map_err(|_| Invalid::new("WebSocket disconnected"))?;
            if let Some(t) = tracker {
                t.finish(End {
                    status: Some(superba_core::RequestStatus::Http {
                        status: status.as_u16(),
                        code: None,
                    }),
                    ..End::default()
                });
            }
            return Ok(());
        }
        let mut body = if let Some(t) = tracker {
            let line = t.line_setup();
            TapBody::new(
                resp.into_body(),
                Mode::Sse(Box::new(SseTap::new(ResponseTap::new(t0, line)))),
                t,
            )
            .boxed_unsync()
        } else {
            resp.into_body().boxed_unsync()
        };
        let mut parser = super::stream::Sse::default();
        let mut last: Option<Value> = None;
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|_| Invalid::new("upstream stream broke"))?;
            if let Ok(bytes) = frame.into_data() {
                for data in parser.push(&bytes)? {
                    if data == b"[DONE]" {
                        continue;
                    }
                    let v: Value = serde_json::from_slice(&data)
                        .map_err(|_| Invalid::new("invalid Responses event"))?;
                    if v["type"] == "response.completed" {
                        last = Some(v["response"].clone());
                    }
                    socket
                        .send(Message::Text(
                            String::from_utf8(data)
                                .map_err(|_| Invalid::new("invalid event UTF-8"))?
                                .into(),
                        ))
                        .await
                        .map_err(|_| Invalid::new("WebSocket disconnected"))?;
                }
            }
        }
        if let Some(response) = last {
            let mut input = value["input"].as_array().cloned().unwrap_or_default();
            input.extend(response["output"].as_array().cloned().unwrap_or_default());
            if serde_json::to_vec(&input)
                .map(|b| b.len())
                .unwrap_or(usize::MAX)
                <= generation.config.tuning.max_body_bytes
            {
                *history = Some((response["id"].as_str().unwrap_or_default().into(), input));
            } else {
                *history = None;
            }
        }
        return Ok(());
    }
    if let Some(t) = tracker {
        t.finish(End {
            status: Some(superba_core::RequestStatus::Broken),
            error: Some("no converted WebSocket route reachable".into()),
            ..End::default()
        });
    }
    Err(Invalid::new("no converted WebSocket route reachable"))
}
