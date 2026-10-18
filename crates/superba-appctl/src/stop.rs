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
//! - graceful first: SIGTERM on Unix (app-servers drain and exit on it). A
//!   codex TUI gets SIGINT, then SIGHUP, then SIGTERM, each after a short
//!   wait, so a TUI that handles one of them can leave on its own. Codex's TUI
//!   (checked in its source and with 0.163.0-alpha.10.2 in a pty, user bug
//!   62(d)) handles none of the three: each ends it at once, with nothing
//!   written, so its terminal kept the alternate screen, mouse reporting and
//!   keyboard modes, and the raw mode — bash puts its own modes back after a
//!   job a signal ended, but zsh adopts the raw output and input modes the
//!   TUI left (`-opost -icrnl -isig` for every later command). Superba
//!   therefore puts the terminal back *before* the first signal (Linux: the
//!   terminal of the TUI's stdin/stdout/stderr; macOS: its controlling
//!   terminal from `ps`): the TUI's own reset sequence and the cooked modes
//!   raw mode turns off; the TUI ends within microseconds after, and the shell
//!   finds a sane terminal. Windows has no way to ask a console
//!   program or an app-server talking over pipes to exit (no window to close;
//!   console control events reach the whole console, the shell included), so
//!   without `force` nothing is sent there ([`StopOutcome::ForceOnly`]);
//! - force only when the caller says so (after an explicit confirmation): the
//!   process and its descendants (npm's `node …/codex.js` wrapper runs the
//!   native codex as a child) are ended through [`crate::osproc::end`]
//!   (`TerminateProcess` on a handle whose creation time is checked first, so
//!   a reused pid is never ended, with the Windows error kept; SIGKILL
//!   elsewhere) — no `taskkill.exe` (user bug 74). What still runs after the
//!   wait is named in the report with why ([`StopReport::left`], like a quit
//!   report, bug 53).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use superba_core::{
    CodexProcessKind, Error, LingeringProcess, ProcessInfo, Result, StopOutcome, StopReport,
    Timestamp,
};

use crate::osproc;
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

