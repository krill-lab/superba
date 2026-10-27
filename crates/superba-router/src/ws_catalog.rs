//! Codex WebSocket sessions under the merged catalog ([`crate::merged_catalog`]).
//!
//! The model is in each `response.create` frame, not in the handshake, so the
//! router answers Codex's handshake itself and dials the member when the first
//! frame (usually the `generate: false` prewarm) arrives. The frame's model
//! chooses the targets exactly as an HTTP body's does (namespace pins, aliases,
//! the upstream name, then the member's own model names); their lines are
//! dialed in the HTTP path's order, kept to the lines that serve WebSocket, and
//! fail over like [`crate::ws`]. The relay then checks every later frame:
//!
//! - a model the socket's member serves goes to it, its name rewritten;
//! - a model only another member serves pins the conversation there and closes
//!   the socket with 1012: Codex sends the turn again on a new socket, whose
//!   first frame dials that member;
//! - a model WebSocket cannot carry (its preferred member is reached through
//!   the standard endpoint or a protocol conversion, serves no WebSocket, or
//!   its lines refused it) or a frame with a router compaction capsule closes
//!   the socket with 1012 too, and the session's next handshake is answered
//!   with 426 (`model_routing_http`): Codex uses HTTP Responses from then on.
//!
//! A model the catalog refuses (unknown namespace, an unverified model) is
//! answered with an `error` event; nothing is sent to any member.

use std::collections::HashMap;
use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http::request::Parts;
use http::{Request, Response, StatusCode};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::protocol::frame::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Message, Utf8Bytes};
use tracing::Instrument;

use crate::compaction::Route;
use crate::limits::{Admission, ConnSlot, LOCAL_BUSY_CODE};
use crate::merged_catalog::Selection;
use crate::select::{Candidate, candidates};
use crate::state::{GaugeGuard, Generation, RespBody, Shared, View, lock};
use crate::stats::hub::{End, now_ms};
use crate::stats::meta;
use crate::stats::sse::sniff_type;
use crate::status::RouteEventKind;
use crate::sticky::{Pin, session_key};
use crate::translate::ir::Protocol;
use crate::ws::{Dialed, accept, answered_status, dial, refuse_as, tunnel};
use crate::ws_relay::{Client, RelayCtx, downstream, linger, refused};

/// Error code of the 426 that sends a session to HTTP Responses.
pub(crate) const HTTP_CODE: &str = "model_routing_http";

/// How long a session's 426 waits for Codex's reconnect.
const HTTP_ONLY_TTL: Duration = Duration::from_secs(60);

/// Most sessions waiting for their 426 at once.
const HTTP_ONLY_CAPACITY: usize = 1024;

/// Largest member answer to a handshake read for its error.
const ANSWER_LIMIT: usize = 64 * 1024;

/// How long such an answer may take.
const ANSWER_READ: Duration = Duration::from_secs(5);

/// Sessions whose next WebSocket handshake is answered with 426: a frame of
/// theirs needed HTTP Responses and the socket was closed for Codex to
/// reconnect (kept across reloads).
#[derive(Debug, Default)]
pub(crate) struct HttpSessions {
    inner: Mutex<HashMap<String, Instant>>,
}

impl HttpSessions {
    /// Answer `key`'s next handshake with 426.
    pub fn mark(&self, key: &str, now: Instant) {
        let mut map = lock(&self.inner);
        map.retain(|_, at| now.saturating_duration_since(*at) < HTTP_ONLY_TTL);
        if map.len() < HTTP_ONLY_CAPACITY || map.contains_key(key) {
            map.insert(key.to_owned(), now);
        }
    }

    /// `key`'s handshake is to be answered with 426 (once).
    pub fn take(&self, key: &str, now: Instant) -> bool {
        lock(&self.inner)
            .remove(key)
            .is_some_and(|at| now.saturating_duration_since(at) < HTTP_ONLY_TTL)
    }
}

