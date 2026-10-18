//! Stopping a codex TUI leaves its terminal usable (user bug 62(d)): the
//! TUI restores nothing when a signal ends it (alternate screen, mouse and
//! keyboard modes, raw mode stayed; zsh then adopts the raw modes), so
//! Superba puts the terminal back before the first signal, and asks with
//! SIGINT, SIGHUP and SIGTERM in turn.
//!
//! The test binary plays the TUI: in a pseudo-terminal of its own (session
//! leader, controlling terminal, like a TUI started from a shell) it switches
//! to raw mode and the alternate screen, optionally ignores SIGINT and SIGHUP
//! (a TUI that handles them by staying), and waits.

#![cfg(unix)]
#![allow(missing_docs)]

use std::ffi::CStr;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use superba_appctl::stop::{self, tty_from_ps};
use superba_core::{CodexProcessKind, EnvObservation, ProcessInfo, StopOutcome};

const ROLE: &str = "SUPERBA_STOP_TUI_ROLE";

/// The fake TUI; a no-op when run as a normal test.
#[test]
fn tui_helper() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    // SAFETY: plain libc calls on our own stdin (the pty) and signal
    // dispositions of this process.
    unsafe {
        if role == "stubborn" {
            libc::signal(libc::SIGINT, libc::SIG_IGN);
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
        }
        let mut t = std::mem::MaybeUninit::<libc::termios>::zeroed();
        assert_eq!(libc::tcgetattr(0, t.as_mut_ptr()), 0);
        let mut t = t.assume_init();
        libc::cfmakeraw(&mut t);
        assert_eq!(libc::tcsetattr(0, libc::TCSANOW, &t), 0);
    }
    let mut out = std::io::stdout();
    out.write_all(b"\x1b[?1049h\x1b[?1000h\x1b[>1uREADY")
        .unwrap();
    out.flush().unwrap();
    std::thread::sleep(Duration::from_secs(60));
}

struct Pty {
    master: OwnedFd,
    slave_path: PathBuf,
}

/// A new pseudo-terminal pair (master side open, the slave's path).
fn open_pty() -> Pty {
    static PTSNAME: Mutex<()> = Mutex::new(());
    let _guard = PTSNAME.lock().unwrap();
    // SAFETY: posix_openpt / grantpt / unlockpt / ptsname on a descriptor we
    // own; ptsname's static buffer is read under the lock and copied.
    unsafe {
        let fd = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        assert!(fd >= 0, "posix_openpt");
        assert_eq!(libc::grantpt(fd), 0);
        assert_eq!(libc::unlockpt(fd), 0);
        let name = libc::ptsname(fd);
        assert!(!name.is_null());
        let slave_path = PathBuf::from(CStr::from_ptr(name).to_string_lossy().into_owned());
        Pty {
            master: OwnedFd::from_raw_fd(fd),
            slave_path,
        }
    }
}

/// Everything the TUI writes to the master side, read on a thread.
fn collect(master: &OwnedFd) -> Arc<Mutex<Vec<u8>>> {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let fd = master.try_clone().unwrap();
    let sink = buf.clone();
    std::thread::spawn(move || {
        let mut f = std::fs::File::from(fd);
        let mut chunk = [0u8; 4096];
        while let Ok(n) = f.read(&mut chunk) {
            if n == 0 {
                break;
            }
            sink.lock().unwrap().extend_from_slice(&chunk[..n]);
        }
    });
    buf
}

