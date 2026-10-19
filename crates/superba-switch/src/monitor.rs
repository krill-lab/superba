//! The environment monitor (user request 34): one long-lived background thread
//! owns the slow-changing facts of this machine, the desktop app installs (paths,
//! versions, MSIX packages) and the Codex binaries (bundled and CLI, with their
//! versions), and refreshes them on events and long intervals. Views (the
//! overview polled every few seconds, the Updates, Models and Settings pages)
//! read its latest snapshot, an `Arc` clone, and never run detection themselves.
//!
//! What makes it look again:
//!
//! - **change check** ([`AppController::environment_stamp`]): metadata (size,
//!   modification time) of a short list of paths: install locations and the
//!   directories installs are found in, the PATH directories, the standalone
//!   Codex layout in the Codex home, the binaries found and the programs their
//!   launchers run (on Windows also the MSIX package registry key). No directory
//!   listing, no process. Every [`MonitorTuning::check_every`] while the window
//!   is shown; a difference means detection again;
//! - **window events** ([`EnvironmentEvent`]): shown (watching starts), hidden
//!   (watching stops: nothing runs while the window is hidden), focus regained
//!   (a change check now, and detection when the last one is older than
//!   [`MonitorTuning::refresh_on_focus_after`]);
//! - **explicit refresh** ([`EnvironmentEvent::Refresh`]): detection with every
//!   cached probe forgotten (each Codex binary is asked for its version again);
//! - **actions** ([`EnvMonitor::invalidate`]): after a switch, a Codex install or
//!   rollback, a pin change: detection before the next read;
//! - **long interval** ([`MonitorTuning::refresh_every`], while shown): detection
//!   as a safety net for changes the check cannot see.
//!
//! Detection on a timer or a change check never starts a process for a binary
//! that did not change: `--version` answers are cached by file stamp
//! (`superba_appctl::install::codex_version`), and Windows' PowerShell fallback
//! runs once until an explicit refresh. Only [`EnvironmentEvent::Refresh`] asks
//! every binary again (bounded by the number of binaries).
//!
//! Without the thread (`spb`, tests) the first read detects and later reads
//! reuse the snapshot until an event or [`EnvMonitor::invalidate`].

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use superba_core::{AppController, AppInstall, CodexBinary, EnvironmentEvent};

/// Intervals of the monitor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorTuning {
    /// Change check of the watched paths while the window is shown.
    pub check_every: Duration,
    /// Detection while shown even when the check saw no change (a safety net
    /// for changes outside the watched paths).
    pub refresh_every: Duration,
    /// When the window gets the focus back, detect again if the last detection
    /// is older than this (else only check).
    pub refresh_on_focus_after: Duration,
}

impl Default for MonitorTuning {
    fn default() -> Self {
        Self {
            check_every: Duration::from_secs(30),
            refresh_every: Duration::from_secs(30 * 60),
            refresh_on_focus_after: Duration::from_secs(10 * 60),
        }
    }
}

/// The slow facts, as of the last detection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvFacts {
    /// Desktop app installs (with their bundled Codex and its version).
    pub installs: Vec<AppInstall>,
    /// Codex binaries (PATH order, versions, origin).
    pub binaries: Vec<CodexBinary>,
    /// Bumped when the installs or binaries changed (1 = first detection).
    pub generation: u64,
}

/// Why a detection runs (a later, stronger request replaces a weaker one).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RefreshKind {
    /// The long interval, or the focus came back after a long time.
    Periodic,
    /// No snapshot yet.
    Initial,
    /// The change check saw a difference, or an action invalidated the facts.
    Changed,
    /// The user asked: cached probes are forgotten first.
    Forced,
}

/// What one [`EnvMonitor::step`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Nothing was due.
    Idle,
    /// The watched paths were checked; `changed` = detection followed.
    Checked {
        /// The check saw a difference (a [`RefreshKind::Changed`] detection ran).
        changed: bool,
    },
    /// Detection ran.
    Refreshed(RefreshKind),
    /// The monitor was dropped.
    Stopped,
}

