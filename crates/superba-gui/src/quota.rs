//! When the shell reads the limits of the saved ChatGPT accounts (PR #20).
//!
//! Every saved account is read every [`QUOTA_EVERY`] on every page and while
//! the window is hidden in the tray (the one other timer besides the router
//! watchdog and the notification watcher, see `cadence`). One read runs at a
//! time across all accounts and none while a switch is planned or running or a
//! sign-in waits; a read is never started for an account whose read is in
//! flight. A failed read keeps the last limits (and their time) and is tried
//! again after a doubling pause ([`backoff`], at most [`BACKOFF_MAX`]); a
//! read the service refused for now (a switch, a sign-in renewal:
//! `Error::Locked`) comes back after [`BUSY_RETRY`] without counting as a
//! failure. A manual refresh runs the account's read next and resets its
//! pause. Pure state on wall-clock [`Timestamp`]s, so tests drive it.

use std::collections::BTreeMap;
use std::time::Duration;

use superba_core::{ChatGptQuota, Error, Result, Timestamp};

/// Interval between two reads of one account.
pub const QUOTA_EVERY: Duration = Duration::from_secs(60);
/// Longest pause after repeated failures.
pub const BACKOFF_MAX: Duration = Duration::from_secs(15 * 60);
/// Pause after a read the service postponed (`Error::Locked`).
pub const BUSY_RETRY: Duration = Duration::from_secs(10);

/// Pause after `failures` consecutive failed reads: [`QUOTA_EVERY`], then
/// doubling up to [`BACKOFF_MAX`].
pub fn backoff(failures: u32) -> Duration {
    let factor = 1u32 << failures.saturating_sub(1).min(8);
    (QUOTA_EVERY * factor).min(BACKOFF_MAX)
}

/// One account's limits as the shell knows them.
#[derive(Debug, Default)]
pub struct QuotaRow {
    /// The last limits read (kept when a later read fails).
    pub value: Option<ChatGptQuota>,
    /// The last read failed (cleared by a success).
    pub error: Option<Error>,
    /// Consecutive failed reads.
    pub failures: u32,
    /// When the next read is due (`None`: at once).
    pub next_due: Option<Timestamp>,
    /// A read runs now.
    pub in_flight: bool,
}

/// The saved accounts' limits and their timers.
#[derive(Debug, Default)]
pub struct QuotaBoard {
    rows: BTreeMap<String, QuotaRow>,
}

impl QuotaBoard {
    /// Keep the rows of these accounts only (new ones are due at once).
    pub fn sync<'a>(&mut self, ids: impl IntoIterator<Item = &'a str>) {
        let ids: Vec<&str> = ids.into_iter().collect();
        self.rows.retain(|id, _| ids.contains(&id.as_str()));
        for id in ids {
            self.rows.entry(id.to_owned()).or_default();
        }
    }

    /// The row of `id`.
    pub fn get(&self, id: &str) -> Option<&QuotaRow> {
        self.rows.get(id)
    }

    /// Some read runs now.
    pub fn busy(&self) -> bool {
        self.rows.values().any(|r| r.in_flight)
    }

    /// The account to read now, marked in flight: the most overdue one, only
    /// when no read runs and nothing pauses reads.
    pub fn start_due(&mut self, now: Timestamp, paused: bool) -> Option<String> {
        if paused || self.busy() {
            return None;
        }
        let (id, row) = self
            .rows
            .iter_mut()
            .filter(|(_, r)| r.next_due.is_none_or(|t| t <= now))
            .min_by_key(|(_, r)| r.next_due.unwrap_or(Timestamp::EPOCH))?;
        row.in_flight = true;
        Some(id.clone())
    }

    /// The answer of `id`'s read.
    pub fn finish(&mut self, id: &str, result: Result<ChatGptQuota>, now: Timestamp) {
        let Some(row) = self.rows.get_mut(id) else {
            return;
        };
        row.in_flight = false;
        match result {
            Ok(value) => {
                row.value = Some(value);
                row.error = None;
                row.failures = 0;
                row.next_due = Some(now.saturating_add(QUOTA_EVERY));
            }
            Err(Error::Locked { .. }) => {
                row.next_due = Some(now.saturating_add(BUSY_RETRY));
            }
            Err(e) => {
                row.failures = row.failures.saturating_add(1);
                row.error = Some(e);
                row.next_due = Some(now.saturating_add(backoff(row.failures)));
            }
        }
    }

    /// The user asked: read `id` next, with its pause reset.
    pub fn refresh_now(&mut self, id: &str) {
        if let Some(row) = self.rows.get_mut(id) {
            row.next_due = None;
            row.failures = 0;
        }
    }

    /// How long until a read is due (`None`: no account, or a read runs).
    pub fn wake_in(&self, now: Timestamp) -> Option<Duration> {
        if self.busy() {
            return None;
        }
        self.rows
            .values()
            .map(|r| {
                r.next_due
                    .and_then(|t| t.duration_since(now))
                    .unwrap_or(Duration::ZERO)
            })
            .min()
    }
}
