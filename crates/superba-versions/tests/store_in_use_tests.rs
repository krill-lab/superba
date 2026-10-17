//! The Codex store never removes or replaces an entry in use (user bug 40:
//! "access denied (os error 5)" on `data\codex\0.163.0-x86_64-pc-windows-msvc`).
//! Windows refuses to rename or delete a directory whose `codex.exe` runs or is
//! held open; these rules are platform independent and tested with injected
//! checks, plus a real process running from an entry on Unix.

use std::cell::RefCell;
use std::io;
use std::path::Path;
use std::time::Duration;

use superba_core::{CodexVersion, Error};
use superba_versions::store::{
    self, StoreDirs, VERIFIED_MARKER, exe_name, is_in_use, prune_with, retry_in_use,
};

fn v(s: &str) -> CodexVersion {
    CodexVersion::parse(s).unwrap()
}

const TARGET: &str = "x86_64-unknown-linux-musl";

fn entry(dirs: &StoreDirs, version: &str) {
    let dir = dirs.entry(&v(version), TARGET);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(dir.join("bin").join(exe_name(TARGET)), "codex").unwrap();
    std::fs::write(dir.join(VERIFIED_MARKER), format!("{version}\n")).unwrap();
}

fn listed(dirs: &StoreDirs) -> Vec<String> {
    store::list(dirs, Some(TARGET))
        .iter()
        .map(|e| e.version.to_string())
        .collect()
}

#[test]
fn an_old_version_in_use_is_kept_and_the_others_are_pruned() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = StoreDirs {
        root: tmp.path().join("codex"),
    };
    for ver in [
        "0.150.0", "0.151.0", "0.160.0", "0.161.0", "0.162.0", "0.163.0",
    ] {
        entry(&dirs, ver);
    }
    let busy = dirs.entry(&v("0.150.0"), TARGET);
    let in_use = |dir: &Path, _: &str| (dir == busy).then(|| "process 42 runs it".to_owned());
    let removed = prune_with(&dirs, TARGET, &[], &in_use, &store::remove_dir).unwrap();
    assert_eq!(removed, vec![v("0.160.0"), v("0.151.0")]);
    assert_eq!(listed(&dirs), ["0.163.0", "0.162.0", "0.161.0", "0.150.0"]);
    // The kept entry is whole: its marker was never touched.
    assert!(busy.join(VERIFIED_MARKER).is_file());
}

#[test]
fn a_removal_that_fails_keeps_the_entry_and_pruning_goes_on() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = StoreDirs {
        root: tmp.path().join("codex"),
    };
    for ver in ["0.140.0", "0.150.0", "0.161.0", "0.162.0", "0.163.0"] {
        entry(&dirs, ver);
    }
    let stuck = dirs.entry(&v("0.140.0"), TARGET);
    let remove = |dir: &Path| {
        if dir == stuck {
            Err(store::in_use_error(dir, "a file in it is in use"))
        } else {
            store::remove_dir(dir)
        }
    };
    let removed = prune_with(&dirs, TARGET, &[], &|_, _| None, &remove).unwrap();
    assert_eq!(removed, vec![v("0.150.0")]);
    assert!(listed(&dirs).contains(&"0.140.0".to_owned()));
}

#[test]
fn removal_renames_the_entry_away_before_deleting_it() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = StoreDirs {
        root: tmp.path().join("codex"),
    };
    entry(&dirs, "0.160.0");
    let dir = dirs.entry(&v("0.160.0"), TARGET);
    store::remove_dir(&dir).unwrap();
    assert!(!dir.exists());
    // Nothing is left behind: no trash, no half-deleted entry.
    let left: Vec<_> = std::fs::read_dir(&dirs.root).unwrap().flatten().collect();
    assert!(left.is_empty(), "{left:?}");
    // Leftover trash of an earlier removal goes with the next prune.
    std::fs::create_dir_all(dirs.root.join(".trash-0123456789abcdef/bin")).unwrap();
    prune_with(&dirs, TARGET, &[], &|_, _| None, &store::remove_dir).unwrap();
    assert!(!dirs.root.join(".trash-0123456789abcdef").exists());
}

#[test]
fn renames_are_retried_only_while_a_file_is_in_use() {
    let busy = || io::Error::from(io::ErrorKind::ExecutableFileBusy);
    assert!(is_in_use(&busy()));
    assert!(!is_in_use(&io::Error::from(io::ErrorKind::NotFound)));
    let pauses = [Duration::from_millis(1), Duration::from_millis(2)];
    let slept = RefCell::new(Vec::new());
    let sleep = |d: Duration| slept.borrow_mut().push(d);

    // In use twice, then free: three attempts, both pauses.
    let mut calls = 0;
    let r = retry_in_use(
        || {
            calls += 1;
            if calls < 3 { Err(busy()) } else { Ok(calls) }
        },
        &pauses,
        &sleep,
    );
    assert_eq!(r.unwrap(), 3);
    assert_eq!(*slept.borrow(), pauses.to_vec());

    // Always in use: the last error after the last pause.
    slept.borrow_mut().clear();
    let r: io::Result<()> = retry_in_use(|| Err(busy()), &pauses, &sleep);
    assert_eq!(r.unwrap_err().kind(), io::ErrorKind::ExecutableFileBusy);
    assert_eq!(slept.borrow().len(), 2);

    // Any other error at once.
    slept.borrow_mut().clear();
    let mut calls = 0;
    let r: io::Result<()> = retry_in_use(
        || {
            calls += 1;
            Err(io::Error::from(io::ErrorKind::NotFound))
        },
        &pauses,
        &sleep,
    );
    assert!(r.is_err());
    assert_eq!(calls, 1);
    assert!(slept.borrow().is_empty());
}

#[test]
fn the_in_use_error_says_what_to_do() {
    let e = store::in_use_error(
        Path::new("store/0.163.0-x"),
        "process 7 runs a program from it",
    );
    let Error::Locked { what } = &e else {
        panic!("{e:?}");
    };
    assert!(what.contains("process 7"), "{what}");
    assert!(what.contains("never removes or replaces"), "{what}");
}

#[cfg(target_os = "linux")]
#[test]
fn a_running_program_marks_its_entry_in_use() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = StoreDirs {
        root: tmp.path().join("codex"),
    };
    entry(&dirs, "0.160.0");
    let dir = dirs.entry(&v("0.160.0"), TARGET);
    let exe = dir.join("bin").join(exe_name(TARGET));
    std::fs::copy("/bin/sleep", &exe).unwrap();
    assert_eq!(store::entry_in_use(&dir, TARGET), None);
    let mut child = std::process::Command::new(&exe).arg("30").spawn().unwrap();
    // The new process's image is visible at once in /proc.
    let mut why = None;
    for _ in 0..50 {
        why = store::entry_in_use(&dir, TARGET);
        if why.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // Removing it is refused, and nothing is moved or deleted.
    let removed = store::remove_dir(&dir);
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(matches!(removed, Err(Error::Locked { .. })), "{removed:?}");
    assert!(dir.join(VERIFIED_MARKER).is_file());
    let why = why.expect("the running program is found");
    assert!(why.contains(&child.id().to_string()), "{why}");
}
