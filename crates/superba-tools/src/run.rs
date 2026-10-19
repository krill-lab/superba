//! Running a tool's installer, package manager or `--version`: stdin closed,
//! output read line by line into the log and the progress line, a deadline,
//! and a cancel that ends the whole process tree. No console window on
//! Windows.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use superba_core::{Error, Result, ToolId, ToolProgress, ToolProgressSink, ToolStage};
use tokio::io::{AsyncBufReadExt as _, BufReader};

use crate::log::OpLog;

/// A command to run.
#[derive(Clone, Debug)]
pub struct Cmd {
    /// The program.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<OsString>,
    /// Variables to set.
    pub set: Vec<(String, OsString)>,
    /// Variables to remove.
    pub remove: Vec<String>,
    /// Working directory.
    pub cwd: Option<PathBuf>,
    /// Deadline.
    pub timeout: Duration,
}

impl Cmd {
    /// `program` with no arguments and a 15 s deadline.
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            set: Vec::new(),
            remove: Vec::new(),
            cwd: None,
            timeout: Duration::from_secs(15),
        }
    }

    /// Add arguments.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Set a variable.
    pub fn env(mut self, name: &str, value: impl Into<OsString>) -> Self {
        self.set.push((name.to_owned(), value.into()));
        self
    }

    /// Set the deadline.
    pub fn timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }

    /// Set the working directory.
    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    /// The command line for the log (arguments as given; no secrets are ever
    /// passed on a command line).
    pub fn display(&self) -> String {
        let mut s = quote(&self.program.to_string_lossy());
        for a in &self.args {
            s.push(' ');
            s.push_str(&quote(&a.to_string_lossy()));
        }
        s
    }

    fn command(&self) -> tokio::process::Command {
        // Through the core helper: no console window on Windows.
        let mut c = tokio::process::Command::from(superba_core::process::command(&self.program));
        c.args(&self.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for name in &self.remove {
            c.env_remove(name);
        }
        for (k, v) in &self.set {
            c.env(k, v);
        }
        if let Some(d) = &self.cwd {
            c.current_dir(d);
        }
        #[cfg(unix)]
        {
            // Its own process group, so a cancel ends the children too.
            c.process_group(0);
        }
        c
    }
}

fn quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@,+%\\".contains(c))
    {
        s.to_owned()
    } else {
        format!("\"{}\"", s.replace('"', "\\\""))
    }
}

/// How a command ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// Exit code (`None`: killed by a signal).
    pub code: Option<i32>,
    /// stdout.
    pub stdout: String,
    /// stderr.
    pub stderr: String,
}

impl Outcome {
    /// Exit code 0.
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// The last non-empty lines of stderr, else stdout (for errors).
    pub fn tail(&self, lines: usize) -> String {
        let src = if self.stderr.trim().is_empty() {
            &self.stdout
        } else {
            &self.stderr
        };
        let all: Vec<&str> = src.lines().filter(|l| !l.trim().is_empty()).collect();
        all[all.len().saturating_sub(lines)..].join("\n")
    }
}

/// Run `cmd` quietly (a `--version`): output collected, no log, no progress.
pub async fn output(cmd: &Cmd) -> Result<Outcome> {
    run_inner(cmd, None).await
}

/// Run `cmd` for an operation: every output line goes to `log` and, as the
/// progress detail, to `sink` (stage `stage`); `sink`'s cancel ends it.
pub async fn run(
    cmd: &Cmd,
    log: &OpLog,
    tool: ToolId,
    stage: ToolStage,
    sink: &dyn ToolProgressSink,
) -> Result<Outcome> {
    log.line(format!("run: {}", cmd.display()));
    let out = run_inner(cmd, Some((log, tool, stage, sink))).await;
    match &out {
        Ok(o) => log.line(format!(
            "exit: {}",
            o.code.map_or("signal".into(), |c| c.to_string())
        )),
        Err(e) => log.line(format!("failed: {e}")),
    }
    out
}

type Live<'a> = (&'a OpLog, ToolId, ToolStage, &'a dyn ToolProgressSink);