/// What a `response.create` frame needs.
enum Decision {
    /// These lines (all serving WebSocket), the preferred member's first.
    Serve(Vec<Candidate>),
    /// Only HTTP Responses can carry it (why).
    Http(String),
    /// No member may get it: answer with `(status, code, message)`.
    Refuse(u16, &'static str, String),
}

/// Accept Codex's handshake; the first frame chooses the member.
pub(crate) fn upgrade(
    shared: Arc<Shared>,
    generation: Arc<Generation>,
    mut req: Request<Incoming>,
    rest: String,
    admission: Admission,
) -> Response<RespBody> {
    let key = session_key(req.headers());
    if key
        .as_deref()
        .is_some_and(|k| shared.ws_http.take(k, Instant::now()))
    {
        return refuse_as(
            &shared,
            HTTP_CODE,
            "this conversation's model requires HTTP Responses",
        );
    }
    if !generation.members.iter().any(|m| m.cfg.websocket) {
        return refuse_as(
            &shared,
            "websocket_unavailable",
            "no member serves WebSocket",
        );
    }
    let response = match accept(req.headers()) {
        Ok(response) => response,
        Err(refusal) => return *refusal,
    };
    let on_upgrade = hyper::upgrade::on(&mut req);
    let (parts, _body) = req.into_parts();
    // The inbound connection's place: an upgraded socket keeps it.
    let conn_slot = parts.extensions.get::<ConnSlot>().cloned();
    tokio::spawn(async move {
        // The socket keeps its admission and the connection's place to the end.
        let _held = (admission, conn_slot);
        let _gauge = GaugeGuard::new(shared.clone(), |t| &t.active_websockets);
        let io = match on_upgrade.await {
            Ok(io) => io,
            Err(e) => {
                tracing::debug!(error = %e, "downstream upgrade failed");
                return;
            }
        };
        let limit = generation.config.tuning.max_body_bytes;
        let down = downstream(TokioIo::new(io), limit).await;
        session(shared, generation, parts, rest, key, down).await;
    });
    response
}

/// Wait for the frame that chooses the member, dial it, then relay. Frames the
/// router answers itself leave the socket waiting for the next one.
async fn session<D>(
    shared: Arc<Shared>,
    generation: Arc<Generation>,
    parts: Parts,
    rest: String,
    key: Option<String>,
    mut down: WebSocketStream<D>,
) where
    D: AsyncRead + AsyncWrite + Unpin + Send,
{
    let mut shutdown = shared.shutdown.subscribe();
    loop {
        let msg = tokio::select! {
            msg = down.next() => msg,
            _ = shutdown.changed() => break,
        };
        let t = match msg {
            Some(Ok(Message::Text(t))) => t,
            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
            // Pings are answered by the socket itself.
            Some(Ok(_)) => continue,
        };
        let t0 = Instant::now();
        let bytes: Bytes = t.clone().into();
        let value = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
        let now = Instant::now();
        let pin = key.as_deref().and_then(|k| generation.pin(k, now));
        let decision = match lookup(&generation, &value).await {
            Ok(selection) => decide(&shared, &generation, selection.as_ref(), &bytes, pin, now),
            Err(d) => d,
        };
        let reply = match decision {
            Decision::Serve(order) => {
                match dial(
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
                        io, c, failovers, ..
                    } => {
                        let ctx = RelayCtx {
                            shared,
                            generation,
                            handshake: parts.headers.clone(),
                            answered: c,
                            failovers: AtomicU32::new(failovers),
                        };
                        let span = tracing::info_span!(
                            "ws",
                            route = %ctx.generation.label(c.member, c.line)
                        );
                        tunnel(down, io, ctx, Some(t)).instrument(span).await;
                        return;
                    }
                    Dialed::Answered { resp, c }
                        if resp.status() == StatusCode::UPGRADE_REQUIRED =>
                    {
                        let label = generation.label(c.member, c.line);
                        http_only(
                            &shared,
                            key.as_deref(),
                            &format!("{label} refused WebSocket (HTTP 426)"),
                        )
                    }
                    Dialed::Answered { resp, c } => {
                        let status = answered_status(&generation, &parts, resp.status(), c);
                        let m = meta::peek_frame(&parts.headers, &bytes);
                        answer_event(&shared, &generation, m, t0, c, status, resp).await
                    }
                    Dialed::FileLimit(message) => {
                        let m = meta::peek_frame(&parts.headers, &bytes);
                        refused(
                            &shared,
                            &generation,
                            m,
                            t0,
                            (503, LOCAL_BUSY_CODE, &message),
                        )
                    }
                    // As without the catalog: an unreachable WebSocket line
                    // leaves the session to HTTP.
                    Dialed::Unreachable(failures) => http_only(
                        &shared,
                        key.as_deref(),
                        &format!("no WebSocket line reachable ({})", failures.join("; ")),
                    ),
                }
            }
            Decision::Http(why) => http_only(&shared, key.as_deref(), &why),
            Decision::Refuse(status, code, message) => {
                let m = meta::peek_frame(&parts.headers, &bytes);
                refused(&shared, &generation, m, t0, (status, code, &message))
            }
        };
        let closing = matches!(reply, Message::Close(_));
        if down.send(reply).await.is_err() || closing {
            break;
        }
    }
    let _ = down.close(None).await;
    linger(&mut down).await;
}

