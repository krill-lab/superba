//! Model catalog files across restores (review platform-security #0): the file
//! a restored `config.toml` names is never pruned, and when it is the previous
//! catalog its record becomes current again.

#![allow(missing_docs)]

mod fakes;

use std::path::PathBuf;
use std::sync::Arc;

use fakes::v3::files_in;
use fakes::*;
use superba_core::{
    AuthChoice, AuthMode, ExecuteOptions, ModelOverride, NoEvents, OutcomeStatus, ProfileId,
    RestoreTarget, SafeDefaults, SwitchIntent, SwitchPlan, SwitchRequest, SwitchService,
};
use superba_switch::Switcher;

fn req(profile: &str) -> SwitchRequest {
    SwitchRequest::profile(ProfileId::new(profile))
}

fn restore(target: RestoreTarget) -> SwitchRequest {
    let mut r = req("work");
    r.intent = SwitchIntent::Restore { target };
    r
}

/// The `work` profile on an API key with one per-model context window (which
/// needs a catalog file).
fn work_with_window(w: &World, n: u64) {
    let mut set = w.profiles.set.lock().unwrap();
    let p = set
        .profiles
        .iter_mut()
        .find(|p| p.id.as_str() == "work")
        .unwrap();
    p.auth_mode = AuthMode::ApiKey;
    p.set_auth(AuthChoice::ApiKey);
    p.model_overrides.insert(
        "gpt-6-astra".into(),
        ModelOverride {
            context_window: Some(n),
            ..ModelOverride::default()
        },
    );
}

async fn execute(sw: &Switcher, plan: SwitchPlan) {
    let outcome = sw
        .execute(
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
        .unwrap();
    assert_eq!(outcome.status, OutcomeStatus::Switched);
}

/// Switch to `work` with window `n`; returns the catalog file it wrote.
async fn switch_with_window(w: &World, sw: &Switcher, n: u64) -> PathBuf {
    work_with_window(w, n);
    let plan = sw.plan(req("work")).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    let path = plan
        .config
        .as_ref()
        .and_then(|c| c.desired.as_ref())
        .and_then(|d| d.model_catalog.clone())
        .expect("a catalog file");
    execute(sw, plan).await;
    assert!(path.exists());
    path
}

/// What the real config engine leaves in `config.toml` (the fake engine of
/// these tests writes no files).
fn config_names(w: &World, file: Option<&PathBuf>) {
    let home = w.ctx.codex_home();
    let body = match file {
        Some(f) => format!("model_catalog_json = \"{}\"\n", f.display()),
        None => String::new(),
    };
    std::fs::write(home.config_toml(), body).unwrap();
}

fn names(w: &World) -> Vec<String> {
    let mut v: Vec<String> = files_in(&w.ctx.codex_home().superba_dir());
    v.sort();
    v
}

fn name(p: &std::path::Path) -> String {
    p.file_name().unwrap().to_string_lossy().into_owned()
}

#[tokio::test]
async fn a_restored_generation_keeps_the_catalog_file_it_names_and_its_record() {
    let w = World::new();
    let sw = w.switcher();
    let f1 = switch_with_window(&w, &sw, 400_000).await;
    let f2 = switch_with_window(&w, &sw, 300_000).await;
    assert_ne!(f1, f2);
    let mut both = vec![name(&f1), name(&f2)];
    both.sort();
    assert_eq!(names(&w), both);

    // Restore generation 1: config.toml names F1 (the previous file) again.
    let plan = sw
        .plan(restore(RestoreTarget::Generation { generation: 1 }))
        .await
        .unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    config_names(&w, Some(&f1));
    execute(&sw, plan).await;
    assert_eq!(names(&w), both, "nothing named or previous is pruned");

    // F1 is the current record again: switching to the official login keeps
    // it as the previous file and prunes F2.
    let plan = sw.plan(req("official")).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    config_names(&w, None);
    execute(&sw, plan).await;
    assert_eq!(names(&w), vec![name(&f1)]);
}

#[tokio::test]
async fn a_restore_never_prunes_the_file_config_toml_names() {
    let w = World::new();
    let sw = w.switcher();
    let f1 = switch_with_window(&w, &sw, 400_000).await;
    let plan = sw.plan(req("official")).await.unwrap();
    config_names(&w, None);
    execute(&sw, plan).await;
    let f2 = switch_with_window(&w, &sw, 300_000).await;
    // F1 was released by the official login and is pruned by the next file.
    assert_eq!(names(&w), vec![name(&f2)]);
    let _ = f1;

    // A restore whose config.toml names F2 (the current file): kept.
    let plan = sw
        .plan(restore(RestoreTarget::Generation { generation: 3 }))
        .await
        .unwrap();
    config_names(&w, Some(&f2));
    execute(&sw, plan).await;
    assert_eq!(names(&w), vec![name(&f2)]);
}
