//! The CLI app-server daemon (`$CODEX_HOME/app-server-control`, plan 7.7, spike S8).
//!
//! Codex 0.162 ships `codex app-server daemon {start,restart,stop,version,...}`;
//! support is detected from the help output of the binary that will run it (the
//! first standalone `codex` on PATH; the app-bundled binary refuses daemon
//! management). Commands run with `CODEX_HOME` set to the target home.

use std::path::Path;
use std::time::Duration;

use superba_core::{CodexHome, Error, Result};

/// Daemon subcommands listed in `codex app-server daemon --help`.
pub fn parse_daemon_commands(help: &str) -> Vec<String> {
    let mut in_commands = false;
    let mut out = Vec::new();
    for line in help.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim().eq_ignore_ascii_case("commands:") {
            in_commands = true;
            continue;
        }
        if in_commands {
            if trimmed.is_empty() || !line.starts_with(' ') {
                if !trimmed.is_empty() {
                    in_commands = false;
                }
                continue;
            }
            if let Some(word) = trimmed.split_whitespace().next()
                && word.chars().all(|c| c.is_ascii_lowercase() || c == '-')
                && line.len() - line.trim_start().len() <= 4
            {
                out.push(word.to_owned());
            }
        }
    }
    out
}

async fn run(
    binary: &Path,
    args: &[&str],
    home: &CodexHome,
    timeout: Duration,
) -> Result<std::process::Output> {
    let mut cmd = tokio::process::Command::new(binary);
    cmd.args(args)
        .env(superba_core::codex::ENV_CODEX_HOME, home.root())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if home.sqlite_home() != home.root() {
        cmd.env(
            superba_core::codex::ENV_CODEX_SQLITE_HOME,
            home.sqlite_home(),
        );
    }
    let child = cmd.spawn().map_err(|e| Error::Process {
        message: format!("cannot run {}: {e}", binary.display()),
    })?;
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => Ok(out),
        Ok(Err(e)) => Err(Error::Process {
            message: format!("{}: {e}", binary.display()),
        }),
        Err(_) => Err(Error::Timeout {
            what: format!("{} {}", binary.display(), args.join(" ")),
        }),
    }
}

/// Subcommands the binary supports (empty if `daemon` is unknown).
pub async fn supported_commands(binary: &Path, home: &CodexHome) -> Vec<String> {
    match run(
        binary,
        &["app-server", "daemon", "--help"],
        home,
        Duration::from_secs(10),
    )
    .await
    {
        Ok(out) if out.status.success() => {
            parse_daemon_commands(&String::from_utf8_lossy(&out.stdout))
        }
        _ => Vec::new(),
    }
}

/// Run `codex app-server daemon <command>` for `home`.
pub async fn daemon_command(binary: &Path, command: &str, home: &CodexHome) -> Result<()> {
    let supported = supported_commands(binary, home).await;
    if !supported.iter().any(|c| c == command) {
        return Err(Error::Unsupported {
            what: format!(
                "`codex app-server daemon {command}` (not offered by {})",
                binary.display()
            ),
        });
    }
    let out = run(
        binary,
        &["app-server", "daemon", command],
        home,
        Duration::from_secs(30),
    )
    .await?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let excerpt: String = stderr.lines().take(3).collect::<Vec<_>>().join(" | ");
    Err(Error::Process {
        message: format!("codex app-server daemon {command} failed: {excerpt}"),
    })
}
