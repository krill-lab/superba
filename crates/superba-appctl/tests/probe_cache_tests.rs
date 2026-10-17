//! Detection repeated on a timer starts no process (user request 34): `--version`
//! answers are kept per binary file until an explicit refresh, CDP listener
//! checks per app process, and the change stamp reads metadata only.
//!
//! Child processes are counted with `superba_core::process::commands_built`;
//! this file is its own test binary, and its tests that count run one after the
//! other inside one test function so no other test spawns meanwhile.

#![allow(missing_docs)]

#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use superba_appctl::codexbin::Caches;
use superba_appctl::owner::OwnerCheck;
#[cfg(unix)]
use superba_core::CodexVersion;
use superba_core::Timestamp;
#[cfg(unix)]
use tempfile::tempdir;

#[cfg(unix)]
fn script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn detection_spawns_only_for_new_or_changed_binaries_and_on_explicit_refresh() {
    use superba_appctl::DesktopAppController;
    use superba_appctl::install::{codex_version, forget_probe_caches};
    use superba_core::process::commands_built;
    use superba_core::{AppController, DataDir, LocalContext, MapEnv};

    let temp = tempdir().unwrap();
    let v = |s: &str| Some(CodexVersion::parse(s).unwrap());

    // --- `codex --version` answers, kept per file --------------------------
    let binary = temp.path().join("a/codex");
    script(&binary, "echo 'codex-cli 0.162.0'");
    let start = commands_built();
    assert_eq!(codex_version(&binary).await, v("0.162.0"));
    assert_eq!(commands_built() - start, 1);
    for _ in 0..50 {
        assert_eq!(codex_version(&binary).await, v("0.162.0"));
    }
    assert_eq!(commands_built() - start, 1, "one process per binary file");

    // No answer is kept as well: a broken binary is not asked on every refresh.
    let broken = temp.path().join("b/codex");
    script(&broken, "exit 3");
    assert_eq!(codex_version(&broken).await, None);
    for _ in 0..50 {
        assert_eq!(codex_version(&broken).await, None);
    }
    assert_eq!(commands_built() - start, 2);

    // An updated binary (new size and time) is asked again at once.
    script(&binary, "echo 'codex-cli 0.163.10'");
    assert_eq!(codex_version(&binary).await, v("0.163.10"));
    assert_eq!(commands_built() - start, 3);

    // An explicit refresh asks every binary again, once.
    forget_probe_caches();
    for _ in 0..10 {
        assert_eq!(codex_version(&binary).await, v("0.163.10"));
        assert_eq!(codex_version(&broken).await, None);
    }
    assert_eq!(commands_built() - start, 5);

    // --- the controller: detection, change stamp, refresh -------------------
    let home = temp.path().join("home");
    let bin = temp.path().join("bin");
    let other = temp.path().join("other-bin");
    fs::create_dir_all(&other).unwrap();
    script(&bin.join("codex"), "echo 'codex-cli 0.162.1'");
    let env = MapEnv::new()
        .with("PATH", format!("{}:{}", bin.display(), other.display()))
        .with("HOME", &home);
    let ctx = LocalContext::new(&home, DataDir::new(temp.path().join("data")), Arc::new(env));
    let app = DesktopAppController::new(ctx);

    let start = commands_built();
    let installs = app.detect_installs().await.unwrap();
    let binaries = app.codex_binaries().await.unwrap();
    let ours = binaries
        .iter()
        .find(|b| b.path == bin.join("codex"))
        .expect("the CLI on PATH");
    assert_eq!(ours.version, v("0.162.1"));
    // Whatever else this machine has installed is asked once, too.
    let first = commands_built() - start;
    let bound = (binaries.len() + installs.len()) as u64;
    assert!(
        first >= 1 && first <= bound,
        "{first} processes for {bound} binaries"
    );

    // Detection repeated (the monitor's long interval) and change checks:
    // no process at all.
    let start = commands_built();
    let stamp = app.environment_stamp(&installs, &binaries);
    assert!(stamp.is_some());
    for _ in 0..20 {
        let installs = app.detect_installs().await.unwrap();
        let binaries = app.codex_binaries().await.unwrap();
        assert_eq!(app.environment_stamp(&installs, &binaries), stamp);
    }
    assert_eq!(
        commands_built() - start,
        0,
        "repeated detection starts nothing"
    );

    // A new CLI in another PATH directory changes the stamp (its directory
    // changed); detecting it asks only the new binary.
    script(&other.join("codex"), "echo 'codex-cli 0.150.0'");
    assert_ne!(app.environment_stamp(&installs, &binaries), stamp);
    let start = commands_built();
    let binaries = app.codex_binaries().await.unwrap();
    assert!(
        binaries
            .iter()
            .any(|b| b.path == other.join("codex") && b.version == v("0.150.0"))
    );
    assert_eq!(commands_built() - start, 1);

    // An explicit refresh asks each binary once more: bounded by their number.
    let start = commands_built();
    app.forget_cached_probes();
    let installs = app.detect_installs().await.unwrap();
    let binaries = app.codex_binaries().await.unwrap();
    let forced = commands_built() - start;
    let bound = (binaries.len() + installs.len()) as u64;
    assert!(
        forced >= 2 && forced <= bound,
        "{forced} processes for {bound} binaries"
    );
}

#[test]
fn listener_checks_are_kept_for_the_life_of_the_app_process() {
    let caches = Caches::default();
    let calls = AtomicUsize::new(0);
    let check = |answer: OwnerCheck| {
        calls.fetch_add(1, Ordering::SeqCst);
        answer
    };
    let started = Some(Timestamp(1_000));

    // Verified once: the overview every few seconds asks nothing more.
    for _ in 0..100 {
        assert_eq!(
            caches.owner_check(9222, 42, started, || check(OwnerCheck::Verified)),
            OwnerCheck::Verified
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Another process on that pid (a new start time) is checked again.
    assert_eq!(
        caches.owner_check(9222, 42, Some(Timestamp(9_000)), || check(
            OwnerCheck::NotListening
        )),
        OwnerCheck::NotListening
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    // A negative answer is not asked again in a tight loop either.
    for _ in 0..100 {
        assert_eq!(
            caches.owner_check(9222, 42, Some(Timestamp(9_000)), || check(
                OwnerCheck::Verified
            )),
            OwnerCheck::NotListening
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    // An explicit check (the launch waits for the listener) replaces it ...
    caches.record_owner(9222, 42, Some(Timestamp(9_000)), OwnerCheck::Verified);
    assert_eq!(
        caches.owner_check(9222, 42, Some(Timestamp(9_000)), || check(
            OwnerCheck::NotListening
        )),
        OwnerCheck::Verified
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    // ... and an explicit refresh forgets everything.
    caches.forget_owners();
    assert_eq!(
        caches.owner_check(9222, 42, started, || check(OwnerCheck::Mismatch {
            owners: vec![7]
        })),
        OwnerCheck::Mismatch { owners: vec![7] }
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}
