//! Progress of a session migration or of its reverse, reported while the
//! sessions move ([`crate::SessionMigrator::migrate_stopped_with_progress`],
//! [`crate::SessionMigrator::rollback_stopped_with_progress`]) and forwarded to
//! the UIs as [`crate::SwitchEvent::Sessions`] (the GUI's migration dialog, the
//! progress line of `spb`).
//!
//! A sink can also ask the work to stop ([`SessionProgressSink::stop_requested`]).
//! The migrator honours that only while [`SessionProgress::can_stop`] is true:
//! before the databases are written, where stopping undoes everything already
//! done (the same path as a failed migration). Restoring never stops half-way.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Which way the sessions move.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionDirection {
    /// A migration: the sessions move to another provider id.
    Migrate,
    /// A migration being undone: a restore, an undo after a later step of a
    /// switch failed, or the automatic rollback of a migration that failed or
    /// was stopped.
    Restore,
}

/// The part of the work in progress, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SessionStage {
    /// Reading the sessions (computing a migration's rewrites, or checking which
    /// migrated items are still unchanged before a restore).
    Reading,
    /// Copying every file that changes into the backup.
    BackingUp,
    /// Rewriting (or restoring) the session files.
    Moving,
    /// Updating (or restoring) the databases.
    Databases,
    /// Checking that every change is in place.
    Verifying,
    /// Finished.
    Done,
}

/// The session being worked on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCurrent {
    /// Its title, when known.
    #[serde(default)]
    pub title: Option<String>,
    /// The file being read or written (a rollout, a database, a backup).
    #[serde(default)]
    pub file: Option<PathBuf>,
}

/// One progress report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionProgress {
    /// Migrating or restoring.
    pub direction: SessionDirection,
    /// Current stage.
    pub stage: SessionStage,
    /// Sessions moved so far ("migrated N of M"); never decreases while the
    /// direction stays the same, and equals `total` at [`SessionStage::Done`].
    pub done: u32,
    /// Sessions this operation moves.
    pub total: u32,
    /// Units of the current stage finished (sessions read, files backed up or
    /// rewritten, databases updated).
    pub step_done: u32,
    /// Units of the current stage.
    pub step_total: u32,
    /// What is being worked on now.
    #[serde(default)]
    pub current: Option<SessionCurrent>,
    /// Asking to stop now is honoured, and undoes everything done so far.
    pub can_stop: bool,
}

impl SessionProgress {
    /// Stages of a direction with their share of the usual running time (sum 1).
    fn weights(direction: SessionDirection) -> &'static [(SessionStage, f32)] {
        match direction {
            // Measured on 600 sessions of 400 KiB: reading and backing up
            // take about two thirds of the time, the databases very little.
            SessionDirection::Migrate => &[
                (SessionStage::Reading, 0.35),
                (SessionStage::BackingUp, 0.25),
                (SessionStage::Moving, 0.3),
                (SessionStage::Databases, 0.03),
                (SessionStage::Verifying, 0.07),
            ],
            SessionDirection::Restore => &[
                (SessionStage::Reading, 0.3),
                (SessionStage::Databases, 0.1),
                (SessionStage::Moving, 0.6),
            ],
        }
    }

    /// What the counter of the current stage shows, `(n, of)`: sessions moved
    /// while rewriting (or restoring) and once done, else the stage's own units
    /// (sessions read, files backed up or checked, databases updated). UIs
    /// name the stage with the numbers, and [`Self::fraction`] advances the
    /// stage's share by exactly this ratio, so the two never disagree.
    pub fn counter(&self) -> (u32, u32) {
        match self.stage {
            SessionStage::Moving | SessionStage::Done => (self.done, self.total),
            _ => (self.step_done, self.step_total),
        }
    }

    /// Overall completion in `0.0..=1.0`: the finished stages' shares plus the
    /// current stage's share times its [`Self::counter`] ratio. Never decreases
    /// while the stages advance in order.
    pub fn fraction(&self) -> f32 {
        if self.stage == SessionStage::Done {
            return 1.0;
        }
        let (n, of) = self.counter();
        let step = if of == 0 {
            0.0
        } else {
            (n.min(of) as f32) / (of as f32)
        };
        let mut before = 0.0;
        for (stage, weight) in Self::weights(self.direction) {
            if *stage == self.stage {
                return (before + weight * step).clamp(0.0, 1.0);
            }
            before += weight;
        }
        // A stage this direction does not go through: count by sessions.
        if self.total == 0 {
            0.0
        } else {
            (self.done.min(self.total) as f32) / (self.total as f32)
        }
    }
}

/// Receives [`SessionProgress`] reports from a migrator. Must not block: it is
/// called on the migrator's worker thread between files.
pub trait SessionProgressSink: Send + Sync {
    /// One report.
    fn report(&self, progress: SessionProgress);

    /// The user asked to stop. Polled between sessions; honoured only while the
    /// last report said [`SessionProgress::can_stop`]. Default: never.
    fn stop_requested(&self) -> bool {
        false
    }
}

/// A [`SessionProgressSink`] that drops every report and never stops.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoSessionProgress;

impl SessionProgressSink for NoSessionProgress {
    fn report(&self, _progress: SessionProgress) {}
}