/// The terminal device of a process: Linux: what its stdin, stdout or stderr
/// points at, when that is a terminal; macOS (and other Unix): its
/// controlling terminal as `ps -o tty=` names it. `None` on Windows.
pub fn terminal_of(pid: u32) -> Option<PathBuf> {
    if cfg!(windows) {
        return None;
    }
    if cfg!(target_os = "linux") {
        return (0..3).find_map(|fd| {
            let link = std::fs::read_link(format!("/proc/{pid}/fd/{fd}")).ok()?;
            is_terminal_device(&link.to_string_lossy()).then_some(link)
        });
    }
    let out = superba_core::process::command("ps")
        .args(["-o", "tty=", "-p", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    tty_from_ps(&String::from_utf8_lossy(&out.stdout))
}

fn is_terminal_device(path: &str) -> bool {
    path.starts_with("/dev/pts/") || path.starts_with("/dev/tty")
}

/// The device a `ps -o tty=` line names (`ttys003` on macOS, `pts/3` on
/// Linux); `None` for `??` / `?` (no terminal).
pub fn tty_from_ps(text: &str) -> Option<PathBuf> {
    let name = text.lines().next()?.trim();
    if name.is_empty() || name.starts_with('?') || name.contains("..") {
        return None;
    }
    let path = if name.starts_with("/dev/") {
        name.to_owned()
    } else {
        format!("/dev/{name}")
    };
    is_terminal_device(&path).then(|| PathBuf::from(path))
}

/// Put a terminal back the way the TUI leaves it on its own (best effort):
/// its reset sequence, and on Unix the modes raw mode turned off (output
/// processing, CR to NL on input, echo, canonical input, signal keys).
/// `true` when the sequence was written. Termios is left alone when this
/// process is a background member of the terminal's own session (it would
/// be stopped by SIGTTOU).
pub fn restore_terminal(tty: &std::path::Path) -> bool {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Never make it our controlling terminal.
        options.custom_flags(libc::O_NOCTTY);
    }
    let mut file = match options.open(tty) {
        Ok(f) => f,
        Err(e) => {
            tracing::debug!(error = %e, tty = %tty.display(), "cannot open the terminal");
            return false;
        }
    };
    let wrote = match file.write_all(TUI_RESET.as_bytes()) {
        Ok(()) => true,
        Err(e) => {
            tracing::debug!(error = %e, tty = %tty.display(), "cannot reset the terminal");
            false
        }
    };
    #[cfg(unix)]
    cooked_modes(&file);
    wrote
}

/// Turn the cooked terminal modes back on (what `cfmakeraw` turned off and a
/// shell expects: `opost onlcr icrnl icanon echo echoe echok isig iexten`);
/// flow control and the character size are left as they are.
#[cfg(unix)]
fn cooked_modes(file: &std::fs::File) {
    use std::os::fd::AsRawFd;
    let fd = file.as_raw_fd();
    // SAFETY: plain calls on an open descriptor of ours; `t` is a termios the
    // kernel fills in before it is read.
    unsafe {
        let sid = libc::tcgetsid(fd);
        if sid != -1 && sid == libc::getsid(0) && libc::tcgetpgrp(fd) != libc::getpgrp() {
            tracing::debug!("the terminal belongs to our session's foreground: modes left alone");
            return;
        }
        let mut t = std::mem::MaybeUninit::<libc::termios>::zeroed();
        if libc::tcgetattr(fd, t.as_mut_ptr()) != 0 {
            return;
        }
        let mut t = t.assume_init();
        let before = (t.c_iflag, t.c_oflag, t.c_lflag);
        t.c_iflag |= libc::ICRNL;
        t.c_oflag |= libc::OPOST | libc::ONLCR;
        t.c_lflag |=
            libc::ICANON | libc::ECHO | libc::ECHOE | libc::ECHOK | libc::ISIG | libc::IEXTEN;
        if before != (t.c_iflag, t.c_oflag, t.c_lflag)
            && libc::tcsetattr(fd, libc::TCSANOW, &t) != 0
        {
            tracing::debug!(error = %std::io::Error::last_os_error(), "cannot restore the terminal modes");
        }
    }
}

/// How long a TUI gets after SIGINT and after SIGHUP before the next signal.
const TUI_SIGNAL_WAIT: Duration = Duration::from_millis(1_500);

/// SIGINT, then SIGHUP, then SIGTERM, each followed by a wait; SIGTERM gets
/// what is left of `timeout` (at least a second).
async fn stop_tui(me: &[(u32, Option<Timestamp>)], timeout: Duration) -> StopOutcome {
    let pid = me[0].0;
    let started = tokio::time::Instant::now();
    for send in [procs::interrupt, procs::hangup] {
        send(pid);
        if wait_gone(me, TUI_SIGNAL_WAIT.min(timeout / 3)).await {
            return StopOutcome::Exited;
        }
    }
    procs::terminate(pid);
    let left = timeout
        .saturating_sub(started.elapsed())
        .max(Duration::from_secs(1));
    if wait_gone(me, left).await {
        StopOutcome::Exited
    } else {
        StopOutcome::StillRunning
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

/// What a forced stop leaves running (pure part of [`stop`]): each of
/// `targets` still alive, with what the OS says about it (`facts`) and why
/// Superba could not end it — the error ending it gave, or [`ENDED_STILL_RUNNING`]
/// when it was ended but has not exited yet.
pub fn left_after_force(
    targets: &[(u32, Option<Timestamp>)],
    entries: &[ProcEntry],
    errors: &BTreeMap<u32, String>,
    ended: &[u32],
    alive: &dyn Fn(u32, Option<Timestamp>) -> bool,
    facts: &dyn Fn(u32) -> osproc::ProcFacts,
) -> Vec<LingeringProcess> {
    targets
        .iter()
        .filter(|(pid, started)| alive(*pid, *started))
        .filter_map(|(pid, started)| {
            let f = facts(*pid);
            if f.gone {
                return None;
            }
            let entry = entries.iter().find(|e| e.pid == *pid);
            Some(LingeringProcess {
                pid: *pid,
                ppid: entry.and_then(|e| e.ppid),
                image: f.image.or_else(|| entry.and_then(|e| e.exe.clone())),
                // pid 0 is never one of them: no process is called "main".
                role: entry.map_or_else(|| "?".to_owned(), |e| crate::quit::role_of(e, 0)),
                arch: f.arch.map(str::to_owned),
                started_at: f.created.or(*started),
                end_error: errors
                    .get(pid)
                    .cloned()
                    .or_else(|| ended.contains(pid).then(|| ENDED_STILL_RUNNING.to_owned()))
                    .or(f.open_error),
            })
        })
        .collect()
}

/// Why a process Superba ended still runs.
pub const ENDED_STILL_RUNNING: &str = if cfg!(windows) {
    crate::quit::TERMINATING
} else {
    "ended by Superba (SIGKILL), but it has not exited yet"
};

/// Stop `listed` (see the module docs). `timeout` bounds the wait for it to
/// exit after the request.
pub async fn stop(listed: &ProcessInfo, force: bool, timeout: Duration) -> Result<StopReport> {
    let report = |outcome| StopReport::new(listed.clone(), outcome);
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
        Identity::Gone => return Ok(report(StopOutcome::NotRunning)),
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
    let tui = listed.kind == CodexProcessKind::CliTui;
    if !force && !graceful_supported() {
        return Ok(report(StopOutcome::ForceOnly));
    }
    let pid = listed.pid;
    let tty = if tui {
        tokio::task::spawn_blocking(move || terminal_of(pid))
            .await
            .ok()
            .flatten()
    } else {
        None
    };
    // The TUI restores nothing when a signal ends it: its terminal is put back
    // first (see the module docs).
    let restored = match tty.clone() {
        Some(tty) => tokio::task::spawn_blocking(move || restore_terminal(&tty))
            .await
            .unwrap_or(false),
        None => false,
    };
    let me = [(listed.pid, started)];
    let mut left = Vec::new();
    let outcome = if force {
        // Through osproc (pid-reuse safe, the OS error kept), never taskkill.
        let mut targets = force_targets(&entries, listed.pid);
        if let Some(first) = targets.first_mut() {
            first.1 = started;
        }
        let (ended, errors) = crate::quit::end_all(targets.clone()).await;
        tracing::info!(
            pid = listed.pid,
            targets = targets.len(),
            ended = ended.len(),
            failed = errors.len(),
            "ended a Codex process and its descendants"
        );
        let gone = wait_gone(&targets, timeout.max(Duration::from_secs(3))).await;
        if !gone {
            let entries =
                tokio::task::spawn_blocking(|| procs::snapshot_os(procs::ReadOpts::default()))
                    .await
                    .unwrap_or_default();
            let alive = |p, s| procs::liveness(p, s) == Liveness::Alive;
            left = left_after_force(&targets, &entries, &errors, &ended, &alive, &osproc::facts);
            for l in &left {
                tracing::warn!(process = %l.describe(), "still running after it was ended");
            }
        }
        if procs::liveness(listed.pid, started) == Liveness::Gone {
            StopOutcome::Killed
        } else {
            StopOutcome::StillRunning
        }
    } else if tui {
        stop_tui(&me, timeout).await
    } else {
        procs::terminate(listed.pid);
        if wait_gone(&me, timeout).await {
            StopOutcome::Exited
        } else {
            StopOutcome::StillRunning
        }
    };
    if outcome.is_gone()
        && !restored
        && let Some(tty) = tty
    {
        // Could not be written before: at least leave the alternate screen now.
        let _ = tokio::task::spawn_blocking(move || restore_terminal(&tty)).await;
    }
    let mut done = report(outcome);
    done.left = left;
    Ok(done)
}
