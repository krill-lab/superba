//! Running POSIX scripts on a remote host through the system OpenSSH client.
//!
//! Every remote operation is one `ssh <endpoint> sh -s` invocation whose script
//! (and any secret it carries, e.g. a key inside a new `auth.json`) travels on
//! stdin, never on a command line. Invocation mirrors the desktop app (plan 8.3):
//! the alias alone when one is known (so `~/.ssh/config` applies), otherwise
//! `-i <identity> -o IdentitiesOnly=yes -p <port> -l <user> <host>`. `BatchMode`
//! and a connect timeout are always set; host key policy is never touched.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use superba_core::{Error, RemoteHost, Result, SshEndpoint};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// `ConnectTimeout` passed to ssh (seconds), as in the desktop app (plan 8.3).
pub const CONNECT_TIMEOUT_SECS: u32 = 8;

/// Most stdout bytes accepted from one remote script (hex doubles file sizes; a
/// probe reads at most 15 files of `script::MAX_READ_BYTES`).
pub const MAX_STDOUT_BYTES: u64 = 96 * 1024 * 1024;

/// Most stderr bytes accepted from one remote script.
pub const MAX_STDERR_BYTES: u64 = 1024 * 1024;

/// Raw result of one remote script run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScriptOutput {
    /// Exit status of the local `ssh` process (`None` if killed by a signal).
    /// OpenSSH reports its own failures as 255; otherwise it is the script's status.
    pub status: Option<i32>,
    /// Remote stdout (may contain noise printed by the user's shell startup files;
    /// parsers only accept nonce-tagged records).
    pub stdout: Vec<u8>,
    /// Remote stderr / ssh diagnostics (lossy UTF-8).
    pub stderr: String,
}

impl ScriptOutput {
    /// True if ssh itself failed (could not connect / authenticate).
    pub fn is_ssh_failure(&self) -> bool {
        self.status == Some(255)
    }
}

/// Runs a POSIX `sh` script on a host. The production implementation is
/// [`SystemSsh`]; tests and the e2e harness may run scripts locally under an
/// isolated `HOME` with [`LocalShell`].
#[async_trait]
pub trait RemoteShell: Send + Sync + fmt::Debug {
    /// Run `script` with `sh -s` on `host`, feeding it on stdin. Gives up (and kills
    /// the client) after `timeout`, returning [`Error::Timeout`].
    async fn run(&self, host: &RemoteHost, script: &str, timeout: Duration)
    -> Result<ScriptOutput>;

    /// Run `script` on `host` and stream the bytes of the local file `payload` to
    /// it after the script, on the same stdin (v0.3 Codex installs: a package is
    /// never put on a command line or written to a local temporary copy). The
    /// remote command is [`PAYLOAD_BOOTSTRAP`]: it reads the announced number of
    /// script bytes exactly, so no shell can read ahead into the payload, and
    /// leaves the rest of stdin to the script (see
    /// `script::wrap_with_payload`).
    async fn run_with_payload(
        &self,
        host: &RemoteHost,
        script: &str,
        payload: &Path,
        timeout: Duration,
    ) -> Result<ScriptOutput> {
        let _ = (host, script, payload, timeout);
        Err(Error::not_implemented("RemoteShell::run_with_payload"))
    }
}

/// Remote command of [`RemoteShell::run_with_payload`] (run with `sh -c`): read
/// one line with the script's length, then exactly that many bytes with `dd
/// bs=1` (never more), and evaluate them; the rest of stdin is the payload.
/// `sh -s` cannot be used: dash reads its script from a pipe in blocks and would
/// swallow the start of the payload.
pub const PAYLOAD_BOOTSTRAP: &str =
    r#"IFS= read -r sb_len && sb_src=$(dd bs=1 count="$sb_len" 2>/dev/null) && eval "$sb_src""#;

/// The stdin of a payload run: `<script length>\n<script><payload bytes>`.
pub fn payload_prefix(script: &str) -> Vec<u8> {
    let mut v = format!("{}\n", script.len()).into_bytes();
    v.extend_from_slice(script.as_bytes());
    v
}

/// [`RemoteShell`] over the system OpenSSH client.
#[derive(Clone, Debug)]
pub struct SystemSsh {
    program: PathBuf,
    connect_timeout_secs: u32,
    extra_args: Vec<String>,
}

impl Default for SystemSsh {
    fn default() -> Self {
        Self {
            program: default_ssh_program(),
            connect_timeout_secs: CONNECT_TIMEOUT_SECS,
            extra_args: Vec::new(),
        }
    }
}

impl SystemSsh {
    /// Use a specific ssh executable.
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            ..Self::default()
        }
    }

    /// The ssh executable used.
    pub fn program(&self) -> &std::path::Path {
        &self.program
    }

    /// Options placed before Superba's own (e.g. `-F <file>` so an isolated test
    /// harness never reads the real `~/.ssh/config` or `known_hosts`). Never used
    /// for host key policy in production.
    pub fn with_extra_args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.extra_args = args.into_iter().map(Into::into).collect();
        self
    }
}