/// Counters for tests and diagnostics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MonitorStats {
    /// Detections run (any kind).
    pub detections: u64,
    /// Of which forced (cached probes forgotten).
    pub forced: u64,
    /// Change checks run.
    pub checks: u64,
}

#[derive(Debug, Default)]
struct State {
    facts: Option<Arc<EnvFacts>>,
    pending: Option<RefreshKind>,
    foreground: bool,
    focus_gained: bool,
    woken: bool,
    stop: bool,
    thread: bool,
    stamp: Option<u64>,
    last_refresh: Option<Instant>,
    last_check: Option<Instant>,
    stats: MonitorStats,
}

struct Shared {
    app: Arc<dyn AppController>,
    data_dir: Option<superba_core::DataDir>,
    tuning: MonitorTuning,
    state: Mutex<State>,
    wake: Condvar,
    /// One detection at a time (readers on any runtime and the thread).
    detecting: tokio::sync::Mutex<()>,
    generation: AtomicU64,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn notify(&self, mut state: MutexGuard<'_, State>) {
        state.woken = true;
        drop(state);
        self.wake.notify_all();
    }
}

/// Stops the thread when the last handle of the monitor is dropped.
struct Owner(Arc<Shared>);

impl Drop for Owner {
    fn drop(&mut self) {
        let mut state = self.0.lock();
        state.stop = true;
        self.0.notify(state);
    }
}

/// The environment monitor. Cloning shares it.
#[derive(Clone)]
pub struct EnvMonitor {
    shared: Arc<Shared>,
    _owner: Arc<Owner>,
}

impl std::fmt::Debug for EnvMonitor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvMonitor")
            .field("generation", &self.generation())
            .finish_non_exhaustive()
    }
}

fn stronger(a: Option<RefreshKind>, b: RefreshKind) -> RefreshKind {
    a.map_or(b, |a| a.max(b))
}

impl EnvMonitor {
    /// A monitor over `app`'s detection. No thread runs until the first
    /// [`EnvironmentEvent::Shown`].
    pub fn new(app: Arc<dyn AppController>, tuning: MonitorTuning) -> Self {
        Self::build(app, tuning, None)
    }

    /// Production monitor: background cache writes participate in data relocation.
    pub(crate) fn guarded(app: Arc<dyn AppController>, data_dir: superba_core::DataDir) -> Self {
        Self::build(app, MonitorTuning::default(), Some(data_dir))
    }

    fn build(
        app: Arc<dyn AppController>,
        tuning: MonitorTuning,
        data_dir: Option<superba_core::DataDir>,
    ) -> Self {
        let shared = Arc::new(Shared {
            app,
            data_dir,
            tuning,
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            detecting: tokio::sync::Mutex::new(()),
            generation: AtomicU64::new(0),
        });
        Self {
            _owner: Arc::new(Owner(shared.clone())),
            shared,
        }
    }

    /// The facts. Detects first only when there is no snapshot yet or a
    /// detection was requested (an event, [`Self::invalidate`]); otherwise this
    /// is a clone of the snapshot.
    pub async fn facts(&self) -> Arc<EnvFacts> {
        loop {
            if let Some(facts) = self.current() {
                return facts;
            }
            let _guard = self.shared.detecting.lock().await;
            let kind = {
                let mut state = self.shared.lock();
                let pending = state.pending.take();
                match (&state.facts, pending) {
                    (Some(facts), None) => return facts.clone(),
                    (None, pending) => stronger(pending, RefreshKind::Initial),
                    (Some(_), Some(kind)) => kind,
                }
            };
            self.shared.detect(kind, Instant::now()).await;
        }
    }

    /// The snapshot when it is current (no detection pending), without waiting.
    pub fn current(&self) -> Option<Arc<EnvFacts>> {
        let state = self.shared.lock();
        state.facts.clone().filter(|_| state.pending.is_none())
    }

