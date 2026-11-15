//! Candidate order for one request.
//!
//! 1. The pinned line of the conversation, if its breaker is not open.
//! 2. The pinned member's other lines, in priority order.
//! 3. The other members in priority order, each with its lines in order.
//!
//! Open lines are skipped. When every eligible line is open, all of them are
//! tried anyway, the one whose backoff ends first first: every line failing at
//! once is most often this computer's network blipping, and the breakers are
//! stale as soon as it is back, so a request is refused only after each line
//! failed it.
//!
//! With failover off ([`crate::Tuning::failover_enabled`]) a request gets
//! only the first of that order, whether its breaker is open or not: the
//! conversation's line, else the first eligible line. Its failure goes back to
//! the client and no other line or member is tried.

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
    /// A failed attempt moves on to the next candidate (see the module docs).
    fn failover(&self) -> bool {
        true
    }
}

/// The attempt order for a request pinned to `pin` (if any).
pub fn candidates(topo: &dyn Topology, pin: Option<Pin>, now: Instant) -> Vec<Candidate> {
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
    if !topo.failover() {
        order.truncate(1);
        return order;
    }
    let available: Vec<Candidate> = order
        .iter()
        .copied()
        .filter(|c| topo.available(c.member, c.line, now))
        .collect();
    if !available.is_empty() {
        return available;
    }
    // Stable: lines that reopen together keep the priority order.
    order.sort_by_key(|c| topo.reopens_at(c.member, c.line));
    order
}