/// A frame of a catalog-routed socket, before [`crate::ws_relay`]'s own
/// handling: the frame with the model name the socket's member gets, and the
/// frame's targets; or what to do instead.
pub(crate) async fn frame(
    ctx: &RelayCtx,
    t: Utf8Bytes,
) -> Result<(Utf8Bytes, Option<Selection>), Client> {
    let bytes: Bytes = t.clone().into();
    if sniff_type(&bytes) != Some("response.create") {
        return Ok((t, None));
    }
    let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok((t, None));
    };
    let g = &ctx.generation;
    let here = ctx.answered.member;
    let t0 = Instant::now();
    let decision = match lookup(g, &value).await {
        Ok(None) => return Ok((t, None)),
        Ok(Some(selection)) => {
            // The socket's member serves it natively: it stays here.
            if let Some(upstream) = selection.upstream(here)
                && !selection.standard(here)
            {
                if value["model"].as_str() == Some(upstream) {
                    return Ok((t, Some(selection)));
                }
                value["model"] = Value::String(upstream.to_owned());
                return Ok((Utf8Bytes::from(value.to_string()), Some(selection)));
            }
            let pin = Pin {
                member: here,
                line: ctx.answered.line,
            };
            decide(
                &ctx.shared,
                g,
                Some(&selection),
                &bytes,
                Some(pin),
                Instant::now(),
            )
        }
        Err(d) => d,
    };
    let key = session_key(&ctx.handshake);
    Err(match decision {
        Decision::Serve(order) => {
            // The member that serves this model gets the conversation; the
            // new socket's first frame dials it.
            let target = order[0];
            let now = Instant::now();
            if let Some(key) = &key {
                lock(&g.sticky).set(
                    key,
                    Pin {
                        member: target.member,
                        line: target.line,
                    },
                    now,
                );
            }
            let (from, to) = (&g.members[here].cfg, &g.members[target.member].cfg);
            let model = value["model"].as_str().unwrap_or_default();
            ctx.shared.event(
                RouteEventKind::MemberChanged,
                Some(&to.id),
                None,
                Some(format!(
                    "model “{model}” moves the conversation from “{}” to “{}”; the WebSocket reconnects",
                    from.name, to.name
                )),
            );
            Client::Move(Message::Close(Some(CloseFrame {
                code: CloseCode::Restart,
                reason: "this model is served by another provider".into(),
            })))
        }
        Decision::Http(why) => Client::Move(http_only(&ctx.shared, key.as_deref(), &why)),
        Decision::Refuse(status, code, message) => {
            let m = meta::peek_frame(&ctx.handshake, &bytes);
            Client::Reply(refused(&ctx.shared, g, m, t0, (status, code, &message)))
        }
    })
}

/// The frame's targets in the catalog (`None`: it does not route the model),
/// or why the frame is not served over WebSocket.
async fn lookup(g: &Arc<Generation>, value: &Value) -> Result<Option<Selection>, Decision> {
    // Capsules are verified and expanded against complete history, which
    // only an HTTP request carries ([`crate::compaction_proxy`]).
    if carries_capsule(value) {
        return Err(Decision::Http(
            "router compaction requires complete history over HTTP".into(),
        ));
    }
    let Some(model) = value["model"].as_str() else {
        return Ok(None);
    };
    crate::merged_catalog::targets(g, model)
        .await
        .map_err(|r| Decision::Refuse(r.status.as_u16(), r.code, r.message.into()))
}