/// The ssh client to run: `ssh` from `PATH`; on Windows the inbox OpenSSH client
/// (`%SystemRoot%\System32\OpenSSH\ssh.exe`) when present, because GUI processes
/// often have a `PATH` without it.
pub fn default_ssh_program() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        // Windows only: the inbox client lives outside many GUI PATHs.
        if let Some(root) = std::env::var_os("SystemRoot") {
            let p = PathBuf::from(root)
                .join("System32")
                .join("OpenSSH")
                .join("ssh.exe");
            if p.is_file() {
                return p;
            }
        }
        // Resolve PATH explicitly: Windows' bare-name search also checks the
        // directory containing superba.exe, which may be a portable installation.
        if let Some(paths) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&paths).filter(|p| !p.as_os_str().is_empty()) {
                if let Ok(path) = std::path::absolute(dir.join("ssh.exe"))
                    && path.is_file()
                {
                    return path;
                }
            }
        }
        PathBuf::from("ssh.exe")
    }
    #[cfg(not(target_os = "windows"))]
    {
        PathBuf::from("ssh")
    }
}

/// True if `s` is usable as an ssh destination (alias or host name): ASCII
/// letters, digits and `. - _ : % @` only, not starting with `-`. Shell
/// metacharacters are refused outright because OpenSSH clients before 9.6 could pass
/// the host name (`%h`) unfiltered to a `ProxyCommand` shell.
fn safe_host_token(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '%' | '@'))
}

/// True if `s` is usable as an ssh login name.
fn safe_user_token(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '@'))
}

/// Build the ssh argument vector (without the program) for running `sh -s` on
/// `endpoint`. Pure; validated so a hostile alias or host cannot inject options.
pub fn ssh_args(endpoint: &SshEndpoint, connect_timeout_secs: u32) -> Result<Vec<String>> {
    ssh_args_for(endpoint, connect_timeout_secs, &["sh", "-s"])
}

/// [`ssh_args`] running [`PAYLOAD_BOOTSTRAP`] with `sh -c` instead of `sh -s`.
/// The remote login shell joins the words and parses them: the bootstrap is a
/// constant without `'` or `!`, so single quotes work in POSIX shells, fish,
/// csh/tcsh and nushell alike.
pub fn ssh_payload_args(endpoint: &SshEndpoint, connect_timeout_secs: u32) -> Result<Vec<String>> {
    let quoted = format!("'{PAYLOAD_BOOTSTRAP}'");
    ssh_args_for(endpoint, connect_timeout_secs, &["sh", "-c", &quoted])
}

fn ssh_args_for(
    endpoint: &SshEndpoint,
    connect_timeout_secs: u32,
    remote: &[&str],
) -> Result<Vec<String>> {
    let mut args: Vec<String> = vec![
        "-T".into(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        format!("ConnectTimeout={connect_timeout_secs}"),
        // Detect a dead link instead of hanging until our own timeout.
        "-o".into(),
        "ServerAliveInterval=5".into(),
        "-o".into(),
        "ServerAliveCountMax=3".into(),
    ];
    let alias = endpoint
        .alias
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty());
    let dest = if let Some(alias) = alias {
        if !safe_host_token(alias) {
            return Err(Error::invalid(format!("unsafe ssh alias {alias:?}")));
        }
        alias.to_owned()
    } else {
        let host = endpoint
            .host
            .as_deref()
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .ok_or_else(|| {
                Error::invalid("remote host has neither an ssh alias nor a host name")
            })?;
        if !safe_host_token(host) {
            return Err(Error::invalid(format!("unsafe ssh host {host:?}")));
        }
        if let Some(identity) = &endpoint.identity {
            let id = identity.to_string_lossy();
            if id.trim().is_empty() || id.starts_with('-') || id.chars().any(char::is_control) {
                return Err(Error::invalid(format!("unsafe identity path {id:?}")));
            }
            args.extend([
                "-i".into(),
                id.into_owned(),
                "-o".into(),
                "IdentitiesOnly=yes".into(),
            ]);
        }
        if let Some(port) = endpoint.port {
            args.extend(["-p".into(), port.to_string()]);
        }
        if let Some(user) = endpoint
            .user
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
        {
            if !safe_user_token(user) {
                return Err(Error::invalid(format!("unsafe ssh user {user:?}")));
            }
            args.extend(["-l".into(), user.to_owned()]);
        }
        host.to_owned()
    };
    args.push("--".into());
    args.push(dest);
    // The remote login shell runs this command; the script arrives on stdin.
    args.extend(remote.iter().map(|w| (*w).to_owned()));
    Ok(args)
}

