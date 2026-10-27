//! Frame-terminating WebSocket relay. Private fields are always removed;
//! request tracking and stats-line shaping require a `stats` section.
//!
//! Both legs are terminated with tokio-tungstenite (no per-message deflate: the
//! extension header is dropped both ways, verified to be accepted by Codex).
//!
//! - client → member: private top-level fields are removed first. With stats,
//!   each `response.create` text frame is peeked (originator
//!   from the handshake, turn metadata from `client_metadata`), stripped of
//!   earlier stats lines, and queued as a tracked request; every other frame
//!   passes unchanged.
//! - member → client: each text frame is one Responses event fed to the tap of
//!   the oldest open request (Codex sends the next `response.create` only after
//!   the previous response ended); the tap may hold the end of the final answer
//!   and inject the stats line. A terminal event (`response.completed`,
//!   `failed`, `incomplete`, `error`) finishes that request's row.
//! - Under the merged catalog each `response.create` is first checked against
//!   the socket's member ([`crate::ws_catalog::frame`]); the member's own model
//!   names apply last, as on the HTTP path ([`crate::model_map`]).
//!
//! The socket stays pinned to its line; closing either side closes the other.
//! Both legs end gracefully (close frame, FIN, the peer's rest read and dropped,
//! see [`linger`]), so the peer reads the close frame on every OS.
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
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
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

/// How long a closed leg still reads what its peer sends ([`linger`]).
const LINGER: Duration = Duration::from_secs(2);

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
    /// The ciphertexts the frame took to the member ([`crate::cipher`]).
    ciphers: Option<crate::cipher::Scan>,
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

/// Codex's leg of an upgraded connection, bounded like the member's.
pub(crate) async fn downstream<D>(io: D, max_message: usize) -> WebSocketStream<D>
where
    D: AsyncRead + AsyncWrite + Unpin,
{
    WebSocketStream::from_raw_socket(io, Role::Server, Some(config(max_message))).await
}

