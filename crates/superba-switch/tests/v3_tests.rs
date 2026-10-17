//! v0.3 switch service over in-memory fakes: model catalog files and speed
//! tiers, the app Codex pin, remote hosts (app-known only, follow, settings
//! sync), the request log, session names, the Updates page, wizard re-runs.

#![allow(missing_docs)]

mod fakes;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fakes::v3::files_in;
use fakes::*;
use pretty_assertions::assert_eq;
use superba_core::{
    AppCodexPin, AuthChoice, AuthMode, BlockReason, CodexVersion, ExecuteOptions, HostSyncAction,
    HostSyncSelection, InstallRequest, InstallTarget, ModelOverride, ModelPicker, ModelsEdit,
    ModelsViewRequest, NoEvents, OutcomeStatus, ProfileId, RemoteApplyScope, ResolvedMode,
    RestartKind, SafeDefaults, SettingEntry, SettingValue, SpeedEffect, SpeedTier, SwitchIntent,
    SwitchPlan, SwitchRequest, SwitchService, TargetId, codex::ENV_CODEX_CLI_PATH,
};
use superba_switch::Switcher;

/// The Codex executable file name of the platform.
const CODEX_EXE: &str = if cfg!(windows) { "codex.exe" } else { "codex" };

fn req(profile: &str) -> SwitchRequest {
    SwitchRequest::profile(ProfileId::new(profile))
}

fn desired(plan: &SwitchPlan) -> &superba_core::ResolvedProfile {
    plan.config
        .as_ref()
        .and_then(|c| c.desired.as_ref())
        .expect("config plan")
}

fn warning_keys(plan: &SwitchPlan) -> Vec<String> {
    plan.warnings.iter().map(|n| n.text.key.clone()).collect()
}

/// Edit the `work` profile as a v2 profile.
fn set_profile(w: &World, f: impl FnOnce(&mut superba_core::Profile)) {
    let mut set = w.profiles.set.lock().unwrap();
    let p = set
        .profiles
        .iter_mut()
        .find(|p| p.id.as_str() == "work")
        .unwrap();
    f(p);
}

fn api_key(p: &mut superba_core::Profile) {
    p.auth_mode = AuthMode::ApiKey;
    p.set_auth(AuthChoice::ApiKey);
}

fn window(n: u64) -> ModelOverride {
    ModelOverride {
        context_window: Some(n),
        ..ModelOverride::default()
    }
}

async fn execute(sw: &Switcher, plan: SwitchPlan) -> superba_core::SwitchOutcome {
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
    .unwrap()
}

fn superba_dir(w: &World) -> PathBuf {
    w.ctx.codex_home().superba_dir()
}

// ---------------------------------------------------------------------------
// Speed tiers and the model catalog file
// ---------------------------------------------------------------------------

#[tokio::test]
async fn key_based_profiles_always_write_the_speed_tier_and_no_catalog_by_default() {
    let w = World::new();
    let sw = w.switcher();
    let plan = sw.plan(req("work")).await.unwrap();
    let d = desired(&plan);
    assert_eq!(d.model.speed, Some(SpeedTier::Standard));
    assert_eq!(d.model_catalog, None);
    assert!(w.v3.builder.calls.lock().unwrap().is_empty());

    set_profile(&w, |p| p.model.speed = Some(SpeedTier::Fast));
    let plan = sw.plan(req("work")).await.unwrap();
    assert_eq!(desired(&plan).model.speed, Some(SpeedTier::Fast));
}

#[tokio::test]
async fn an_override_writes_a_preflighted_content_addressed_catalog_before_config() {
    let w = World::new();
    set_profile(&w, |p| {
        api_key(p);
        p.model_overrides
            .insert("gpt-6-astra".into(), window(400_000));
    });
    let sw = w.switcher();
    let plan = sw.plan(req("work")).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    let d = desired(&plan);
    let path = d.model_catalog.clone().expect("catalog path");
    assert_eq!(path.parent().unwrap(), superba_dir(&w));
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("models-") && name.ends_with(".json"),
        "{name}"
    );
    // Built for the app's bundled Codex, preflighted with the app's and the
    // CLI's binaries (different versions), nothing written while planning.
    let call = w.v3.builder.calls.lock().unwrap()[0].clone();
    assert_eq!(
        call.consumer,
        CodexVersion::parse("0.162.0-alpha.17.2").unwrap()
    );
    assert_eq!(call.source, "bundled");
    assert_eq!(call.overrides["gpt-6-astra"].context_window, Some(400_000));
    assert_eq!(
        *w.app.preflights.lock().unwrap(),
        vec![
            PathBuf::from("/opt/chatgpt/resources/codex"),
            PathBuf::from("/usr/local/bin/codex")
        ]
    );
    assert!(!path.exists());
    assert!(files_in(&w.ctx.data_dir.root().join("tmp")).is_empty());
    assert!(warning_keys(&plan).contains(&"switch.models.fingerprint".to_owned()));

    let outcome = execute(&sw, plan).await;
    assert_eq!(outcome.status, OutcomeStatus::Switched);
    assert!(path.exists(), "the file exists once config.toml names it");
}

#[tokio::test]
async fn a_preflight_failure_blocks_the_switch_and_writes_nothing() {
    let w = World::new();
    set_profile(&w, |p| {
        api_key(p);
        p.model_overrides
            .insert("gpt-6-astra".into(), window(400_000));
    });
    *w.app.preflight_error.lock().unwrap() = Some(superba_core::Error::parse(
        "model_catalog_json",
        "missing field `base_instructions`",
    ));
    let sw = w.switcher();
    let plan = sw.plan(req("work")).await.unwrap();
    assert!(plan.is_blocked());
    match &plan.primary {
        RestartKind::Blocked { reasons } => assert!(reasons.iter().any(|r| matches!(
            r,
            BlockReason::ModelCatalog { reason }
                if reason.render_fallback().contains("base_instructions")
        ))),
        other => panic!("{other:?}"),
    }
    assert!(plan.config.is_none());
    assert!(w.writes().is_empty());
    assert!(files_in(&superba_dir(&w)).is_empty());
    assert!(files_in(&w.ctx.data_dir.root().join("tmp")).is_empty());
}

#[tokio::test]
async fn catalog_triggers_and_where_a_catalog_is_never_written() {
    let w = World::new();
    let sw = w.switcher();
    // PAT (the default profile): an override gives no catalog, with a note.
    set_profile(&w, |p| {
        p.model_overrides
            .insert("gpt-6-astra".into(), window(400_000));
    });
    let plan = sw.plan(req("work")).await.unwrap();
    assert_eq!(desired(&plan).model_catalog, None);
    assert!(warning_keys(&plan).contains(&"switch.models.pat_no_catalog".to_owned()));

    // API key + Ultrafast needs one.
    set_profile(&w, |p| {
        api_key(p);
        p.model_overrides.clear();
        p.model.speed = Some(SpeedTier::Ultrafast);
    });
    let plan = sw.plan(req("work")).await.unwrap();
    assert!(desired(&plan).model_catalog.is_some());
    assert_eq!(desired(&plan).model.speed, Some(SpeedTier::Ultrafast));

    // The official login never gets one.
    let plan = sw
        .plan(req(superba_core::catalog::PROFILE_OFFICIAL))
        .await
        .unwrap();
    assert_eq!(desired(&plan).model_catalog, None);
    assert_eq!(desired(&plan).model.speed, None);

    // Remote hosts never get model keys.
    w.remote
        .hosts
        .lock()
        .unwrap()
        .push(remote_host("dev", Some("h-dev")));
    let mut r = req("work");
    r.targets.remotes = vec![TargetId::remote("dev")];
    sw.plan(r).await.unwrap();
    let scoped = w.remote.scoped.lock().unwrap().clone();
    let (_, scope, remote) = scoped.last().unwrap();
    assert_eq!(*scope, RemoteApplyScope::Connection);
    assert_eq!(remote.model_catalog, None);
    assert_eq!(remote.model.speed, None);
    assert!(remote.settings.is_empty());
}

