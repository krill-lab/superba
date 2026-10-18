//! Which member reads which ciphertext (in memory; fingerprints and member
//! ids only, never content).

use std::collections::HashMap;
use std::sync::Mutex;

use crate::compaction::Fingerprint;
use crate::state::lock;

/// Fingerprints kept at most over all members (the least recently used go
/// first).
pub const MAX_ENTRIES: usize = 100_000;

/// What a member did with a ciphertext.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// A request carrying it was answered.
    Reads,
    /// It refused it (or it was left out of a request it refused).
    Refuses,
}

#[derive(Debug)]
struct Entry {
    verdict: Verdict,
    used: u64,
}

#[derive(Debug, Default)]
struct Inner {
    members: HashMap<String, HashMap<Fingerprint, Entry>>,
    len: usize,
    clock: u64,
}

/// Per member: the ciphertexts it read and those it refused.
#[derive(Debug, Default)]
pub struct Ledger {
    inner: Mutex<Inner>,
}

impl Ledger {
    /// An empty ledger.
    pub fn new() -> Self {
        Self::default()
    }

    /// What `member` did with `fp`.
    pub fn verdict(&self, member: &str, fp: &str) -> Option<Verdict> {
        let mut g = lock(&self.inner);
        g.clock += 1;
        let now = g.clock;
        let e = g.members.get_mut(member)?.get_mut(fp)?;
        e.used = now;
        Some(e.verdict)
    }

    /// True when `member` refused `fp`.
    pub fn refuses(&self, member: &str, fp: &str) -> bool {
        self.verdict(member, fp) == Some(Verdict::Refuses)
    }

    /// True when `member` read `fp`.
    pub fn reads(&self, member: &str, fp: &str) -> bool {
        self.verdict(member, fp) == Some(Verdict::Reads)
    }

    /// Record `verdict` of `member` for `fps`. A refusal replaces an earlier
    /// read; a read never replaces a refusal (a refused ciphertext is never
    /// sent to that member again, so a later read would be a mistake).
    pub fn learn<'a>(
        &self,
        member: &str,
        fps: impl IntoIterator<Item = &'a Fingerprint>,
        verdict: Verdict,
    ) {
        let mut g = lock(&self.inner);
        let mut added = 0usize;
        let mut now = g.clock;
        {
            let map = g.members.entry(member.to_owned()).or_default();
            for fp in fps {
                // One tick each, so the least recently used go first even
                // within one call.
                now += 1;
                match map.get_mut(fp) {
                    Some(e) => {
                        e.used = now;
                        if verdict == Verdict::Refuses {
                            e.verdict = verdict;
                        }
                    }
                    None => {
                        map.insert(fp.clone(), Entry { verdict, used: now });
                        added += 1;
                    }
                }
            }
        }
        g.clock = now;
        g.len += added;
        if g.len > MAX_ENTRIES {
            evict(&mut g);
        }
    }

    /// Fingerprints known.
    pub fn len(&self) -> usize {
        lock(&self.inner).len
    }

    /// True when nothing is known.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Drop the least recently used eighth of [`MAX_ENTRIES`] (amortised: one
/// scan per that many insertions).
fn evict(g: &mut Inner) {
    let target = MAX_ENTRIES - MAX_ENTRIES / 8;
    let mut uses: Vec<u64> = g
        .members
        .values()
        .flat_map(|m| m.values().map(|e| e.used))
        .collect();
    let drop = uses.len().saturating_sub(target);
    if drop == 0 {
        return;
    }
    let (_, cut, _) = uses.select_nth_unstable(drop - 1);
    let cut = *cut;
    let mut len = 0;
    for m in g.members.values_mut() {
        m.retain(|_, e| e.used > cut);
        len += m.len();
    }
    g.members.retain(|_, m| !m.is_empty());
    g.len = len;
}
