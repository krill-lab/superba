//! Every Codex that reads `config.toml` must be able to load it (review
//! platform-security #1, #2, #5, #6, #7): pin changes check and rebuild the
//! catalog file, the start-up repair keeps the user's own values, IDE
//! extensions' Codex binaries are preflighted, tier-suffix names are not
//! suffixed twice, and old Codex versions get no `service_tier` they cannot
//! parse.

#![allow(missing_docs)]

mod fakes;

use std::path::PathBuf;
use std::sync::Arc;

use fakes::v3::files_in;
use fakes::*;
use superba_core::{
    ActiveState, AppCodexPin, AuthChoice, AuthMode, BlockReason, CodexProcessKind, CodexVersion,
    Error, ExecuteOptions, KeyChange, ModelOverride, NoEvents, OutcomeStatus, ProcessInfo,
    ProfileId, SafeDefaults, Severity, SiteId, SpeedTier, SwitchIntent, SwitchOutcome, SwitchPlan,
    SwitchRequest, SwitchService, Timestamp,
};
use superba_switch::Switcher;

fn req(profile: &str) -> SwitchRequest {
    SwitchRequest::profile(ProfileId::new(profile))
}

fn pin_request(pin: AppCodexPin) -> SwitchRequest {
    let mut r = req("work");
    r.intent = SwitchIntent::AppCodexPin { pin };
    r
}

fn work(w: &World, f: impl FnOnce(&mut superba_core::Profile)) {
    let mut set = w.profiles.set.lock().unwrap();
    let p = set
        .profiles
        .iter_mut()
        .find(|p| p.id.as_str() == "work")
        .unwrap();
    p.auth_mode = AuthMode::ApiKey;
    p.set_auth(AuthChoice::ApiKey);
    f(p);
}

fn with_window(p: &mut superba_core::Profile) {
    p.model_overrides.insert(
        "gpt-6-astra".into(),
        ModelOverride {
            context_window: Some(400_000),
            ..ModelOverride::default()
        },
    );
}

async fn run(sw: &Switcher, plan: SwitchPlan) -> superba_core::Result<SwitchOutcome> {
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

fn desired(plan: &SwitchPlan) -> &superba_core::ResolvedProfile {
    plan.config
        .as_ref()
        .and_then(|c| c.desired.as_ref())
        .expect("config plan")
}

fn keys(notices: &[superba_core::Notice]) -> Vec<String> {
    notices.iter().map(|n| n.text.key.clone()).collect()
}

fn applies(w: &World) -> usize {
    w.calls()
        .iter()
        .filter(|c| c.starts_with("config.apply"))
        .count()
}

/// `work` applied with a per-model window: its catalog file F1 exists and
/// `config.toml` names it (the fake engine writes no files, so the test does).
async fn applied_with_catalog(w: &World, sw: &Switcher) -> PathBuf {
    work(w, with_window);
    let plan = sw.plan(req("work")).await.unwrap();
    let file = desired(&plan).model_catalog.clone().expect("catalog file");
    // The fake config engine does not record the applied profile itself.
    w.config.state.lock().unwrap().active = Some(ActiveState {
        profile_id: Some(ProfileId::new("work")),
        site_id: Some(SiteId::new("krill")),
        line_id: None,
        auth_mode: Some(AuthMode::ApiKey),
        generation: 1,
        applied_at: Timestamp(T0),
    });
    let outcome = run(sw, plan).await.unwrap();
    assert_eq!(outcome.status, OutcomeStatus::Switched);
    std::fs::write(
        w.ctx.codex_home().config_toml(),
        format!("model_catalog_json = {:?}\n", file.display().to_string()),
    )
    .unwrap();
    file
}

// ---------------------------------------------------------------------------
// Pinning (#1)
// ---------------------------------------------------------------------------

#[tokio::test]
#[cfg_attr(
    windows,
    ignore = "pinning the app's Codex is not offered on Windows (PIN_SUPPORTED)"
)]
async fn a_pin_whose_codex_refuses_the_catalog_rebuilds_it_before_the_app_restarts() {
    let w = World::new();
    let sw = w.switcher();
    let f1 = applied_with_catalog(&w, &sw).await;
    let v = CodexVersion::new(0, 163, 0);
    let exe = w.v3.versions.add_stored(v.clone());
    // The file made for the bundled Codex is refused by 0.163.0; a rebuilt one loads.
    w.app.refused_catalogs.lock().unwrap().push(f1.clone());

    let plan = sw
        .plan(pin_request(AppCodexPin::Version { version: v.clone() }))
        .await
        .unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert!(keys(&plan.warnings).contains(&"switch.pin.catalog_check".to_owned()));
    let before = applies(&w);
    let outcome = run(&sw, plan).await.unwrap();
    assert_eq!(outcome.status, OutcomeStatus::Switched, "{outcome:?}");
    assert!(
        keys(&outcome.notices).contains(&"switch.pin.catalog_rebuilt".to_owned()),
        "{:?}",
        keys(&outcome.notices)
    );
    assert_eq!(applies(&w), before + 1, "config.toml rewritten once");
    // Rebuilt from the pinned Codex's own template, for its version.
    let call = w.v3.builder.calls.lock().unwrap().last().unwrap().clone();
    assert_eq!(call.consumer, v);
    assert!(w.app.preflights.lock().unwrap().contains(&exe));
    // The new file is written; the old one stays as the previous file.
    assert_eq!(files_in(&w.ctx.codex_home().superba_dir()).len(), 2);
    // Written before the app was relaunched with the pin.
    let calls = w.calls();
    let apply_at = calls.iter().rposition(|c| c.starts_with("config.apply"));
    let launch_at = calls.iter().rposition(|c| c.starts_with("app.launch"));
    assert!(apply_at < launch_at, "{calls:?}");
    assert_eq!(
        w.profiles.set.lock().unwrap().settings.app_codex.pin,
        AppCodexPin::Version { version: v }
    );
}

