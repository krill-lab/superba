//! Progress reports and safe stopping for the writing operations
//! ([`superba_core::SessionProgressSink`]).
//!
//! Kept apart from the engine, which only calls [`Progress`] at a few points:
//! [`Progress::begin`] once the sessions of an operation are known,
//! [`Progress::stage`] and [`Progress::item`] as files are read and written,
//! [`Progress::file_done`] when a session's file is in place,
//! [`Progress::checkpoint`] between files (the only place a stop request is
//! honoured) and [`Progress::finish`] at the end.
//!
//! Stopping is honoured only while a migration has not reached its databases
//! (reading, backing up, rewriting rollouts): the engine then fails with
//! [`Error::Cancelled`] and its usual failure path restores every rollout from
//! the backup. Restores never stop half-way.
//!
//! Reports are throttled to one every [`MIN_INTERVAL`]; stage changes and the
//! final report always go out, so a large home does not flood the UI.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use superba_core::{
    CodexHome, DataDir, Error, Result, SessionCurrent, SessionDirection, SessionProgress,
    SessionProgressSink, SessionStage, TitleSource,
};

use crate::CodexSessionMigrator;
use crate::engine::Engine;

/// Shortest time between two throttled reports.
pub(crate) const MIN_INTERVAL: Duration = Duration::from_millis(40);

/// The progress of operations that report nothing (the plain trait methods).
pub(crate) static NONE: Progress = Progress::none();

/// Progress of one writing operation (and of the automatic rollback that may
/// follow it). Every method is a no-op without a sink.
pub(crate) struct Progress {
    sink: Option<Arc<dyn SessionProgressSink>>,
    state: Mutex<State>,
}

struct State {
    direction: SessionDirection,
    stage: SessionStage,
    done: u32,
    total: u32,
    step_done: u32,
    step_total: u32,
    current: Option<SessionCurrent>,
    /// Thread id -> title (known titles only).
    titles: BTreeMap<String, String>,
    /// Sessions of the operation not counted in `done` yet.
    pending: BTreeSet<String>,
    /// Thread id -> rollout files still to rewrite in the `Moving` stage.
    files_left: BTreeMap<String, u32>,
    last: Option<Instant>,
}

impl Progress {
    const fn none() -> Self {
        Self {
            sink: None,
            state: Mutex::new(State {
                direction: SessionDirection::Migrate,
                stage: SessionStage::Reading,
                done: 0,
                total: 0,
                step_done: 0,
                step_total: 0,
                current: None,
                titles: BTreeMap::new(),
                pending: BTreeSet::new(),
                files_left: BTreeMap::new(),
                last: None,
            }),
        }
    }

    /// Reports to `sink`; `titles` names the sessions it may meet (thread id ->
    /// title), more are learnt while reading.
    pub(crate) fn new(
        sink: Arc<dyn SessionProgressSink>,
        titles: BTreeMap<String, String>,
    ) -> Self {
        let progress = Self {
            sink: Some(sink),
            ..Self::none()
        };
        progress.lock().titles = titles;
        progress
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// An operation over the sessions `threads` starts (`direction`).
    pub(crate) fn begin<'a>(
        &self,
        direction: SessionDirection,
        threads: impl IntoIterator<Item = &'a str>,
    ) {
        if self.sink.is_none() {
            return;
        }
        let mut st = self.lock();
        st.direction = direction;
        st.pending = threads
            .into_iter()
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
            .collect();
        st.total = u32::try_from(st.pending.len()).unwrap_or(u32::MAX);
        st.done = 0;
        st.files_left.clear();
        st.current = None;
    }

    /// Remember the title of a session.
    pub(crate) fn title(&self, thread: &str, title: Option<&str>) {
        if self.sink.is_none() {
            return;
        }
        if let Some(t) = title.map(str::trim).filter(|t| !t.is_empty()) {
            self.lock().titles.insert(thread.to_owned(), t.to_owned());
        }
    }

    /// Enter `stage`, which has `steps` units of work.
    pub(crate) fn stage(&self, stage: SessionStage, steps: usize) {
        if self.sink.is_none() {
            return;
        }
        let mut st = self.lock();
        st.stage = stage;
        st.step_done = 0;
        st.step_total = u32::try_from(steps).unwrap_or(u32::MAX);
        // The previous stage's last file is not this stage's.
        st.current = None;
        self.send(&mut st, true);
    }

