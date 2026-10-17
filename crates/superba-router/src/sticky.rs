//! Conversation stickiness: session key → (member, line) with an idle TTL and a
//! capacity bound.
//!
//! The key is Codex's `session-id` request header (falling back to `thread-id`,
//! then `x-client-request-id`): it is present on every Responses request and on
//! the WebSocket handshake, equals the thread id for a root thread, and is
//! inherited by sub-agents. Lines of one member are interchangeable at request
//! boundaries; the member changes only on failover, and the new member stays
//! pinned afterwards (no flapping back).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use http::HeaderMap;

/// Request headers that identify a conversation, in order of preference.
pub const SESSION_HEADERS: &[&str] = &["session-id", "thread-id", "x-client-request-id"];

/// Longest session key kept (longer values are ignored, not truncated).
const MAX_KEY_LEN: usize = 256;

/// The conversation key of a request, if any.
pub fn session_key(headers: &HeaderMap) -> Option<String> {
    SESSION_HEADERS.iter().find_map(|name| {
        let v = headers.get(*name)?.to_str().ok()?.trim();
        (!v.is_empty() && v.len() <= MAX_KEY_LEN).then(|| format!("{name}:{v}"))
    })
}

/// Where a conversation is pinned (indexes into the current configuration).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pin {
    /// Member index.
    pub member: usize,
    /// Line index within the member.
    pub line: usize,
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    pin: Pin,
    last_seen: Instant,
}

/// Pins with an idle TTL and an LRU bound.
#[derive(Debug)]
pub struct StickyTable {
    ttl: Duration,
    capacity: usize,
    map: HashMap<String, Entry>,
}

impl StickyTable {
    /// Empty table.
    pub fn new(ttl: Duration, capacity: usize) -> Self {
        Self {
            ttl,
            capacity: capacity.max(1),
            map: HashMap::new(),
        }
    }

    /// The pin of `key`, refreshing its idle timer; expired pins are dropped.
    pub fn get(&mut self, key: &str, now: Instant) -> Option<Pin> {
        let entry = self.map.get_mut(key)?;
        if now.saturating_duration_since(entry.last_seen) >= self.ttl {
            self.map.remove(key);
            return None;
        }
        entry.last_seen = now;
        Some(entry.pin)
    }

    /// Pin `key`.
    pub fn set(&mut self, key: &str, pin: Pin, now: Instant) {
        if let Some(e) = self.map.get_mut(key) {
            e.pin = pin;
            e.last_seen = now;
            return;
        }
        if self.map.len() >= self.capacity {
            self.purge(now);
        }
        if self.map.len() >= self.capacity
            && let Some(oldest) = self
                .map
                .iter()
                .min_by_key(|(_, e)| e.last_seen)
                .map(|(k, _)| k.clone())
        {
            self.map.remove(&oldest);
        }
        self.map.insert(
            key.to_owned(),
            Entry {
                pin,
                last_seen: now,
            },
        );
    }

    /// Drop expired pins.
    pub fn purge(&mut self, now: Instant) {
        let ttl = self.ttl;
        self.map
            .retain(|_, e| now.saturating_duration_since(e.last_seen) < ttl);
    }

    /// Number of pins (expired ones included until purged).
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}
