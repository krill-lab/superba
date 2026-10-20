//! Graceful quit of the app main process (plan 6.1, 7.2 Quitting).
//!
//! - Linux: SIGTERM to the main pid (Chromium shuts down cleanly).
//! - macOS: `NSRunningApplication::terminate` for the main pid, without Apple
//!   Events or Automation consent; SIGTERM for processes not registered as apps.
//! - Windows: closing its windows does not quit the app (it keeps running in
//!   the notification area: its `window-all-closed` handler skips `app.quit()`
//!   on win32). With CDP the controller asks the renderer bridge to quit
//!   ([`QUIT_APP_JS`], the app's own `quit-app` message, as its menu does);
//!   without it `WM_CLOSE` closes the windows and the wait is short
//!   ([`WINDOWS_CLOSE_ONLY_WAIT`]) before the caller is told it timed out.
//!   Force-kill only when the caller allows it, and then of the whole process
//!   tree: the main process and every descendant (recorded when the quit
//!   began and found again right before ending them, so a helper started in
//!   between goes too), each ended with `TerminateProcess` only while it is
//!   still the process recorded ([`crate::osproc`]); what is still alive after
//!   [`KILL_WAIT`] is retried only if termination was not accepted. Accepted
//!   requests get a bounded wait for Windows to finish them. A forced quit
//!   without CDP does not wait for windows to close again.
//!
//! Exit is "real" when the pid is gone, a zombie, or reused (start time differs).
//!
//! Every quit records what it did ([`QuitAttempt`]); after a timeout
//! [`report`] lists the app's processes still running (pid, image,
//! architecture, parent, start, why ending it failed) so no timeout is ever
//! reported without naming them (user bug 53).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use superba_core::{
    AppSession, LingeringProcess, QuitOptions, QuitOutcome, QuitReport, QuitStep, Timestamp,
};

use crate::osproc::{self, EndResult};
use crate::procs::{self, Liveness, ProcEntry};

/// JavaScript for the main renderer that asks the app to quit through its own
/// `quit-app` message (`app.quit()` in the main process, which runs the app's
/// shutdown handlers). Evaluates to `true` when the bridge took the message.
pub const QUIT_APP_JS: &str = "(() => { const b = window.electronBridge; \
     if (!b || typeof b.sendMessageFromView !== 'function') return false; \
     b.sendMessageFromView({type:'quit-app'}); return true; })()";

/// On Windows without CDP only the windows can be closed and the app stays
/// running; waiting the whole graceful timeout for it is pointless.
pub const WINDOWS_CLOSE_ONLY_WAIT: Duration = Duration::from_secs(5);

/// How long a forced quit waits per round when no Windows termination request
/// was accepted (at most two rounds).
pub const KILL_WAIT: Duration = Duration::from_secs(5);

/// Windows termination is asynchronous. Allow pending I/O / sandbox teardown
/// to finish after an accepted request, without issuing TerminateProcess again
/// (which can return access denied for a process already terminating).
pub const TERMINATION_WAIT: Duration = Duration::from_secs(30);

/// Ask the process to quit (platform specific; best effort).
pub fn request_quit(session: &AppSession) -> bool {
    imp::request_quit(session)
}

/// How the quit was asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuitRequest {
    /// The app was asked to quit (CDP `quit-app`, SIGTERM, `terminate`).
    App,
    /// Only its windows were asked to close (Windows without CDP).
    WindowsOnly,
}

/// The processes a forced quit ends, main process first: the main process
/// and every descendant still alive in `entries` (a recorded child that is
/// gone or whose pid was reused is not in the list).
pub fn kill_targets(
    main_pid: u32,
    main_started: Option<superba_core::Timestamp>,
    children: &[(u32, Option<superba_core::Timestamp>)],
    alive: &dyn Fn(u32, Option<superba_core::Timestamp>) -> bool,
) -> Vec<u32> {
    let mut out = Vec::new();
    if alive(main_pid, main_started) {
        out.push(main_pid);
    }
    for (pid, started) in children {
        if *pid != main_pid && !out.contains(pid) && alive(*pid, *started) {
            out.push(*pid);
        }
    }
    out
}