#[tokio::test]
async fn reasons_are_pure_and_cover_the_picker_and_tier_suffix() {
    use superba_switch::models::{CatalogReason, catalog_reasons};
    let mut site = superba_core::catalog::compatible_template_site();
    let mut p = krill_profile("x", None, AuthMode::ApiKey);
    p.site_id = site.id.clone();
    assert!(catalog_reasons(&p, &site, AuthMode::ApiKey).is_empty());
    p.model_picker = ModelPicker::Provider;
    assert_eq!(
        catalog_reasons(&p, &site, AuthMode::ApiKey),
        vec![CatalogReason::ProviderPicker]
    );
    p.model_picker = ModelPicker::CodexDefault;
    p.model.speed = Some(SpeedTier::Fast);
    assert!(catalog_reasons(&p, &site, AuthMode::ApiKey).is_empty());
    site.capabilities.tier_model_suffix = true;
    assert_eq!(
        catalog_reasons(&p, &site, AuthMode::ApiKey),
        vec![CatalogReason::TierSuffix]
    );
    // PAT never: the app's own speed menu works there.
    assert!(catalog_reasons(&p, &site, AuthMode::Pat).is_empty());
    // An upstream name alone edits the model map, not the catalog.
    p.model.speed = None;
    p.model_overrides.insert(
        "m".into(),
        ModelOverride {
            upstream_name: Some("site-m".into()),
            ..ModelOverride::default()
        },
    );
    assert!(catalog_reasons(&p, &site, AuthMode::ApiKey).is_empty());
}

#[tokio::test]
async fn tier_suffix_lines_carry_the_tier_in_the_model_name() {
    let w = World::new();
    let mut sites = superba_core::catalog::builtin_sites();
    let mut krill = sites.remove(0);
    krill.id = superba_core::SiteId::new("suffix-test");
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
    set_profile(&w, |p| {
        api_key(p);
        p.site_id = superba_core::SiteId::new("suffix-test");
        p.line_id = None;
        p.model.model = Some("gpt-6-astra".into());
        p.model.speed = Some(SpeedTier::Ultrafast);
    });
    let sw = Switcher::with_tuning(deps, w.switcher().tuning().clone());
    let plan = sw.plan(req("work")).await.unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    let d = desired(&plan);
    assert_eq!(d.model.model.as_deref(), Some("gpt-6-astra-ultrafast"));
    assert_eq!(d.model.speed, Some(SpeedTier::Standard));
    assert!(d.model_catalog.is_some());
    assert!(w.v3.builder.calls.lock().unwrap()[0].suffix);
}

#[tokio::test]
async fn globals_fold_into_the_catalog_and_are_never_written_with_per_model_values() {
    let w = World::new();
    set_profile(&w, |p| {
        api_key(p);
        p.model.context_window = Some(300_000);
        p.model.auto_compact = Some(200_000);
    });
    let sw = w.switcher();
    // Only the all-models row: the global keys, no catalog.
    let plan = sw.plan(req("work")).await.unwrap();
    assert_eq!(desired(&plan).model.context_window, Some(300_000));
    assert_eq!(desired(&plan).model_catalog, None);

    // A per-model value too: the global ones fold into the file.
    set_profile(&w, |p| {
        p.model_overrides
            .insert("gpt-6-astra".into(), window(500_000));
    });
    let plan = sw.plan(req("work")).await.unwrap();
    let d = desired(&plan);
    assert!(d.model_catalog.is_some());
    assert_eq!(d.model.context_window, None);
    assert_eq!(d.model.auto_compact, None);
    let call = w.v3.builder.calls.lock().unwrap().last().cloned().unwrap();
    assert_eq!(call.overrides["gpt-6-astra"].context_window, Some(500_000));
    assert_eq!(call.overrides["gpt-6-astra"].auto_compact, Some(200_000));
    assert_eq!(call.overrides["gpt-6-luna"].context_window, Some(300_000));
}

#[tokio::test]
async fn pruning_keeps_the_referenced_and_the_previous_file() {
    let w = World::new();
    let sw = w.switcher();
    let mut names = Vec::new();
    for n in [300_000u64, 400_000, 500_000] {
        set_profile(&w, |p| {
            api_key(p);
            p.model_overrides.insert("gpt-6-astra".into(), window(n));
        });
        let plan = sw.plan(req("work")).await.unwrap();
        let path = desired(&plan).model_catalog.clone().unwrap();
        names.push(path.file_name().unwrap().to_string_lossy().into_owned());
        assert_eq!(execute(&sw, plan).await.status, OutcomeStatus::Switched);
    }
    let mut expected = vec![names[1].clone(), names[2].clone()];
    expected.sort();
    assert_eq!(files_in(&superba_dir(&w)), expected);

    // Without a catalog the key is released; the last file stays as previous.
    set_profile(&w, |p| p.model_overrides.clear());
    let plan = sw.plan(req("work")).await.unwrap();
    assert_eq!(desired(&plan).model_catalog, None);
    execute(&sw, plan).await;
    assert_eq!(files_in(&superba_dir(&w)), vec![names[2].clone()]);

    // The next catalog: the configuration before it referenced no file, so the
    // released one is pruned (e2e s18: it stayed until yet another catalog).
    set_profile(&w, |p| {
        p.model_overrides
            .insert("gpt-6-astra".into(), window(600_000));
    });
    let plan = sw.plan(req("work")).await.unwrap();
    let path = desired(&plan).model_catalog.clone().unwrap();
    assert_eq!(execute(&sw, plan).await.status, OutcomeStatus::Switched);
    assert_eq!(
        files_in(&superba_dir(&w)),
        vec![path.file_name().unwrap().to_string_lossy().into_owned()]
    );
}

#[tokio::test]
async fn the_models_view_shows_the_list_source_speed_effect_and_catalog_status() {
    let w = World::new();
    set_profile(&w, |p| {
        api_key(p);
        p.model.speed = Some(SpeedTier::Fast);
    });
    let sw = w.switcher();
    let view = sw
        .models_view(ModelsViewRequest {
            profile_id: Some(ProfileId::new("work")),
            refresh: false,
        })
        .await
        .unwrap();
    assert!(!view.read_only);
    assert!(matches!(
        view.list.source,
        superba_core::ModelListSource::Bundled { .. }
    ));
    assert_eq!(view.list.models.len(), 2);
    assert_eq!(
        view.speed.offered,
        vec![SpeedTier::Standard, SpeedTier::Fast, SpeedTier::Ultrafast]
    );
    assert_eq!(view.speed.cli, SpeedEffect::Applies);
    assert_eq!(view.speed.app, SpeedEffect::AppForcesStandard);
    // Every tier's own effect, so a tier picked but not saved shows what it
    // will do (review ux-visual #0).
    assert_eq!(
        view.speed.effect(SpeedTier::Standard),
        (&SpeedEffect::Applies, &SpeedEffect::Applies)
    );
    assert_eq!(
        view.speed.effect(SpeedTier::Fast),
        (&SpeedEffect::Applies, &SpeedEffect::AppForcesStandard)
    );
    assert_eq!(
        view.catalog_file,
        superba_core::CatalogFileStatus::NotNeeded
    );

    // The official login is read-only and decided by the app's menu.
    let view = sw
        .models_view(ModelsViewRequest {
            profile_id: Some(ProfileId::new(superba_core::catalog::PROFILE_OFFICIAL)),
            refresh: false,
        })
        .await
        .unwrap();
    assert!(view.read_only);
    assert_eq!(view.speed.app, SpeedEffect::AppMenu);
}