/// Relay frames between Codex (`down`) and the member (`up`) until either side
/// closes. `first` is a frame Codex sent before the member was dialed
/// ([`crate::ws_catalog`]): it is handled as if it arrived now.
pub(crate) async fn relay<D, U>(
    down: WebSocketStream<D>,
    up: U,
    ctx: RelayCtx,
    first: Option<Utf8Bytes>,
) where
    D: AsyncRead + AsyncWrite + Unpin + Send,
    U: AsyncRead + AsyncWrite + Unpin + Send,
{
    let limit = ctx.generation.config.tuning.max_body_bytes;
    let up = WebSocketStream::from_raw_socket(up, Role::Client, Some(config(limit))).await;
    let (mut down_tx, mut down_rx) = down.split();
    let (mut up_tx, mut up_rx) = up.split();
    let queue: Queue = Arc::new(Mutex::new(VecDeque::new()));
    let ctx = Arc::new(ctx);
    let oversize = Arc::new(Oversize::default());
    // Frames the router itself sends to Codex (compaction refusals, the close
    // that moves a compacted conversation to its member).
    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::unbounded_channel::<Message>();

    // The directions borrow the halves: they are put back together for the
    // graceful close at the end.
    let c2s = {
        let queue = queue.clone();
        let ctx = ctx.clone();
        let oversize = oversize.clone();
        let (down_rx, up_tx) = (&mut down_rx, &mut up_tx);
        let mut first = first;
        // Under the merged catalog each frame's model is checked against the
        // socket's member first.
        let routed = ctx.generation.config.catalog.is_some();
        async move {
            loop {
                let msg = match first.take() {
                    Some(t) => Some(Ok(Message::Text(t))),
                    None => tokio::select! {
                        msg = down_rx.next() => msg,
                        () = oversize.notify.notified() => {
                            let _ = up_tx.send(close_too_big()).await;
                            break;
                        }
                    },
                };
                let Some(msg) = msg else { break };
                match msg {
                    Ok(Message::Text(t)) => match routed_text(&ctx, &queue, t, routed).await {
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
        let ctx = ctx.clone();
        let oversize = oversize.clone();
        let (up_rx, down_tx) = (&mut up_rx, &mut down_tx);
        async move {
            // Until the client side ends and drops its sender.
            let mut replies_open = true;
            loop {
                let msg = tokio::select! {
                    // The router's own frames first: a close that moves the
                    // conversation must reach Codex before the member's close.
                    biased;
                    reply = reply_rx.recv(), if replies_open => {
                        let Some(reply) = reply else {
                            replies_open = false;
                            continue;
                        };
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
                    msg = up_rx.next() => msg,
                };
                let Some(msg) = msg else { break };
                match msg {
                    Ok(Message::Text(t)) => {
                        let out = server_text(&ctx, &queue, t);
                        let mut done = false;
                        for m in out {
                            // A close (the conversation is sent again on a
                            // new socket) ends this one.
                            let closing = matches!(m, Message::Close(_));
                            if down_tx.send(m).await.is_err() || closing {
                                done = true;
                                break;
                            }
                        }
                        if done {
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
    let mut c2s = Box::pin(c2s);
    let mut s2c = Box::pin(s2c);
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
    drop((c2s, s2c));
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
    if let (Ok(mut down), Ok(mut up)) = (down_rx.reunite(down_tx), up_rx.reunite(up_tx)) {
        tokio::join!(linger(&mut down), linger(&mut up));
    }
}

/// End a leg the relay closed gracefully: send what is queued (the close
/// frame), then FIN, then read and drop what the peer still sends until it
/// closes its side too, for at most [`LINGER`]. Dropping a socket with unread
/// input resets the connection instead (RST), and Windows discards the data
/// its peer has not read yet: after a message over the limit, whose rest is
/// still unread, Codex there saw "connection reset" instead of 1009 (message
/// too big).
pub(crate) async fn linger<S>(ws: &mut WebSocketStream<S>)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let _ = tokio::time::timeout(LINGER, async {
        let _ = ws.flush().await;
        let io = ws.get_mut();
        if io.shutdown().await.is_err() {
            return;
        }
        let mut buf = vec![0u8; 16 * 1024];
        while matches!(io.read(&mut buf).await, Ok(n) if n > 0) {}
    })
    .await;
}

/// What to do with a client frame.
pub(crate) enum Client {
    /// Send this to the member.
    Forward(Message),
    /// Do not forward; answer Codex with this frame.
    Reply(Message),
    /// Do not forward; close the socket with this frame (Codex reconnects and
    /// its handshake follows the conversation's new pin).
    Move(Message),
}

/// A `response.create` frame the socket's member must not get
/// ([`crate::compaction`]): `None` when it may. A member it moves to also
/// serves the frame's model (`selection`, under the merged catalog).
fn compaction_detour(
    ctx: &RelayCtx,
    m: &meta::RequestMeta,
    t0: Instant,
    selection: Option<&crate::merged_catalog::Selection>,
) -> Option<Client> {
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
        return Some(Client::Reply(refused(
            &ctx.shared,
            &ctx.generation,
            m.clone(),
            t0,
            (*status, code, message),
        )));
    }
    // Pin the conversation to a member that may serve it, then make Codex
    // reconnect: the new handshake goes there.
    let now = Instant::now();
    let order = match selection {
        Some(s) => s.candidates(generation, &route, None, now),
        None => crate::select::candidates(
            &crate::state::View {
                generation,
                websocket_only: false,
                route: &route,
            },
            None,
            now,
        ),
    };
    let target = order.into_iter().next()?;
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

/// The `error` event the router answers a frame with itself.
fn error_event(status: u16, code: &str, message: &str) -> Message {
    let event = serde_json::json!({
        "type": "error",
        "status": status,
        "error": {
            "type": "invalid_request_error",
            "code": code,
            "message": format!("Superba local router: {message}"),
        }
    });
    Message::Text(event.to_string().into())
}

/// A frame the router answers itself with `(status, code, message)`: recorded
/// (with a `stats` section) like the HTTP path's refusal, answered with an
/// `error` event.
pub(crate) fn refused(
    shared: &Arc<Shared>,
    generation: &Arc<Generation>,
    m: meta::RequestMeta,
    t0: Instant,
    (status, code, message): (u16, &str, &str),
) -> Message {
    if generation.config.stats.is_some() {
        let tracker = shared.stats.track(generation.clone(), m, t0, now_ms());
        tracker.finish(End {
            status: Some(RequestStatus::Http {
                status,
                code: Some(code.into()),
            }),
            error: Some(message.chars().take(300).collect()),
            ..End::default()
        });
    }
    error_event(status, code, message)
}

/// A client text frame: under the merged catalog checked against the
/// socket's member first ([`crate::ws_catalog::frame`]), then
/// [`client_text`].
async fn routed_text(ctx: &RelayCtx, queue: &Queue, t: Utf8Bytes, routed: bool) -> Client {
    if !routed {
        return client_text(ctx, queue, t, None);
    }
    match crate::ws_catalog::frame(ctx, t).await {
        Ok((t, selection)) => client_text(ctx, queue, t, selection.as_ref()),
        Err(client) => client,
    }
}

/// The member refused ciphertexts of the oldest open request: its refusal is
/// not passed on; the request is recorded as refused, and the socket is closed
/// with 1012 so Codex sends the turn again on a new one, where what the member
/// refused is left out (the frame Codex sends again is the same, so the
/// conversation keeps its bytes from then on).
fn resend_on_new_socket(
    ctx: &RelayCtx,
    q: &mut std::sync::MutexGuard<'_, VecDeque<Open>>,
    set: &std::collections::BTreeSet<crate::compaction::Fingerprint>,
) -> Vec<Message> {
    let member = &ctx.generation.members[ctx.answered.member].cfg;
    let Some(mut done) = q.pop_front() else {
        return Vec::new();
    };
    // What leaving out `set` changes, counted on the ciphertexts it named.
    let removed = removed_of(done.ciphers.as_ref(), set);
    ctx.shared.event(
        RouteEventKind::Ciphertext,
        Some(&member.id),
        None,
        Some(crate::cipher::event_detail(&member.name, &removed, true)),
    );
    done.tracker.note(format!(
        "{}: {}; left out {} and closed the WebSocket (1012) so Codex sends the turn again",
        member.id,
        crate::cipher::REJECTION_CODE,
        removed.describe()
    ));
    let summary = done.tap.summary();
    done.tracker.finish(End {
        summary: Some(summary),
        status: Some(RequestStatus::Http {
            status: 400,
            code: Some(crate::cipher::REJECTION_CODE.into()),
        }),
        ..End::default()
    });
    vec![Message::Close(Some(CloseFrame {
        code: CloseCode::Restart,
        reason: "sending again without content this provider cannot read".into(),
    }))]
}

/// [`crate::cipher::Removed`] counted from the places of the ciphertexts in
/// `set` (a WebSocket relay keeps no copy of the frame).
fn removed_of(
    scan: Option<&crate::cipher::Scan>,
    set: &std::collections::BTreeSet<crate::compaction::Fingerprint>,
) -> crate::cipher::Removed {
    use crate::cipher::Place;
    let mut r = crate::cipher::Removed::default();
    let mut seen = std::collections::BTreeSet::new();
    for c in scan.into_iter().flat_map(|s| s.ciphers.iter()) {
        if !set.contains(&c.fp) || !seen.insert((c.item, c.fp.clone())) {
            continue;
        }
        match c.place {
            Place::Reasoning => r.reasoning += 1,
            Place::Field => r.fields += 1,
            Place::Part { plaintext: true } => r.parts_as_text += 1,
            Place::Part { plaintext: false } => r.parts_omitted += 1,
            Place::Argument => r.arguments += 1,
            Place::Compaction => r.compactions += 1,
        }
    }
    r
}

/// A client frame: track and strip `response.create`, pass everything else.
/// The member's own model names apply last, as on the HTTP path
/// ([`crate::model_map`]).
fn client_text(
    ctx: &RelayCtx,
    queue: &Queue,
    t: Utf8Bytes,
    selection: Option<&crate::merged_catalog::Selection>,
) -> Client {
    let bytes: Bytes = t.clone().into();
    // Privacy shaping is independent of recording and applies before a
    // frame can be forwarded or retained for a retry.
    let t = crate::openai::shape::shape_top(&bytes, &crate::openai::shape::is_private_field, false)
        .and_then(|s| String::from_utf8(s.body).ok())
        .map(Utf8Bytes::from)
        .unwrap_or(t);
    let bytes: Bytes = t.clone().into();
    let member = &ctx.generation.members[ctx.answered.member].cfg;
    let limit = ctx.generation.config.tuning.max_body_bytes;
    let mapped = |t: Utf8Bytes| {
        let bytes: Bytes = t.clone().into();
        crate::model_map::map_model(&bytes, &HeaderMap::new(), &member.model_map, limit)
            .and_then(text)
            .unwrap_or(Message::Text(t))
    };
    if ctx.generation.config.stats.is_none() {
        return Client::Forward(mapped(t));
    }
    if sniff_type(&bytes) != Some("response.create") {
        return Client::Forward(Message::Text(t));
    }
    let t0 = Instant::now();
    let mut m = meta::peek_frame(&ctx.handshake, &bytes);
    if let Some(detour) = compaction_detour(ctx, &m, t0, selection) {
        return detour;
    }
    let out = match strip::strip(&bytes) {
        Some((new, n)) => {
            m.stripped = n;
            Bytes::from(new)
        }
        None => bytes,
    };
    // Ciphertexts another provider made: what this member refused before is
    // left out ([`crate::cipher`]).
    let scan = crate::cipher::scan(&out);
    let (out, ciphers, left_out) = if scan.is_empty() {
        (out, None, None)
    } else {
        let ledger = &ctx.shared.stats.ciphers;
        let sent = crate::cipher::Scan {
            ciphers: scan
                .ciphers
                .iter()
                .filter(|c| !ledger.refuses(&member.id, &c.fp))
                .cloned()
                .collect(),
            task_item: scan.task_item,
        };
        match crate::cipher::neutralise(&out, &|fp| ledger.refuses(&member.id, fp)) {
            Some((new, removed)) => (Bytes::from(new), Some(sent), Some(removed)),
            None => (out, Some(sent), None),
        }
    };
    let send = match Utf8Bytes::try_from(out) {
        Ok(out) => mapped(out),
        Err(_) => Message::Text(t),
    };
    let mut tracker = ctx
        .shared
        .stats
        .track(ctx.generation.clone(), m, t0, now_ms());
    tracker.ws_pinned(ctx.answered, ctx.failovers.swap(0, Ordering::Relaxed));
    if let Some(r) = left_out {
        tracker.note(format!(
            "{}: left out {} it refused before",
            member.id,
            r.describe()
        ));
    }
    if let Some(sent) = &ciphers {
        tracker.ciphers_sent(sent.ciphers.iter().map(|c| c.fp.clone()).collect());
    }
    let tap = ResponseTap::new(t0, tracker.line_setup());
    // The frame goes to the member right after this returns.
    tracker.set_sent(Instant::now());
    lock(queue).push_back(Open {
        tracker,
        tap,
        held: Vec::new(),
        first_frame: true,
        ciphers,
    });
    Client::Forward(send)
}

/// A member frame: feed the oldest open request's tap; returns the frames to
/// send to the client now.
fn server_text(ctx: &RelayCtx, queue: &Queue, t: Utf8Bytes) -> Vec<Message> {
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
    let member = &ctx.generation.members[ctx.answered.member].cfg;
    // A refusal of a ciphertext comes before any output.
    if let Some(sent) = head.ciphers.as_ref()
        && matches!(kind, EventKind::Error | EventKind::Failed)
        && head.tap.first_token().is_none()
        && let Some(rejection) = crate::cipher::rejection::from_event(&bytes)
    {
        match crate::cipher::refused(&ctx.shared.stats.ciphers, &member.id, sent, &rejection) {
            crate::cipher::Plan::Neutralise(set) => {
                return resend_on_new_socket(ctx, &mut q, &set);
            }
            crate::cipher::Plan::TaskUnreadable => {
                let message = crate::cipher::task_unreadable_message(&member.name);
                ctx.shared.event(
                    RouteEventKind::Ciphertext,
                    Some(&member.id),
                    None,
                    Some(message.clone()),
                );
                let event = serde_json::json!({
                    "type": "error",
                    "status": 400,
                    "error": {
                        "type": "invalid_request_error",
                        "code": crate::cipher::TASK_UNREADABLE_CODE,
                        "message": format!("Superba local router: {message}"),
                    }
                });
                let event = Bytes::from(event.to_string());
                if let Some(done) = q.pop_front() {
                    drop(q);
                    let summary = done.tap.summary();
                    done.tracker.finish(End {
                        summary: Some(summary),
                        status: Some(RequestStatus::Http {
                            status: 400,
                            code: Some(crate::cipher::TASK_UNREADABLE_CODE.into()),
                        }),
                        error: Some(message.chars().take(300).collect()),
                        ..End::default()
                    });
                }
                return text(event).into_iter().collect();
            }
            crate::cipher::Plan::PassThrough => {}
        }
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