/// Turn ssh's stderr after a failed connection (exit 255) into a short, actionable
/// message (plan 8.6 failure table). Never contains credentials.
pub fn classify_ssh_failure(stderr: &str) -> String {
    let lower = stderr.to_ascii_lowercase();
    let detail = last_meaningful_line(stderr);
    let hint = if lower.contains("host key verification failed")
        || lower.contains("remote host identification has changed")
        || lower.contains("no ed25519 host key is known")
        || lower.contains("host key is known")
    {
        "the host key is unknown or has changed; connect once from a terminal to review and accept it"
    } else if lower.contains("permission denied")
        || lower.contains("too many authentication failures")
    {
        "authentication failed in batch mode (no password prompts); load the key into ssh-agent or reference it in ~/.ssh/config (on Windows, enable the OpenSSH Authentication Agent service)"
    } else if lower.contains("could not resolve hostname")
        || lower.contains("name or service not known")
    {
        "the host name cannot be resolved"
    } else if lower.contains("connection timed out") || lower.contains("operation timed out") {
        "the connection timed out (host offline or blocked)"
    } else if lower.contains("connection refused") {
        "the connection was refused (no ssh server on that port)"
    } else if lower.contains("no route to host") || lower.contains("network is unreachable") {
        "the host is unreachable"
    } else if lower.contains("connection closed") || lower.contains("connection reset") {
        "the server closed the connection"
    } else {
        "ssh failed"
    };
    if detail.is_empty() {
        hint.to_owned()
    } else {
        format!("{hint} ({detail})")
    }
}

fn last_meaningful_line(stderr: &str) -> String {
    stderr
        .lines()
        .map(str::trim)
        .rfind(|l| {
            !l.is_empty() && !l.starts_with("debug") && !l.starts_with("Warning: Permanently added")
        })
        .map(|l| l.chars().take(200).collect())
        .unwrap_or_default()
}

#[async_trait]
impl RemoteShell for SystemSsh {
    async fn run(
        &self,
        host: &RemoteHost,
        script: &str,
        timeout: Duration,
    ) -> Result<ScriptOutput> {
        let args = ssh_args(&host.ssh, self.connect_timeout_secs)?;
        let mut cmd = tokio::process::Command::new(&self.program);
        cmd.args(&self.extra_args).args(&args);
        no_console_window(&mut cmd);
        let out = run_with_stdin(cmd, script.as_bytes(), timeout, &host.label).await?;
        if out.is_ssh_failure() {
            return Err(Error::Ssh {
                host: host.label.clone(),
                message: classify_ssh_failure(&out.stderr),
            });
        }
        Ok(out)
    }

    async fn run_with_payload(
        &self,
        host: &RemoteHost,
        script: &str,
        payload: &Path,
        timeout: Duration,
    ) -> Result<ScriptOutput> {
        let args = ssh_payload_args(&host.ssh, self.connect_timeout_secs)?;
        let mut cmd = tokio::process::Command::new(&self.program);
        cmd.args(&self.extra_args).args(&args);
        no_console_window(&mut cmd);
        let input = Input {
            prefix: payload_prefix(script),
            file: Some(payload.to_path_buf()),
        };
        let out = run_with_input(cmd, input, timeout, &host.label).await?;
        if out.is_ssh_failure() {
            return Err(Error::Ssh {
                host: host.label.clone(),
                message: classify_ssh_failure(&out.stderr),
            });
        }
        Ok(out)
    }
}

