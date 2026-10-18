//! The router's background supervisor never keeps the caller's output open
//! (user bug 54 (c)): `spb router install | Out-String` hung on Windows because
//! the supervisor inherited `spb`'s stdout pipe handle.
//!
//! The test binary plays all three parts: the test spawns it as "parent" with
//! piped stdout and stderr; the parent starts a "sleeper" (the same binary) with
//! [`SystemRunner::spawn_detached`] — the call `router install` makes — prints
//! its pid and exits; both pipes must reach end-of-file at once, long before the
//! sleeper ends.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use superba_router::service::{CommandRunner, SystemRunner};

const ROLE: &str = "SUPERBA_DETACH_TEST_ROLE";
const LOG: &str = "SUPERBA_DETACH_TEST_LOG";
const SLEEP: Duration = Duration::from_secs(40);

fn me() -> PathBuf {
    std::env::current_exe().unwrap()
}

/// The parent and sleeper parts; a no-op when run as a normal test.
#[test]
fn detach_helper() {
    match std::env::var(ROLE).as_deref() {
        Ok("parent") => {
            let log = PathBuf::from(std::env::var(LOG).unwrap());
            let pid = SystemRunner
                .spawn_detached(
                    &me(),
                    &[
                        "--exact".into(),
                        "detach_helper".into(),
                        "--nocapture".into(),
                        "--test-threads=1".into(),
                    ],
                    &[(ROLE.to_owned(), "sleeper".to_owned())],
                    &log,
                )
                .unwrap();
            println!("sleeper-pid={pid}");
            eprintln!("parent done");
        }
        Ok("sleeper") => {
            #[cfg(unix)]
            {
                // SAFETY: getsid(0) / getpid() have no preconditions.
                let (sid, pid) = unsafe { (libc::getsid(0), libc::getpid()) };
                println!("sleeper sid={sid} pid={pid}");
            }
            println!("sleeper started");
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

/// The digits right after `marker` in `text` (libtest prints our lines after
/// its own `test … ...` prefix).
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
fn a_detached_child_leaves_the_callers_captured_output() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("router.log");
    let started = Instant::now();
    let mut parent = Command::new(me())
        .args([
            "--exact",
            "detach_helper",
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
        .and_then(|o| after(o, "sleeper-pid="))
        .and_then(|p| p.parse().ok());
    // Wait for the sleeper's first lines before it is stopped.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut log_text = String::new();
    while Instant::now() < deadline {
        log_text = std::fs::read_to_string(&log).unwrap_or_default();
        if log_text.contains("sleeper started") {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if let Some(pid) = pid {
        kill(pid);
    }
    let out = out.expect("the parent's stdout stayed open: the detached child inherited it");
    let err = err.expect("the parent's stderr stayed open: the detached child inherited it");
    assert!(status.success(), "parent failed: {out}\n{err}");
    assert!(pid.is_some(), "no sleeper pid in {out:?}");
    assert!(err.contains("parent done"), "{err}");
    assert!(
        started.elapsed() < SLEEP,
        "the pipes closed only when the child ended"
    );
    // The child's own output goes to the log.
    assert!(log_text.contains("sleeper started"), "log: {log_text:?}");
    #[cfg(unix)]
    {
        // Its own session (setsid): no controlling terminal of ours.
        let sid = after(&log_text, "sleeper sid=");
        let pid = after(&log_text, " pid=");
        assert!(sid.is_some() && sid == pid, "log: {log_text:?}");
    }
}