/// Poll until `pid` is gone or `timeout` passes. Returns true if it exited.
pub async fn wait_exit(session: &AppSession, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if procs::liveness(session.main_pid, session.started_at) == Liveness::Gone {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// How long a quit waits for the app's child processes after the main process is
/// gone. The Codex app-server outlives the main process briefly while it stops its
/// MCP servers; a migration started right away would see it as a Codex process the
/// user must close (found by the e2e harness).
pub const CHILD_EXIT_GRACE: Duration = Duration::from_secs(10);

/// The main process's descendants (pid and start time), from `entries`.
pub fn descendants(
    entries: &[procs::ProcEntry],
    main_pid: u32,
) -> Vec<(u32, Option<superba_core::Timestamp>)> {
    let mut out = Vec::new();
    let mut parents = vec![main_pid];
    while let Some(parent) = parents.pop() {
        let Some(parent_entry) = entries.iter().find(|e| e.pid == parent && !e.foreign_user) else {
            continue;
        };
        let parent_started = parent_entry.started_at;
        for e in entries
            .iter()
            .filter(|e| e.ppid == Some(parent) && !e.foreign_user)
        {
            // Windows retains a dead parent's PID, which can subsequently be
            // reused. An older process cannot be this parent's child.
            let older = matches!((e.started_at, parent_started), (Some(c), Some(p)) if c < p);
            if e.pid != main_pid && !older && !out.iter().any(|(pid, _)| *pid == e.pid) {
                out.push((e.pid, e.started_at));
                parents.push(e.pid);
            }
        }
    }
    out
}

/// `recorded` plus the descendants of `main_pid` in `entries` that are not in
/// it yet (pid and start time).
pub fn merge_tree(
    recorded: &[(u32, Option<Timestamp>)],
    entries: &[ProcEntry],
    main_pid: u32,
    main_started: Option<Timestamp>,
) -> Vec<(u32, Option<Timestamp>)> {
    let mut out = recorded.to_vec();
    if !entries.iter().any(|e| {
        e.pid == main_pid
            && !e.foreign_user
            && (!cfg!(windows) || (main_started.is_some() && e.started_at.is_some()))
            && osproc::same_process(main_started, e.started_at)
    }) {
        return out;
    }
    for (pid, started) in descendants(entries, main_pid) {
        if !out.iter().any(|(p, _)| *p == pid) {
            out.push((pid, started));
        }
    }
    out
}

/// Poll until every process in `children` is gone or `timeout` passes. Returns
/// true if they all exited (a reused pid counts as gone).
pub async fn wait_children_exit(
    children: &[(u32, Option<superba_core::Timestamp>)],
    timeout: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if children
            .iter()
            .all(|(pid, started)| procs::liveness(*pid, *started) == Liveness::Gone)
        {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Quit with the given options. After the main process is gone, waits up to
/// [`CHILD_EXIT_GRACE`] for its child processes (renderers, the Codex app-server)
/// so the caller sees the whole app gone.
pub async fn quit(session: &AppSession, options: &QuitOptions) -> QuitOutcome {
    quit_after(session, options, None).await
}

/// [`quit`] where the quit may already have been asked for (`requested`, e.g.
/// through CDP); otherwise it is asked for here.
pub async fn quit_after(
    session: &AppSession,
    options: &QuitOptions,
    requested: Option<QuitRequest>,
) -> QuitOutcome {
    attempt(session, options, requested, Vec::new())
        .await
        .outcome
}

/// What one quit did (user bug 53).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuitAttempt {
    /// Outcome.
    pub outcome: QuitOutcome,
    /// What was tried, in order.
    pub steps: Vec<QuitStep>,
    /// The main process's descendants (pid, start) recorded during the quit.
    pub tree: Vec<(u32, Option<Timestamp>)>,
    /// pid -> why it could not be ended.
    pub end_errors: BTreeMap<u32, String>,
    /// Processes `TerminateProcess` / SIGKILL was sent to successfully.
    pub terminated: Vec<u32>,
}

impl QuitAttempt {
    /// An attempt with an outcome and steps only.
    pub fn new(outcome: QuitOutcome, steps: Vec<QuitStep>) -> Self {
        Self {
            outcome,
            steps,
            tree: Vec::new(),
            end_errors: BTreeMap::new(),
            terminated: Vec::new(),
        }
    }

    /// Append a later attempt on the same session (a forced quit after a
    /// graceful one): steps in order, trees and errors merged.
    pub fn merge(mut self, later: QuitAttempt) -> QuitAttempt {
        self.outcome = later.outcome;
        self.steps.extend(later.steps);
        for t in later.tree {
            if !self.tree.iter().any(|(p, _)| *p == t.0) {
                self.tree.push(t);
            }
        }
        self.end_errors.extend(later.end_errors);
        for pid in later.terminated {
            if !self.terminated.contains(&pid) {
                self.terminated.push(pid);
            }
        }
        self
    }
}

/// What a report says about a process Superba ended but that still runs.
pub const TERMINATING: &str = "ended by Superba, but Windows has not finished ending it (a driver or pending I/O may hold it)";

fn waited(since: tokio::time::Instant) -> QuitStep {
    QuitStep::Waited {
        ms: u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX),
    }
}

/// End every process of `targets` that is still the one recorded, without
/// waiting; returns the pids ended and pid -> error for the others. Also the
/// forced stop of a Codex process the user asked for ([`crate::stop`]).
pub(crate) async fn end_all(
    targets: Vec<(u32, Option<Timestamp>)>,
) -> (Vec<u32>, BTreeMap<u32, String>) {
    tokio::task::spawn_blocking(move || {
        let mut ended = Vec::new();
        let mut errors = BTreeMap::new();
        for (pid, started) in targets {
            let t = std::time::Instant::now();
            let r = osproc::end(pid, started, Duration::ZERO);
            tracing::debug!(
                pid,
                result = %r.describe(),
                ms = u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX),
                "ending an app process"
            );
            match r {
                EndResult::Failed { stage, error } => {
                    errors.insert(pid, format!("{stage}: {error}"));
                }
                EndResult::Reused | EndResult::AlreadyGone => {}
                EndResult::Ended | EndResult::StillRunning => ended.push(pid),
            }
        }
        (ended, errors)
    })
    .await
    .unwrap_or_default()
}

/// [`quit_after`] with what it did; `steps` are steps the caller already took
/// (e.g. [`QuitStep::AskedApp`] over CDP).
pub async fn attempt(
    session: &AppSession,
    options: &QuitOptions,
    requested: Option<QuitRequest>,
    mut steps: Vec<QuitStep>,
) -> QuitAttempt {
    if procs::liveness(session.main_pid, session.started_at) == Liveness::Gone {
        return QuitAttempt::new(QuitOutcome::NotRunning, steps);
    }
    let entries = procs::snapshot_os(procs::ReadOpts { environ: false });
    let children = merge_tree(&[], &entries, session.main_pid, session.started_at);
    let request = match requested {
        Some(r) => r,
        // A forced quit without CDP goes straight to ending the tree: closing
        // the windows again cannot quit the app.
        None if cfg!(windows) && options.force => QuitRequest::WindowsOnly,
        None if cfg!(windows) => {
            if request_quit(session) {
                steps.push(QuitStep::ClosedWindows);
            }
            QuitRequest::WindowsOnly
        }
        None => {
            request_quit(session);
            steps.push(QuitStep::Signalled);
            QuitRequest::App
        }
    };
    let mut graceful = Duration::from_millis(if options.graceful_timeout_ms == 0 {
        15_000
    } else {
        options.graceful_timeout_ms
    });
    if request == QuitRequest::WindowsOnly {
        graceful = if options.force && requested.is_none() {
            Duration::ZERO
        } else {
            graceful.min(WINDOWS_CLOSE_ONLY_WAIT)
        };
        tracing::info!(
            main = session.main_pid,
            "only the app's windows can be closed (no CDP); it keeps running in the \
             notification area until it is asked through the app or ended"
        );
    }
    let since = tokio::time::Instant::now();
    if wait_exit(session, graceful).await {
        steps.push(waited(since));
        if !wait_children_exit(&children, CHILD_EXIT_GRACE).await {
            tracing::warn!(
                main = session.main_pid,
                "app child processes still running after quit"
            );
        }
        let mut done = QuitAttempt::new(QuitOutcome::Exited, steps);
        done.tree = children;
        return done;
    }
    if !graceful.is_zero() {
        steps.push(waited(since));
    }
    if !options.force {
        let mut done = QuitAttempt::new(QuitOutcome::TimedOut, steps);
        done.tree = children;
        return done;
    }
    forced(session, steps, children).await
}

/// The forced part of [`attempt`].
async fn forced(
    session: &AppSession,
    mut steps: Vec<QuitStep>,
    children: Vec<(u32, Option<Timestamp>)>,
) -> QuitAttempt {
    let mut tree = children;
    let mut end_errors = BTreeMap::new();
    let mut terminated: Vec<u32> = Vec::new();
    // Windows: end the whole tree (renderers, GPU and utility processes, the
    // Codex app-server), main process first so it starts nothing new.
    // Elsewhere the main process only (Chromium's helpers follow it).
    for round in 0..2 {
        let targets: Vec<(u32, Option<Timestamp>)> = if cfg!(windows) {
            let entries = procs::snapshot_os(procs::ReadOpts { environ: false });
            tree = merge_tree(&tree, &entries, session.main_pid, session.started_at);
            let alive = |p, s| procs::liveness(p, s) == Liveness::Alive;
            let mut out = Vec::new();
            for pid in kill_targets(session.main_pid, session.started_at, &tree, &alive) {
                if terminated.contains(&pid) {
                    continue;
                }
                let started = if pid == session.main_pid {
                    session.started_at
                } else {
                    tree.iter().find(|(p, _)| *p == pid).and_then(|(_, s)| *s)
                };
                out.push((pid, started));
            }
            out
        } else {
            vec![(session.main_pid, session.started_at)]
        };
        let count = targets.len();
        let (ended, errors) = end_all(targets).await;
        tracing::info!(
            main = session.main_pid,
            round,
            targets = count,
            ended = ended.len(),
            failed = errors.len(),
            "ended the app's processes"
        );
        steps.push(QuitStep::Ended {
            ended: u32::try_from(ended.len()).unwrap_or(u32::MAX),
            failed: u32::try_from(errors.len()).unwrap_or(u32::MAX),
        });
        for pid in ended {
            end_errors.remove(&pid);
            if !terminated.contains(&pid) {
                terminated.push(pid);
            }
        }
        end_errors.extend(errors);
        let since = tokio::time::Instant::now();
        let mut waiting = tree.clone();
        waiting.push((session.main_pid, session.started_at));
        let timeout = if cfg!(windows) && !terminated.is_empty() {
            TERMINATION_WAIT
        } else {
            KILL_WAIT
        };
        let exited = if cfg!(windows) {
            wait_children_exit(&waiting, timeout).await
        } else {
            wait_exit(session, timeout).await
        };
        if exited {
            steps.push(waited(since));
            if !cfg!(windows) && !wait_children_exit(&tree, CHILD_EXIT_GRACE).await {
                tracing::warn!(
                    main = session.main_pid,
                    "app child processes still running after kill"
                );
            }
            return QuitAttempt {
                outcome: QuitOutcome::Killed,
                steps,
                tree,
                end_errors,
                terminated,
            };
        }
        steps.push(waited(since));
        // There is nothing more to send when all live targets already have an
        // accepted request. A timeout still reports them as pending; it never
        // pretends that they exited or advises elevation for this state.
        if waiting.iter().all(|(pid, started)| {
            terminated.contains(pid) || procs::liveness(*pid, *started) == Liveness::Gone
        }) {
            break;
        }
    }
    QuitAttempt {
        outcome: QuitOutcome::TimedOut,
        steps,
        tree,
        end_errors,
        terminated,
    }
}

/// What a process of the app is: `main`, its Electron process type
/// (`renderer`, `gpu-process`, ...), `codex <subcommand>`, or its image name.
pub fn role_of(entry: &ProcEntry, main_pid: u32) -> String {
    if entry.pid == main_pid {
        return "main".into();
    }
    if let Some(t) = entry.argv.iter().find_map(|a| {
        a.strip_prefix("--type=")
            .or_else(|| a.split(" --type=").nth(1))
    }) {
        return t.split_whitespace().next().unwrap_or(t).to_owned();
    }
    if let Some(args) = procs::codex_args(&entry.argv, entry.exe.as_deref()) {
        let (words, _) = procs::codex_words(&args);
        return words
            .first()
            .map_or_else(|| "codex".to_owned(), |w| format!("codex {w}"));
    }
    entry
        .exe
        .as_deref()
        .or_else(|| entry.argv.first().map(Path::new))
        .and_then(|x| x.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "?".into())
}

/// The app's processes still running: the main process, its descendants now
/// and those recorded in `tree` (an orphaned child keeps its parent's pid on
/// Windows), with what the OS says about each (pure part of [`report`]).
pub fn lingering(
    session: &AppSession,
    entries: &[ProcEntry],
    tree: &[(u32, Option<Timestamp>)],
    alive: &dyn Fn(u32, Option<Timestamp>) -> bool,
) -> Vec<(ProcEntry, Option<Timestamp>)> {
    let mut wanted: Vec<(u32, Option<Timestamp>)> = vec![(session.main_pid, session.started_at)];
    for (pid, started) in merge_tree(tree, entries, session.main_pid, session.started_at) {
        if !wanted.iter().any(|(p, _)| *p == pid) {
            wanted.push((pid, started));
        }
    }
    wanted
        .into_iter()
        .filter(|(pid, started)| alive(*pid, *started))
        .filter_map(|(pid, started)| {
            entries
                .iter()
                .find(|e| e.pid == pid)
                .map(|e| (e.clone(), started.or(e.started_at)))
        })
        .collect()
}

/// The report after a quit (see the module docs): steps and errors from the
/// attempt, the processes from a fresh snapshot.
pub fn report(session: &AppSession, attempt: &QuitAttempt) -> QuitReport {
    let entries = procs::snapshot_os(procs::ReadOpts { environ: false });
    let alive = |p, s| procs::liveness(p, s) == Liveness::Alive;
    let end_error = |pid: u32| {
        attempt.end_errors.get(&pid).cloned().or_else(|| {
            attempt
                .terminated
                .contains(&pid)
                .then(|| TERMINATING.to_owned())
        })
    };
    let left = lingering(session, &entries, &attempt.tree, &alive)
        .into_iter()
        .filter_map(|(e, started)| {
            let facts = osproc::facts(e.pid);
            // Exited since the snapshot (short-lived helpers such as git).
            if facts.gone
                || facts.running == Some(false)
                || !osproc::same_process(started, facts.created)
            {
                return None;
            }
            Some(LingeringProcess {
                pid: e.pid,
                ppid: e.ppid,
                image: facts.image.or_else(|| e.exe.clone()),
                role: role_of(&e, session.main_pid),
                arch: facts.arch.map(str::to_owned),
                started_at: facts.created.or(started),
                end_error: end_error(e.pid).or(facts.open_error),
            })
        })
        .collect();
    QuitReport {
        steps: attempt.steps.clone(),
        left,
    }
}

/// End the listed processes (the user's "end these processes"): each, with
/// its descendants, only while it is still the process listed. Returns what
/// it did as an attempt to merge into the session's record.
pub async fn end_listed(listed: &[LingeringProcess]) -> QuitAttempt {
    let entries = procs::snapshot_os(procs::ReadOpts { environ: false });
    let mut targets: Vec<(u32, Option<Timestamp>)> = Vec::new();
    for p in listed {
        let Some(e) = entries.iter().find(|e| e.pid == p.pid) else {
            continue;
        };
        if !osproc::same_process(p.started_at, e.started_at) {
            continue;
        }
        if !targets.iter().any(|(pid, _)| *pid == p.pid) {
            targets.push((p.pid, p.started_at));
        }
        for d in descendants(&entries, p.pid) {
            if !targets.iter().any(|(pid, _)| *pid == d.0) {
                targets.push(d);
            }
        }
    }
    let all = targets.clone();
    let (ended, errors) = end_all(targets).await;
    let gone = wait_children_exit(&all, KILL_WAIT).await;
    let mut done = QuitAttempt::new(
        if gone {
            QuitOutcome::Killed
        } else {
            QuitOutcome::TimedOut
        },
        vec![QuitStep::Ended {
            ended: u32::try_from(ended.len()).unwrap_or(u32::MAX),
            failed: u32::try_from(errors.len()).unwrap_or(u32::MAX),
        }],
    );
    done.tree = all;
    done.end_errors = errors;
    done.terminated = ended;
    done
}

#[cfg(target_os = "linux")]
mod imp {
    use superba_core::AppSession;

    pub(super) fn request_quit(session: &AppSession) -> bool {
        crate::procs::terminate(session.main_pid)
    }
}

#[cfg(target_os = "macos")]
mod imp {
    //! macOS: request termination of this exact app instance without Apple Events.
    //! Native behavior is not exercised in Linux CI; wait_exit still observes the
    //! actual process exit because terminate() only acknowledges the request.
    use objc2_app_kit::NSRunningApplication;
    use superba_core::AppSession;

    pub(super) fn request_quit(session: &AppSession) -> bool {
        let Ok(pid) = i32::try_from(session.main_pid) else {
            return false;
        };
        if let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) {
            return app.terminate();
        }
        crate::procs::terminate(session.main_pid)
    }
}