    /// Bumped each time the facts change.
    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::SeqCst)
    }

    /// Counters (tests, diagnostics).
    pub fn stats(&self) -> MonitorStats {
        self.shared.lock().stats.clone()
    }

    /// Whether the background thread runs.
    pub fn thread_started(&self) -> bool {
        self.shared.lock().thread
    }

    /// The facts may have changed (a switch, an install): detect again before
    /// the next read, and in the background while the window is shown.
    pub fn invalidate(&self) {
        let mut state = self.shared.lock();
        state.pending = Some(stronger(state.pending, RefreshKind::Changed));
        self.shared.notify(state);
    }

    /// A window event or an explicit refresh. The first `Shown` (or
    /// `FocusGained`) starts the thread.
    pub fn event(&self, event: EnvironmentEvent) {
        if self.apply(event, true) {
            Shared::spawn_thread(&self.shared);
        }
    }

    /// [`Self::event`] without starting the thread (tests drive [`Self::step`]
    /// themselves).
    pub fn record(&self, event: EnvironmentEvent) {
        self.apply(event, false);
    }

    /// Note `event`; true when the thread is to be started now.
    fn apply(&self, event: EnvironmentEvent, start_thread: bool) -> bool {
        let mut state = self.shared.lock();
        match event {
            EnvironmentEvent::Shown => state.foreground = true,
            EnvironmentEvent::Hidden => state.foreground = false,
            EnvironmentEvent::FocusGained => {
                state.foreground = true;
                state.focus_gained = true;
            }
            EnvironmentEvent::Refresh => {
                state.pending = Some(stronger(state.pending, RefreshKind::Forced));
            }
            _ => {}
        }
        let start = start_thread && state.foreground && !state.thread;
        if start {
            state.thread = true;
        }
        self.shared.notify(state);
        start
    }

    /// Run what is due at `now` (the thread calls this after every wake-up;
    /// tests call it with simulated instants).
    pub async fn step(&self, now: Instant) -> Step {
        self.shared.step(now).await
    }

    /// Stop background probes and drain an in-flight probe before moving its
    /// cache files. The owning service constructs a fresh monitor afterwards.
    pub async fn stop_and_drain(&self) {
        {
            let mut state = self.shared.lock();
            state.stop = true;
            self.shared.notify(state);
        }
        let _guard = self.shared.detecting.lock().await;
    }
}

enum Work {
    Detect(RefreshKind),
    Check,
}

impl Shared {
    async fn step(&self, now: Instant) -> Step {
        let t = &self.tuning;
        let work = {
            let mut state = self.lock();
            state.woken = false;
            if state.stop {
                return Step::Stopped;
            }
            let older = |at: Option<Instant>, age: Duration| {
                at.is_none_or(|at| now.saturating_duration_since(at) >= age)
            };
            if !state.foreground {
                // Hidden: nothing runs here; a pending detection waits for the
                // next read or for the window to be shown.
                return Step::Idle;
            }
            if let Some(kind) = state.pending.take() {
                Work::Detect(kind)
            } else if state.facts.is_none() {
                Work::Detect(RefreshKind::Initial)
            } else if std::mem::take(&mut state.focus_gained) {
                if older(state.last_refresh, t.refresh_on_focus_after) {
                    Work::Detect(RefreshKind::Periodic)
                } else {
                    Work::Check
                }
            } else if older(state.last_refresh, t.refresh_every) {
                Work::Detect(RefreshKind::Periodic)
            } else if older(state.last_check, t.check_every) {
                Work::Check
            } else {
                return Step::Idle;
            }
        };
        match work {
            Work::Detect(kind) => {
                let _guard = self.detecting.lock().await;
                if self.lock().stop {
                    return Step::Stopped;
                }
                self.detect(kind, now).await;
                Step::Refreshed(kind)
            }
            Work::Check => {
                let changed = self.check(now);
                if changed {
                    let _guard = self.detecting.lock().await;
                    if self.lock().stop {
                        return Step::Stopped;
                    }
                    self.detect(RefreshKind::Changed, now).await;
                }
                Step::Checked { changed }
            }
        }
    }

