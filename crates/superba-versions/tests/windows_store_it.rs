//! The Codex store on Windows with a running `codex.exe` (user bug 40: "access
//! denied (os error 5)" on `data\codex\0.163.0-x86_64-pc-windows-msvc`).
//! Ignored by default; the `winapp` CI job runs it on a Windows runner:
//!
//! ```text
//! SUPERBA_WINAPP_IT=1 cargo test -p superba-versions --test windows_store_it -- --ignored --nocapture
//! ```
//!
//! A copy of `ping.exe` stands in for a running `codex.exe` in a store entry
//! under a temporary directory; only that process is stopped, by its handle.

#![cfg(windows)]

use std::time::Duration;

use superba_core::{CodexVersion, Error};
use superba_versions::store::{self, StoreDirs, VERIFIED_MARKER};

const TARGET: &str = "x86_64-pc-windows-msvc";

fn say(line: impl AsRef<str>) {
    println!("[winapp] store: {}", line.as_ref());
}

#[test]
#[ignore = "Windows file locking on a disposable runner (winapp CI job)"]
fn a_store_entry_whose_codex_runs_is_never_removed_or_replaced() {
    if std::env::var("SUPERBA_WINAPP_IT").as_deref() != Ok("1") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dirs = StoreDirs {
        root: tmp.path().join("codex"),
    };
    let make = |version: &str| {
        let dir = dirs.entry(&CodexVersion::parse(version).unwrap(), TARGET);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::copy(
            r"C:\Windows\System32\PING.EXE",
            dir.join("bin").join("codex.exe"),
        )
        .unwrap();
        std::fs::write(dir.join(VERIFIED_MARKER), format!("{version}\n")).unwrap();
        dir
    };
    let old = make("0.150.0");
    let busy = make("0.151.0");
    for v in ["0.161.0", "0.162.0", "0.163.0"] {
        make(v);
    }
    let mut child = std::process::Command::new(busy.join("bin").join("codex.exe"))
        .args(["-n", "120", "127.0.0.1"])
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));

    // What Windows does with a directory whose program runs.
    let moved = busy.with_file_name("moved-away");
    let rename = std::fs::rename(&busy, &moved);
    say(format!(
        "plain rename of the entry while its codex.exe runs: {rename:?}"
    ));
    if rename.is_ok() {
        std::fs::rename(&moved, &busy).unwrap();
    }
    let exe = busy.join("bin").join("codex.exe");
    let delete = std::fs::remove_file(&exe);
    say(format!("deleting the running codex.exe: {delete:?}"));

    // Superba's check sees it, and removal keeps the entry whole.
    let why = store::entry_in_use(&busy, TARGET);
    say(format!("entry_in_use: {why:?}"));
    assert!(why.is_some());
    let removed = store::prune(&dirs, TARGET, &[]).unwrap();
    say(format!("prune removed {removed:?}"));
    assert_eq!(removed, vec![CodexVersion::parse("0.150.0").unwrap()]);
    assert!(!old.exists());
    assert!(busy.join(VERIFIED_MARKER).is_file());
    let direct = store::remove_dir(&busy);
    say(format!("remove_dir of the busy entry: {direct:?}"));
    assert!(matches!(direct, Err(Error::Locked { .. })), "{direct:?}");
    let listed: Vec<String> = store::list(&dirs, Some(TARGET))
        .iter()
        .map(|e| e.version.to_string())
        .collect();
    say(format!("listed while it runs: {listed:?}"));
    assert!(listed.contains(&"0.151.0".to_owned()));

    // A program another process holds open without delete sharing (an
    // antivirus scan of a new binary) blocks renaming its directory; the
    // rename is retried until it is let go.
    let fresh = make("0.164.0");
    let scanned = {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1) // FILE_SHARE_READ only
            .open(fresh.join("bin").join("codex.exe"))
            .unwrap()
    };
    let target = fresh.with_file_name("renamed-after-scan");
    let blocked = std::fs::rename(&fresh, &target);
    say(format!(
        "renaming a directory while a file in it is open without delete sharing: {blocked:?}"
    ));
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        drop(scanned);
    });
    let retried = store::rename_retrying(&fresh, &target);
    release.join().unwrap();
    say(format!(
        "rename_retrying while the scan lets go: {retried:?}"
    ));
    assert!(retried.is_ok());

    child.kill().unwrap();
    child.wait().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(store::entry_in_use(&busy, TARGET), None);
    store::remove_dir(&busy).unwrap();
    assert!(!busy.exists());
    say("after the program exited the entry is removed");
}