#[tokio::test]
#[cfg_attr(
    windows,
    ignore = "pinning the app's Codex is not offered on Windows (PIN_SUPPORTED)"
)]
async fn a_pin_whose_codex_accepts_the_catalog_rewrites_nothing() {
    let w = World::new();
    let sw = w.switcher();
    applied_with_catalog(&w, &sw).await;
    let v = CodexVersion::new(0, 163, 0);
    w.v3.versions.add_stored(v.clone());
    let plan = sw
        .plan(pin_request(AppCodexPin::Version { version: v }))
        .await
        .unwrap();
    let before = applies(&w);
    let outcome = run(&sw, plan).await.unwrap();
    assert_eq!(outcome.status, OutcomeStatus::Switched);
    assert_eq!(applies(&w), before);
}

#[tokio::test]
async fn a_pin_without_a_catalog_file_does_not_check_one() {
    let w = World::new();
    let sw = w.switcher();
    let v = CodexVersion::new(0, 163, 0);
    w.v3.versions.add_stored(v.clone());
    let plan = sw
        .plan(pin_request(AppCodexPin::Version { version: v }))
        .await
        .unwrap();
    assert!(!keys(&plan.warnings).contains(&"switch.pin.catalog_check".to_owned()));
}

#[tokio::test]
#[cfg_attr(
    windows,
    ignore = "pinning the app's Codex is not offered on Windows (PIN_SUPPORTED)"
)]
async fn a_pin_whose_catalog_cannot_be_repaired_is_blocked_before_anything_changes() {
    let w = World::new();
    let sw = w.switcher();
    let f1 = applied_with_catalog(&w, &sw).await;
    let v = CodexVersion::new(0, 163, 0);
    w.v3.versions.add_stored(v.clone());
    w.app.refused_catalogs.lock().unwrap().push(f1);
    let plan = sw
        .plan(pin_request(AppCodexPin::Version { version: v }))
        .await
        .unwrap();
    // Neither a rebuilt file nor a configuration without one can be planned.
    *w.config.fail_plan.lock().unwrap() = Some(Error::other("cannot plan"));
    let before = w.calls().len();
    match run(&sw, plan).await {
        Err(Error::Blocked { reasons }) => assert!(
            reasons
                .iter()
                .any(|r| matches!(r, BlockReason::ModelCatalog { .. })),
            "{reasons:?}"
        ),
        other => panic!("{other:?}"),
    }
    let calls = w.calls();
    assert!(
        calls[before..]
            .iter()
            .all(|c| !c.starts_with("app.") || c.starts_with("app.preflight")),
        "the app is not touched: {:?}",
        &calls[before..]
    );
    assert_eq!(
        w.profiles.set.lock().unwrap().settings.app_codex.pin,
        AppCodexPin::Bundled
    );
}

// ---------------------------------------------------------------------------
// Start-up repair (#2)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_start_up_repair_keeps_the_users_own_values_and_rebuilds_the_file() {
    let w = World::new();
    let sw = w.switcher();
    let f1 = applied_with_catalog(&w, &sw).await;
    // The terminal Codex's /fast and /model rewrote owned keys since.
    w.config.state.lock().unwrap().drift = vec![
        KeyChange {
            path: "service_tier".into(),
            before: Some("\"default\"".into()),
            after: Some("\"fast\"".into()),
        },
        KeyChange {
            path: "model".into(),
            before: None,
            after: Some("\"gpt-6-astra\"".into()),
        },
    ];
    // The app updated its Codex, which refuses the old file.
    w.app.installs.lock().unwrap()[0].bundled_codex_version = Some(CodexVersion::new(0, 164, 0));
    w.app.refused_catalogs.lock().unwrap().push(f1);
    let before = applies(&w);
    let report = sw.recover().await.unwrap();
    let k = keys(&report.notices);
    assert!(
        k.contains(&"switch.models.consumer_changed".to_owned()),
        "{k:?}"
    );
    assert!(
        k.contains(&"switch.models.catalog_rebuilt".to_owned()),
        "{k:?}"
    );
    assert_eq!(applies(&w), before + 1);
}

