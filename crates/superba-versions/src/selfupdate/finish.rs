//! `superba --finish-update`: the tiny second stage that runs after the process
//! that downloaded the update has exited.
//!
//! It waits for the old process (`--wait-pid`), optionally runs the verified MSI
//! (`--msi`, Windows: `msiexec /i <msi> /passive`, started from a copy of
//! `superba.exe` outside the install folder), restarts the
//! router service through the new `spb` (`--restart-router`), and starts the new
//! Superba (`--relaunch` a path, or `--open-app` a macOS bundle). It does
//! nothing else and every path it touches is named on its command line.
//!
//! Every step goes to the attempt's log (`--log`, [`super::runlog`]), and the
//! outcome to the attempt's record (`--state`, [`super::record`]) before the
//! new version starts, so that version can tell the user when the update
//! failed.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use superba_core::{Error, LocalizedText, Result, Timestamp};

use super::apply;
use super::record::{self, Stage};
use super::release::Platform;
use super::runlog::{self, RunLog};

/// The flag that selects this mode (first argument).
pub const FLAG: &str = "--finish-update";

/// How long the old process may take to exit.
pub const WAIT: Duration = Duration::from_secs(60);

/// What the second stage does.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FinishArgs {
    /// Wait until this process has exited.
    pub wait_pid: Option<u32>,
    /// Install this MSI (`msiexec /i <msi> /passive`, Windows) once the old
    /// process has exited.
    pub msi: Option<PathBuf>,
    /// Restart the router service with `<dir of --relaunch>/spb self-update restart-router`.
    pub restart_router: bool,
    /// Start this executable at the end.
    pub relaunch: Option<PathBuf>,
    /// `open` this macOS bundle at the end.
    pub open_app: Option<PathBuf>,
    /// The attempt's log ([`super::runlog`]): every step is appended.
    pub log: Option<PathBuf>,
    /// The self-update state file whose attempt record gets the outcome.
    pub state: Option<PathBuf>,
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
        path("--msi", &self.msi);
        path("--relaunch", &self.relaunch);
        path("--open-app", &self.open_app);
        path("--log", &self.log);
        path("--state", &self.state);
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
                "--msi" => out.msi = Some(path),
                "--relaunch" => out.relaunch = Some(path),
                "--open-app" => out.open_app = Some(path),
                "--log" => out.log = Some(path),
                "--state" => out.state = Some(path),
                other => return Err(bad(format!("unknown argument {other}"))),
            }
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

/// What a program the second stage ran returned.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ran {
    /// Exit code (`None`: ended by a signal).
    pub code: Option<i32>,
    /// Its stdout and stderr, as text.
    pub output: String,
}

/// The processes the second stage deals with: the real system in
/// [`run`], a script in tests.
pub trait FinishHost {
    /// Whether process `pid` still exists.
    fn alive(&self, pid: u32) -> bool;
    /// Wait a moment before looking again (100 ms).
    fn pause(&self);
    /// Time since the second stage started.
    fn elapsed(&self) -> Duration;
    /// Run `program` with `args` and extra `env`, and wait for it.
    fn run(&self, program: &Path, args: &[OsString], env: &[(&str, &str)]) -> std::io::Result<Ran>;
    /// Start `program` with `args` detached; returns its process id.
    fn start(&self, program: &Path, args: &[OsString]) -> std::io::Result<u32>;
}

/// The real system.
#[derive(Debug)]
pub struct SystemHost {
    started: Instant,
}

