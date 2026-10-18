//! Opening the desktop app with a PAT profile applied (user bug 69a): where
//! the app gets no per-launch environment (Windows: `HKCU\Environment` at
//! activation, `WSLENV` for "Run Codex in WSL") the auth address is persisted
//! before the app starts, so its Codex has it and the first PAT switch can be
//! hot. The value is the one Superba persisted for the applied profile.

#![allow(missing_docs)]

mod fakes;

use std::sync::Arc;

use fakes::*;
use superba_core::{
    ActiveState, AuthMode, EnvLocation, EnvOp, ExecuteOptions, NoEvents, OutcomeStatus, ProfileId,
    SafeDefaults, SwitchIntent, SwitchRequest, SwitchService, Timestamp,
};

fn applied(w: &World, auth: AuthMode) {
    w.config.state.lock().unwrap().active = Some(ActiveState {
        profile_id: Some("work".into()),
        site_id: Some("krill".into()),
        line_id: Some("cn-fast".into()),
        auth_mode: Some(auth),
        generation: 1,
        applied_at: Timestamp(T0),
    });
}

/// Windows with the `WSLENV` entry missing.
fn windows_missing_wslenv(w: &World) {
    *w.env.launch_edit.lock().unwrap() = Some(EnvLocation::WslEnvForward);
}

fn env_apply_before_launch(w: &World) -> bool {
    let calls = w.calls();
    let apply = calls.iter().position(|c| c == "env.apply");
    let launch = calls.iter().position(|c| c.starts_with("app.launch"));
    matches!((apply, launch), (Some(a), Some(l)) if a < l)
}

#[tokio::test]
async fn open_app_persists_the_auth_address_first_for_a_pat_profile() {
    let w = World::new();
    w.set_session(None);
    applied(&w, AuthMode::Pat);
    windows_missing_wslenv(&w);
    w.switcher().open_app().await.unwrap();
    assert!(env_apply_before_launch(&w), "{:?}", w.calls());
    // The applied line's auth address (nothing persisted by Superba yet).
    assert_eq!(
        *w.env.launch_plans.lock().unwrap(),
        vec![CN_FAST.to_owned()]
    );
    assert!(matches!(
        w.env.applied.lock().unwrap().last(),
        Some((_, EnvOp::Set { value, .. })) if value == CN_FAST
    ));

    // What Superba persisted for the applied profile wins.
    let w = World::new();
    w.set_session(None);
    applied(&w, AuthMode::Pat);
    windows_missing_wslenv(&w);
    w.env.report.lock().unwrap().superba_value = Some(OVERSEAS.into());
    w.switcher().open_app().await.unwrap();
    assert_eq!(
        *w.env.launch_plans.lock().unwrap(),
        vec![OVERSEAS.to_owned()]
    );
}

#[tokio::test]
async fn nothing_is_written_without_pat_or_where_the_launch_passes_it() {
    // API key: the app needs no auth address.
    let w = World::new();
    w.set_session(None);
    applied(&w, AuthMode::ApiKey);
    windows_missing_wslenv(&w);
    w.switcher().open_app().await.unwrap();
    assert!(w.env.launch_plans.lock().unwrap().is_empty());
    assert!(!w.calls().contains(&"env.apply".to_owned()));

    // Linux / macOS: the launch passes it (the persister plans nothing).
    let w = World::new();
    w.set_session(None);
    applied(&w, AuthMode::Pat);
    w.switcher().open_app().await.unwrap();
    assert_eq!(w.env.launch_plans.lock().unwrap().len(), 1);
    assert!(!w.calls().contains(&"env.apply".to_owned()));
    assert!(w.calls().iter().any(|c| c.starts_with("app.launch")));

    // A failure to write does not keep the app closed.
    let w = World::new();
    w.set_session(None);
    applied(&w, AuthMode::Pat);
    windows_missing_wslenv(&w);
    *w.env.fail_apply.lock().unwrap() = Some(superba_core::Error::other("registry"));
    w.switcher().open_app().await.unwrap();
    assert!(w.calls().iter().any(|c| c.starts_with("app.launch")));
}

#[tokio::test(start_paused = true)]
async fn spb_launch_restart_persists_it_too() {
    let w = World::new();
    w.set_session(None);
    applied(&w, AuthMode::Pat);
    windows_missing_wslenv(&w);
    let sw = w.switcher();
    let mut r = SwitchRequest::profile(ProfileId::new("work"));
    r.intent = SwitchIntent::RestartApp { install_id: None };
    let plan = sw.plan(r).await.unwrap();
    assert!(plan.env.is_none());
    let o = sw
        .execute(
            plan,
            ExecuteOptions::default(),
            Arc::new(SafeDefaults),
            Arc::new(NoEvents),
        )
        .await
        .unwrap();
    assert_eq!(o.status, OutcomeStatus::Switched);
    assert!(env_apply_before_launch(&w), "{:?}", w.calls());
}
