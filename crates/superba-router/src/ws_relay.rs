//! Frame-terminating WebSocket relay, used while the configuration has a
//! `stats` section (spike RS 4.6).
//!
//! Both legs are terminated with tokio-tungstenite (no per-message deflate: the
//! extension header is dropped both ways, verified to be accepted by Codex).
//!
//! - client → member: each `response.create` text frame is peeked (originator
//!   from the handshake, turn metadata from `client_metadata`), stripped of
//!   earlier stats lines, and queued as a tracked request; every other frame
//!   passes unchanged.
//! - member → client: each text frame is one Responses event fed to the tap of
//!   the oldest open request (Codex sends the next `response.create` only after
//!   the previous response ended); the tap may hold the end of the final answer
//!   and inject the stats line. A terminal event (`response.completed`,
//!   `failed`, `incomplete`, `error`) finishes that request's row.
//!
//! The socket stays pinned to its line; closing either side closes the other.
//!
//! Both legs are bounded like the HTTP path: a message may be as large as
//! `tuning.max_body_bytes`, one frame at most [`MAX_FRAME_BYTES`] (tungstenite
//! reserves a frame's announced length before its payload arrives). A bigger
//! message or frame from either side closes both legs with 1009 (message too
//! big) and records the open requests as broken.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http::HeaderMap;
use superba_core::{InflightPhase, RequestStatus};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Notify;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::Utf8Bytes;
use tokio_tungstenite::tungstenite::error::{CapacityError, Error as WsError};
use tokio_tungstenite::tungstenite::protocol::frame::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};

use crate::select::Candidate;
use crate::state::{Generation, Shared, lock};
use crate::stats::hub::{End, Tracker, now_ms};
use crate::stats::sse::{EventKind, sniff_type};
use crate::stats::tap::{Decision, ResponseTap};
use crate::stats::{inject, meta, strip};
use crate::status::RouteEventKind;

/// How long the other direction may still run after one side closed.
const CLOSE_GRACE: Duration = Duration::from_secs(3);

/// Largest single frame either leg accepts (capped by the message limit).
pub(crate) const MAX_FRAME_BYTES: usize = 64 << 20;

/// What a relay needs to know about its socket.
pub(crate) struct RelayCtx {
    pub shared: Arc<Shared>,
    pub generation: Arc<Generation>,
    /// Headers of Codex's handshake (originator, session ids).
    pub handshake: HeaderMap,
    /// The line the socket is pinned to.
    pub answered: Candidate,
    /// Handshake attempts that failed over before this line (counted on the
    /// socket's first request).
    pub failovers: AtomicU32,
}

/// One `response.create` waiting for its response's end.
struct Open {
    tracker: Tracker,
    tap: ResponseTap,
    held: Vec<(EventKind, Bytes)>,
    first_frame: bool,
}

type Queue = Arc<Mutex<VecDeque<Open>>>;

/// Both legs: messages up to `max_message` bytes (the HTTP body limit),
/// frames up to [`MAX_FRAME_BYTES`].
fn config(max_message: usize) -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(max_message))
        .max_frame_size(Some(max_message.min(MAX_FRAME_BYTES)))
}

/// A message or frame over the limits of [`config`].
fn too_big(e: &WsError) -> bool {
    matches!(e, WsError::Capacity(CapacityError::MessageTooLong { .. }))
}

/// The close frame both legs get when one side sent too much.
fn close_too_big() -> Message {
    Message::Close(Some(CloseFrame {
        code: CloseCode::Size,
        reason: "message too big".into(),
    }))
}

/// Tripped by the direction that read an over-limit message; the other
/// direction then closes its leg with 1009 too.
#[derive(Default)]
struct Oversize {
    seen: AtomicBool,
    notify: Notify,
}

impl Oversize {
    fn trip(&self) {
        self.seen.store(true, Ordering::Relaxed);
        self.notify.notify_one();
    }
}

fn text(bytes: Bytes) -> Option<Message> {
    Utf8Bytes::try_from(bytes).ok().map(Message::Text)
}

