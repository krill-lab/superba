//! The "Verify core" check: did a new Codex core (the desktop app's
//! `codex app-server` child) come up after a hot restart or a relaunch, and
//! does it keep running? (plan 7.2 Verifying, user bug 62(a)).
//!
//! The desktop app does not always start its app-server once. Seen on Windows
//! (ChatGPT 26.1013, `e2e/windows` W10): after a launch it first runs a
//! short-lived `codex app-server` without its plugin flags, ends it about a
//! second later and starts the real one (with `-c plugins.…enabled=true`) two
//! to three seconds after that; on the first start of a machine it restarts
//! the app-server again once its Windows sandbox setup is done. The check
//! therefore follows every new app-server it sees and passes as soon as one
//! of them has been running for the stability period while no previous one is
//! left — not only the first one it happened to see, which made a relaunch
//! fail with "no new Codex core process started" while the real app-server
//! was running.
//!
//! [`CoreWatch`] is a pure state machine over process snapshots (the executor
//! polls and feeds it); [`CoreWatch::failure`] says what was checked and what
//! was seen when the time is up.

use std::collections::BTreeMap;
use std::time::Duration;

use superba_core::{CodexProcessKind, LocalizedText, ProcessInfo, Timestamp};

use crate::messages::text;
use crate::rules::PROCESS_START_SLACK_MS;

/// What one observation concluded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoreState {
    /// A new app-server has run for the stability period and no previous one
    /// is left.
    Passed {
        /// The new app-server.
        pid: u32,
    },
    /// A new app-server runs and no previous one is left, but it has not run
    /// for the stability period yet: worth waiting for even past the
    /// appearance deadline.
    Settling,
    /// Nothing conclusive yet.
    Waiting,
}

#[derive(Clone, Copy, Debug)]
struct Seen {
    /// Elapsed time of the first observation.
    first: Duration,
    /// Still in the latest snapshot.
    alive: bool,
}

/// Follows the app-servers of one restart or launch.
#[derive(Clone, Debug)]
pub struct CoreWatch {
    t0: Timestamp,
    before: Vec<u32>,
    stable: Duration,
    seen: BTreeMap<u32, Seen>,
    old_alive: Vec<u32>,
    seen_any: bool,
}

impl CoreWatch {
    /// Watch for app-servers started at or after `t0` (restart request or
    /// launch; [`PROCESS_START_SLACK_MS`] tolerance) that are not one of
    /// `before`; one must run for `stable` to pass.
    pub fn new(t0: Timestamp, before: &[u32], stable: Duration) -> Self {
        Self {
            t0,
            before: before.to_vec(),
            stable,
            seen: BTreeMap::new(),
            old_alive: before.to_vec(),
            seen_any: !before.is_empty(),
        }
    }

    fn is_new(&self, p: &ProcessInfo) -> bool {
        !self.before.contains(&p.pid)
            && p.started_at.is_none_or(|s| {
                s.as_millis().saturating_add(PROCESS_START_SLACK_MS) >= self.t0.as_millis()
            })
    }

    /// Feed one process snapshot taken `elapsed` after the watch started.
    pub fn observe(&mut self, elapsed: Duration, processes: &[ProcessInfo]) -> CoreState {
        let servers: Vec<&ProcessInfo> = processes
            .iter()
            .filter(|p| matches!(p.kind, CodexProcessKind::AppServer))
            .collect();
        self.seen_any |= !servers.is_empty();
        self.old_alive = servers
            .iter()
            .filter(|p| self.before.contains(&p.pid))
            .map(|p| p.pid)
            .collect();
        let new: Vec<u32> = servers
            .iter()
            .filter(|p| self.is_new(p))
            .map(|p| p.pid)
            .collect();
        for s in self.seen.values_mut() {
            s.alive = false;
        }
        for pid in &new {
            self.seen
                .entry(*pid)
                .or_insert(Seen {
                    first: elapsed,
                    alive: true,
                })
                .alive = true;
        }
        if !self.old_alive.is_empty() || new.is_empty() {
            return CoreState::Waiting;
        }
        // The longest-running new app-server decides.
        let oldest = new
            .iter()
            .filter_map(|pid| self.seen.get(pid).map(|s| (s.first, *pid)))
            .min();
        match oldest {
            Some((first, pid)) if elapsed.saturating_sub(first) >= self.stable => {
                CoreState::Passed { pid }
            }
            _ => CoreState::Settling,
        }
    }

    /// Whether any app-server was ever observed (before the restart or since);
    /// when none ever was, a missing one is not evidence of a failed restart
    /// (app-servers may not be observable on this system).
    pub fn seen_any(&self) -> bool {
        self.seen_any
    }

    /// The new app-servers seen so far that exited again.
    pub fn exited_new(&self) -> Vec<u32> {
        self.seen
            .iter()
            .filter(|(_, s)| !s.alive)
            .map(|(pid, _)| *pid)
            .collect()
    }

    /// The new app-servers running in the latest snapshot.
    pub fn alive_new(&self) -> Vec<u32> {
        self.seen
            .iter()
            .filter(|(_, s)| s.alive)
            .map(|(pid, _)| *pid)
            .collect()
    }

    /// Why the check failed after watching for `waited`: what was looked for
    /// (the Codex core of the app `app_pid`) and what was seen.
    pub fn failure(&self, waited: Duration, app_pid: u32) -> LocalizedText {
        let seconds = waited.as_secs_f64().round() as u64;
        let alive = self.alive_new();
        let exited = self.exited_new();
        let old = pids(&self.old_alive);
        let mut t = if !alive.is_empty() && !self.old_alive.is_empty() {
            text("switch.verify.old_app_server_alive")
                .arg("new", pids(&alive))
                .arg("old", old)
                .arg("seconds", seconds)
        } else if !alive.is_empty() {
            // Running but not for the stability period (only at the very end).
            text("switch.verify.new_app_server_unsettled")
                .arg("pid", pids(&alive))
                .arg("stable", self.stable.as_secs())
                .arg("seconds", seconds)
        } else if !exited.is_empty() {
            text("switch.verify.new_app_servers_exited")
                .arg("pids", pids(&exited))
                .arg("seconds", seconds)
                .arg("app", app_pid)
        } else {
            text("switch.verify.no_new_app_server")
                .arg("app", app_pid)
                .arg("seconds", seconds)
        };
        if alive.is_empty() && !self.before.is_empty() {
            let gone: Vec<u32> = self
                .before
                .iter()
                .copied()
                .filter(|p| !self.old_alive.contains(p))
                .collect();
            let previous = if self.old_alive.is_empty() {
                text("switch.verify.previous_exited").arg("pids", pids(&gone))
            } else {
                text("switch.verify.previous_running").arg("pids", pids(&self.old_alive))
            };
            t = text("switch.verify.with_previous")
                .arg_text("detail", t)
                .arg_text("previous", previous);
        }
        t
    }
}

/// `1234` / `1234, 5678`.
fn pids(list: &[u32]) -> String {
    list.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}
