//! One-off alpha.16 PAT→API-key migration (user bug 139; remove with it):
//! `spb` runs it before a command, prints its notices to stderr and settles
//! them; a failure is one warning line and the command still succeeds; hooks,
//! other plumbing and dry runs never run it.

mod common;

use common::*;
use superba_core::{Error, LocalizedText, Notice};

#[tokio::test]
async fn a_command_runs_it_first_and_prints_its_notice_to_stderr() {
    let service = FakeService::new();
    service.pat_migration.lock().unwrap().answer = Some(Ok(vec![Notice::info(
        LocalizedText::new("switch.pat_migration.done", "Applied {profile} again.")
            .arg("profile", "Krill"),
    )]));
    let (code, stdout, stderr) = invoke(service.clone(), &["status", "--json"]).await;
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("Superba applied Krill again"), "{stderr}");
    one_json(&stdout);
    let m = service.pat_migration.lock().unwrap();
    assert_eq!((m.runs, m.delivered), (1, 1));
}

#[tokio::test]
async fn a_read_only_command_still_succeeds_when_it_fails() {
    let service = FakeService::new();
    service.pat_migration.lock().unwrap().answer = Some(Err(Error::other("store locked")));
    let (code, stdout, stderr) = invoke(service.clone(), &["status"]).await;
    assert_eq!(code, 0, "{stderr}");
    assert!(!stdout.is_empty());
    let warning: Vec<&str> = stderr.lines().filter(|l| l.contains("API key")).collect();
    assert_eq!(warning.len(), 1, "{stderr}");
    assert!(warning[0].contains("store locked"), "{stderr}");
    let m = service.pat_migration.lock().unwrap();
    assert_eq!((m.runs, m.delivered), (1, 0), "left owed for the retry");
}

#[tokio::test]
async fn nothing_to_do_prints_nothing() {
    let service = FakeService::new();
    let (code, _, stderr) = invoke(service.clone(), &["status"]).await;
    assert_eq!(code, 0);
    assert!(stderr.is_empty(), "{stderr}");
    assert_eq!(service.pat_migration.lock().unwrap().delivered, 0);
}

#[tokio::test]
async fn hooks_restores_and_dry_runs_never_run_it() {
    for args in [
        &["notify", "event", "router"][..],
        &["restore", "original", "--dry-run"][..],
        &["data", "usage"][..],
        &["switch", "--profile", "work", "--dry-run"][..],
        &["import", "ccs", "--dry-run"][..],
        &["models", "mode", "extended", "--dry-run"][..],
    ] {
        let service = FakeService::new();
        let _ = invoke(service.clone(), args).await;
        assert_eq!(service.pat_migration.lock().unwrap().runs, 0, "{args:?}");
    }
}
