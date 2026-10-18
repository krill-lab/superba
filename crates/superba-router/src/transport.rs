//! Upstream transports per member line: zstd request bodies and the Responses
//! WebSocket.
//!
//! Superba writes what its transport probe found into each line
//! ([`LineConfig::zstd`], [`LineConfig::websocket`]); the router adds what it
//! meets at run time and keeps it across reloads ([`TransportMemory`]):
//!
//! - **zstd**: a Responses `POST …/responses` whose body Codex sent
//!   uncompressed (API-key mode never compresses) is compressed with zstd for a
//!   line that accepts it ([`LineConfig::zstd`] = `Some(true)`), when the body
//!   is at least [`MIN_COMPRESS_BYTES`] and shrinks. Bodies Codex compressed
//!   itself pass through untouched. When such a line answers 400 / 415 / 422,
//!   the same request goes again uncompressed to the same line (nothing was
//!   processed: the answer is a refusal to read the body); a 415, or a plain
//!   retry that is not refused the same way, marks the line as refusing zstd for
//!   [`ZSTD_REFUSAL_TTL`]. A genuine 400 (the plain body gets it too) is passed
//!   through and not held against the line.
//! - **WebSocket**: a line whose probe got 426 ([`LineConfig::websocket`] =
//!   `Some(false)`), or that answered a handshake with 426, is not offered for
//!   [`WS_REFUSAL_TTL`]; a 101 clears the memory. The member-level
//!   [`crate::MemberConfig::websocket`] still decides whether a session uses
//!   WebSocket at all.
//!
//! Codex's HTTP/SSE turns are not upgraded to an upstream WebSocket: Codex
//! already opens a WebSocket per session whenever the router accepts the
//! handshake, so an HTTP turn reaches the router only where WebSocket was
//! refused (or after Codex's own fallback); a per-request socket carries the
//! same full input as the POST plus a handshake; and the incremental
//! `previous_response_id` mode, the only gain, would need a per-session
//! transcript of what the upstream holds that must match Codex's own
//! incremental-items logic exactly, or a conversation silently loses context —
//! with failover across lines (whose stored responses differ) and the request
//! log's per-request timing on top. zstd gives HTTP turns most of the upload
//! saving at none of that risk.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use bytes::Bytes;
use http::StatusCode;

use crate::config::{LineConfig, MemberConfig};
use crate::state::lock;

/// zstd level of compressed request bodies (fast; Codex input JSON shrinks
/// 5-10x).
pub(crate) const ZSTD_LEVEL: i32 = 3;

/// Smaller bodies are sent uncompressed (no gain worth the CPU).
pub const MIN_COMPRESS_BYTES: usize = 1024;

/// Bodies above this are compressed off the async threads.
const BLOCKING_COMPRESS_BYTES: usize = 128 * 1024;

/// How long a line that refused the router's zstd body gets uncompressed
/// bodies.
pub const ZSTD_REFUSAL_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// How long a line that answered a WebSocket handshake with 426 is not offered
/// for WebSocket (the gate is per account and may be lifted).
pub const WS_REFUSAL_TTL: Duration = Duration::from_secs(60 * 60);

/// What the router learned about lines at run time, by member, line and base
/// URL (kept across configuration reloads).
#[derive(Debug, Default)]
pub(crate) struct TransportMemory {
    inner: Mutex<HashMap<String, Learned>>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Learned {
    zstd_refused: Option<Instant>,
    ws_refused: Option<Instant>,
}

fn key(member: &MemberConfig, line: &LineConfig) -> String {
    format!("{}\u{1f}{}\u{1f}{}", member.id, line.id, line.base_url)
}

fn within(at: Option<Instant>, ttl: Duration, now: Instant) -> bool {
    at.is_some_and(|at| now.saturating_duration_since(at) < ttl)
}

impl TransportMemory {
    /// The line refused a zstd body less than [`ZSTD_REFUSAL_TTL`] ago.
    pub fn zstd_refused(&self, m: &MemberConfig, l: &LineConfig, now: Instant) -> bool {
        let map = lock(&self.inner);
        within(
            map.get(&key(m, l)).and_then(|e| e.zstd_refused),
            ZSTD_REFUSAL_TTL,
            now,
        )
    }

    /// Remember that the line refused a zstd body.
    pub fn note_zstd_refused(&self, m: &MemberConfig, l: &LineConfig, now: Instant) {
        lock(&self.inner).entry(key(m, l)).or_default().zstd_refused = Some(now);
    }

    /// The line answered a WebSocket handshake with 426 less than
    /// [`WS_REFUSAL_TTL`] ago.
    pub fn ws_refused(&self, m: &MemberConfig, l: &LineConfig, now: Instant) -> bool {
        let map = lock(&self.inner);
        within(
            map.get(&key(m, l)).and_then(|e| e.ws_refused),
            WS_REFUSAL_TTL,
            now,
        )
    }

    /// Remember a 426 to a WebSocket handshake.
    pub fn note_ws_refused(&self, m: &MemberConfig, l: &LineConfig, now: Instant) {
        lock(&self.inner).entry(key(m, l)).or_default().ws_refused = Some(now);
    }