/// Windows only: keep the GUI from flashing a console window for each ssh call.
#[cfg(target_os = "windows")]
fn no_console_window(cmd: &mut tokio::process::Command) {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn no_console_window(_cmd: &mut tokio::process::Command) {}

/// What a run writes to the child's stdin: `prefix`, then the bytes of `file`.
#[derive(Debug, Default)]
pub(crate) struct Input {
    prefix: Vec<u8>,
    file: Option<PathBuf>,
}

/// Spawn `cmd`, write `input` to its stdin (then close it), collect stdout and
/// stderr, and kill it after `timeout`.
pub(crate) async fn run_with_stdin(
    cmd: tokio::process::Command,
    input: &[u8],
    timeout: Duration,
    label: &str,
) -> Result<ScriptOutput> {
    let input = Input {
        prefix: input.to_vec(),
        file: None,
    };
    run_with_input(cmd, input, timeout, label).await
}

/// [`run_with_stdin`] with a streamed file after the prefix. A file that cannot be
/// opened fails the run before anything starts.
pub(crate) async fn run_with_input(
    mut cmd: tokio::process::Command,
    input: Input,
    timeout: Duration,
    label: &str,
) -> Result<ScriptOutput> {
    let file = match &input.file {
        Some(p) => Some(
            tokio::fs::File::open(p)
                .await
                .map_err(|e| Error::io(p, &e))?,
        ),
        None => None,
    };
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| Error::Ssh {
        host: label.to_owned(),
        message: format!("cannot start the ssh client: {e}"),
    })?;
    let mut stdin = child.stdin.take();
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let Input { prefix, .. } = input;
    let mut file = file;
    let too_much = |what: &str, limit: u64| Error::Ssh {
        host: label.to_owned(),
        message: format!("the host sent more than {limit} bytes on {what}; aborted"),
    };
    let work = async move {
        let write = async {
            if let Some(mut s) = stdin.take() {
                // A remote that exits early closes the pipe; that is not our error
                // (its result records say why it stopped).
                if s.write_all(&prefix).await.is_ok()
                    && let Some(f) = file.as_mut()
                {
                    let _ = tokio::io::copy(f, &mut s).await;
                }
                let _ = s.shutdown().await;
            }
            Ok::<(), Error>(())
        };
        // Bounded reads: a hostile or broken host cannot exhaust local memory; the
        // first overflow aborts everything (the child is killed on drop).
        let read_out = async {
            let mut buf = Vec::new();
            if let Some(s) = stdout.as_mut() {
                let _ = s.take(MAX_STDOUT_BYTES + 1).read_to_end(&mut buf).await;
            }
            if buf.len() as u64 > MAX_STDOUT_BYTES {
                return Err(too_much("stdout", MAX_STDOUT_BYTES));
            }
            Ok(buf)
        };
        let read_err = async {
            let mut buf = Vec::new();
            if let Some(s) = stderr.as_mut() {
                let _ = s.take(MAX_STDERR_BYTES + 1).read_to_end(&mut buf).await;
            }
            if buf.len() as u64 > MAX_STDERR_BYTES {
                return Err(too_much("stderr", MAX_STDERR_BYTES));
            }
            Ok(buf)
        };
        let ((), out, err) = tokio::try_join!(write, read_out, read_err)?;
        let status = child.wait().await;
        Ok::<_, Error>((status, out, err))
    };
    match tokio::time::timeout(timeout, work).await {
        Ok(Err(e)) => Err(e),
        Ok(Ok((status, stdout, stderr))) => {
            let status = status.map_err(|e| Error::Ssh {
                host: label.to_owned(),
                message: format!("waiting for ssh failed: {e}"),
            })?;
            Ok(ScriptOutput {
                status: status.code(),
                stdout,
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
            })
        }
        // Dropping the future drops the child, which is killed (kill_on_drop).
        Err(_) => Err(Error::Timeout {
            what: format!("ssh {label} ({} ms)", timeout.as_millis()),
        }),
    }
}

/// [`RemoteShell`] that runs scripts with the local `/bin/sh` under an injected
/// `HOME` (and optional extra environment), ignoring the host's ssh endpoint.
///
/// For tests and the e2e harness only: it lets the exact scripts Superba sends to
/// remote hosts run against a throwaway directory, without ssh and without ever
/// touching the real user's home.
#[derive(Clone, Debug)]
pub struct LocalShell {
    home: PathBuf,
    shell: Option<String>,
    env: Vec<(String, String)>,
}

impl LocalShell {
    /// Run scripts with `HOME=<home>`; `SHELL` is cleared (no login shell) unless
    /// set with [`LocalShell::with_login_shell`].
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self {
            home: home.into(),
            shell: None,
            env: Vec::new(),
        }
    }

    /// Set `SHELL` for the scripts (e.g. `/bin/sh` or `/bin/bash`).
    pub fn with_login_shell(mut self, shell: impl Into<String>) -> Self {
        self.shell = Some(shell.into());
        self
    }

    /// Add an environment variable.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }
}

#[async_trait]
impl RemoteShell for LocalShell {
    async fn run(
        &self,
        host: &RemoteHost,
        script: &str,
        timeout: Duration,
    ) -> Result<ScriptOutput> {
        let mut cmd = self.command();
        cmd.arg("-s");
        run_with_stdin(cmd, script.as_bytes(), timeout, &host.label).await
    }

    async fn run_with_payload(
        &self,
        host: &RemoteHost,
        script: &str,
        payload: &Path,
        timeout: Duration,
    ) -> Result<ScriptOutput> {
        let mut cmd = self.command();
        cmd.arg("-c").arg(PAYLOAD_BOOTSTRAP);
        let input = Input {
            prefix: payload_prefix(script),
            file: Some(payload.to_path_buf()),
        };
        run_with_input(cmd, input, timeout, &host.label).await
    }
}

impl LocalShell {
    fn command(&self) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.env_clear();
        cmd.env("HOME", &self.home);
        cmd.env("PATH", "/usr/local/bin:/usr/bin:/bin");
        cmd.env("LC_ALL", "C");
        if let Some(shell) = &self.shell {
            cmd.env("SHELL", shell);
        }
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd
    }
}
