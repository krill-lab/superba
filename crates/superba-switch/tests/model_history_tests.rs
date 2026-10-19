//! Verified legacy IDs must be repaired only after the desktop app stops.
#![allow(missing_docs)]

mod fakes;
use fakes::*;
use superba_core::*;

fn legacy_world() -> World {
    let w = World::new();
    *w.prober.models.lock().unwrap() = Ok(vec!["gpt-6.1-sol".into()]);
    *w.config.recent_models.lock().unwrap() =
        vec!["krill/gpt-6.1-sol".into(), "other/unknown".into()];
    w
}

#[tokio::test]
async fn verified_recents_require_full_restart_even_in_safe_mode_and_when_config_is_unchanged() {
    for safe in [false, true] {
        let w = legacy_world();
        *w.config.noop_plan.lock().unwrap() = true;
        w.profiles.set.lock().unwrap().settings.safe_mode = safe;
        let plan = w
            .switcher()
            .plan(SwitchRequest::profile(ProfileId::new("work")))
            .await
            .unwrap();
        assert_eq!(
            plan.primary,
            RestartKind::FullAppRestart {
                reason: FullRestartReason::RecentModels
            }
        );
        assert!(plan.needs_confirmation());
        assert_eq!(
            w.config.recent_models.lock().unwrap()[0],
            "krill/gpt-6.1-sol"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn repair_occurs_after_quit_before_launch_and_preserves_unknown_ids() {
    let w = legacy_world();
    let sw = w.switcher();
    let plan = sw
        .plan(SwitchRequest::profile(ProfileId::new("work")))
        .await
        .unwrap();
    let result = sw
        .execute(
            plan,
            ExecuteOptions {
                allow_full_restart: true,
                ..Default::default()
            },
            ScriptedDecider::with(&[]),
            std::sync::Arc::new(NoEvents),
        )
        .await
        .unwrap();
    assert_eq!(
        result.status,
        OutcomeStatus::Switched,
        "{:?}",
        result.notices
    );
    let calls = w.calls();
    let repair = calls
        .iter()
        .position(|c| c == "config:repair_recent_models")
        .unwrap();
    assert!(
        calls
            .iter()
            .position(|c| c.starts_with("app.quit "))
            .unwrap()
            < repair
    );
    assert!(
        calls
            .iter()
            .position(|c| c.starts_with("app.launch"))
            .unwrap()
            > repair
    );
    assert_eq!(
        *w.config.recent_models.lock().unwrap(),
        ["gpt-6.1-sol", "other/unknown"]
    );
}

#[tokio::test(start_paused = true)]
async fn unconfirmed_and_write_only_switches_leave_running_preferences_untouched() {
    for write_only in [false, true] {
        let w = legacy_world();
        let sw = w.switcher();
        let plan = sw
            .plan(SwitchRequest::profile(ProfileId::new("work")))
            .await
            .unwrap();
        let result = sw
            .execute(
                plan,
                ExecuteOptions {
                    write_only,
                    ..Default::default()
                },
                ScriptedDecider::with(&[]),
                std::sync::Arc::new(NoEvents),
            )
            .await;
        assert!(!w.calls().contains(&"config:repair_recent_models".into()));
        assert_eq!(
            w.config.recent_models.lock().unwrap()[0],
            "krill/gpt-6.1-sol"
        );
        if write_only {
            let result = result.unwrap();
            assert_eq!(result.status, OutcomeStatus::WrittenOnly);
            assert!(
                result
                    .notices
                    .iter()
                    .any(|n| n.text.key == "switch.models.recent_deferred")
            );
        } else {
            assert!(result.is_err());
        }
    }
}

#[tokio::test]
async fn failed_discovery_never_guesses_or_forces_a_repair_restart() {
    let w = legacy_world();
    *w.prober.models.lock().unwrap() = Err(Error::other("offline"));
    let plan = w
        .switcher()
        .plan(SwitchRequest::profile(ProfileId::new("work")))
        .await
        .unwrap();
    assert_ne!(
        plan.primary,
        RestartKind::FullAppRestart {
            reason: FullRestartReason::RecentModels
        }
    );
    assert_eq!(
        w.config.recent_models.lock().unwrap()[0],
        "krill/gpt-6.1-sol"
    );
}

#[tokio::test]
async fn configured_legacy_model_is_canonicalized_from_the_available_catalog() {
    let w = legacy_world();
    w.profiles
        .set
        .lock()
        .unwrap()
        .profiles
        .iter_mut()
        .find(|p| p.id == "work")
        .unwrap()
        .model
        .model = Some("krill/gpt-6.1-sol".into());
    let plan = w
        .switcher()
        .plan(SwitchRequest::profile(ProfileId::new("work")))
        .await
        .unwrap();
    assert!(
        plan.model_check
            .as_ref()
            .is_some_and(|m| m.current.as_deref() == Some("gpt-6.1-sol") && m.available)
    );
}

#[tokio::test]
async fn router_uses_the_krill_members_catalog_for_legacy_names() {
    let w = legacy_world();
    *w.router.port.lock().unwrap() = Some(19440);
    *w.router.up.lock().unwrap() = true;
    {
        let mut set = w.profiles.set.lock().unwrap();
        set.settings.router.enabled = true;
        set.settings.router.members = vec![ProfileId::new("work")];
        set.profiles
            .iter_mut()
            .find(|p| p.id == "work")
            .unwrap()
            .model
            .model = Some("krill/gpt-6.1-sol".into());
    }
    let plan = w
        .switcher()
        .plan(SwitchRequest::profile(ProfileId::new("work")))
        .await
        .unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert_eq!(
        plan.primary,
        RestartKind::FullAppRestart {
            reason: FullRestartReason::RecentModels
        }
    );
    assert!(
        plan.model_check
            .as_ref()
            .is_some_and(|m| m.current.as_deref() == Some("gpt-6.1-sol") && m.available)
    );
}
