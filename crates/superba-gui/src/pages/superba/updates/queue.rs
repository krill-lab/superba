//! A reviewed, finite queue. Never retry a failed job implicitly.
use super::items::{Entry, UpdateItem};
use std::collections::VecDeque;

/// Jobs reviewed in the confirmation, and their common cautions.
#[derive(Clone, Debug, Default)]
pub struct Batch {
    /// Exact operations and versions shown for review.
    pub jobs: Vec<Entry>,
    /// Distinct cautions, prefixed with the affected item name.
    pub cautions: Vec<String>,
}
impl Batch {
    /// Only actionable install/update rows; a self-update runs last since it
    /// can restart the application. Vendor, unknown and disconnected rows skip.
    pub fn from_entries(entries: &[Entry]) -> Self {
        let mut jobs: Vec<_> = entries.iter().filter(|e| e.batchable()).cloned().collect();
        jobs.sort_by_key(|e| e.id == UpdateItem::Superba);
        let mut cautions = Vec::new();
        for e in &jobs {
            for c in &e.cautions {
                let c = format!("{}: {c}", e.name);
                if !cautions.contains(&c) {
                    cautions.push(c);
                }
            }
        }
        Self { jobs, cautions }
    }
}
/// At most one active job, including jobs whose cancellation is pending.
#[derive(Debug, Default)]
pub struct Queue {
    pending: VecDeque<Entry>,
    active: Option<UpdateItem>,
    /// Number of completed attempts, including failures and cancellations.
    pub completed: usize,
    /// Number of attempts in this batch.
    pub total: usize,
}
impl Queue {
    /// Begin an explicitly confirmed batch; a running batch is never replaced.
    pub fn start(&mut self, batch: Batch) {
        if self.busy() {
            return;
        }
        self.completed = 0;
        self.total = batch.jobs.len();
        self.pending = batch.jobs.into();
    }
    /// Any active or queued work.
    pub fn busy(&self) -> bool {
        self.active.is_some() || !self.pending.is_empty()
    }
    /// The operation currently awaited.
    pub fn active(&self) -> Option<&UpdateItem> {
        self.active.as_ref()
    }
    /// Stop scheduling; the active operation must still finish or acknowledge cancellation.
    pub fn cancel_pending(&mut self) {
        self.pending.clear();
    }
    /// Complete an observed result, then admit one next operation only.
    pub fn next(&mut self, active_running: bool, another_running: bool) -> Option<Entry> {
        if self.active.is_some() {
            if active_running {
                return None;
            }
            self.active = None;
            self.completed += 1;
        }
        if another_running {
            return None;
        }
        let next = self.pending.pop_front()?;
        self.active = Some(next.id.clone());
        Some(next)
    }
}
