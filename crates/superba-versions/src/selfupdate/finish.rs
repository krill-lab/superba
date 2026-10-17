//! `superba --finish-update`: the tiny second stage that runs after the process
//! that downloaded the update has exited.
//!
//! It waits for the old process (`--wait-pid`), optionally swaps staged files
//! into the install directory (Windows: a running `.exe` is replaced from the
//! *new* `superba.exe`, started from the staging directory), restarts the
//! router service through the new `spb` (`--restart-router`), and starts the new
//! Superba (`--relaunch` a path, or `--open-app` a macOS bundle). It does
//! nothing else and every path it touches is named on its command line.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use superba_core::{Error, Result};

use super::apply;
use super::release::Platform;

/// The flag that selects this mode (first argument).
pub const FLAG: &str = "--finish-update";

/// How long the old process may take to exit.
pub const WAIT: Duration = Duration::from_secs(60);

/// What the second stage does.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FinishArgs {
    /// Wait until this process has exited.
    pub wait_pid: Option<u32>,
    /// Swap the binaries found in this directory...
    pub swap_from: Option<PathBuf>,
    /// ...into this one.
    pub swap_to: Option<PathBuf>,
    /// Restart the router service with `<swap-to>/spb self-update restart-router`.
    pub restart_router: bool,
    /// Start this executable at the end.
    pub relaunch: Option<PathBuf>,
    /// `open` this macOS bundle at the end.
    pub open_app: Option<PathBuf>,
    /// Append a line about the outcome to this log file.
    pub log: Option<PathBuf>,
}

/// A second stage to start: the executable and what it does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinishPlan {
    /// The (new) `superba` to run.
    pub exe: PathBuf,
    /// Its work.
    pub args: FinishArgs,
}

impl FinishArgs {
    /// The command line after the program name.
    pub fn to_args(&self) -> Vec<OsString> {
        let mut a: Vec<OsString> = vec![FLAG.into()];
        let mut path = |flag: &str, p: &Option<PathBuf>| {
            if let Some(p) = p {
                a.push(flag.into());
                a.push(p.clone().into_os_string());
            }
        };
        path("--swap-from", &self.swap_from);
        path("--swap-to", &self.swap_to);
        path("--relaunch", &self.relaunch);
        path("--open-app", &self.open_app);
        path("--log", &self.log);
        if let Some(pid) = self.wait_pid {
            a.push("--wait-pid".into());
            a.push(pid.to_string().into());
        }
        if self.restart_router {
            a.push("--restart-router".into());
        }
        a
    }

    /// Parse the command line (`args[0]` is the program name).
    pub fn parse(args: &[OsString]) -> Result<Self> {
        let bad = |m: String| Error::Invalid { message: m };
        let mut out = Self::default();
        let mut it = args.iter().skip(1);
        if it.next().is_none_or(|a| a != FLAG) {
            return Err(bad(format!("expected {FLAG}")));
        }
        while let Some(flag) = it.next() {
            let flag = flag.to_string_lossy().into_owned();
            if flag == "--restart-router" {
                out.restart_router = true;
                continue;
            }
            let value = it
                .next()
                .ok_or_else(|| bad(format!("{flag} needs a value")))?;
            let path = PathBuf::from(value);
            match flag.as_str() {
                "--wait-pid" => {
                    out.wait_pid = Some(
                        value
                            .to_string_lossy()
                            .parse()
                            .map_err(|_| bad("--wait-pid needs a number".into()))?,
                    );
                }
                "--swap-from" => out.swap_from = Some(path),
                "--swap-to" => out.swap_to = Some(path),
                "--relaunch" => out.relaunch = Some(path),
                "--open-app" => out.open_app = Some(path),
                "--log" => out.log = Some(path),
                other => return Err(bad(format!("unknown argument {other}"))),
            }
        }
        if out.swap_from.is_some() != out.swap_to.is_some() {
            return Err(bad("--swap-from and --swap-to go together".into()));
        }
        Ok(out)
    }
}

/// Whether `argv` asks for this mode.
pub fn is_finish_invocation(argv: &[OsString]) -> bool {
    argv.get(1).is_some_and(|a| a == FLAG)
}

/// Whether process `pid` still exists.
pub fn process_alive(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let pid = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    sys.process(pid).is_some()
}

fn log_line(args: &FinishArgs, line: &str) {
    if let Some(path) = &args.log
        && let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    {
        let _ = std::io::Write::write_all(&mut f, format!("{line}\n").as_bytes());
    }
}

/// Start the second stage detached from this process.
pub fn spawn(plan: &FinishPlan) -> Result<()> {
    let mut cmd = superba_core::process::command(&plan.exe);
    cmd.args(plan.args.to_args())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach(&mut cmd);
    cmd.spawn().map(drop).map_err(|e| Error::Process {
        message: format!("could not start {}: {e}", plan.exe.display()),
    })
}

#[cfg(unix)]
fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(windows)]
fn detach(cmd: &mut Command) {
    // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP (CREATE_NO_WINDOW is ignored together
    // with DETACHED_PROCESS, and a detached child has no console anyway).
    superba_core::process::detach(cmd);
}

#[cfg(not(any(unix, windows)))]
fn detach(_cmd: &mut Command) {}

/// Run the second stage.
pub fn run(args: &FinishArgs) -> Result<()> {
    if let Some(pid) = args.wait_pid {
        let started = Instant::now();
        while process_alive(pid) {
            if started.elapsed() > WAIT {
                let e = Error::Timeout {
                    what: format!("process {pid} did not exit"),
                };
                log_line(args, &format!("finish-update: {e}"));
                return Err(e);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let platform = Platform::current();
    if let (Some(from), Some(to)) = (&args.swap_from, &args.swap_to) {
        let pairs: Vec<(PathBuf, PathBuf)> = apply::BINARIES
            .iter()
            .map(|b| apply::bin_name(b, platform.os))
            .filter(|n| from.join(n).is_file() && to.join(n).is_file())
            .map(|n| (from.join(&n), to.join(&n)))
            .collect();
        match apply::replace_files(&pairs) {
            Ok(names) => log_line(args, &format!("finish-update: replaced {names:?}")),
            Err(e) => {
                log_line(args, &format!("finish-update: swap failed: {e}"));
                // The old version is back in place: start it again.
                relaunch(args);
                return Err(e);
            }
        }
        apply::remove_leftovers(to);
    }
    if args.restart_router {
        let dir = args.swap_to.clone().or_else(|| {
            args.relaunch
                .as_deref()
                .and_then(Path::parent)
                .map(Path::to_owned)
        });
        if let Some(dir) = dir {
            let spb = dir.join(apply::bin_name("spb", platform.os));
            let status = superba_core::process::command(&spb)
                .args(["self-update", "restart-router"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            log_line(args, &format!("finish-update: restart-router {status:?}"));
        }
    }
    relaunch(args);
    Ok(())
}

fn relaunch(args: &FinishArgs) {
    let mut cmd = if let Some(app) = &args.open_app {
        let mut c = superba_core::process::command("open");
        c.arg(app);
        c
    } else if let Some(exe) = &args.relaunch {
        superba_core::process::command(exe)
    } else {
        return;
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach(&mut cmd);
    match cmd.spawn() {
        Ok(_) => log_line(args, "finish-update: started the new version"),
        Err(e) => log_line(args, &format!("finish-update: could not start it: {e}")),
    }
}
