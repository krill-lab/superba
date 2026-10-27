//! Candidate order for one request.
//!
//! 1. The pinned line of the conversation, if its breaker is not open.
//! 2. The pinned member's other lines, in priority order.
//! 3. The other members in priority order, each with its lines in order.
//!
//! Open lines are skipped. When every eligible line is open, the one whose backoff
//! ends first is tried anyway, so a request is never refused without at least one
//! attempt (the breakers may be stale).

use std::time::Instant;

use crate::sticky::Pin;

/// One attempt target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// Member index.
    pub member: usize,
    /// Line index within the member.
    pub line: usize,
}

/// What [`candidates`] needs to know about the configuration and health.
pub trait Topology {
    /// Whether automatic candidate selection is enabled.
    fn failover_enabled(&self) -> bool {
        true
    }
    /// Manually selected exit. Used without consulting health or stickiness.
    fn fixed_exit(&self) -> Option<Candidate> {
        None
    }
    /// Number of members.
    fn members(&self) -> usize;
    /// Number of lines of `member`.
    fn lines(&self, member: usize) -> usize;
    /// The member may serve this request (e.g. it supports WebSocket).
    fn eligible(&self, member: usize) -> bool;
    /// The line's breaker is not open at `now`.
    fn available(&self, member: usize, line: usize, now: Instant) -> bool;
    /// When an open line turns half-open.
    fn reopens_at(&self, member: usize, line: usize) -> Option<Instant>;
}

/// The attempt order for a request pinned to `pin` (if any).
pub fn candidates(topo: &dyn Topology, pin: Option<Pin>, now: Instant) -> Vec<Candidate> {
    if !topo.failover_enabled() {
        return topo
            .fixed_exit()
            .filter(|c| topo.eligible(c.member))
            .into_iter()
            .collect();
    }
    let mut order: Vec<Candidate> = Vec::new();
    let pin = pin.filter(|p| p.member < topo.members() && p.line < topo.lines(p.member));
    let pinned_member = pin.map(|p| p.member).filter(|m| topo.eligible(*m));
    if let (Some(p), Some(_)) = (pin, pinned_member) {
        order.push(Candidate {
            member: p.member,
            line: p.line,
        });
        for line in 0..topo.lines(p.member) {
            if line != p.line {
                order.push(Candidate {
                    member: p.member,
                    line,
                });
            }
        }
    }
    for member in 0..topo.members() {
        if Some(member) == pinned_member || !topo.eligible(member) {
            continue;
        }
        for line in 0..topo.lines(member) {
            order.push(Candidate { member, line });
        }
    }
    let available: Vec<Candidate> = order
        .iter()
        .copied()
        .filter(|c| topo.available(c.member, c.line, now))
        .collect();
    if !available.is_empty() {
        return available;
    }
    order
        .into_iter()
        .min_by_key(|c| topo.reopens_at(c.member, c.line))
        .into_iter()
        .collect()
}