fn start_tui(role: &str) -> (Pty, std::process::Child, Arc<Mutex<Vec<u8>>>) {
    let pty = open_pty();
    let slave = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&pty.slave_path)
        .unwrap();
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", "tui_helper", "--nocapture", "--test-threads=1"])
        .env(ROLE, role)
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave));
    // SAFETY: setsid and ioctl are async-signal-safe; they make the pty the
    // child's controlling terminal (a TUI started from a shell has one).
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            #[allow(clippy::useless_conversion)]
            if libc::ioctl(0, libc::TIOCSCTTY.into(), 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn().unwrap();
    let out = collect(&pty.master);
    let deadline = Instant::now() + Duration::from_secs(20);
    while !String::from_utf8_lossy(&out.lock().unwrap()).contains("READY") {
        assert!(Instant::now() < deadline, "the fake TUI did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    (pty, child, out)
}

fn listed(child: &std::process::Child) -> ProcessInfo {
    let entries = superba_appctl::procs::snapshot_os(Default::default());
    let e = entries.iter().find(|e| e.pid == child.id()).unwrap();
    ProcessInfo {
        pid: child.id(),
        ppid: None,
        kind: CodexProcessKind::CliTui,
        exe: e.exe.clone(),
        cmdline: vec!["codex".into()],
        started_at: e.started_at,
        authapi_env: EnvObservation::Unknown,
        codex_home: None,
        stale: false,
        ide: None,
        cwd: None,
    }
}

/// The terminal modes now (the slave side).
fn modes(pty: &Pty) -> libc::termios {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .open(&pty.slave_path)
        .unwrap();
    // SAFETY: tcgetattr fills `t` for an open terminal descriptor.
    unsafe {
        let mut t = std::mem::MaybeUninit::<libc::termios>::zeroed();
        assert_eq!(libc::tcgetattr(f.as_raw_fd(), t.as_mut_ptr()), 0);
        t.assume_init()
    }
}

fn assert_restored(pty: &Pty, out: &Arc<Mutex<Vec<u8>>>) {
    let t = modes(pty);
    assert_ne!(t.c_lflag & libc::ICANON, 0, "canonical input back on");
    assert_ne!(t.c_lflag & libc::ECHO, 0, "echo back on");
    assert_ne!(t.c_lflag & libc::ISIG, 0, "signal keys back on");
    assert_ne!(t.c_oflag & libc::OPOST, 0, "output processing back on");
    assert_ne!(t.c_iflag & libc::ICRNL, 0, "CR to NL back on");
    let written = out.lock().unwrap().clone();
    let after = &written[written.windows(5).position(|w| w == b"READY").unwrap()..];
    let text = String::from_utf8_lossy(after);
    for seq in ["\x1b[?1049l", "\x1b[?1000l", "\x1b[<1u", "\x1b[?25h"] {
        assert!(text.contains(seq), "{seq:?} not written: {text:?}");
    }
}

#[tokio::test]
async fn a_stopped_tui_leaves_its_terminal_cooked_and_on_the_main_screen() {
    let (pty, mut child, out) = start_tui("plain");
    assert_eq!(modes(&pty).c_lflag & libc::ICANON, 0, "the fake TUI is raw");
    assert_eq!(
        stop::terminal_of(child.id()).as_deref(),
        Some(pty.slave_path.as_path()),
        "the TUI's terminal is found"
    );
    let outcome = stop::stop(&listed(&child), false, Duration::from_secs(5))
        .await
        .unwrap();
    let status = child.wait().unwrap();
    assert_eq!(outcome, StopOutcome::Exited);
    // SIGINT first: it ends a TUI without a handler at once.
    assert_eq!(status.signal(), Some(libc::SIGINT));
    // Give the reader thread a moment for the last bytes.
    std::thread::sleep(Duration::from_millis(200));
    assert_restored(&pty, &out);
}

#[tokio::test]
async fn a_tui_that_stays_on_sigint_and_sighup_gets_sigterm() {
    let (pty, mut child, out) = start_tui("stubborn");
    let started = Instant::now();
    let outcome = stop::stop(&listed(&child), false, Duration::from_secs(10))
        .await
        .unwrap();
    let status = child.wait().unwrap();
    assert_eq!(outcome, StopOutcome::Exited);
    assert_eq!(status.signal(), Some(libc::SIGTERM));
    // SIGINT and SIGHUP each waited for before the next signal.
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    std::thread::sleep(Duration::from_millis(200));
    assert_restored(&pty, &out);
}

#[tokio::test]
async fn a_forced_stop_also_restores_the_terminal_first() {
    let (pty, mut child, out) = start_tui("stubborn");
    let outcome = stop::stop(&listed(&child), true, Duration::from_secs(5))
        .await
        .unwrap();
    let status = child.wait().unwrap();
    assert_eq!(outcome, StopOutcome::Killed);
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    std::thread::sleep(Duration::from_millis(200));
    assert_restored(&pty, &out);
}

#[test]
fn ps_names_the_controlling_terminal() {
    assert_eq!(
        tty_from_ps("ttys003\n"),
        Some(PathBuf::from("/dev/ttys003"))
    );
    assert_eq!(tty_from_ps(" pts/7 \n"), Some(PathBuf::from("/dev/pts/7")));
    assert_eq!(
        tty_from_ps("/dev/ttys001"),
        Some(PathBuf::from("/dev/ttys001"))
    );
    for none in ["??", "?", "", "  \n", "console", "../../etc/x"] {
        assert_eq!(tty_from_ps(none), None, "{none:?}");
    }
}
