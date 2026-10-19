mod fakes;

use fakes::{ScriptedDecider, World};
use std::sync::Arc;
use superba_config::autostart::{EntryStore, Manager, MemoryEntry, Platform};
use superba_core::{
    ExecuteOptions, NoEvents, OutcomeStatus, ProfileId, ProviderRestore, RestoreTarget,
    SwitchIntent, SwitchRequest, SwitchService,
};
use superba_switch::Switcher;

fn service(w: &World) -> (Switcher, Arc<MemoryEntry>) {
    let exe = w.tmp.path().join("superba");
    std::fs::write(&exe, "binary").unwrap();
    let entry = Arc::new(MemoryEntry::default());
    let mut deps = w.deps();
    deps.autostart = Arc::new(Manager::with_store(Platform::Linux, entry.clone(), Ok(exe)));
    (Switcher::new(deps), entry)
}

#[tokio::test]
async fn system_registration_is_the_truth_and_preferences_do_not_reenable_it() {
    let w = World::new();
    let (sw, entry) = service(&w);
    assert!(!sw.profile_set().await.unwrap().settings.autostart);
    sw.set_autostart(true).await.unwrap();
    let mut settings = sw.profile_set().await.unwrap().settings;
    assert!(settings.autostart);
    entry.write(None).unwrap();
    settings.start_in_tray = true;
    sw.save_settings(settings).await.unwrap();
    let current = sw.profile_set().await.unwrap().settings;
    assert!(!current.autostart);
    assert!(current.start_in_tray);
    assert!(!sw.autostart_status().await.unwrap().enabled);
}

#[tokio::test(start_paused = true)]
async fn uninstall_and_restore_original_remove_the_login_entry() {
    for intent in [
        SwitchIntent::Uninstall {
            delete_backups: false,
            provider: Some(ProviderRestore::KeepOpenai),
        },
        SwitchIntent::Restore {
            target: RestoreTarget::Original {
                provider: ProviderRestore::KeepOpenai,
            },
        },
    ] {
        let w = World::new();
        w.set_session(None);
        let (sw, _) = service(&w);
        sw.set_autostart(true).await.unwrap();
        let mut request = SwitchRequest::profile(ProfileId::new("work"));
        request.intent = intent;
        let plan = sw.plan(request).await.unwrap();
        assert!(
            sw.autostart_status().await.unwrap().enabled,
            "planning does not remove anything"
        );
        let outcome = sw
            .execute(
                plan,
                ExecuteOptions::default(),
                ScriptedDecider::with(&[]),
                Arc::new(NoEvents),
            )
            .await
            .unwrap();
        assert_eq!(
            outcome.status,
            OutcomeStatus::Switched,
            "{:?}",
            outcome.notices
        );
        assert!(!sw.autostart_status().await.unwrap().enabled);
    }
}