    /// How long the thread may sleep after a step at `now` (`None` = until an
    /// event).
    fn sleep_for(&self, now: Instant) -> Option<Duration> {
        let state = self.lock();
        if state.stop || state.woken {
            return Some(Duration::ZERO);
        }
        if !state.foreground {
            return None;
        }
        if state.pending.is_some() || state.facts.is_none() || state.focus_gained {
            return Some(Duration::ZERO);
        }
        let t = &self.tuning;
        let due = |at: Option<Instant>, every: Duration| {
            at.map_or(Duration::ZERO, |at| {
                (at + every).saturating_duration_since(now)
            })
        };
        Some(due(state.last_check, t.check_every).min(due(state.last_refresh, t.refresh_every)))
    }

    /// Compare the change stamp with the one of the last detection. Metadata
    /// reads only.
    fn check(&self, now: Instant) -> bool {
        let (facts, before) = {
            let mut state = self.lock();
            state.stats.checks += 1;
            state.last_check = Some(now);
            (state.facts.clone(), state.stamp)
        };
        let Some(facts) = facts else {
            return true;
        };
        let stamp = self.app.environment_stamp(&facts.installs, &facts.binaries);
        stamp.is_some() && stamp != before
    }

    /// Detect and store the facts. The caller holds `detecting`.
    async fn detect(&self, kind: RefreshKind, now: Instant) {
        let _data_lock = if let Some(data) = &self.data_dir {
            match superba_storage::DataLock::shared(data.anchor()).and_then(|lock| {
                if superba_storage::resolve(data.anchor())? == *data {
                    Ok(lock)
                } else {
                    Err(superba_core::Error::other("data.changed"))
                }
            }) {
                Ok(lock) => Some(lock),
                Err(_) => {
                    // A move owns the files, or this monitor belongs to the old
                    // location. A fresh service will create the next monitor.
                    let mut state = self.lock();
                    state.stop = true;
                    if state.facts.is_none() {
                        state.facts = Some(Arc::default());
                    }
                    return;
                }
            }
        } else {
            None
        };
        if kind == RefreshKind::Forced {
            self.app.forget_cached_probes();
        }
        let installs = self.app.detect_installs().await.unwrap_or_default();
        let binaries = self.app.codex_binaries().await.unwrap_or_default();
        let stamp = self.app.environment_stamp(&installs, &binaries);
        let mut state = self.lock();
        state.stats.detections += 1;
        if kind == RefreshKind::Forced {
            state.stats.forced += 1;
        }
        let unchanged = state
            .facts
            .as_ref()
            .is_some_and(|f| f.installs == installs && f.binaries == binaries);
        if !unchanged {
            let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
            state.facts = Some(Arc::new(EnvFacts {
                installs,
                binaries,
                generation,
            }));
        }
        state.stamp = stamp;
        state.last_refresh = Some(now);
        state.last_check = Some(now);
    }

    fn spawn_thread(shared: &Arc<Self>) {
        let weak = Arc::downgrade(shared);
        let spawned = std::thread::Builder::new()
            .name("superba-env-monitor".into())
            .spawn(move || run(weak));
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "the environment monitor thread did not start");
            shared.lock().thread = false;
        }
    }
}

/// The monitor thread: step, then sleep until the next due time or an event.
/// Between steps it holds only a weak reference, so it ends with the monitor.
fn run(weak: Weak<Shared>) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            tracing::warn!(error = %e, "the environment monitor has no runtime");
            return;
        }
    };
    loop {
        let Some(shared) = weak.upgrade() else {
            return;
        };
        if runtime.block_on(shared.step(Instant::now())) == Step::Stopped {
            return;
        }
        let sleep = shared.sleep_for(Instant::now());
        let state = shared.lock();
        let state = match sleep {
            Some(d) if d.is_zero() => state,
            Some(d) => {
                shared
                    .wake
                    .wait_timeout_while(state, d, |s| !s.woken && !s.stop)
                    .unwrap_or_else(|p| p.into_inner())
                    .0
            }
            None => shared
                .wake
                .wait_while(state, |s| !s.woken && !s.stop)
                .unwrap_or_else(|p| p.into_inner()),
        };
        drop(state);
    }
}