impl Default for SystemHost {
    fn default() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl FinishHost for SystemHost {
    fn alive(&self, pid: u32) -> bool {
        process_alive(pid)
    }

    fn pause(&self) {
        std::thread::sleep(Duration::from_millis(100));
    }

    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    fn run(&self, program: &Path, args: &[OsString], env: &[(&str, &str)]) -> std::io::Result<Ran> {
        let mut cmd = superba_core::process::command(program);
        cmd.args(args).stdin(Stdio::null());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output()?;
        let mut output = String::from_utf8_lossy(&out.stdout).into_owned();
        output.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok(Ran {
            code: out.status.code(),
            output,
        })
    }

    fn start(&self, program: &Path, args: &[OsString]) -> std::io::Result<u32> {
        let mut cmd = superba_core::process::command(program);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        detach(&mut cmd);
        cmd.spawn().map(|c| c.id())
    }
}

/// Run the second stage.
pub fn run(args: &FinishArgs) -> Result<()> {
    run_with(args, &SystemHost::default(), WAIT)
}

/// The second stage with `host`, waiting at most `wait` for the old process.
pub fn run_with(args: &FinishArgs, host: &dyn FinishHost, wait: Duration) -> Result<()> {
    let stage = SecondStage { args, host };
    stage.log(format!(
        "second stage started: {}",
        shown_command("superba", &args.to_args())
    ));
    if let Some(pid) = args.wait_pid {
        stage.log(format!(
            "waiting for the old Superba (process {pid}) to exit (at most {} s)",
            wait.as_secs()
        ));
        let started = host.elapsed();
        while host.alive(pid) {
            if host.elapsed().saturating_sub(started) > wait {
                let e = Error::Timeout {
                    what: format!("process {pid} did not exit"),
                };
                stage.log(format!(
                    "FAILED: process {pid} is still running after {} s; nothing was installed",
                    wait.as_secs()
                ));
                let version = stage.target();
                stage.record(
                    Stage::Failed,
                    Some(record::wait_timeout_reason(&version, wait)),
                );
                return Err(e);
            }
            host.pause();
        }
        stage.log(format!(
            "process {pid} has exited (after {:.1} s)",
            host.elapsed().saturating_sub(started).as_secs_f64()
        ));
    }
    let platform = Platform::current();
    let (mut failed, mut problem) = (false, None);
    if let Some(msi) = &args.msi {
        match stage.install_msi(msi) {
            Ok(1641 | 3010) => problem = Some(record::restart_pending_reason()),
            Ok(_) => {}
            Err(reason) => (failed, problem) = (true, Some(reason)),
        }
    }
    if args.restart_router
        && let Some(reason) = stage.restart_router(platform)
        && !failed
    {
        problem = Some(reason);
    }
    let outcome = if failed {
        stage.log("the update FAILED; the installed version was left as it was");
        Stage::Failed
    } else {
        stage.log("the update is done");
        Stage::Done
    };
    stage.record(outcome, problem);
    stage.relaunch();
    Ok(())
}

struct SecondStage<'a> {
    args: &'a FinishArgs,
    host: &'a dyn FinishHost,
}

