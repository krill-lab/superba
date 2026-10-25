//! Stopped WSL distributions (user bug 54 (a)): a switch never starts one. A
//! stopped distribution worth coming back to (Superba wrote it, its last check
//! found a Codex, the user turned following on) is left out with a note and
//! recorded as pending; the next switch that finds it running, or "Update now"
//! (its pending request), brings it up to date. Restores also reach a
//! distribution Superba wrote that no longer follows.

#![allow(missing_docs)]

mod fakes;

use fakes::*;
use superba_core::{
    ExecuteOptions, NoEvents, ProfileId, RestoreTarget, SafeDefaults, SwitchIntent, SwitchPlan,
    SwitchRequest, SwitchService, TargetId, WslNetworking,
};
use superba_switch::Switcher;

const UBUNTU: &str = "WSL: Ubuntu";

fn req(profile: &str) -> SwitchRequest {
    SwitchRequest::profile(ProfileId::new(profile))
}

fn notice_keys(plan: &SwitchPlan) -> Vec<String> {
    plan.warnings.iter().map(|n| n.text.key.clone()).collect()
}

fn target() -> TargetId {
    superba_core::wsl::target_id("Ubuntu")
}

/// One WSL 2 distribution, running or stopped as Windows says.
fn world(running: bool) -> World {
    let w = World::new();
    let mut host = wsl_host("Ubuntu", 2, WslNetworking::Mirrored);
    host.wsl.as_mut().unwrap().set_running(running);
    w.remote.hosts.lock().unwrap().push(host);
    w
}

fn set_running(w: &World, running: bool) {
    for h in w.remote.hosts.lock().unwrap().iter_mut() {
        if let Some(d) = h.wsl.as_mut() {
            d.set_running(running);
        }
    }
}

async fn run(sw: &Switcher, request: SwitchRequest) -> SwitchPlan {
    let plan = sw.plan(request).await.unwrap();
    sw.execute(
        plan.clone(),
        ExecuteOptions::default(),
        std::sync::Arc::new(SafeDefaults),
        std::sync::Arc::new(NoEvents),
    )
    .await
    .unwrap();
    plan
}

async fn pending(sw: &Switcher) -> Option<superba_core::HostPending> {
    sw.hosts()
        .await
        .unwrap()
        .into_iter()
        .find(|h| h.host.target_id == target())
        .and_then(|h| h.pending)
}

#[tokio::test]
async fn a_switch_never_starts_a_stopped_distribution_and_records_it_as_pending() {
    let w = world(false);
    w.remote.written.lock().unwrap().push(UBUNTU.into());
    let sw = w.switcher();
    let plan = run(&sw, req("work")).await;
    // Asked Windows which distributions run; never ran a script inside it.
    assert!(*w.remote.running_calls.lock().unwrap() >= 1);
    assert!(w.remote.probes.lock().unwrap().is_empty());
    assert!(plan.remotes.is_empty());
    assert!(notice_keys(&plan).contains(&"switch.wsl.stopped_pending".to_owned()));
    let p = pending(&sw).await.expect("pending after the switch");
    assert!(
        matches!(p.intent, SwitchIntent::Profile { ref profile_id, .. } if profile_id.as_str() == "work")
    );

    // "Update now": the same intent for the distribution alone; the user asked,
    // so it is started (probed) and written.
    let request = p.request(&target());
    assert!(!request.targets.local);
    assert_eq!(request.targets.remotes, vec![target()]);
    let plan = run(&sw, request).await;
    assert_eq!(plan.remotes.len(), 1);
    assert!(w.remote.probes.lock().unwrap().contains(&UBUNTU.to_owned()));
    assert!(w.calls().iter().any(|c| c == "remote.apply WSL: Ubuntu"));
    assert!(pending(&sw).await.is_none());
}

#[tokio::test]
async fn the_next_switch_that_finds_it_running_updates_it() {
    let w = world(false);
    w.remote.written.lock().unwrap().push(UBUNTU.into());
    let sw = w.switcher();
    run(&sw, req("work")).await;
    assert!(pending(&sw).await.is_some());
    set_running(&w, true);
    let plan = run(&sw, req("work")).await;
    assert_eq!(plan.remotes.len(), 1);
    assert!(
        !notice_keys(&plan)
            .iter()
            .any(|k| k.starts_with("switch.wsl.stopped"))
    );
    assert!(pending(&sw).await.is_none());
}

#[tokio::test]
async fn a_stopped_distribution_without_anything_to_update_is_only_noted() {
    // Never written, never checked: nothing says it has a Codex.
    let w = world(false);
    let sw = w.switcher();
    let plan = run(&sw, req("work")).await;
    assert!(w.remote.probes.lock().unwrap().is_empty());
    assert!(notice_keys(&plan).contains(&"switch.wsl.stopped_skipped".to_owned()));
    assert!(pending(&sw).await.is_none());

    // A Codex found by an earlier check makes it pending.
    set_running(&w, true);
    sw.host_probe(&target()).await.unwrap();
    set_running(&w, false);
    let plan = run(&sw, req("work")).await;
    assert!(notice_keys(&plan).contains(&"switch.wsl.stopped_pending".to_owned()));
    assert!(pending(&sw).await.is_some());
    // Turning following off drops it.
    sw.set_host_follow(&target(), false).await.unwrap();
    assert!(pending(&sw).await.is_none());
}