#[tokio::test]
async fn a_start_up_catalog_that_cannot_be_repaired_is_a_danger_notice() {
    let w = World::new();
    let sw = w.switcher();
    let f1 = applied_with_catalog(&w, &sw).await;
    w.app.installs.lock().unwrap()[0].bundled_codex_version = Some(CodexVersion::new(0, 164, 0));
    w.app.refused_catalogs.lock().unwrap().push(f1);
    *w.config.fail_plan.lock().unwrap() = Some(Error::other("cannot plan"));
    let report = sw.recover().await.unwrap();
    let danger = report
        .notices
        .iter()
        .find(|n| n.text.key == "switch.models.catalog_unrepaired")
        .expect("a danger notice");
    assert_eq!(danger.severity, Severity::Danger);
    let text = danger.text.render_fallback();
    assert!(text.contains("model_catalog_json"), "{text}");
    assert!(text.contains("0.164.0"), "{text}");
}

// ---------------------------------------------------------------------------
// IDE extensions (#5)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_ide_codex_is_preflighted_and_a_refusal_is_reported() {
    let w = World::new();
    let ide = PathBuf::from("/home/u/.vscode/extensions/openai.chatgpt-9.9.9/bin/codex");
    w.app.processes.lock().unwrap().push(ProcessInfo {
        pid: 4242,
        ppid: None,
        kind: CodexProcessKind::IdeAppServer,
        exe: Some(ide.clone()),
        cmdline: Vec::new(),
        started_at: None,
        authapi_env: Default::default(),
        codex_home: None,
        stale: false,
    });
    w.app.refusing_binaries.lock().unwrap().push(ide.clone());
    work(&w, with_window);
    let sw = w.switcher();
    let plan = sw.plan(req("work")).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert!(w.app.preflights.lock().unwrap().contains(&ide));
    let warning = plan
        .warnings
        .iter()
        .find(|n| n.text.key == "switch.models.ide_refuses")
        .expect("the refusal is reported");
    assert!(
        warning
            .text
            .render_fallback()
            .contains(&ide.display().to_string())
    );
}

// ---------------------------------------------------------------------------
// Tier-suffix names (#6) and old Codex versions (#7)
// ---------------------------------------------------------------------------

/// A switcher whose `work` profile is on a Krill copy with tier-suffix names.
fn suffix_switcher(w: &World) -> Switcher {
    let mut sites = superba_core::catalog::builtin_sites();
    let mut krill = sites.remove(0);
    krill.id = SiteId::new("suffix-test");
    krill.capabilities.tier_model_suffix = true;
    let path = w.tmp.path().join("sites.json");
    std::fs::write(&path, serde_json::to_vec(&vec![krill]).unwrap()).unwrap();
    let mut deps = w.deps();
    deps.ctx = superba_core::LocalContext::new(
        w.ctx.home_dir.clone(),
        w.ctx.data_dir.clone(),
        Arc::new(
            superba_core::MapEnv::new()
                .with(superba_switch::ENV_SUPERBA_TEST_SITES, path.as_os_str()),
        ),
    );
    Switcher::with_tuning(deps, w.switcher().tuning().clone())
}

#[tokio::test]
async fn a_tier_suffix_model_written_last_time_is_not_suffixed_twice() {
    let w = World::new();
    work(&w, |p| {
        p.site_id = SiteId::new("suffix-test");
        p.line_id = None;
        p.model.model = None;
        p.model.speed = Some(SpeedTier::Ultrafast);
    });
    // The previous switch wrote the Fast name; the profile has no default model.
    w.config.state.lock().unwrap().model = Some("gpt-6-astra-fast".into());
    let sw = suffix_switcher(&w);
    let plan = sw.plan(req("work")).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    assert_eq!(
        desired(&plan).model.model.as_deref(),
        Some("gpt-6-astra-ultrafast")
    );
}

#[tokio::test]
async fn an_old_terminal_codex_gets_no_service_tier_it_cannot_parse() {
    let w = World::new();
    w.app.binaries.lock().unwrap()[0].version = Some(CodexVersion::new(0, 125, 0));
    work(&w, |_| {});
    let sw = w.switcher();
    // The user lets the switch go ahead for the app only (cli_too_old).
    let mut r = req("work");
    r.accept_unknown_versions = true;
    let plan = sw.plan(r.clone()).await.unwrap();
    assert_eq!(desired(&plan).model.speed, None);
    assert!(
        keys(&plan.warnings).contains(&"switch.models.service_tier_old_codex".to_owned()),
        "{:?}",
        keys(&plan.warnings)
    );
    // `fast` is a value those versions know.
    work(&w, |p| p.model.speed = Some(SpeedTier::Fast));
    let plan = sw.plan(r.clone()).await.unwrap();
    assert_eq!(desired(&plan).model.speed, Some(SpeedTier::Fast));

    // A current Codex gets `default` for Standard.
    w.app.binaries.lock().unwrap()[0].version = Some(CodexVersion::new(0, 162, 0));
    work(&w, |p| p.model.speed = None);
    let plan = sw.plan(r).await.unwrap();
    assert_eq!(desired(&plan).model.speed, Some(SpeedTier::Standard));
}