impl SecondStage<'_> {
    fn log(&self, line: impl AsRef<str>) {
        if let Some(path) = &self.args.log {
            RunLog::resume(path, "finish").line(line);
        }
    }

    /// The version being installed, from the attempt record.
    fn target(&self) -> String {
        self.args
            .state
            .as_deref()
            .and_then(|p| super::state::State::load(p).last_attempt)
            .map(|a| a.to)
            .unwrap_or_else(|| "the new version".into())
    }

    /// Finish the attempt record (keeping a problem recorded earlier when
    /// there is no new one).
    fn record(&self, stage: Stage, problem: Option<LocalizedText>) {
        let Some(path) = &self.args.state else {
            return;
        };
        record::update(path, Timestamp::now(), |a| {
            a.stage = stage;
            if problem.is_some() {
                a.problem = problem;
                a.seen = false;
            }
        });
    }

    /// Windows: run the verified MSI (per-user install, fixed UpgradeCode, so
    /// it upgrades in place) with `msiexec /i <msi> /passive` and wait for it.
    /// The one place the MSI installer is started; the old process has exited
    /// by now. A verbose installer log goes next to the attempt's log.
    /// Returns the (successful) exit code.
    fn install_msi(&self, msi: &Path) -> std::result::Result<i32, LocalizedText> {
        let program = msiexec();
        // msiexec rejects verbatim `\\?\C:\…` paths with 1619 (alpha.8):
        // hand it the plain form.
        let msi = superba_core::paths::plain_path(msi);
        let mut args: Vec<OsString> = vec!["/i".into(), msi.into_os_string(), "/passive".into()];
        if let Some(dir) = self.args.log.as_deref().and_then(Path::parent) {
            args.push("/L*V".into());
            args.push(
                superba_core::paths::plain_path(&dir.join(runlog::MSIEXEC_LOG_FILE))
                    .into_os_string(),
            );
        }
        self.log(format!(
            "running the installer: {}",
            shown_command(&program.to_string_lossy(), &args)
        ));
        match self.host.run(&program, &args, &[]) {
            Ok(ran) => {
                let code = ran.code.unwrap_or(-1);
                let (meaning, installed) = runlog::msiexec_meaning(code);
                self.log(format!("msiexec exit code {code}: {meaning}"));
                if installed {
                    Ok(code)
                } else {
                    Err(record::msiexec_reason(code))
                }
            }
            Err(e) => {
                self.log(format!("FAILED: msiexec could not be started: {e}"));
                Err(record::msiexec_start_reason(&e.to_string()))
            }
        }
    }

    /// Start the router service again with the new `spb`; the reason when it
    /// failed.
    fn restart_router(&self, platform: Platform) -> Option<LocalizedText> {
        let Some(dir) = self.args.relaunch.as_deref().and_then(Path::parent) else {
            self.log("router restart skipped: no install folder to find spb in");
            return None;
        };
        let spb = dir.join(apply::bin_name("spb", platform.os));
        let args: Vec<OsString> = vec!["self-update".into(), "restart-router".into()];
        self.log(format!(
            "restarting the router: {}",
            shown_command(&spb.to_string_lossy(), &args)
        ));
        // The MSI's removal step stopped the router: start it although it is down.
        let env = [(super::msi::ENV_FORCE_ROUTER_RESTART, "1")];
        let detail = match self.host.run(&spb, &args, &env) {
            Ok(ran) if ran.code == Some(0) => {
                self.log(format!("router restart: exit code 0 {}", ran.output.trim()));
                return None;
            }
            Ok(ran) => {
                let code = ran
                    .code
                    .map_or_else(|| "a signal".to_owned(), |c| c.to_string());
                let output = ran.output.trim();
                self.log(format!(
                    "FAILED: router restart ended with exit code {code}: {output}"
                ));
                last_line(output).map_or_else(|| format!("exit code {code}"), str::to_owned)
            }
            Err(e) => {
                self.log(format!(
                    "FAILED: {} could not be started: {e}",
                    spb.display()
                ));
                e.to_string()
            }
        };
        Some(record::router_reason(&detail))
    }

    /// Start the new Superba (or the old one, after a failed MSI).
    fn relaunch(&self) {
        let (program, args): (PathBuf, Vec<OsString>) = if let Some(app) = &self.args.open_app {
            ("open".into(), vec![app.as_os_str().to_owned()])
        } else if let Some(exe) = &self.args.relaunch {
            (exe.clone(), Vec::new())
        } else {
            self.log("not starting Superba (not asked to)");
            return;
        };
        let shown = shown_command(&program.to_string_lossy(), &args);
        match self.host.start(&program, &args) {
            Ok(pid) => self.log(format!("started Superba: {shown} (process {pid})")),
            Err(e) => self.log(format!("FAILED: could not start Superba ({shown}): {e}")),
        }
    }
}

/// `msiexec.exe` of this Windows (never one found first on `PATH`).
fn msiexec() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join("System32").join("msiexec.exe"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("msiexec"))
}

/// The last non-empty line of `text`.
fn last_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).rfind(|l| !l.is_empty())
}

/// A command line for the log (arguments with spaces quoted).
pub fn shown_command(program: &str, args: &[OsString]) -> String {
    let quote = |s: &str| {
        if s.is_empty() || s.contains([' ', '\t']) {
            format!("\"{s}\"")
        } else {
            s.to_owned()
        }
    };
    std::iter::once(quote(program))
        .chain(args.iter().map(|a| quote(&a.to_string_lossy())))
        .collect::<Vec<_>>()
        .join(" ")
}