async fn run_inner(cmd: &Cmd, live: Option<Live<'_>>) -> Result<Outcome> {
    let mut child = cmd.command().spawn().map_err(|e| Error::Process {
        message: format!("cannot start {}: {e}", cmd.program.display()),
    })?;
    let pid = child.id();
    // Split at `\n` and decode each line lossily: a tool that prints in the
    // console's code page must not end the reading early.
    let stdout = child.stdout.take().map(|s| BufReader::new(s).split(b'\n'));
    let stderr = child.stderr.take().map(|s| BufReader::new(s).split(b'\n'));
    let (mut out_lines, mut err_lines) = (stdout, stderr);
    let (mut out, mut err) = (String::new(), String::new());
    let deadline = tokio::time::Instant::now() + cmd.timeout;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    // A child that exited while something it started still holds the pipes
    // open (a background helper): stop reading shortly after.
    let mut exited: Option<(std::process::ExitStatus, tokio::time::Instant)> = None;
    let status = loop {
        tokio::select! {
            line = next(&mut out_lines) => match line {
                Some(l) => take_line(&mut out, &l, live.as_ref()),
                None => out_lines = None,
            },
            line = next(&mut err_lines) => match line {
                Some(l) => take_line(&mut err, &l, live.as_ref()),
                None => err_lines = None,
            },
            status = child.wait(), if out_lines.is_none() && err_lines.is_none() => {
                break status.map_err(|e| Error::Process { message: e.to_string() })?;
            }
            _ = tick.tick() => {
                if exited.is_none()
                    && let Ok(Some(s)) = child.try_wait()
                {
                    exited = Some((s, tokio::time::Instant::now()));
                }
                if let Some((s, at)) = exited
                    && at.elapsed() > Duration::from_secs(3)
                {
                    break s;
                }
                let cancelled = live.as_ref().is_some_and(|l| l.3.cancel_requested());
                if cancelled || tokio::time::Instant::now() >= deadline {
                    kill_tree(&mut child, pid).await;
                    if let Some(l) = &live {
                        l.0.line(if cancelled { "cancelled: process tree ended" } else { "deadline passed: process tree ended" });
                    }
                    return Err(if cancelled {
                        Error::Cancelled
                    } else {
                        Error::Timeout { what: cmd.display() }
                    });
                }
            }
        }
    };
    Ok(Outcome {
        code: status.code(),
        stdout: out,
        stderr: err,
    })
}

async fn next<R: tokio::io::AsyncBufRead + Unpin>(
    lines: &mut Option<tokio::io::Split<R>>,
) -> Option<String> {
    match lines {
        Some(l) => l.next_segment().await.ok().flatten().map(|b| {
            String::from_utf8_lossy(&b)
                .trim_end_matches('\r')
                .to_owned()
        }),
        None => std::future::pending().await,
    }
}

const MAX_CAPTURE: usize = 1 << 20;

fn take_line(buf: &mut String, line: &str, live: Option<&Live<'_>>) {
    let clean = strip_ansi(line);
    if buf.len() < MAX_CAPTURE {
        buf.push_str(&clean);
        buf.push('\n');
    }
    if let Some((log, tool, stage, sink)) = live {
        log.line(format!("| {clean}"));
        let t = clean.trim();
        if !t.is_empty() {
            sink.report(ToolProgress {
                detail: Some(t.chars().take(160).collect()),
                ..ToolProgress::stage(*tool, *stage)
            });
        }
    }
}

/// Remove terminal escape sequences (colours, cursor moves, `\r` redraws).
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if ('@'..='~').contains(&d) {
                        break;
                    }
                }
            } else {
                chars.next();
            }
            continue;
        }
        if c == '\r' {
            out.clear();
            continue;
        }
        if !c.is_control() || c == '\t' {
            out.push(c);
        }
    }
    out
}

/// Start `cmd` without waiting for it (an installer that leaves the app
/// running as its child); its output is discarded.
pub fn spawn(cmd: &Cmd) -> Result<tokio::process::Child> {
    let mut c = cmd.command();
    c.stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(false);
    c.spawn().map_err(|e| Error::Process {
        message: format!("cannot start {}: {e}", cmd.program.display()),
    })
}

/// End a started process and everything it started.
pub async fn end(child: &mut tokio::process::Child) {
    let pid = child.id();
    kill_tree(child, pid).await;
}

async fn kill_tree(child: &mut tokio::process::Child, pid: Option<u32>) {
    if let Some(pid) = pid {
        #[cfg(unix)]
        {
            // The process group started with the child (`process_group(0)`).
            let _ = tokio::process::Command::from(superba_core::process::command("kill"))
                .args(["-KILL", "--", &format!("-{pid}")])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
        #[cfg(windows)]
        {
            let mut c = tokio::process::Command::from(superba_core::process::command("taskkill"));
            c.args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let _ = c.status().await;
        }
    }
    let _ = child.kill().await;
}

/// `name` on the directories of `path_var` (`PATH`), with Windows' `PATHEXT`
/// extensions; the first match.
pub fn which_in(name: &str, path_var: Option<&std::ffi::OsStr>, windows: bool) -> Option<PathBuf> {
    let path_var = path_var?;
    let exts: Vec<String> = if windows && Path::new(name).extension().is_none() {
        vec![".exe".into(), ".cmd".into(), ".bat".into(), ".com".into()]
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for ext in &exts {
            let p = dir.join(format!("{name}{ext}"));
            if is_executable(&p) {
                return Some(p);
            }
        }
    }
    None
}

/// A regular file this user may run (Unix: an execute bit).
pub fn is_executable(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}