#[tokio::test]
async fn an_unknown_running_state_keeps_checking_the_distribution() {
    // Windows could not say (no `--list --running` answer): probed as before.
    let w = World::new();
    w.remote
        .hosts
        .lock()
        .unwrap()
        .push(wsl_host("Ubuntu", 2, WslNetworking::Mirrored));
    let plan = w.switcher().plan(req("work")).await.unwrap();
    assert_eq!(plan.remotes.len(), 1);
    assert!(w.remote.probes.lock().unwrap().contains(&UBUNTU.to_owned()));
}

#[tokio::test]
async fn a_restore_also_reaches_a_written_distribution_that_no_longer_follows() {
    let w = world(true);
    w.remote.written.lock().unwrap().push(UBUNTU.into());
    let sw = w.switcher();
    sw.set_host_follow(&target(), false).await.unwrap();
    // A profile switch leaves it alone ...
    let plan = sw.plan(req("work")).await.unwrap();
    assert!(plan.remotes.is_empty());
    assert!(w.remote.probes.lock().unwrap().is_empty());
    // ... a restore of the original configuration restores it.
    let restore = SwitchRequest {
        intent: SwitchIntent::Restore {
            target: RestoreTarget::Original {
                provider: superba_core::ProviderRestore::KeepOpenai,
            },
        },
        ..req("work")
    };
    let plan = sw.plan(restore.clone()).await.unwrap();
    assert_eq!(plan.remotes.len(), 1);
    // Not written: not part of the restore.
    let w = world(true);
    let sw = w.switcher();
    sw.set_host_follow(&target(), false).await.unwrap();
    let plan = sw.plan(restore).await.unwrap();
    assert!(plan.remotes.is_empty());
}

#[tokio::test]
async fn uninstall_starts_a_stopped_distribution_superba_wrote() {
    // Removing Superba's changes everywhere is the user's own request.
    let w = world(false);
    w.remote.written.lock().unwrap().push(UBUNTU.into());
    let sw = w.switcher();
    let plan = sw
        .plan(SwitchRequest {
            intent: SwitchIntent::Uninstall {
                delete_backups: false,
                provider: None,
            },
            ..req("work")
        })
        .await
        .unwrap();
    assert!(w.remote.probes.lock().unwrap().contains(&UBUNTU.to_owned()));
    assert!(
        !notice_keys(&plan)
            .iter()
            .any(|k| k.starts_with("switch.wsl.stopped"))
    );
    // One Superba never wrote stays stopped.
    let w = world(false);
    let plan = w
        .switcher()
        .plan(SwitchRequest {
            intent: SwitchIntent::Uninstall {
                delete_backups: false,
                provider: None,
            },
            ..req("work")
        })
        .await
        .unwrap();
    assert!(w.remote.probes.lock().unwrap().is_empty());
    assert!(notice_keys(&plan).contains(&"switch.wsl.stopped_skipped".to_owned()));
}

#[tokio::test]
async fn a_failed_first_write_is_persisted_in_the_journal_and_pending_until_retry() {
    let w = world(true);
    w.remote.fail_apply.lock().unwrap().insert(
        UBUNTU.into(),
        superba_core::Error::Timeout {
            what: "WSL Ubuntu: files".into(),
        },
    );
    let sw = w.switcher();
    let plan = run(&sw, req("work")).await;
    assert!(pending(&sw).await.is_some());
    let journal = superba_switch::journal::SwitchJournal::new(&w.ctx.data_dir);
    let entry = journal
        .entries()
        .unwrap()
        .into_iter()
        .find(|e| e.id == plan.id)
        .unwrap();
    assert_eq!(
        entry.status,
        superba_switch::journal::JournalStatus::Partial
    );
    assert!(entry.remotes.is_empty(), "there was no write receipt");
    assert_eq!(entry.remote_failures.len(), 1);
    assert_eq!(entry.remote_failures[0].target_id, target());
    assert!(
        pending(&w.switcher()).await.is_some(),
        "pending survives service restart"
    );
    assert!(
        journal.unfinished().unwrap().is_empty(),
        "a failed remote must not trigger local rollback on startup"
    );
    w.remote.fail_apply.lock().unwrap().clear();
    run(&sw, req("work")).await;
    assert!(pending(&sw).await.is_none());
}

/// alpha.10 re-test (W81): a distribution whose login shell did not answer and
/// that was never read before is still switched, with a warning that names
/// the files to check, in the plan and in the outcome.
#[tokio::test]
async fn a_login_read_without_cache_still_switches_with_a_warning() {
    let w = world(true);
    *w.remote.login_fallback.lock().unwrap() = Some("none".into());
    let sw = w.switcher();
    let plan = sw.plan(req("work")).await.unwrap();
    assert!(
        notice_keys(&plan).contains(&"switch.wsl.login_none".to_owned()),
        "{:?}",
        notice_keys(&plan)
    );
    let out = sw
        .execute(
            plan,
            ExecuteOptions::default(),
            std::sync::Arc::new(SafeDefaults),
            std::sync::Arc::new(NoEvents),
        )
        .await
        .unwrap();
    let wsl = out
        .remotes
        .iter()
        .find(|r| r.target_id == target())
        .unwrap();
    assert_ne!(wsl.status, superba_core::OutcomeStatus::Failed, "{out:?}");
    assert!(
        out.notices
            .iter()
            .any(|n| n.text.key == "switch.wsl.login_none"),
        "{:?}",
        out.notices
    );
    assert!(pending(&sw).await.is_none());
}
