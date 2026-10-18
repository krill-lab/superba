//! A directly started desktop app keeps none of Superba's handles open (the
//! class of user bug 54 (c)): `std::process::Command` lets a Windows child
//! inherit every inheritable handle of ours — the pipes a captured `spb`
//! writes to, log files — and the app would keep them open for as long as it
//! runs (a `spb launch | Out-String` never returning, files that cannot be
//! replaced). Not packaged installs are started like the router supervisor,
//! with an explicit handle list.
//!
//! The test binary plays all three parts: the test starts it as "parent" with
//! piped stdout and stderr; the parent launches the "app" (the same binary)
//! through `launch::spawn` — the call `DesktopAppController::launch` makes for
//! an install that is not an MSIX package — prints its pid and exits; both
//! pipes must reach end-of-file at once, long before the "app" ends.
//!
//! macOS launches through LaunchServices (`open`), which never shares our
//! descriptors; the test runs on Linux and Windows.

#![cfg(not(target_os = "macos"))]
#![allow(missing_docs)]

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use superba_appctl::launch::{CdpPlan, LaunchSpec, spawn};
use superba_core::{AppInstall, AppVariant};

const ROLE: &str = "SUPERBA_LAUNCH_HANDLES_ROLE";
const LOG: &str = "SUPERBA_LAUNCH_HANDLES_LOG";
const SLEEP: Duration = Duration::from_secs(40);

fn me() -> PathBuf {
    std::env::current_exe().unwrap()
}

/// The parent and "app" parts; a no-op when run as a normal test.
#[test]
fn launch_helper() {
    match std::env::var(ROLE).as_deref() {
        Ok("parent") => {
            let log = PathBuf::from(std::env::var(LOG).unwrap());
            let spec = LaunchSpec {
                program: me(),
                args: vec![
                    "--exact".into(),
                    "launch_helper".into(),
                    "--nocapture".into(),
                    "--test-threads=1".into(),
                ],
                env_set: BTreeMap::from([(ROLE.to_owned(), "app".to_owned())]),
                env_remove: vec![LOG.to_owned()],
                cdp: CdpPlan::Disabled,
            };
            let install = AppInstall {
                id: "path:launch-handles-test".into(),
                variant: AppVariant::ChatGpt,
                path: me().parent().unwrap().to_path_buf(),
                executable: me(),
                app_version: None,
                bundled_codex: None,
                bundled_codex_version: None,
            };
            let spawned = spawn(&spec, &install, Some(&log)).unwrap();
            println!("app-pid={}", spawned.pid.unwrap());
            eprintln!("parent done");
        }
        Ok("app") => {
            println!(
                "app started, log var removed: {}",
                std::env::var_os(LOG).is_none()
            );
            std::thread::sleep(SLEEP);
        }
        _ => {}
    }
}

fn read_to_end(pipe: impl Read + Send + 'static) -> std::sync::mpsc::Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut pipe = pipe;
        let mut out = String::new();
        let _ = pipe.read_to_string(&mut out);
        let _ = tx.send(out);
    });
    rx
}

/// The digits right after `marker` in `text`.
fn after<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
    let rest = &text[text.find(marker)? + marker.len()..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    (end > 0).then(|| &rest[..end])
}

fn kill(pid: u32) {
    #[cfg(unix)]
    let _ = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status();
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[test]
fn a_directly_started_app_leaves_the_callers_captured_output() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("logs").join("app-launch.log");
    let started = Instant::now();
    let mut parent = Command::new(me())
        .args([
            "--exact",
            "launch_helper",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ROLE, "parent")
        .env(LOG, &log)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = read_to_end(parent.stdout.take().unwrap());
    let stderr = read_to_end(parent.stderr.take().unwrap());
    let limit = Duration::from_secs(20);
    let out = stdout.recv_timeout(limit);
    let err = stderr.recv_timeout(limit.saturating_sub(started.elapsed()));
    let status = parent.wait().unwrap();
    let pid: Option<u32> = out
        .as_ref()
        .ok()
        .and_then(|o| after(o, "app-pid="))
        .and_then(|p| p.parse().ok());
    // The "app" writes its first line to the launch log.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut log_text = String::new();
    while Instant::now() < deadline {
        log_text = std::fs::read_to_string(&log).unwrap_or_default();
        if log_text.contains("app started") {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if let Some(pid) = pid {
        kill(pid);
    }
    let out = out.expect("the parent's stdout stayed open: the app inherited it");
    let err = err.expect("the parent's stderr stayed open: the app inherited it");
    assert!(status.success(), "parent failed: {out}\n{err}");
    assert!(pid.is_some(), "no app pid in {out:?}");
    assert!(err.contains("parent done"), "{err}");
    assert!(
        started.elapsed() < SLEEP,
        "the pipes closed only when the app ended"
    );
    // The app's output goes to the launch log, with the launch's environment.
    assert!(
        log_text.contains("app started, log var removed: true"),
        "log: {log_text:?}"
    );
}