#[tokio::test]
async fn without_a_catalog_file_an_api_key_cli_follows_the_bundled_tiers() {
    // An API-key Codex never fetches the provider's list: without a catalog file
    // it filters the tier with the model's bundled entry. A provider list without
    // tier data (an OpenAI list) must not hide Fast from the terminal Codex (e2e
    // s19: `codex exec` sent `priority` while the view said "not offered").
    let w = World::new();
    set_profile(&w, |p| {
        api_key(p);
        p.model.speed = Some(SpeedTier::Fast);
        p.model.model = Some("gpt-6-luna".into());
    });
    *w.v3.feeds.models.lock().unwrap() = ["gpt-6-luna", "gpt-6-astra"]
        .iter()
        .filter_map(|s| {
            superba_switch::models::spec_from_entry(
                &serde_json::json!({"slug": s, "display_name": s}),
            )
        })
        .collect();
    let sw = w.switcher();
    let view = sw.models_view(ModelsViewRequest::default()).await.unwrap();
    assert!(matches!(
        view.list.source,
        superba_core::ModelListSource::Feed { .. }
    ));
    assert_eq!(
        view.speed.offered,
        vec![SpeedTier::Standard, SpeedTier::Fast]
    );
    assert_eq!(view.speed.cli, SpeedEffect::Applies);
    assert_eq!(view.speed.app, SpeedEffect::AppForcesStandard);
    // Ultrafast needs a catalog file, built from the provider's list: no tier there.
    set_profile(&w, |p| p.model.speed = Some(SpeedTier::Ultrafast));
    let view = sw.models_view(ModelsViewRequest::default()).await.unwrap();
    assert_eq!(view.speed.cli, SpeedEffect::NotOffered);
    // Fast picked from there writes no catalog file: the bundled list decides it.
    assert_eq!(
        view.speed.effect(SpeedTier::Fast),
        (&SpeedEffect::Applies, &SpeedEffect::AppForcesStandard)
    );
    assert_eq!(
        view.speed.effect(SpeedTier::Ultrafast).0,
        &SpeedEffect::NotOffered
    );
    // A PAT Codex fetches the provider's list itself: its tiers decide.
    set_profile(&w, |p| {
        p.auth_mode = AuthMode::Pat;
        p.set_auth(AuthChoice::Pat);
        p.model.speed = Some(SpeedTier::Fast);
    });
    let view = sw.models_view(ModelsViewRequest::default()).await.unwrap();
    assert_eq!(view.speed.offered, vec![SpeedTier::Standard]);
    assert_eq!(view.speed.cli, SpeedEffect::NotOffered);
}

#[tokio::test]
async fn save_models_validates_and_stores_overrides_defaults_and_picker() {
    let w = World::new();
    set_profile(&w, api_key);
    let sw = w.switcher();
    // The view caches the list the overrides are validated against.
    sw.models_view(ModelsViewRequest::default()).await.unwrap();
    let too_small = sw
        .save_models(ModelsEdit {
            set: [("gpt-6-astra".to_owned(), window(1_000))].into(),
            ..ModelsEdit::default()
        })
        .await;
    assert!(too_small.is_err());
    let too_big = sw
        .save_models(ModelsEdit {
            set: [("gpt-6-astra".to_owned(), window(2_000_000))].into(),
            ..ModelsEdit::default()
        })
        .await;
    assert!(too_big.is_err());
    let report = sw
        .save_models(ModelsEdit {
            set: [(
                "gpt-6-astra".to_owned(),
                ModelOverride {
                    context_window: Some(400_000),
                    auto_compact: Some(390_000),
                    ..ModelOverride::default()
                },
            )]
            .into(),
            ..ModelsEdit::default()
        })
        .await
        .unwrap();
    let keys: Vec<String> = report.notes.iter().map(|n| n.text.key.clone()).collect();
    assert!(
        keys.contains(&"switch.models.compact_clamped".to_owned()),
        "{keys:?}"
    );
    assert!(keys.contains(&"switch.models.catalog_from_next_switch".to_owned()));
    assert_eq!(
        report.profile.model_overrides["gpt-6-astra"].context_window,
        Some(400_000)
    );
    // The picker is for custom sites only.
    let picker = sw
        .save_models(ModelsEdit {
            picker: Some(ModelPicker::Provider),
            ..ModelsEdit::default()
        })
        .await;
    assert!(picker.is_err());
    // Reset all.
    let report = sw
        .save_models(ModelsEdit {
            reset_all: true,
            ..ModelsEdit::default()
        })
        .await
        .unwrap();
    assert!(report.profile.model_overrides.is_empty());
}

#[tokio::test]
async fn a_saved_standard_tier_still_tells_what_fast_does_in_the_app() {
    // Review ux-visual #0: an API-key profile at Standard; the Models page shows
    // Fast's effect as soon as it is picked, before saving.
    let w = World::new();
    set_profile(&w, |p| {
        api_key(p);
        p.model.speed = None;
    });
    let view = w
        .switcher()
        .models_view(ModelsViewRequest {
            profile_id: Some(ProfileId::new("work")),
            refresh: false,
        })
        .await
        .unwrap();
    assert_eq!(view.speed.app, SpeedEffect::Applies);
    assert_eq!(
        view.speed.effect(SpeedTier::Fast).1,
        &SpeedEffect::AppForcesStandard
    );
    assert_eq!(
        view.speed.tiers.iter().map(|t| t.tier).collect::<Vec<_>>(),
        SpeedTier::ALL.to_vec()
    );
}