/// Relay frames between Codex (`down`) and the member (`up`) until either side
/// closes.
pub(crate) async fn relay<D, U>(down: D, up: U, ctx: RelayCtx)
where
    D: AsyncRead + AsyncWrite + Unpin + Send,
    U: AsyncRead + AsyncWrite + Unpin + Send,
{
    let limit = ctx.generation.config.tuning.max_body_bytes;
    let down = WebSocketStream::from_raw_socket(down, Role::Server, Some(config(limit))).await;
    let up = WebSocketStream::from_raw_socket(up, Role::Client, Some(config(limit))).await;
    let (mut down_tx, mut down_rx) = down.split();
    let (mut up_tx, mut up_rx) = up.split();
    let queue: Queue = Arc::new(Mutex::new(VecDeque::new()));
    let ctx = Arc::new(ctx);
    let oversize = Arc::new(Oversize::default());
    // Frames the router itself sends to Codex (compaction refusals, the close
    // that moves a compacted conversation to its member).
    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel::<Message>();

    let c2s = {
        let queue = queue.clone();
        let ctx = ctx.clone();
        let oversize = oversize.clone();
        async move {
            loop {
                let msg = tokio::select! {
                    msg = down_rx.next() => msg,
                    () = oversize.notify.notified() => {
                        let _ = up_tx.send(close_too_big()).await;
                        break;
                    }
                };
                let Some(msg) = msg else { break };
                match msg {
                    Ok(Message::Text(t)) => match client_text(&ctx, &queue, t) {
                        Client::Forward(out) => {
                            if up_tx.send(out).await.is_err() {
                                break;
                            }
                        }
                        Client::Reply(reply) => {
                            if reply_tx.send(reply).is_err() {
                                break;
                            }
                        }
                        Client::Move(close) => {
                            let _ = reply_tx.send(close);
                            let _ = up_tx.send(Message::Close(None)).await;
                            break;
                        }
                    },
                    Ok(Message::Close(frame)) => {
                        let _ = up_tx.send(Message::Close(frame)).await;
                        break;
                    }
                    Ok(other) => {
                        if up_tx.send(other).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "websocket client side ended");
                        if too_big(&e) {
                            let _ = up_tx.send(close_too_big()).await;
                            oversize.trip();
                        }
                        break;
                    }
                }
            }
            let _ = up_tx.close().await;
        }
    };
    let s2c = {
        let queue = queue.clone();
        let oversize = oversize.clone();
        async move {
            loop {
                let msg = tokio::select! {
                    msg = up_rx.next() => msg,
                    reply = reply_rx.recv() => {
                        let Some(reply) = reply else { continue };
                        let closing = matches!(reply, Message::Close(_));
                        if down_tx.send(reply).await.is_err() || closing {
                            break;
                        }
                        continue;
                    }
                    () = oversize.notify.notified() => {
                        let _ = down_tx.send(close_too_big()).await;
                        break;
                    }
                };
                let Some(msg) = msg else { break };
                match msg {
                    Ok(Message::Text(t)) => {
                        let out = server_text(&queue, t);
                        let mut failed = false;
                        for m in out {
                            if down_tx.send(m).await.is_err() {
                                failed = true;
                                break;
                            }
                        }
                        if failed {
                            break;
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        let _ = down_tx.send(Message::Close(frame)).await;
                        break;
                    }
                    Ok(other) => {
                        if down_tx.send(other).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "websocket member side ended");
                        if too_big(&e) {
                            let _ = down_tx.send(close_too_big()).await;
                            oversize.trip();
                        }
                        break;
                    }
                }
            }
            let _ = down_tx.close().await;
        }
    };
    tokio::pin!(c2s);
    tokio::pin!(s2c);
    // Who ended first decides how open requests are recorded: the client
    // leaving cancels them, anything else (the member leaving, a message over
    // the limits from either side) breaks them.
    let client_left = tokio::select! {
        () = &mut c2s => {
            let _ = tokio::time::timeout(CLOSE_GRACE, &mut s2c).await;
            true
        }
        () = &mut s2c => {
            let _ = tokio::time::timeout(CLOSE_GRACE, &mut c2s).await;
            false
        }
    };
    let cancelled = client_left && !oversize.seen.load(Ordering::Relaxed);
    let open: Vec<Open> = lock(&queue).drain(..).collect();
    for o in open {
        let summary = o.tap.summary();
        o.tracker.finish(End {
            summary: Some(summary),
            status: Some(if cancelled {
                RequestStatus::Cancelled
            } else {
                RequestStatus::Broken
            }),
            ..End::default()
        });
    }
}

/// What to do with a client frame.
enum Client {
    /// Send this to the member.
    Forward(Message),
    /// Do not forward; answer Codex with this frame.
    Reply(Message),
    /// Do not forward; close the socket with this frame (Codex reconnects and
    /// its handshake follows the conversation's new pin).
    Move(Message),
}

