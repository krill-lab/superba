//! Stopping Codex consumer processes the user asked Superba to stop (GH#2).
//!
//! Only what the user saw listed and agreed to stop is touched, and only while
//! it is still that process:
//!
//! - the pid must still be a Codex consumer (codex CLI session, `codex exec`,
//!   the CLI daemon, an IDE extension's or another app-server) of the current
//!   user, with the start time it was listed with (a reused pid is left
//!   alone and reported as gone); the desktop app is never stopped here (its
//!   own quit goes through [`crate::quit`]);
//! - graceful first: SIGTERM on Unix. Codex's TUI does not handle it and ends
//!   at once (verified with 0.163.0-alpha.10.2 in a pty: no reset sequences
//!   are written), so its terminal keeps the alternate screen, mouse reporting
//!   and keyboard modes; on Linux Superba writes the TUI's own reset sequence
//!   to that terminal after it exited. Windows has no way to ask a console
//!   program or an app-server talking over pipes to exit (no window to close;
//!   console control events reach the whole console, the shell included), so
//!   without `force` nothing is sent there ([`StopOutcome::ForceOnly`]);
//! - force only when the caller says so (after an explicit confirmation): the
//!   process and its descendants (npm's `node …/codex.js` wrapper runs the
//!   native codex as a child) get SIGKILL / `TerminateProcess`.

use std::path::PathBuf;
use std::time::Duration;

use superba_core::{CodexProcessKind, Error, ProcessInfo, Result, StopOutcome, Timestamp};

use crate::procs::{self, Liveness, ProcEntry, START_TIME_SLACK_MS};

/// Kinds Superba may stop for the user.
pub fn stoppable(kind: &CodexProcessKind) -> bool {
    matches!(
        kind,
        CodexProcessKind::CliTui
            | CodexProcessKind::CliExec
            | CodexProcessKind::CliDaemon
            | CodexProcessKind::IdeAppServer
            | CodexProcessKind::Other
    )
}

/// Whether this platform can ask a stoppable process to exit (Unix: SIGTERM).
pub fn graceful_supported() -> bool {
    cfg!(unix)
}

/// The listed process as it is now, or why it must not be touched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Identity {
    /// Still the same process.
    Same,
    /// Gone, a zombie, or the pid belongs to another program now.
    Gone,
    /// Owned by another OS user: Superba cannot stop it.
    ForeignUser,
}

/// Compare the listed process with the current snapshot entry of its pid.
pub fn identity(listed: &ProcessInfo, now: Option<&ProcEntry>) -> Identity {
    let Some(e) = now else {
        return Identity::Gone;
    };
    if e.zombie {
        return Identity::Gone;
    }
    match (listed.started_at, e.started_at) {
        (Some(a), Some(b)) if (a.as_millis() - b.as_millis()).abs() > START_TIME_SLACK_MS => {
            return Identity::Gone;
        }
        _ => {}
    }
    if e.foreign_user {
        return Identity::ForeignUser;
    }
    Identity::Same
}

/// The processes a forced stop ends: the process itself first, then its
/// descendants still in `entries`.
pub fn force_targets(entries: &[ProcEntry], pid: u32) -> Vec<(u32, Option<Timestamp>)> {
    let mut out: Vec<(u32, Option<Timestamp>)> = entries
        .iter()
        .filter(|e| e.pid == pid)
        .map(|e| (e.pid, e.started_at))
        .collect();
    for d in crate::quit::descendants(entries, pid) {
        if !out.iter().any(|(p, _)| *p == d.0) {
            out.push(d);
        }
    }
    out
}

/// The sequence Codex's TUI writes when it leaves (mouse and focus reporting
/// off, keyboard enhancement flags popped, bracketed paste off, main screen,
/// cursor shown), as captured from 0.163 on its normal exit.
pub const TUI_RESET: &str = "\x1b[?1006l\x1b[?1015l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[<1u\
     \x1b[?1007l\x1b[?1049l\x1b[<u\x1b[>4;0m\x1b[?2004l\x1b[?1004l\x1b[0 q\x1b[?25h";

/// The terminal device of a process (Linux: what its stdin, stdout or stderr
/// points at, when that is a terminal).
pub fn terminal_of(pid: u32) -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    (0..3).find_map(|fd| {
        let link = std::fs::read_link(format!("/proc/{pid}/fd/{fd}")).ok()?;
        let s = link.to_string_lossy();
        (s.starts_with("/dev/pts/") || s.starts_with("/dev/tty")).then_some(link)
    })
}

/// Put a terminal back the way the TUI leaves it on its own (best effort).
fn reset_terminal(tty: &std::path::Path) {
    use std::io::Write;
    let file = std::fs::OpenOptions::new().write(true).open(tty);
    match file {
        Ok(mut f) => {
            if let Err(e) = f.write_all(TUI_RESET.as_bytes()) {
                tracing::debug!(error = %e, tty = %tty.display(), "cannot reset the terminal");
            }
        }
        Err(e) => tracing::debug!(error = %e, tty = %tty.display(), "cannot open the terminal"),
    }
}

async fn wait_gone(targets: &[(u32, Option<Timestamp>)], timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if targets
            .iter()
            .all(|(pid, started)| procs::liveness(*pid, *started) == Liveness::Gone)
        {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

/// Stop `listed` (see the module docs). `timeout` bounds the wait for it to
/// exit after the request.
pub async fn stop(listed: &ProcessInfo, force: bool, timeout: Duration) -> Result<StopOutcome> {
    if !stoppable(&listed.kind) {
        return Err(Error::Unsupported {
            what: "stopping the desktop app this way (Superba quits it instead)".into(),
        });
    }
    let entries = tokio::task::spawn_blocking(|| procs::snapshot_os(procs::ReadOpts::default()))
        .await
        .map_err(|e| Error::other(format!("background task failed: {e}")))?;
    let now = entries.iter().find(|e| e.pid == listed.pid);
    match identity(listed, now) {
        Identity::Gone => return Ok(StopOutcome::NotRunning),
        Identity::ForeignUser => {
            return Err(Error::Process {
                message: format!(
                    "process {} belongs to another user; Superba cannot stop it",
                    listed.pid
                ),
            });
        }
        Identity::Same => {}
    }
    let started = now.and_then(|e| e.started_at).or(listed.started_at);
    let tty = (listed.kind == CodexProcessKind::CliTui)
        .then(|| terminal_of(listed.pid))
        .flatten();
    let me = [(listed.pid, started)];
    let outcome = if force {
        let targets = force_targets(&entries, listed.pid);
        for (pid, at) in &targets {
            if procs::liveness(*pid, *at) == Liveness::Alive {
                procs::kill(*pid);
            }
        }
        if wait_gone(&me, timeout.max(Duration::from_secs(3))).await {
            StopOutcome::Killed
        } else {
            StopOutcome::StillRunning
        }
    } else if !graceful_supported() {
        return Ok(StopOutcome::ForceOnly);
    } else {
        procs::terminate(listed.pid);
        if wait_gone(&me, timeout).await {
            StopOutcome::Exited
        } else {
            StopOutcome::StillRunning
        }
    };
    if outcome.is_gone()
        && let Some(tty) = tty
    {
        reset_terminal(&tty);
    }
    Ok(outcome)
}