#[test]
fn tier_effects_follow_each_tier() {
    use superba_switch::models::tier_effects;
    let list = superba_core::ModelList {
        source: superba_core::ModelListSource::OfficialReadOnly,
        models: serde_json::from_str::<serde_json::Value>(&fakes::v3::bundled_dump(
            &["gpt-6-astra"],
            true,
        ))
        .unwrap()["models"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(superba_switch::models::spec_from_entry)
            .collect(),
        fallback_reason: None,
    };
    let effects = |pat, suffix| {
        tier_effects(&list, |_| None, Some("gpt-6-astra"), false, pat, suffix)
            .into_iter()
            .map(|t| (t.tier, t.app))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        effects(false, false),
        vec![
            (SpeedTier::Standard, SpeedEffect::Applies),
            (SpeedTier::Fast, SpeedEffect::AppForcesStandard),
            (SpeedTier::Ultrafast, SpeedEffect::AppForcesStandard),
        ]
    );
    assert_eq!(
        effects(false, true)[2].1,
        SpeedEffect::ViaModelName {
            model: "gpt-6-astra-ultrafast".into()
        }
    );
    assert!(
        effects(true, false)
            .iter()
            .all(|(_, e)| *e == SpeedEffect::AppMenu)
    );
}

#[test]
fn speed_support_is_per_consumer() {
    use superba_switch::models::speed_support;
    let list = superba_core::ModelList {
        source: superba_core::ModelListSource::OfficialReadOnly,
        models: superba_core::ModelList {
            source: superba_core::ModelListSource::OfficialReadOnly,
            models: Vec::new(),
            fallback_reason: None,
        }
        .models
        .into_iter()
        .chain(
            serde_json::from_str::<serde_json::Value>(&fakes::v3::bundled_dump(
                &["gpt-6-astra", "gpt-6-luna"],
                true,
            ))
            .unwrap()["models"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(superba_switch::models::spec_from_entry),
        )
        .collect(),
        fallback_reason: None,
    };
    let s = speed_support(
        SpeedTier::Ultrafast,
        &list,
        Some("gpt-6-luna"),
        false,
        false,
        false,
    );
    assert_eq!(s.cli, SpeedEffect::NotOffered);
    let s = speed_support(
        SpeedTier::Ultrafast,
        &list,
        Some("gpt-6-astra"),
        false,
        false,
        true,
    );
    assert_eq!(s.cli, SpeedEffect::Applies);
    assert_eq!(
        s.app,
        SpeedEffect::ViaModelName {
            model: "gpt-6-astra-ultrafast".into()
        }
    );
    let s = speed_support(
        SpeedTier::Fast,
        &list,
        Some("gpt-6-astra"),
        false,
        true,
        false,
    );
    assert_eq!(s.app, SpeedEffect::AppMenu);
}

// ---------------------------------------------------------------------------
// The app's Codex pin
// ---------------------------------------------------------------------------

fn pin_request(pin: AppCodexPin) -> SwitchRequest {
    let mut r = req("work");
    r.intent = SwitchIntent::AppCodexPin { pin };
    r
}

#[tokio::test]
#[cfg_attr(
    windows,
    ignore = "pinning the app's Codex is not offered on Windows (PIN_SUPPORTED)"
)]
async fn pinning_downloads_first_then_fully_restarts_with_the_executable() {
    let w = World::new();
    let sw = w.switcher();
    let v = CodexVersion::new(0, 162, 0);
    let plan = sw
        .plan(pin_request(AppCodexPin::Version { version: v.clone() }))
        .await
        .unwrap();
    assert!(matches!(plan.primary, RestartKind::FullAppRestart { .. }));
    assert!(warning_keys(&plan).contains(&"switch.pin.download".to_owned()));
    let exe = w.v3.versions.executable(&v);
    *w.app.app_server_exe.lock().unwrap() = Some(exe.clone());
    let outcome = execute(&sw, plan).await;
    assert_eq!(
        outcome.status,
        OutcomeStatus::Switched,
        "{:?}",
        outcome.notices
    );
    let calls = w.calls();
    let ensure = calls
        .iter()
        .position(|c| c.starts_with("versions.ensure"))
        .unwrap();
    let quit = calls
        .iter()
        .position(|c| c.starts_with("app.quit"))
        .unwrap();
    assert!(ensure < quit, "{calls:?}");
    let launch = w.app.launches.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        launch.env.get(ENV_CODEX_CLI_PATH).map(PathBuf::from),
        Some(exe)
    );
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
async fn a_mismatching_app_server_is_reported_and_the_pin_not_stored() {
    let w = World::new();
    let v = CodexVersion::new(0, 162, 0);
    w.v3.versions.add_stored(v.clone());
    // The app-server keeps running the bundled Codex (an rc export wins).
    *w.app.app_server_exe.lock().unwrap() = Some(PathBuf::from("/opt/chatgpt/resources/codex"));
    let sw = w.switcher();
    let plan = sw
        .plan(pin_request(AppCodexPin::Version { version: v }))
        .await
        .unwrap();
    let outcome = execute(&sw, plan).await;
    assert!(
        outcome
            .notices
            .iter()
            .any(|n| n.text.key == "switch.pin.mismatch"),
        "{:?}",
        outcome.notices
    );
    assert_eq!(
        w.profiles.set.lock().unwrap().settings.app_codex.pin,
        AppCodexPin::Bundled
    );
}

#[tokio::test]
#[cfg_attr(
    windows,
    ignore = "pinning the app's Codex is not offered on Windows (PIN_SUPPORTED)"
)]
async fn unpinned_launches_remove_an_inherited_value_and_below_floor_is_blocked() {
    let w = World::new();
    let sw = w.switcher();
    let plan = sw.plan(pin_request(AppCodexPin::Bundled)).await.unwrap();
    execute(&sw, plan).await;
    let launch = w.app.launches.lock().unwrap().last().cloned().unwrap();
    assert!(launch.env_remove.contains(&ENV_CODEX_CLI_PATH.to_owned()));
    assert!(!launch.env.contains_key(ENV_CODEX_CLI_PATH));

    let plan = sw
        .plan(pin_request(AppCodexPin::Version {
            version: CodexVersion::new(0, 140, 0),
        }))
        .await
        .unwrap();
    assert!(plan.is_blocked());

    // Any later full restart keeps the configured pin.
    let v = CodexVersion::new(0, 162, 0);
    let exe = w.v3.versions.add_stored(v.clone());
    w.profiles.set.lock().unwrap().settings.app_codex.pin = AppCodexPin::Version { version: v };
    let mut r = req("work");
    r.intent = SwitchIntent::RestartApp { install_id: None };
    let plan = sw.plan(r).await.unwrap();
    execute(&sw, plan).await;
    let launch = w.app.launches.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        launch.env.get(ENV_CODEX_CLI_PATH).map(PathBuf::from),
        Some(exe)
    );
}

#[tokio::test]
#[cfg_attr(
    windows,
    ignore = "pinning the app's Codex is not offered on Windows (PIN_SUPPORTED)"
)]
async fn the_pinned_version_is_the_app_consumer_for_catalogs() {
    let w = World::new();
    let v = CodexVersion::new(0, 163, 0);
    w.v3.versions.add_stored(v.clone());
    {
        let mut set = w.profiles.set.lock().unwrap();
        set.settings.app_codex.pin = AppCodexPin::Version { version: v.clone() };
    }
    set_profile(&w, |p| {
        api_key(p);
        p.model.speed = Some(SpeedTier::Ultrafast);
    });
    let sw = w.switcher();
    let plan = sw.plan(req("work")).await.unwrap();
    assert!(!plan.is_blocked());
    let call = w.v3.builder.calls.lock().unwrap()[0].clone();
    assert_eq!(call.consumer, v);
    assert_eq!(
        w.app.preflights.lock().unwrap()[0],
        w.v3.versions.executable(&CodexVersion::new(0, 163, 0))
    );
}

// ---------------------------------------------------------------------------
// Remote hosts
// ---------------------------------------------------------------------------

fn ssh_config_host(key: &str) -> superba_core::RemoteHost {
    let mut h = remote_host(key, None);
    h.sources = vec![superba_core::HostSource::SshConfig];
    h
}

#[tokio::test]
async fn ssh_config_only_hosts_are_never_listed_or_switched() {
    let w = World::new();
    w.remote
        .hosts
        .lock()
        .unwrap()
        .extend([remote_host("dev", Some("h-dev")), ssh_config_host("lab")]);
    let sw = w.switcher();
    let hosts = sw.hosts().await.unwrap();
    assert_eq!(hosts.len(), 1);
    assert_eq!(hosts[0].host.label, "dev");
    let overview = sw.overview().await.unwrap();
    assert_eq!(overview.remotes.len(), 1);
    assert!(
        sw.set_host_follow(&TargetId::remote("lab"), true)
            .await
            .is_err()
    );
    let mut r = req("work");
    r.targets.remotes = vec![TargetId::remote("lab")];
    let plan = sw.plan(r).await.unwrap();
    assert!(plan.remotes.is_empty());
}