/// A `response.create` frame the socket's member must not get
/// ([`crate::compaction`]): `None` when it may.
fn compaction_detour(ctx: &RelayCtx, m: &meta::RequestMeta, t0: Instant) -> Option<Client> {
    let generation = &ctx.generation;
    let route = crate::compaction::route(
        &generation.member_facts(),
        &m.compaction,
        &ctx.shared.stats.compactions,
    );
    if route.allows(ctx.answered.member) {
        return None;
    }
    let here = &generation.members[ctx.answered.member].cfg;
    if let crate::compaction::Route::Refuse {
        status,
        code,
        message,
    } = &route
    {
        ctx.shared.event(
            RouteEventKind::Compaction,
            Some(&here.id),
            None,
            Some(message.clone()),
        );
        let tracker = ctx
            .shared
            .stats
            .track(ctx.generation.clone(), m.clone(), t0, now_ms());
        tracker.finish(End {
            status: Some(RequestStatus::Http {
                status: *status,
                code: Some((*code).into()),
            }),
            error: Some(message.chars().take(300).collect()),
            ..End::default()
        });
        let event = serde_json::json!({
            "type": "error",
            "status": status,
            "error": {
                "type": "invalid_request_error",
                "code": code,
                "message": format!("Superba local router: {message}"),
            }
        });
        return Some(Client::Reply(Message::Text(event.to_string().into())));
    }
    // Pin the conversation to a member that may serve it, then make Codex
    // reconnect: the new handshake goes there.
    let now = Instant::now();
    let target = crate::select::candidates(
        &crate::state::View {
            generation,
            websocket_only: false,
            route: &route,
        },
        None,
        now,
    )
    .into_iter()
    .next()?;
    if let Some(key) = crate::sticky::session_key(&ctx.handshake) {
        lock(&generation.sticky).set(
            &key,
            crate::sticky::Pin {
                member: target.member,
                line: target.line,
            },
            now,
        );
    }
    let to = &generation.members[target.member].cfg;
    let detail = match &route {
        crate::compaction::Route::Members {
            kept_on: Some(_), ..
        } => format!(
            "compacted conversation moves from “{}” to “{}” (made its compaction item); the WebSocket reconnects",
            here.name, to.name
        ),
        _ => format!(
            "compaction moves from “{}” (no remote compaction) to “{}”; the WebSocket reconnects",
            here.name, to.name
        ),
    };
    ctx.shared
        .event(RouteEventKind::Compaction, Some(&to.id), None, Some(detail));
    Some(Client::Move(Message::Close(Some(CloseFrame {
        code: CloseCode::Restart,
        reason: "compacted conversation moves to its provider".into(),
    }))))
}

/// A client frame: track and strip `response.create`, pass everything else.
fn client_text(ctx: &RelayCtx, queue: &Queue, t: Utf8Bytes) -> Client {
    let bytes: Bytes = t.clone().into();
    if sniff_type(&bytes) != Some("response.create") {
        return Client::Forward(Message::Text(t));
    }
    let t0 = Instant::now();
    let mut m = meta::peek_frame(&ctx.handshake, &bytes);
    if let Some(detour) = compaction_detour(ctx, &m, t0) {
        return detour;
    }
    let send = match strip::strip(&bytes) {
        Some((new, n)) => {
            m.stripped = n;
            text(Bytes::from(new)).unwrap_or(Message::Text(t))
        }
        None => Message::Text(t),
    };
    let mut tracker = ctx
        .shared
        .stats
        .track(ctx.generation.clone(), m, t0, now_ms());
    tracker.ws_pinned(ctx.answered, ctx.failovers.swap(0, Ordering::Relaxed));
    let tap = ResponseTap::new(t0, tracker.line_setup());
    // The frame goes to the member right after this returns.
    tracker.set_sent(Instant::now());
    lock(queue).push_back(Open {
        tracker,
        tap,
        held: Vec::new(),
        first_frame: true,
    });
    Client::Forward(send)
}

/// A member frame: feed the oldest open request's tap; returns the frames to
/// send to the client now.
fn server_text(queue: &Queue, t: Utf8Bytes) -> Vec<Message> {
    let bytes: Bytes = t.clone().into();
    let now = Instant::now();
    let kind = sniff_type(&bytes).map_or(EventKind::Other, EventKind::of);
    let mut q = lock(queue);
    let Some(head) = q.front_mut() else {
        return vec![Message::Text(t)];
    };
    if head.first_frame {
        head.first_frame = false;
        head.tracker.set_ttfb(now);
    }
    let decision = head.tap.on_event(kind, &bytes, now);
    let out: Vec<Message> = match decision {
        Decision::Pass => vec![Message::Text(t)],
        Decision::Hold => {
            let len = bytes.len();
            head.held.push((kind, bytes));
            if head.tap.note_held(len) {
                Vec::new()
            } else {
                head.held.drain(..).filter_map(|(_, b)| text(b)).collect()
            }
        }
        Decision::Release => {
            let mut v: Vec<Message> = head.held.drain(..).filter_map(|(_, b)| text(b)).collect();
            v.push(Message::Text(t));
            v
        }
        Decision::Inject(suffix) => {
            let held = std::mem::take(&mut head.held);
            let rewritten = head
                .tap
                .candidate()
                .cloned()
                .and_then(|c| inject::rewrite_frames(&held, &bytes, &c, &suffix));
            match rewritten {
                Some(frames) => frames.into_iter().filter_map(text).collect(),
                None => {
                    head.tap.injection_failed();
                    let mut v: Vec<Message> =
                        held.into_iter().filter_map(|(_, b)| text(b)).collect();
                    v.push(Message::Text(t));
                    v
                }
            }
        }
    };
    let phase = if head.tap.holding() {
        InflightPhase::Holding
    } else {
        InflightPhase::Streaming
    };
    head.tracker
        .inflight()
        .update(phase, head.tap.deltas(), head.tap.first_token());
    if kind.is_terminal()
        && let Some(done) = q.pop_front()
    {
        drop(q);
        let summary = done.tap.summary();
        done.tracker.finish(End {
            summary: Some(summary),
            ..End::default()
        });
    }
    out
}
