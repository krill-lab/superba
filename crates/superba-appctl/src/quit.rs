//! Graceful quit of the app main process (plan 6.1, 7.2 Quitting).
//!
//! - Linux: SIGTERM to the main pid (Chromium shuts down cleanly).
//! - macOS: `NSRunningApplication::terminate` for the main pid, without Apple
//!   Events or Automation consent; SIGTERM for processes not registered as apps.
//! - Windows: `WM_CLOSE` to the main process's top-level windows; force-kill only
//!   when the caller allows it (`taskkill /T` without `/F` cannot close the app).
//!
//! Exit is "real" when the pid is gone, a zombie, or reused (start time differs).

use std::time::Duration;

use superba_core::{AppSession, QuitOptions, QuitOutcome};

use crate::procs::{self, Liveness};

/// Ask the process to quit (platform specific; best effort).
pub fn request_quit(session: &AppSession) -> bool {
    imp::request_quit(session)
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
        for e in entries.iter().filter(|e| e.ppid == Some(parent)) {
            if !out.iter().any(|(pid, _)| *pid == e.pid) {
                out.push((e.pid, e.started_at));
                parents.push(e.pid);
            }
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
    if procs::liveness(session.main_pid, session.started_at) == Liveness::Gone {
        return QuitOutcome::NotRunning;
    }
    let entries = procs::snapshot_os(procs::ReadOpts { environ: false });
    let children = descendants(&entries, session.main_pid);
    request_quit(session);
    let graceful = Duration::from_millis(if options.graceful_timeout_ms == 0 {
        15_000
    } else {
        options.graceful_timeout_ms
    });
    if wait_exit(session, graceful).await {
        if !wait_children_exit(&children, CHILD_EXIT_GRACE).await {
            tracing::warn!(
                main = session.main_pid,
                "app child processes still running after quit"
            );
        }
        return QuitOutcome::Exited;
    }
    if !options.force {
        return QuitOutcome::TimedOut;
    }
    procs::kill(session.main_pid);
    if wait_exit(session, Duration::from_secs(5)).await {
        if !wait_children_exit(&children, CHILD_EXIT_GRACE).await {
            tracing::warn!(
                main = session.main_pid,
                "app child processes still running after kill"
            );
        }
        QuitOutcome::Killed
    } else {
        QuitOutcome::TimedOut
    }
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
    //! Windows: post WM_CLOSE to every visible top-level window of the main pid.
    //! Not exercised in CI.
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