#[tokio::test]
async fn a_switch_leaves_hosts_alone_unless_they_follow_and_then_only_the_connection() {
    let w = World::new();
    w.remote
        .hosts
        .lock()
        .unwrap()
        .push(remote_host("dev", Some("h-dev")));
    let sw = w.switcher();
    let plan = sw.plan(req("work")).await.unwrap();
    assert!(plan.remotes.is_empty());
    assert!(w.remote.scoped.lock().unwrap().is_empty());

    let view = sw
        .set_host_follow(&TargetId::remote("dev"), true)
        .await
        .unwrap();
    assert!(view.settings.follow_switches);
    let plan = sw.plan(req("work")).await.unwrap();
    assert_eq!(plan.remotes.len(), 1);
    let (host, scope, _) = w.remote.scoped.lock().unwrap()[0].clone();
    assert_eq!(
        (host.as_str(), scope),
        ("dev", RemoteApplyScope::Connection)
    );
    let outcome = execute(&sw, plan).await;
    assert!(w.calls().contains(&"remote.apply dev".to_owned()));
    assert_eq!(outcome.status, OutcomeStatus::Switched);

    sw.set_host_follow(&TargetId::remote("dev"), false)
        .await
        .unwrap();
    assert!(w.profiles.set.lock().unwrap().settings.hosts.is_empty());
}

#[tokio::test]
async fn host_sync_rows_cover_write_skip_remove_and_conflict() {
    use std::collections::BTreeMap;
    use superba_core::{CodexSettings, HostSync, SettingsPlan, SkipReason};
    use superba_switch::hosts::{sync_rows, value_hash};
    let mut wanted = CodexSettings::new();
    wanted.set("model_verbosity", "high").unwrap();
    wanted.set("approval_policy", "never").unwrap();
    wanted.set("features.new", true).unwrap();
    let mut sp = SettingsPlan::default();
    sp.apply.set("model_verbosity", "high").unwrap();
    sp.apply.set("approval_policy", "never").unwrap();
    sp.skipped.push(superba_core::SkippedSetting {
        path: "features.new".into(),
        entry: SettingEntry::Set(SettingValue::Bool(true)),
        reason: SkipReason::NotSupported {
            consumers: vec!["dev 0.150.0".into()],
        },
    });
    let host: BTreeMap<String, SettingValue> = [
        (
            "approval_policy".to_owned(),
            SettingValue::String("on-request".into()),
        ),
        ("hide_agent_reasoning".to_owned(), SettingValue::Bool(true)),
        ("old_key".to_owned(), SettingValue::Bool(true)),
        ("released_key".to_owned(), SettingValue::Bool(true)),
    ]
    .into();
    let last = HostSync {
        paths: vec![
            "hide_agent_reasoning".into(),
            "old_key".into(),
            "released_key".into(),
        ],
        value_hashes: [
            ("approval_policy".to_owned(), value_hash(Some("\"never\""))),
            ("old_key".to_owned(), value_hash(Some("false"))),
        ]
        .into(),
        ..HostSync::default()
    };
    // Superba still owns the keys of the last sync, except one a restore
    // released since (it holds the host's own value again).
    let owned = ["hide_agent_reasoning".to_owned(), "old_key".to_owned()];
    let rows = sync_rows(&wanted, &sp, &host, Some(&last), &owned);
    let action = |p: &str| {
        rows.iter()
            .find(|r| r.path == p)
            .map(|r| (r.action.clone(), r.selected))
    };
    assert_eq!(
        action("model_verbosity"),
        Some((HostSyncAction::Write, true))
    );
    // The host changed approval_policy since the last sync.
    assert_eq!(
        action("approval_policy"),
        Some((HostSyncAction::Conflict, false))
    );
    assert!(matches!(
        action("features.new"),
        Some((HostSyncAction::Skip { .. }, false))
    ));
    assert_eq!(
        action("hide_agent_reasoning"),
        Some((HostSyncAction::Remove, true))
    );
    // Written as false, now true on the host: a conflict, not a silent removal.
    assert_eq!(action("old_key"), Some((HostSyncAction::Conflict, false)));
    assert_eq!(action("released_key"), None);
}

#[tokio::test]
async fn a_host_sync_writes_settings_only_restarts_the_host_and_records_the_sync() {
    let w = World::new();
    set_profile(&w, |p| {
        p.codex_settings.set("model_verbosity", "high").unwrap();
    });
    w.remote
        .hosts
        .lock()
        .unwrap()
        .push(remote_host("dev", Some("h-dev")));
    let sw = w.switcher();
    let preview = sw.host_sync_plan(&TargetId::remote("dev")).await.unwrap();
    assert_eq!(preview.rows.len(), 1);
    let mut r = req("work");
    r.intent = SwitchIntent::HostSync {
        target_id: TargetId::remote("dev"),
        selection: HostSyncSelection::default(),
    };
    let plan = sw.plan(r).await.unwrap();
    assert!(!plan.request.targets.local);
    assert!(plan.config.is_none());
    let (_, scope, desired) = w.remote.scoped.lock().unwrap().last().cloned().unwrap();
    assert_eq!(scope, RemoteApplyScope::Settings);
    assert_eq!(
        desired.settings.get("model_verbosity"),
        Some(&SettingEntry::Set(SettingValue::String("high".into())))
    );
    let outcome = execute(&sw, plan).await;
    assert_eq!(outcome.status, OutcomeStatus::Switched);
    let calls = w.calls();
    assert!(calls.contains(&"remote.apply dev".to_owned()));
    assert!(calls.contains(&"app.hot_restart h-dev".to_owned()));
    assert!(!calls.iter().any(|c| c.starts_with("config.apply")));
    let set = w.profiles.set.lock().unwrap().clone();
    let last = set.settings.hosts[&TargetId::remote("dev")]
        .last_sync
        .clone()
        .unwrap();
    assert_eq!(last.paths, vec!["model_verbosity".to_owned()]);
}

#[tokio::test]
async fn a_host_sync_to_a_host_without_a_running_codex_writes_without_restarting() {
    // e2e s22: the app was not connected to the Docker host, so nothing ran
    // there; the sync waited 15 s for a new app-server, failed and rolled back.
    let w = World::new();
    set_profile(&w, |p| {
        p.codex_settings.set("model_verbosity", "high").unwrap();
    });
    w.remote
        .hosts
        .lock()
        .unwrap()
        .push(remote_host("dev", Some("h-dev")));
    *w.remote.idle.lock().unwrap() = true;
    let sw = w.switcher();
    let mut r = req("work");
    r.intent = SwitchIntent::HostSync {
        target_id: TargetId::remote("dev"),
        selection: HostSyncSelection::default(),
    };
    let plan = sw.plan(r).await.unwrap();
    assert!(
        plan.restarts
            .iter()
            .any(|c| matches!(c.restart, superba_core::RestartKind::NextLaunch)),
        "{:?}",
        plan.restarts
    );
    let outcome = execute(&sw, plan).await;
    let calls = w.calls();
    assert!(calls.contains(&"remote.apply dev".to_owned()), "{calls:?}");
    assert!(
        !calls.iter().any(|c| c.starts_with("app.hot_restart")),
        "{calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.starts_with("remote.rollback")),
        "{calls:?}"
    );
    assert_ne!(outcome.status, OutcomeStatus::Failed);
    let set = w.profiles.set.lock().unwrap().clone();
    assert!(
        set.settings.hosts[&TargetId::remote("dev")]
            .last_sync
            .is_some()
    );
}