/// The HTTP path's order for a frame, kept to the lines a WebSocket reaches.
/// As without the catalog, the preferred member decides whether the frame
/// uses WebSocket at all: a session never moves for the transport.
fn decide(
    shared: &Shared,
    g: &Generation,
    selection: Option<&Selection>,
    frame: &[u8],
    pin: Option<Pin>,
    now: Instant,
) -> Decision {
    let route = crate::compaction::route(
        &g.member_facts(),
        &crate::compaction::inspect(frame),
        &shared.stats.compactions,
    );
    if let Route::Refuse {
        status,
        code,
        message,
    } = route
    {
        shared.event(
            RouteEventKind::Compaction,
            None,
            None,
            Some(message.clone()),
        );
        return Decision::Refuse(status, code, message);
    }
    let order = match selection {
        Some(s) => s.candidates(g, &route, pin, now),
        None => candidates(
            &View {
                generation: g,
                websocket_only: false,
                route: &route,
            },
            pin,
            now,
        ),
    };
    let Some(preferred) = order.first().map(|c| c.member) else {
        return Decision::Refuse(502, "router_unavailable", "no line reachable".into());
    };
    // Why a member cannot take the frame over WebSocket.
    let unfit = |member: usize| {
        let m = &g.members[member].cfg;
        if selection.is_some_and(|s| s.standard(member)) {
            Some("serves this model through its standard endpoint")
        } else if g.config.upstream_protocol(&m.id, Protocol::Responses)
            != Some(Protocol::Responses)
        {
            Some("needs a protocol conversion")
        } else if !m.websocket {
            Some("does not serve WebSocket")
        } else {
            None
        }
    };
    let name = &g.members[preferred].cfg.name;
    if let Some(why) = unfit(preferred) {
        return Decision::Http(format!("“{name}” {why}"));
    }
    let order: Vec<Candidate> = order
        .into_iter()
        .filter(|c| {
            let m = &g.members[c.member];
            unfit(c.member).is_none()
                && crate::transport::offers_websocket(
                    &shared.transports,
                    &m.cfg,
                    &m.lines[c.line].cfg,
                    now,
                )
        })
        .collect();
    if order.first().map(|c| c.member) != Some(preferred) {
        return Decision::Http(format!("“{name}”'s lines refused WebSocket (HTTP 426)"));
    }
    Decision::Serve(order)
}

/// A router compaction capsule in the frame's input.
fn carries_capsule(value: &Value) -> bool {
    value["input"].as_array().is_some_and(|items| {
        items.iter().any(|i| {
            i["encrypted_content"]
                .as_str()
                .is_some_and(|s| s.starts_with(crate::compaction_proxy::PREFIX))
        })
    })
}

/// Close the socket for Codex to reconnect; the session's handshake then gets
/// 426 and Codex uses HTTP Responses. Without a session key the reconnect is
/// accepted again, and Codex falls back once its retries run out.
fn http_only(shared: &Shared, key: Option<&str>, why: &str) -> Message {
    if let Some(key) = key {
        shared.ws_http.mark(key, Instant::now());
    }
    tracing::debug!(reason = why, "websocket session moves to HTTP");
    Message::Close(Some(CloseFrame {
        code: CloseCode::Restart,
        reason: "this model requires HTTP Responses".into(),
    }))
}

/// A member's answer to the handshake that did not upgrade (401/403, ...) as
/// the frame's `error` event, with the member's own error object when it sent
/// one; recorded on the member's line.
async fn answer_event(
    shared: &Arc<Shared>,
    generation: &Arc<Generation>,
    m: meta::RequestMeta,
    t0: Instant,
    c: Candidate,
    status: StatusCode,
    mut resp: reqwest::Response,
) -> Message {
    let mut body = Vec::new();
    let _ = tokio::time::timeout(ANSWER_READ, async {
        while let Ok(Some(chunk)) = resp.chunk().await {
            body.extend_from_slice(&chunk);
            if body.len() > ANSWER_LIMIT {
                break;
            }
        }
    })
    .await;
    let error = serde_json::from_slice::<Value>(&body)
        .ok()
        .map(|v| v["error"].clone())
        .filter(Value::is_object)
        .unwrap_or_else(|| {
            json!({
                "type": "upstream_error",
                "message": format!(
                    "{} answered the WebSocket handshake with HTTP {}",
                    generation.label(c.member, c.line),
                    status.as_u16()
                ),
            })
        });
    if generation.config.stats.is_some() {
        let mut tracker = shared.stats.track(generation.clone(), m, t0, now_ms());
        tracker.ws_pinned(c, 0);
        tracker.finish(End {
            status: Some(superba_core::RequestStatus::Http {
                status: status.as_u16(),
                code: error["code"].as_str().map(str::to_owned),
            }),
            error: error["message"]
                .as_str()
                .map(|s| s.chars().take(300).collect()),
            ..End::default()
        });
    }
    let event = json!({"type": "error", "status": status.as_u16(), "error": error});
    Message::Text(event.to_string().into())
}