    /// In the `Moving` stage: the rollout files of each session (`threads`, one
    /// entry per file); a session counts as moved when its last one is in place.
    pub(crate) fn expect_files<'a>(&self, threads: impl IntoIterator<Item = &'a str>) {
        if self.sink.is_none() {
            return;
        }
        let mut st = self.lock();
        st.files_left.clear();
        for t in threads {
            *st.files_left.entry(t.to_owned()).or_default() += 1;
        }
    }

    /// Work on the `index`-th unit of the stage starts (the ones before it are
    /// done): `thread`'s `file`.
    pub(crate) fn item(&self, index: usize, thread: Option<&str>, file: Option<&Path>) {
        if self.sink.is_none() {
            return;
        }
        let mut st = self.lock();
        st.step_done = u32::try_from(index).unwrap_or(u32::MAX).min(st.step_total);
        let title = thread.and_then(|t| st.titles.get(t).cloned());
        st.current = Some(SessionCurrent {
            title,
            file: file.map(Path::to_path_buf),
        });
        self.send(&mut st, false);
    }

    /// One rollout file of `thread` is in place.
    pub(crate) fn file_done(&self, thread: &str) {
        if self.sink.is_none() {
            return;
        }
        let mut st = self.lock();
        let last = match st.files_left.get_mut(thread) {
            Some(n) => {
                *n = n.saturating_sub(1);
                *n == 0
            }
            None => true,
        };
        if last && st.pending.remove(thread) {
            st.done = st.done.saturating_add(1).min(st.total);
        }
    }

    /// Fail with [`Error::Cancelled`] when the sink asks to stop and stopping
    /// is safe now.
    pub(crate) fn checkpoint(&self) -> Result<()> {
        let Some(sink) = &self.sink else {
            return Ok(());
        };
        let can_stop = can_stop(&self.lock());
        if can_stop && sink.stop_requested() {
            return Err(Error::Cancelled);
        }
        Ok(())
    }

    /// The operation finished: every session is accounted for.
    pub(crate) fn finish(&self) {
        if self.sink.is_none() {
            return;
        }
        let mut st = self.lock();
        st.stage = SessionStage::Done;
        st.done = st.total;
        st.pending.clear();
        st.step_done = st.step_total;
        st.current = None;
        self.send(&mut st, true);
    }

    fn send(&self, st: &mut State, force: bool) {
        let Some(sink) = &self.sink else {
            return;
        };
        let now = Instant::now();
        if !force
            && st
                .last
                .is_some_and(|last| now.duration_since(last) < MIN_INTERVAL)
        {
            return;
        }
        st.last = Some(now);
        sink.report(SessionProgress {
            direction: st.direction,
            stage: st.stage,
            done: st.done,
            total: st.total,
            step_done: st.step_done,
            step_total: st.step_total,
            current: st.current.clone(),
            can_stop: can_stop(st),
        });
    }
}

/// Stopping undoes everything only before a migration writes its databases.
fn can_stop(st: &State) -> bool {
    st.direction == SessionDirection::Migrate
        && matches!(
            st.stage,
            SessionStage::Reading | SessionStage::BackingUp | SessionStage::Moving
        )
}

/// Titles of the sessions a recorded migration changed (for a restore's
/// progress), read from Codex's databases; unknown ones are left out.
pub(crate) fn ledger_titles(
    data_dir: &DataDir,
    home_dir: Option<&Path>,
    home: &CodexHome,
    migration_id: &str,
) -> BTreeMap<String, String> {
    let Ok(stored) = crate::ledger::load(data_dir, migration_id) else {
        return BTreeMap::new();
    };
    let ids: BTreeSet<String> = stored
        .ledger
        .rollouts
        .iter()
        .map(|r| r.thread_id.clone())
        .chain(
            stored
                .ledger
                .databases
                .iter()
                .flat_map(|d| d.changes.iter().map(|c| c.thread_id.clone())),
        )
        .filter(|id| !id.is_empty())
        .collect();
    let keys: Vec<String> = ids.into_iter().collect();
    match crate::titles::lookup_in(home, home_dir, &keys) {
        Ok(found) => found
            .into_iter()
            .filter(|(_, t)| t.source != TitleSource::ShortKey)
            .map(|(id, t)| (id, t.title))
            .collect(),
        Err(_) => BTreeMap::new(),
    }
}

impl CodexSessionMigrator {
    /// The engine reporting to `progress`.
    pub(crate) fn engine_with<'a>(&'a self, progress: &'a Progress) -> Engine<'a> {
        Engine {
            progress,
            ..self.engine()
        }
    }
}