#[tokio::test]
async fn a_failed_host_verification_rolls_back_and_records_nothing() {
    let w = World::new();
    set_profile(&w, |p| {
        p.codex_settings.set("model_verbosity", "high").unwrap();
    });
    w.remote
        .hosts
        .lock()
        .unwrap()
        .push(remote_host("dev", Some("h-dev")));
    w.remote
        .verify_ok
        .lock()
        .unwrap()
        .insert("dev".into(), false);
    let sw = w.switcher();
    let mut r = req("work");
    r.intent = SwitchIntent::HostSync {
        target_id: TargetId::remote("dev"),
        selection: HostSyncSelection::default(),
    };
    let plan = sw.plan(r).await.unwrap();
    let outcome = execute(&sw, plan).await;
    assert_eq!(outcome.status, OutcomeStatus::Failed);
    assert!(w.calls().contains(&"remote.rollback dev".to_owned()));
    assert!(w.profiles.set.lock().unwrap().settings.hosts.is_empty());
}

#[tokio::test]
async fn host_sessions_are_local_or_a_read_only_remote_scan() {
    let w = World::new();
    w.remote
        .hosts
        .lock()
        .unwrap()
        .push(remote_host("dev", Some("h-dev")));
    let sw = w.switcher();
    let local = sw.host_sessions(&TargetId::local()).await.unwrap();
    assert!(!local.read_only);
    let remote = sw.host_sessions(&TargetId::remote("dev")).await.unwrap();
    assert!(remote.read_only);
    assert_eq!(
        remote.migrate_command.as_deref(),
        Some("spb sessions migrate --from krill")
    );
}

// ---------------------------------------------------------------------------
// Versions, installs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn versions_rate_candidates_and_offer_cli_actions() {
    let w = World::new();
    let rel = |v: &str, pre: bool| superba_core::CodexRelease {
        version: CodexVersion::parse(v).unwrap(),
        tag: format!("rust-v{v}"),
        date: None,
        prerelease: pre,
        bundled_by: Vec::new(),
        sha256: Default::default(),
    };
    *w.v3.versions.releases.lock().unwrap() = vec![
        rel("0.164.0", false),
        rel("0.163.0", false),
        rel("0.162.0-alpha.17.2", true),
        rel("0.140.0", false),
    ];
    let sw = w.switcher();
    let view = sw.versions(false).await.unwrap();
    let tier = |v: &str| {
        view.app_codex
            .candidates
            .iter()
            .find(|c| c.release.version.to_string() == v)
            .unwrap()
            .tier
    };
    assert_eq!(tier("0.162.0-alpha.17.2"), superba_core::CompatTier::Same);
    assert_eq!(tier("0.163.0"), superba_core::CompatTier::Compatible);
    assert_eq!(tier("0.164.0"), superba_core::CompatTier::Newer);
    assert_eq!(tier("0.140.0"), superba_core::CompatTier::BelowFloor);
    assert_eq!(view.cli.len(), 1);
    assert_eq!(
        view.cli[0].action,
        superba_core::CliUpdateAction::Command {
            command: "npm install -g @openai/codex@0.164.0".into()
        }
    );
    assert!(view.checked_at.is_some());
}

#[tokio::test]
async fn installs_go_to_the_store_locally_or_to_a_host_with_a_hot_restart() {
    let w = World::new();
    let sw = w.switcher();
    let v = CodexVersion::new(0, 162, 0);
    let report = sw
        .install_codex(InstallRequest {
            version: v.clone(),
            target: InstallTarget::Store,
        })
        .await
        .unwrap();
    assert!(Path::new(&report.executable.unwrap()).ends_with(Path::new("bin").join(CODEX_EXE)));

    let mut host = remote_host("dev", Some("h-dev"));
    host.sources = vec![superba_core::HostSource::GlobalState];
    w.remote.hosts.lock().unwrap().push(host);
    // The probe knows the host's architecture.
    sw.host_probe(&TargetId::remote("dev")).await.unwrap();
    let report = sw
        .install_codex(InstallRequest {
            version: v.clone(),
            target: InstallTarget::Remote {
                target_id: TargetId::remote("dev"),
            },
        })
        .await;
    // The fake probe has no architecture: refused before anything is sent.
    assert!(report.is_err());
    assert!(
        !w.calls()
            .iter()
            .any(|c| c.starts_with("remote.install_codex"))
    );

    let report = sw
        .install_codex(InstallRequest {
            version: v,
            target: InstallTarget::Local,
        })
        .await
        .unwrap();
    assert!(
        Path::new(&report.executable.unwrap()).ends_with(Path::new(".local/bin").join(CODEX_EXE))
    );
    assert!(
        w.calls()
            .contains(&"versions.install_standalone 0.162.0".to_owned())
    );
}

#[tokio::test]
async fn rollbacks_go_to_the_standalone_layout_or_a_host_with_a_hot_restart() {
    let w = World::new();
    let sw = w.switcher();
    let report = sw.rollback_codex(InstallTarget::Local).await.unwrap();
    assert_eq!(report.version, CodexVersion::new(0, 150, 1));
    assert!(
        Path::new(&report.executable.unwrap()).ends_with(Path::new(".local/bin").join(CODEX_EXE))
    );
    assert!(
        w.calls()
            .contains(&"versions.rollback_standalone".to_owned())
    );
    // The store keeps versions side by side: nothing to roll back.
    assert!(sw.rollback_codex(InstallTarget::Store).await.is_err());

    let mut host = remote_host("dev", Some("h-dev"));
    host.sources = vec![superba_core::HostSource::GlobalState];
    w.remote.hosts.lock().unwrap().push(host);
    let report = sw
        .rollback_codex(InstallTarget::Remote {
            target_id: TargetId::remote("dev"),
        })
        .await
        .unwrap();
    assert_eq!(report.version, CodexVersion::new(0, 150, 1));
    assert!(w.calls().contains(&"remote.rollback_codex dev".to_owned()));
    // An ssh-config-only host is never touched.
    let mut other = remote_host("cfg", None);
    other.sources = vec![superba_core::HostSource::SshConfig];
    w.remote.hosts.lock().unwrap().push(other);
    assert!(
        sw.rollback_codex(InstallTarget::Remote {
            target_id: TargetId::remote("cfg"),
        })
        .await
        .is_err()
    );
}

// ---------------------------------------------------------------------------
// Request log, session names, feeds
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_request_page_says_whether_the_running_router_records() {
    let w = World::new();
    let sw = w.switcher();
    w.router.page.lock().unwrap().recording = true;
    // Stopped: nothing is recorded, whatever the database says.
    let page = sw.router_requests(Default::default()).await.unwrap();
    assert!(!page.recording);
    *w.router.up.lock().unwrap() = true;
    *w.router.stats_status.lock().unwrap() = Some(superba_core::RouterStatsStatus {
        recording: true,
        ..Default::default()
    });
    assert!(
        sw.router_requests(Default::default())
            .await
            .unwrap()
            .recording
    );
    *w.router.stats_status.lock().unwrap() = Some(superba_core::RouterStatsStatus::default());
    assert!(
        !sw.router_requests(Default::default())
            .await
            .unwrap()
            .recording
    );
}

