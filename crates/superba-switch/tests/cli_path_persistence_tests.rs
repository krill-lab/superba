//! A CODEX_CLI_PATH Superba persisted for the app's Codex pin never outlives
//! the pin (review platform-security #4): uninstall, turning "also outside
//! Superba" off and unpinning without it all remove the value.

#![allow(missing_docs)]

mod fakes;

use std::sync::Arc;

use fakes::*;
use superba_core::{
    AppCodexPin, CodexVersion, EnvOp, ExecuteOptions, NoEvents, OutcomeStatus, ProfileId,
    SafeDefaults, SwitchIntent, SwitchRequest, SwitchService, codex::ENV_CODEX_CLI_PATH,
};

fn req(profile: &str) -> SwitchRequest {
    SwitchRequest::profile(ProfileId::new(profile))
}

fn pin_request(pin: AppCodexPin) -> SwitchRequest {
    let mut r = req("work");
    r.intent = SwitchIntent::AppCodexPin { pin };
    r
}

async fn run(
    sw: &superba_switch::Switcher,
    plan: superba_core::SwitchPlan,
) -> superba_core::Result<superba_core::SwitchOutcome> {
    sw.execute(
        plan,
        ExecuteOptions {
            allow_full_restart: true,
            turn_quiet_ms: 0,
            ..ExecuteOptions::default()
        },
        Arc::new(SafeDefaults),
        Arc::new(NoEvents),
    )
    .await
}

fn cli_path_ops(w: &World) -> Vec<EnvOp> {
    w.env
        .applied
        .lock()
        .unwrap()
        .iter()
        .filter(|(var, _)| var == ENV_CODEX_CLI_PATH)
        .map(|(_, op)| op.clone())
        .collect()
}

#[tokio::test]
async fn uninstall_clears_a_persisted_cli_path_and_the_pin() {
    let w = World::new();
    {
        let mut set = w.profiles.set.lock().unwrap();
        set.settings.app_codex.pin = AppCodexPin::Version {
            version: CodexVersion::new(0, 162, 0),
        };
        set.settings.app_codex.persist_env = true;
    }
    let sw = w.switcher();
    let mut r = req("work");
    r.intent = SwitchIntent::Uninstall {
        delete_backups: false,
        provider: None,
    };
    let plan = sw.plan(r).await.unwrap();
    let outcome = run(&sw, plan).await.unwrap();
    assert!(matches!(
        outcome.status,
        OutcomeStatus::Switched | OutcomeStatus::WrittenOnly
    ));
    assert_eq!(cli_path_ops(&w), vec![EnvOp::Clear]);
    assert!(
        w.profiles
            .set
            .lock()
            .unwrap()
            .settings
            .app_codex
            .is_default()
    );
    // A relaunch during the uninstall runs the app's own Codex.
    for l in w.app.launches.lock().unwrap().iter() {
        assert!(!l.env.contains_key(ENV_CODEX_CLI_PATH), "{l:?}");
    }
}

#[tokio::test]
async fn the_also_outside_superba_choice_moves_the_persisted_value_with_it() {
    let w = World::new();
    let v = CodexVersion::new(0, 162, 0);
    let exe = w.v3.versions.add_stored(v.clone());
    w.profiles.set.lock().unwrap().settings.app_codex.pin = AppCodexPin::Version { version: v };
    let sw = w.switcher();
    let mut settings = w.profiles.set.lock().unwrap().settings.clone();
    settings.app_codex.persist_env = true;
    sw.save_settings(settings.clone()).await.unwrap();
    settings.app_codex.persist_env = false;
    sw.save_settings(settings.clone()).await.unwrap();
    // Unchanged: nothing more.
    sw.save_settings(settings).await.unwrap();
    assert_eq!(
        cli_path_ops(&w),
        vec![
            EnvOp::Set {
                value: exe.display().to_string(),
                take_over_foreign: false,
            },
            EnvOp::Clear,
        ]
    );
}

#[tokio::test]
async fn unpinning_without_persistence_still_clears_a_persisted_value() {
    let w = World::new();
    let sw = w.switcher();
    let plan = sw.plan(pin_request(AppCodexPin::Bundled)).await.unwrap();
    let env = plan.env.as_ref().expect("an env plan");
    assert_eq!(env.var, ENV_CODEX_CLI_PATH);
    assert_eq!(env.op, EnvOp::Clear);
}
