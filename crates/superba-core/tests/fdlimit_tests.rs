//! Raising the soft open-file limit at startup. Each case runs in a child
//! process (this test binary started again for one test) because the limit is
//! process-wide and a lowered hard limit cannot be raised again.

#![allow(missing_docs)]
#![cfg(unix)]

use std::process::Command;

use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
use superba_core::fdlimit::{self, OPEN_FILES_TARGET, OpenFilesLimit};

const CHILD: &str = "SUPERBA_FDLIMIT_CHILD";

fn in_child(name: &str) -> bool {
    if std::env::var_os(CHILD).is_some() {
        return true;
    }
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "child {name} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    false
}

fn set(soft: u64, hard: Option<u64>) {
    setrlimit(
        Resource::Nofile,
        Rlimit {
            current: Some(soft),
            maximum: hard,
        },
    )
    .unwrap();
}

#[test]
fn a_macos_gui_limit_of_256_is_raised_to_the_target() {
    if !in_child("a_macos_gui_limit_of_256_is_raised_to_the_target") {
        return;
    }
    let hard = getrlimit(Resource::Nofile).maximum;
    set(256, hard);
    let raised = fdlimit::raise_open_files_limit(OPEN_FILES_TARGET).unwrap();
    let want = hard.map_or(OPEN_FILES_TARGET, |h| h.min(OPEN_FILES_TARGET));
    assert_eq!(
        raised,
        OpenFilesLimit {
            before: Some(256),
            after: Some(want),
            hard,
        }
    );
    assert_eq!(fdlimit::open_files_limit(), Some(want));
}

#[test]
fn the_soft_limit_stops_at_the_hard_limit() {
    if !in_child("the_soft_limit_stops_at_the_hard_limit") {
        return;
    }
    set(256, Some(1000));
    let raised = fdlimit::raise_open_files_limit(OPEN_FILES_TARGET).unwrap();
    assert_eq!(raised.before, Some(256));
    assert_eq!(raised.after, Some(1000));
    assert_eq!(raised.hard, Some(1000));
}

#[test]
fn a_higher_limit_is_never_lowered() {
    if !in_child("a_higher_limit_is_never_lowered") {
        return;
    }
    let hard = getrlimit(Resource::Nofile).maximum;
    let high = hard.map_or(20_000, |h| h.min(20_000));
    if high <= OPEN_FILES_TARGET {
        // This machine's hard limit is below the target: nothing to check.
        return;
    }
    set(high, hard);
    let raised = fdlimit::raise_open_files_limit(OPEN_FILES_TARGET).unwrap();
    assert_eq!(raised.before, Some(high));
    assert_eq!(raised.after, Some(high));
    assert_eq!(fdlimit::open_files_limit(), Some(high));
}