    /// A handshake was upgraded: forget an earlier 426.
    pub fn note_ws_ok(&self, m: &MemberConfig, l: &LineConfig) {
        if let Some(e) = lock(&self.inner).get_mut(&key(m, l)) {
            e.ws_refused = None;
        }
    }
}

/// The router compresses request bodies to this line now.
pub(crate) fn compresses(
    memory: &TransportMemory,
    m: &MemberConfig,
    l: &LineConfig,
    now: Instant,
) -> bool {
    l.zstd == Some(true) && !memory.zstd_refused(m, l, now)
}

/// The line may be offered for a WebSocket now.
pub(crate) fn offers_websocket(
    memory: &TransportMemory,
    m: &MemberConfig,
    l: &LineConfig,
    now: Instant,
) -> bool {
    l.websocket != Some(false) && !memory.ws_refused(m, l, now)
}

/// Requests whose bodies may be compressed: `POST …/responses` (the endpoint
/// the probe checked; compaction and every other path pass as they are).
pub(crate) fn compressible_path(method: &http::Method, rest: &str) -> bool {
    method == http::Method::POST && rest.trim_end_matches('/').ends_with("/responses")
}

/// zstd-compressed `body`, or `None` when it is too small or does not shrink.
/// Large bodies are compressed off the async threads.
pub(crate) async fn compress_body(body: Bytes) -> Option<Bytes> {
    let len = body.len();
    if len < MIN_COMPRESS_BYTES {
        return None;
    }
    let run = move || {
        zstd::bulk::compress(&body, ZSTD_LEVEL)
            .ok()
            .filter(|z| z.len() < body.len())
            .map(Bytes::from)
    };
    if len < BLOCKING_COMPRESS_BYTES {
        run()
    } else {
        tokio::task::spawn_blocking(run).await.ok().flatten()
    }
}

/// An answer to a zstd body that may mean "could not read the body".
pub(crate) fn may_refuse_zstd(status: StatusCode) -> bool {
    matches!(status.as_u16(), 400 | 415 | 422)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member() -> (MemberConfig, LineConfig) {
        let line = LineConfig {
            id: "a".into(),
            name: "A".into(),
            base_url: url::Url::parse("https://h.example/backend-api/codex").unwrap(),
            probe_url: None,
            zstd: Some(true),
            websocket: None,
        };
        let m = MemberConfig {
            id: "m".into(),
            name: "M".into(),
            kind: crate::config::MemberKind::Krill,
            lines: vec![line.clone()],
            websocket: true,
            model_map: Default::default(),
            compaction: Default::default(),
        };
        (m, line)
    }

    #[test]
    fn refusals_are_remembered_for_their_ttl() {
        let (m, l) = member();
        let mem = TransportMemory::default();
        let t = Instant::now();
        assert!(compresses(&mem, &m, &l, t));
        mem.note_zstd_refused(&m, &l, t);
        assert!(!compresses(&mem, &m, &l, t));
        assert!(compresses(&mem, &m, &l, t + ZSTD_REFUSAL_TTL));
        assert!(offers_websocket(&mem, &m, &l, t));
        mem.note_ws_refused(&m, &l, t);
        assert!(!offers_websocket(&mem, &m, &l, t + Duration::from_secs(1)));
        assert!(offers_websocket(&mem, &m, &l, t + WS_REFUSAL_TTL));
        mem.note_ws_ok(&m, &l);
        assert!(offers_websocket(&mem, &m, &l, t));
        // Another base URL is another line.
        let mut moved = l.clone();
        moved.base_url = url::Url::parse("https://other.example/backend-api/codex").unwrap();
        assert!(compresses(&mem, &m, &moved, t));
        // Hints: unknown and "no" never compress; "no" never offers WebSocket.
        for hint in [None, Some(false)] {
            let mut x = moved.clone();
            x.zstd = hint;
            assert!(!compresses(&mem, &m, &x, t));
        }
        let mut no_ws = moved.clone();
        no_ws.websocket = Some(false);
        assert!(!offers_websocket(&mem, &m, &no_ws, t));
    }

    #[tokio::test]
    async fn bodies_are_compressed_only_when_it_pays() {
        assert_eq!(compress_body(Bytes::from_static(b"{}")).await, None);
        let big = Bytes::from(format!("{{\"input\":\"{}\"}}", "abc ".repeat(4096)));
        let z = compress_body(big.clone()).await.expect("compressed");
        assert!(z.len() < big.len() / 10);
        assert_eq!(zstd::decode_all(&z[..]).unwrap(), big);
        let huge = Bytes::from(format!("{{\"input\":\"{}\"}}", "xyz ".repeat(200_000)));
        let z = compress_body(huge.clone())
            .await
            .expect("compressed off-thread");
        assert_eq!(zstd::decode_all(&z[..]).unwrap(), huge);
        // Incompressible data stays as it is.
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let noise: Vec<u8> = (0..4096)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect();
        assert_eq!(compress_body(Bytes::from(noise)).await, None);
        assert!(compressible_path(&http::Method::POST, "/responses"));
        assert!(!compressible_path(
            &http::Method::POST,
            "/responses/compact"
        ));
        assert!(!compressible_path(&http::Method::GET, "/responses"));
        for s in [400, 415, 422] {
            assert!(may_refuse_zstd(StatusCode::from_u16(s).unwrap()));
        }
        for s in [401, 403, 404, 429, 500, 502] {
            assert!(!may_refuse_zstd(StatusCode::from_u16(s).unwrap()));
        }
    }
}