#[tokio::test]
async fn router_stats_settings_are_stored_and_reach_a_configured_router() {
    let w = World::new();
    let sw = w.switcher();
    let mut stats = superba_core::RouterStatsSettings::default();
    stats.line.enabled = true;
    // Not configured: stored only.
    sw.router_set_stats(stats.clone()).await.unwrap();
    assert!(w.router.stats_configured.lock().unwrap().is_empty());
    assert!(
        w.profiles
            .set
            .lock()
            .unwrap()
            .settings
            .router
            .stats
            .line
            .enabled
    );
    // Configured: handed over with the feed's prices.
    *w.router.port.lock().unwrap() = Some(18_431);
    w.v3.feeds.prices.lock().unwrap().rev = "2026-10-01".into();
    sw.router_set_stats(stats.clone()).await.unwrap();
    let (got, prices) = w.router.stats_configured.lock().unwrap()[0].clone();
    assert_eq!(got, stats);
    assert_eq!(prices.rev, "2026-10-01");
    // No in-flight list from a router that does not run.
    assert!(sw.router_inflight().await.unwrap().is_empty());
    sw.router_clear(superba_core::ClearRequest::default())
        .await
        .unwrap();
    assert_eq!(w.router.clears.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn session_titles_read_the_local_state_database() {
    let w = World::new();
    let home = w.ctx.codex_home();
    std::fs::create_dir_all(home.root()).unwrap();
    let conn = rusqlite_free_db(&home.state_db());
    drop(conn);
    let sw = w.switcher();
    let names = sw
        .session_titles(vec!["019a0000-0000-7000-8000-000000000001".into()])
        .await
        .unwrap();
    let t = &names["019a0000-0000-7000-8000-000000000001"];
    assert_eq!(t.title, "019a0000");
    assert_eq!(t.source, superba_core::TitleSource::ShortKey);
}

/// No database driver here: an empty file stands in (unreadable = short keys).
fn rusqlite_free_db(path: &std::path::Path) -> std::fs::File {
    std::fs::File::create(path).unwrap()
}

#[tokio::test]
async fn feeds_status_and_refresh_go_through_the_feed_source() {
    let w = World::new();
    let sw = w.switcher();
    assert_eq!(sw.feeds_status().await.unwrap().serial, 7);
    sw.refresh_feeds().await.unwrap();
    assert_eq!(*w.v3.feeds.refreshes.lock().unwrap(), 1);
}

#[tokio::test]
async fn the_settings_view_marks_common_rows() {
    let mut w = World::new();
    w.common = vec!["model_verbosity".into()];
    set_profile(&w, |p| {
        p.codex_settings.set("model_verbosity", "high").unwrap();
    });
    let sw = w.switcher();
    let view = sw
        .settings_view(superba_core::SettingsViewRequest::default())
        .await
        .unwrap();
    let row = view
        .rows
        .iter()
        .find(|r| r.path == "model_verbosity")
        .unwrap();
    assert!(row.common);
}

// ---------------------------------------------------------------------------
// Wizard re-runs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_wizard_save_under_the_preset_id_edits_the_existing_profile() {
    let w = World::new();
    let sw = w.switcher();
    let before = w.profiles.set.lock().unwrap().profiles.len();
    let mut p = krill_profile(
        superba_core::catalog::PRESET_KRILL,
        Some("cn-fast"),
        AuthMode::Pat,
    );
    p.name = "Krill".into();
    let saved = sw
        .save_profile(p, Some(superba_core::Secret::new("nb-new-key-0000")))
        .await
        .unwrap();
    assert_eq!(saved.id.as_str(), "work");
    assert_eq!(saved.name, "work");
    assert_eq!(saved.line_id.as_ref().map(|l| l.as_str()), Some("cn-fast"));
    assert_eq!(w.profiles.set.lock().unwrap().profiles.len(), before);
    // Another id is a new profile.
    let p = krill_profile("second", None, AuthMode::Pat);
    sw.save_profile(p, None).await.unwrap();
    assert_eq!(w.profiles.set.lock().unwrap().profiles.len(), before + 1);
}

#[test]
fn every_switch_text_key_used_in_the_sources_exists() {
    let en = superba_core::i18n::flatten_toml(superba_switch::messages::SWITCH_EN).unwrap();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut missing = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let text = std::fs::read_to_string(entry.path()).unwrap();
        for part in text.split("text(\"switch.").skip(1) {
            let key = format!("switch.{}", part.split('"').next().unwrap());
            if !en.contains_key(&key) {
                missing.push(key);
            }
        }
    }
    assert!(missing.is_empty(), "{missing:?}");
}

#[tokio::test]
async fn blocked_reasons_name_the_catalog() {
    let w = World::new();
    set_profile(&w, |p| {
        api_key(p);
        p.model.speed = Some(SpeedTier::Ultrafast);
    });
    w.app.installs.lock().unwrap().clear();
    let sw = w.switcher();
    let plan = sw.plan(req("work")).await.unwrap();
    match &plan.primary {
        RestartKind::Blocked { reasons } => assert!(
            reasons
                .iter()
                .any(|r| matches!(r, BlockReason::ModelCatalog { .. }))
        ),
        other => panic!("{other:?}"),
    }
    let _ = ResolvedMode::Official;
}

#[tokio::test]
#[cfg_attr(
    windows,
    ignore = "pinning the app's Codex is not offered on Windows (PIN_SUPPORTED)"
)]
async fn a_persisted_pin_plans_the_cli_path_variable() {
    let w = World::new();
    w.profiles
        .set
        .lock()
        .unwrap()
        .settings
        .app_codex
        .persist_env = true;
    let v = CodexVersion::new(0, 162, 0);
    let exe = w.v3.versions.add_stored(v.clone());
    let sw = w.switcher();
    let plan = sw
        .plan(pin_request(AppCodexPin::Version { version: v }))
        .await
        .unwrap();
    let env = plan.env.as_ref().expect("an env plan");
    assert_eq!(env.var, ENV_CODEX_CLI_PATH);
    assert!(
        matches!(&env.op, superba_core::EnvOp::Set { value, .. } if value == &exe.display().to_string())
    );
    let plan = sw.plan(pin_request(AppCodexPin::Bundled)).await.unwrap();
    assert!(matches!(
        plan.env.as_ref().map(|e| &e.op),
        Some(superba_core::EnvOp::Clear)
    ));
}

#[tokio::test]
async fn the_overview_says_when_the_app_bundles_a_newer_codex_than_the_pin() {
    let w = World::new();
    w.profiles.set.lock().unwrap().settings.app_codex.pin = AppCodexPin::Version {
        version: CodexVersion::new(0, 150, 1),
    };
    let sw = w.switcher();
    let overview = sw.overview().await.unwrap();
    assert!(
        overview
            .notices
            .iter()
            .any(|n| n.text.key == "switch.pin.bundled_ahead")
    );
}

#[tokio::test]
async fn installing_the_router_hands_it_the_stored_stats_settings() {
    let w = World::new();
    set_profile(&w, api_key);
    let mut stats = superba_core::RouterStatsSettings::default();
    stats.line.enabled = true;
    w.profiles.set.lock().unwrap().settings.router.stats = stats.clone();
    let sw = w.switcher();
    sw.router_install(superba_core::RouterInstallRequest {
        members: vec![ProfileId::new("work")],
    })
    .await
    .unwrap();
    assert_eq!(w.router.stats_configured.lock().unwrap()[0].0, stats);
}

