//! Processes of this user whose program lies in a folder (a desktop app that
//! must be closed before it is removed), and ending them by pid.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// A running process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proc {
    /// Process id.
    pub pid: u32,
    /// Its program.
    pub exe: PathBuf,
    /// Start time (seconds since the epoch).
    pub start: u64,
}

fn norm(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    if cfg!(windows) { s.to_lowercase() } else { s }
}

/// Processes whose program is inside `dir` (or is `dir`). Other users'
/// processes are not readable and so never listed.
pub fn under(dir: &Path) -> Vec<Proc> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
    );
    let root = norm(dir);
    let root = root.trim_end_matches('/');
    sys.processes()
        .values()
        .filter(|p| p.thread_kind().is_none())
        .filter_map(|p| {
            let exe = p.exe()?.to_path_buf();
            let e = norm(&exe);
            (e == root || e.starts_with(&format!("{root}/"))).then(|| Proc {
                pid: p.pid().as_u32(),
                exe,
                start: p.start_time(),
            })
        })
        .collect()
}

/// End `procs` (by pid, only while the pid still runs the same program):
/// a polite request first, then a forced end after `grace`. Returns the
/// processes still running.
pub fn stop(procs: &[Proc], grace: Duration) -> Vec<Proc> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System, UpdateKind};
    if procs.is_empty() {
        return Vec::new();
    }
    let refresh = |sys: &mut System| {
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
        );
    };
    let alive = |sys: &System, p: &Proc| {
        sys.process(Pid::from_u32(p.pid)).is_some_and(|q| {
            q.exe().is_some_and(|e| norm(e) == norm(&p.exe)) && q.start_time() == p.start
        })
    };
    let mut sys = System::new();
    refresh(&mut sys);
    for p in procs {
        if !alive(&sys, p) {
            continue;
        }
        if cfg!(windows) {
            // WM_CLOSE to its windows (taskkill without /F): the app may save.
            polite_windows(p.pid);
        } else if let Some(q) = sys.process(Pid::from_u32(p.pid)) {
            let _ = q.kill_with(Signal::Term);
        }
    }
    let end = std::time::Instant::now() + grace;
    loop {
        std::thread::sleep(Duration::from_millis(250));
        refresh(&mut sys);
        if procs.iter().all(|p| !alive(&sys, p)) || std::time::Instant::now() >= end {
            break;
        }
    }
    for p in procs {
        if alive(&sys, p)
            && let Some(q) = sys.process(Pid::from_u32(p.pid))
        {
            q.kill();
        }
    }
    std::thread::sleep(Duration::from_millis(500));
    refresh(&mut sys);
    procs.iter().filter(|p| alive(&sys, p)).cloned().collect()
}

fn polite_windows(pid: u32) {
    let _ = superba_core::process::command("taskkill")
        .args(["/PID", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}