#[cfg(windows)]
mod imp {
    //! Windows: post WM_CLOSE to every visible top-level window of the main pid
    //! (exercised on a real Windows runner by `tests/windows_app_it.rs`).
    use superba_core::AppSession;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE,
    };

    struct Ctx {
        pid: u32,
        posted: bool,
    }

    unsafe extern "system" fn on_window(hwnd: HWND, lparam: LPARAM) -> i32 {
        // SAFETY: lparam is the &mut Ctx passed to EnumWindows below and outlives
        // the enumeration; hwnd comes from the system.
        unsafe {
            let ctx = &mut *(lparam as *mut Ctx);
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == ctx.pid && IsWindowVisible(hwnd) != 0 {
                PostMessageW(hwnd, WM_CLOSE, 0 as WPARAM, 0 as LPARAM);
                ctx.posted = true;
            }
        }
        1
    }

    pub(super) fn request_quit(session: &AppSession) -> bool {
        let mut ctx = Ctx {
            pid: session.main_pid,
            posted: false,
        };
        // SAFETY: the callback only dereferences `ctx` during this call.
        unsafe {
            EnumWindows(Some(on_window), &mut ctx as *mut Ctx as LPARAM);
        }
        ctx.posted
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod imp {
    use superba_core::AppSession;

    pub(super) fn request_quit(session: &AppSession) -> bool {
        crate::procs::terminate(session.main_pid)
    }
}