#[tokio::test]
async fn the_start_up_check_keeps_a_valid_catalog_and_rewrites_a_refused_one() {
    let w = World::new();
    set_profile(&w, |p| {
        api_key(p);
        p.model_overrides
            .insert("gpt-6-astra".into(), window(400_000));
    });
    let sw = w.switcher();
    let plan = sw.plan(req("work")).await.unwrap();
    // The fake config engine does not record the applied profile itself.
    w.config.state.lock().unwrap().active = Some(superba_core::ActiveState {
        profile_id: Some(ProfileId::new("work")),
        site_id: Some(superba_core::SiteId::new("krill")),
        line_id: None,
        auth_mode: Some(AuthMode::ApiKey),
        generation: 1,
        applied_at: superba_core::Timestamp(T0),
    });
    execute(&sw, plan).await;
    let before = files_in(&superba_dir(&w));
    assert_eq!(before.len(), 1);

    // The app updated its Codex; the file still loads: only the record moves.
    w.app.installs.lock().unwrap()[0].bundled_codex_version = Some(CodexVersion::new(0, 163, 0));
    let preflights = w.app.preflights.lock().unwrap().len();
    let report = sw.recover().await.unwrap();
    assert!(w.app.preflights.lock().unwrap().len() > preflights);
    assert!(report.notices.is_empty(), "{:?}", report.notices);
    let applies = w
        .calls()
        .iter()
        .filter(|c| c.starts_with("config.apply"))
        .count();
    assert_eq!(applies, 1, "nothing rewritten");

    // Next update: the new Codex refuses the file, then also any rebuilt one:
    // the profile is rewritten without a catalog (write-only, no restart).
    w.app.installs.lock().unwrap()[0].bundled_codex_version = Some(CodexVersion::new(0, 164, 0));
    *w.app.preflight_error.lock().unwrap() = Some(superba_core::Error::parse(
        "model_catalog_json",
        "unknown field",
    ));
    let restarts = w
        .calls()
        .iter()
        .filter(|c| c.starts_with("app.hot_restart"))
        .count();
    let report = sw.recover().await.unwrap();
    // The user is told what happened (the GUI shows these as toasts).
    let keys: Vec<_> = report.notices.iter().map(|n| n.text.key.as_str()).collect();
    assert!(keys.contains(&"switch.models.consumer_changed"), "{keys:?}");
    assert!(keys.contains(&"switch.models.catalog_dropped"), "{keys:?}");
    let applies = w
        .calls()
        .iter()
        .filter(|c| c.starts_with("config.apply"))
        .count();
    assert_eq!(applies, 2, "{:?}", w.calls());
    let after = w
        .calls()
        .iter()
        .filter(|c| c.starts_with("app.hot_restart"))
        .count();
    assert_eq!(after, restarts, "write-only");
}

#[tokio::test]
async fn the_start_up_check_names_the_terminal_codex_when_only_it_changed() {
    let w = World::new();
    set_profile(&w, |p| {
        api_key(p);
        p.model_overrides
            .insert("gpt-6-astra".into(), window(400_000));
    });
    let sw = w.switcher();
    let plan = sw.plan(req("work")).await.unwrap();
    w.config.state.lock().unwrap().active = Some(superba_core::ActiveState {
        profile_id: Some(ProfileId::new("work")),
        site_id: Some(superba_core::SiteId::new("krill")),
        line_id: None,
        auth_mode: Some(AuthMode::ApiKey),
        generation: 1,
        applied_at: superba_core::Timestamp(T0),
    });
    execute(&sw, plan).await;
    // Only the `codex` first on PATH moved to another version, which refuses
    // the file: the notice names that Codex, not the app's (e2e s18 said "made
    // for 0.162.0-alpha.17.2; the app now runs 0.162.0-alpha.17.2").
    w.app.binaries.lock().unwrap()[0].version = Some(CodexVersion::new(0, 163, 0));
    *w.app.preflight_error.lock().unwrap() = Some(superba_core::Error::parse(
        "model_catalog_json",
        "missing field",
    ));
    let report = sw.recover().await.unwrap();
    let changed = report
        .notices
        .iter()
        .find(|n| n.text.key == "switch.models.cli_changed")
        .unwrap_or_else(|| panic!("{:?}", report.notices));
    assert!(
        changed.text.render_fallback().contains("now runs 0.163.0"),
        "{}",
        changed.text.render_fallback()
    );
    assert!(
        !report
            .notices
            .iter()
            .any(|n| n.text.key == "switch.models.consumer_changed")
    );
}

#[tokio::test]
async fn the_settings_view_lists_what_each_host_supports() {
    let w = World::new();
    set_profile(&w, |p| {
        p.codex_settings.set("model_verbosity", "high").unwrap();
    });
    w.remote
        .hosts
        .lock()
        .unwrap()
        .push(remote_host("dev", Some("h-dev")));
    let sw = w.switcher();
    // Without a probe nothing is known about the host.
    let view = sw
        .settings_view(superba_core::SettingsViewRequest::default())
        .await
        .unwrap();
    assert!(view.rows.iter().all(|r| r.host_support.is_empty()));
    sw.host_probe(&TargetId::remote("dev")).await.unwrap();
    let view = sw
        .settings_view(superba_core::SettingsViewRequest::default())
        .await
        .unwrap();
    let row = view
        .rows
        .iter()
        .find(|r| r.path == "model_verbosity")
        .unwrap();
    assert_eq!(row.host_support.len(), 1);
    assert_eq!(row.host_support[0].label, "dev");
}

#[tokio::test]
async fn uninstall_removes_the_catalog_directory() {
    let w = World::new();
    let dir = superba_dir(&w);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("models-0a1b2c3d.json"), b"{}").unwrap();
    let sw = w.switcher();
    let mut r = req("work");
    r.intent = SwitchIntent::Uninstall {
        delete_backups: false,
        provider: None,
    };
    let plan = sw.plan(r).await.unwrap();
    let outcome = execute(&sw, plan).await;
    assert!(
        matches!(
            outcome.status,
            OutcomeStatus::Switched | OutcomeStatus::WrittenOnly
        ),
        "{:?}",
        outcome.status
    );
    assert!(!dir.exists());
}

#[test]
fn the_install_command_names_a_local_file() {
    // e2e s23: before the download only the file name is known, and
    // `sudo apt install chatgpt_x_amd64.deb` would look for a package of that name.
    use superba_core::{AppOs, AppPackage, AppPackageKind};
    use superba_switch::versions::install_command;
    let deb = AppPackage {
        os: AppOs::Linux,
        kind: AppPackageKind::Deb,
        arch: "amd64".into(),
        url: "https://example.invalid/pool/chatgpt_26.1020.10000_amd64.deb".into(),
        sha256: None,
        size: None,
        signature: None,
    };
    assert_eq!(
        install_command(
            &deb,
            std::path::Path::new("chatgpt_26.1020.10000_amd64.deb")
        )
        .as_deref(),
        Some("sudo apt install ./chatgpt_26.1020.10000_amd64.deb")
    );
    assert_eq!(
        install_command(
            &deb,
            std::path::Path::new("/data/downloads/chatgpt_26.1020.10000_amd64.deb")
        )
        .as_deref(),
        Some("sudo apt install /data/downloads/chatgpt_26.1020.10000_amd64.deb")
    );
    let rpm = AppPackage {
        kind: AppPackageKind::Rpm,
        ..deb
    };
    assert_eq!(
        install_command(
            &rpm,
            std::path::Path::new("chatgpt-26.1020.10000-1.x86_64.rpm")
        )
        .as_deref(),
        Some("sudo dnf install ./chatgpt-26.1020.10000-1.x86_64.rpm")
    );
}
