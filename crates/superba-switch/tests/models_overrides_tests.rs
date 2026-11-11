//! Per-model context window and compaction (user bug 7): an override
//! round-trips through the profile store, the model catalog file built for the
//! app's Codex (the real `superba-models` builder over a real 0.162 bundled
//! dump) and the "applied" values the Models page and `spb models` compare
//! with; all-models values are capped per model in the file, as Codex caps
//! `model_context_window`; the bounds the page checks are enforced on save; a
//! personal access token uses the same catalog overrides.

#![allow(missing_docs)]

mod fakes;

use std::sync::Arc;
use std::time::Duration;

use fakes::*;
use pretty_assertions::assert_eq;
use superba_core::{
    AppliedModel, AuthMode, ExecuteOptions, ModelDefaults, ModelOverride, ModelSpec, ModelsEdit,
    ModelsView, ModelsViewRequest, NoEvents, OutcomeStatus, ProfileId, SafeDefaults, SwitchPlan,
    SwitchRequest, SwitchService, model_limits,
};
use superba_switch::models::{ModelsCrate, applied_models};
use superba_switch::{Switcher, Tuning};

/// `codex debug models --bundled` of the Codex the e2e desktop app bundles.
const BUNDLED: &str =
    include_str!("../../superba-models/tests/fixtures/bundled-0.162.0-alpha.17.2.json");

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
}

fn stored(w: &World) -> superba_core::Profile {
    w.profiles
        .set
        .lock()
        .unwrap()
        .profile(&ProfileId::new("work"))
        .cloned()
        .unwrap()
}

fn custom(window: Option<u64>, compact: Option<u64>) -> ModelOverride {
    ModelOverride {
        context_window: window,
        auto_compact: compact,
        ..ModelOverride::default()
    }
}

/// The world's switcher with the real catalog builder and the real bundled
/// list of the app's Codex.
fn switcher(w: &World) -> Switcher {
    *w.app.bundled_dump.lock().unwrap() = BUNDLED.to_owned();
    let mut deps = w.deps();
    deps.models = Arc::new(ModelsCrate);
    Switcher::with_tuning(
        deps,
        Tuning {
            poll: Duration::from_millis(10),
            verify_timeout: Duration::from_millis(200),
            launch_verify_timeout: Duration::from_millis(200),
            stable: Duration::ZERO,
            reask_after: Duration::from_secs(60),
            plan_cache: 16,
            windows_app: false,
        },
    )
}

async fn view(sw: &Switcher) -> ModelsView {
    sw.models_view(ModelsViewRequest::default()).await.unwrap()
}

async fn switch(sw: &Switcher) -> SwitchPlan {
    let plan = sw
        .plan(SwitchRequest::profile(ProfileId::new("work")))
        .await
        .unwrap();
    assert!(!plan.is_blocked(), "{:?}", plan.primary);
    let outcome = sw
        .execute(
            plan.clone(),
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
    plan
}

/// What the config engine leaves in `config.toml` for `plan` (the in-memory
/// fake engine writes no file; the catalog file itself is real).
fn write_config(w: &World, plan: &SwitchPlan) {
    let desired = plan
        .config
        .as_ref()
        .and_then(|c| c.desired.as_ref())
        .unwrap();
    let mut toml = String::from("model_provider = \"openai\"\n");
    if let Some(path) = &desired.model_catalog {
        toml.push_str(&format!(
            "model_catalog_json = {:?}\n",
            path.display().to_string()
        ));
    }
    if let Some(n) = desired.model.context_window {
        toml.push_str(&format!("model_context_window = {n}\n"));
    }
    if let Some(n) = desired.model.auto_compact {
        toml.push_str(&format!("model_auto_compact_token_limit = {n}\n"));
    }
    let home = w.ctx.codex_home();
    std::fs::create_dir_all(home.root()).unwrap();
    std::fs::write(home.config_toml(), toml).unwrap();
}

fn spec<'a>(view: &'a ModelsView, slug: &str) -> &'a ModelSpec {
    view.list.model(slug).unwrap()
}

#[tokio::test]
async fn an_override_round_trips_through_the_store_the_catalog_file_and_the_applied_view() {
    let w = World::new();
    set_profile(&w, api_key);
    let sw = switcher(&w);
    let before = view(&sw).await;
    assert!(before.per_model);
    assert_eq!(before.list.models.len(), 11, "the real bundled list");
    // Not applied yet: nothing to compare with.
    assert_eq!(before.applied, None);

    // Save: gpt-5.5 200K / 150K, every other model 1M (capped per model).
    let report = sw
        .save_models(ModelsEdit {
            defaults: Some(ModelDefaults {
                context_window: Some(1_000_000),
                ..before.defaults.clone()
            }),
            set: [("gpt-5.5".to_owned(), custom(Some(200_000), Some(150_000)))].into(),
            ..ModelsEdit::default()
        })
        .await
        .unwrap();
    // The profile store has exactly what was saved, and the view echoes it.
    let p = stored(&w);
    assert_eq!(report.profile, p);
    assert_eq!(
        p.model_overrides["gpt-5.5"],
        custom(Some(200_000), Some(150_000))
    );
    assert_eq!(p.model.context_window, Some(1_000_000));
    let after = view(&sw).await;
    assert_eq!(after.overrides, p.model_overrides);
    assert_eq!(after.defaults, p.model);

    // The switch builds the catalog file for the app's Codex: the all-models
    // window is folded into the file (no global key), capped per model.
    let plan = switch(&sw).await;
    let desired = plan
        .config
        .as_ref()
        .and_then(|c| c.desired.as_ref())
        .unwrap();
    let path = desired.model_catalog.clone().expect("a catalog file");
    assert_eq!(desired.model.context_window, None);
    let file: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let entries: std::collections::BTreeMap<String, AppliedModel> = file["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(AppliedModel::from_entry)
        .collect();
    let entry = |slug: &str| entries[slug];
    assert_eq!(
        entry("gpt-5.5"),
        AppliedModel {
            hidden: false,
            context_window: Some(200_000),
            max_context_window: Some(272_000),
            auto_compact: Some(150_000),
        }
    );
    // 1M is above these models' maximum: Codex would cap the global key, so the
    // file says the maximum, never a raised one.
    assert_eq!(entry("gpt-6-astra").context_window, Some(872_000));
    assert_eq!(entry("gpt-6-astra").max_context_window, Some(872_000));
    assert_eq!(
        entry("gpt-daybreak-red-latest").context_window,
        Some(372_000)
    );

    // Codex now reads that file: every model's applied values equal the saved
    // ones (the page and `spb models` say "applied").
    write_config(&w, &plan);
    let applied_view = view(&sw).await;
    let applied = applied_view.applied.clone().expect("the applied profile");
    assert_eq!(applied.catalog.as_deref(), Some(path.as_path()));
    for m in &applied_view.list.models {
        let saved = model_limits(
            m,
            applied_view.overrides.get(&m.slug),
            &applied_view.defaults,
        );
        assert_eq!(applied.limits(m).pair(), saved.pair(), "{}", m.slug);
    }
    assert_eq!(
        applied.limits(spec(&applied_view, "gpt-5.5")).pair(),
        (Some(200_000), Some(150_000))
    );

    // A new save is not applied until the next switch: only that model differs.
    sw.save_models(ModelsEdit {
        set: [("gpt-5.5".to_owned(), custom(Some(180_000), Some(150_000)))].into(),
        ..ModelsEdit::default()
    })
    .await
    .unwrap();
    let pending = view(&sw).await;
    let applied = pending.applied.clone().unwrap();
    let differs: Vec<&str> = pending
        .list
        .models
        .iter()
        .filter(|m| {
            applied.limits(m).pair()
                != model_limits(m, pending.overrides.get(&m.slug), &pending.defaults).pair()
        })
        .map(|m| m.slug.as_str())
        .collect();
    assert_eq!(differs, ["gpt-5.5"]);

    // Reset: the override is gone from the store; the next switch writes the
    // listed values again.
    sw.save_models(ModelsEdit {
        reset: vec!["gpt-5.5".into()],
        defaults: Some(ModelDefaults::default()),
        ..ModelsEdit::default()
    })
    .await
    .unwrap();
    assert!(stored(&w).model_overrides.is_empty());
    let plan = switch(&sw).await;
    let desired = plan
        .config
        .as_ref()
        .and_then(|c| c.desired.as_ref())
        .unwrap();
    assert_eq!(desired.model_catalog, None, "no override, no catalog file");
}

#[tokio::test]
async fn save_refuses_values_the_models_page_refuses() {
    let w = World::new();
    set_profile(&w, api_key);
    let sw = switcher(&w);
    view(&sw).await;
    let save = |set: Vec<(&'static str, ModelOverride)>, defaults: Option<ModelDefaults>| {
        let edit = ModelsEdit {
            defaults,
            set: set.into_iter().map(|(s, o)| (s.to_owned(), o)).collect(),
            ..ModelsEdit::default()
        };
        sw.save_models(edit)
    };
    // Compaction not below the window it applies to (its own, else listed).
    let e = save(
        vec![("gpt-5.5", custom(Some(200_000), Some(200_000)))],
        None,
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("gpt-5.5"), "{e}");
    assert!(e.to_string().contains("below the context window"), "{e}");
    assert!(
        save(vec![("gpt-5.5", custom(None, Some(272_000)))], None)
            .await
            .is_err()
    );
    // Above the model's maximum, a tiny threshold, an absurd all-models window.
    assert!(
        save(vec![("gpt-5.5", custom(Some(272_001), None))], None)
            .await
            .is_err()
    );
    assert!(
        save(vec![("gpt-5.5", custom(None, Some(100)))], None)
            .await
            .is_err()
    );
    let all = |w: Option<u64>, c: Option<u64>| ModelDefaults {
        context_window: w,
        auto_compact: c,
        ..ModelDefaults::default()
    };
    assert!(
        save(vec![], Some(all(Some(20_000_000), None)))
            .await
            .is_err()
    );
    assert!(
        save(vec![], Some(all(Some(100_000), Some(100_000))))
            .await
            .is_err()
    );
    // Valid: stored. Then new all-models values that would make it invalid are
    // refused, naming the model.
    save(vec![("gpt-5.5", custom(None, Some(150_000)))], None)
        .await
        .unwrap();
    let e = save(vec![], Some(all(Some(128_000), None)))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("gpt-5.5"), "{e}");
    assert_eq!(
        stored(&w).model_overrides["gpt-5.5"],
        custom(None, Some(150_000))
    );
    assert_eq!(stored(&w).model.context_window, None, "nothing stored");
}

#[tokio::test]
async fn a_personal_access_token_folds_defaults_without_overwriting_per_model_values() {
    let w = World::new();
    set_profile(&w, |p| {
        p.auth_mode = AuthMode::ApiKey;
        p.model_overrides
            .insert("gpt-5.5".into(), custom(Some(200_000), Some(150_000)));
        p.model.context_window = Some(300_000);
        p.model.auto_compact = Some(160_000);
    });
    let sw = switcher(&w);
    assert!(view(&sw).await.per_model);
    let plan = switch(&sw).await;
    let desired = plan
        .config
        .as_ref()
        .and_then(|c| c.desired.as_ref())
        .unwrap();
    assert!(desired.model_catalog.is_some());
    assert_eq!(desired.model.context_window, None);
    assert_eq!(desired.model.auto_compact, None);
    write_config(&w, &plan);
    let v = view(&sw).await;
    let applied = v.applied.as_ref().unwrap();
    assert_eq!(applied.context_window, None);
    assert_eq!(
        applied.limits(spec(&v, "gpt-5.5")).pair(),
        (Some(200_000), Some(150_000))
    );
    assert_eq!(
        applied.limits(spec(&v, "gpt-6-astra")).pair(),
        (Some(300_000), Some(160_000))
    );
}

#[tokio::test]
async fn applied_values_are_read_from_config_toml_and_the_file_it_names() {
    let w = World::new();
    let home = w.ctx.codex_home();
    // Nothing there yet.
    assert_eq!(applied_models(&home).await, Default::default());
    std::fs::create_dir_all(home.superba_dir()).unwrap();
    std::fs::write(
        home.superba_dir().join("models-0a1b2c3d.json"),
        r#"{"models":[{"slug":"gpt-5.5","context_window":200000,"max_context_window":272000,"auto_compact_token_limit":150000},{"slug":""}]}"#,
    )
    .unwrap();
    // A relative path resolves against the Codex home, as Codex does.
    std::fs::write(
        home.config_toml(),
        "model = \"gpt-5.5\"\nmodel_catalog_json = \"superba/models-0a1b2c3d.json\"\nmodel_context_window = 400_000\nmodel_auto_compact_token_limit = 300000\n",
    )
    .unwrap();
    let applied = applied_models(&home).await;
    assert_eq!(applied.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(applied.context_window, Some(400_000));
    assert_eq!(applied.auto_compact, Some(300_000));
    assert_eq!(
        applied.catalog,
        Some(home.root().join("superba/models-0a1b2c3d.json"))
    );
    assert_eq!(
        applied
            .models
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["gpt-5.5"]
    );
    assert_eq!(applied.models["gpt-5.5"].auto_compact, Some(150_000));
}

#[tokio::test]
async fn hidden_override_reaches_catalog_and_unhiding_drops_an_unneeded_catalog() {
    let w = World::new();
    set_profile(&w, api_key);
    let sw = switcher(&w);
    sw.save_models(ModelsEdit {
        set: [(
            "gpt-5.5".into(),
            ModelOverride {
                hidden: true,
                ..Default::default()
            },
        )]
        .into(),
        ..Default::default()
    })
    .await
    .unwrap();
    let plan = switch(&sw).await;
    write_config(&w, &plan);
    let applied = applied_models(&w.ctx.codex_home()).await;
    assert!(applied.catalog.is_some());
    assert!(applied.models["gpt-5.5"].hidden);
    sw.save_models(ModelsEdit {
        reset: vec!["gpt-5.5".into()],
        ..Default::default()
    })
    .await
    .unwrap();
    let plan = switch(&sw).await;
    write_config(&w, &plan);
    assert!(applied_models(&w.ctx.codex_home()).await.catalog.is_none());
}

/// Krill's own list (Codex-native entries) from the bundled dump: astra, and
/// with `terra` the GPT-6.1 Terra a later fetch adds. Terra carries the fields
/// newer upstream entries have (a client version above this Codex, plans);
/// Codex filters a catalog only on `visibility` and `supported_in_api`.
fn krill_native(terra: bool) -> superba_core::ModelList {
    let bundled: serde_json::Value = serde_json::from_str(BUNDLED).unwrap();
    let astra = bundled["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["slug"] == "gpt-6-astra")
        .unwrap()
        .clone();
    let mut entries = vec![astra.clone()];
    if terra {
        let mut t = astra;
        t["slug"] = "gpt-6.1-terra".into();
        t["display_name"] = "GPT-6.1 Terra".into();
        t["minimal_client_version"] = serde_json::json!([0, 999, 0]);
        t["available_in_plans"] = serde_json::json!(["pro", "team"]);
        entries.push(t);
    }
    superba_core::ModelList {
        source: superba_core::ModelListSource::Upstream {
            path: "/backend-api/codex/models?client_version=0.162.0".into(),
            fetched_at: superba_core::Timestamp(0),
            etag: None,
        },
        models: entries
            .iter()
            .filter_map(superba_switch::models::spec_from_entry)
            .collect(),
        fallback_reason: None,
        sources: Vec::new(),
    }
}

/// GH#26 / user bug 132: a custom window makes the switch write a static
/// catalog file; a model the provider lists later is reported as missing from
/// it (nothing is rewritten on its own), and the next apply puts it in with its
/// upstream entry intact, visible in Codex's picker.
#[tokio::test]
async fn a_model_the_provider_adds_later_reaches_the_catalog_at_the_next_apply() {
    let w = World::new();
    set_profile(&w, api_key);
    *w.prober.model_list.lock().unwrap() = Some(Ok(krill_native(false)));
    let sw = switcher(&w);
    sw.save_models(ModelsEdit {
        set: [("gpt-6-astra".to_owned(), custom(Some(400_000), None))].into(),
        ..ModelsEdit::default()
    })
    .await
    .unwrap();
    let plan = switch(&sw).await;
    write_config(&w, &plan);
    let first = plan
        .config
        .as_ref()
        .and_then(|c| c.desired.as_ref())
        .and_then(|d| d.model_catalog.clone())
        .expect("a catalog file");
    let v = view(&sw).await;
    assert!(v.catalog_changes().is_empty(), "{:?}", v.catalog_changes());

    // The provider now lists Terra: the page sees it, Codex's file does not.
    *w.prober.model_list.lock().unwrap() = Some(Ok(krill_native(true)));
    let v = view(&sw).await;
    assert!(v.missing_from_catalog(spec(&v, "gpt-6.1-terra")));
    assert!(!v.missing_from_catalog(spec(&v, "gpt-6-astra")));
    assert_eq!(v.catalog_changes().missing, ["gpt-6.1-terra"]);
    assert_eq!(
        v.applied.as_ref().unwrap().catalog.as_deref(),
        Some(first.as_path()),
        "reading the list rewrites nothing"
    );

    // "Apply now" is a switch to the applied profile: a new file with Terra.
    let plan = switch(&sw).await;
    write_config(&w, &plan);
    let path = plan
        .config
        .as_ref()
        .and_then(|c| c.desired.as_ref())
        .and_then(|d| d.model_catalog.clone())
        .unwrap();
    assert_ne!(path, first);
    let file: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let terra = file["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["slug"] == "gpt-6.1-terra")
        .expect("Terra in the catalog");
    assert_eq!(terra["visibility"], "list");
    assert_eq!(terra["supported_in_api"], true);
    assert_eq!(terra["display_name"], "GPT-6.1 Terra");
    assert_eq!(
        terra["minimal_client_version"],
        serde_json::json!([0, 999, 0])
    );
    assert_eq!(
        terra["available_in_plans"],
        serde_json::json!(["pro", "team"])
    );
    let v = view(&sw).await;
    assert!(v.catalog_changes().is_empty(), "{:?}", v.catalog_changes());
    let applied = v.applied.unwrap();
    assert!(!applied.models["gpt-6.1-terra"].hidden);
    assert_eq!(applied.models["gpt-6-astra"].context_window, Some(400_000));

    // Terra leaves the provider's list again: reported as no longer listed.
    *w.prober.model_list.lock().unwrap() = Some(Ok(krill_native(false)));
    let v = view(&sw).await;
    assert_eq!(v.catalog_changes().removed, ["gpt-6.1-terra"]);
}

/// The provider's own list without any per-model value: no catalog file, so
/// Codex reads the provider's list itself and nothing can be missing.
#[tokio::test]
async fn without_per_model_values_codex_reads_the_providers_list_itself() {
    let w = World::new();
    set_profile(&w, api_key);
    *w.prober.model_list.lock().unwrap() = Some(Ok(krill_native(false)));
    let sw = switcher(&w);
    let plan = switch(&sw).await;
    write_config(&w, &plan);
    assert_eq!(
        plan.config
            .as_ref()
            .and_then(|c| c.desired.as_ref())
            .and_then(|d| d.model_catalog.clone()),
        None
    );
    *w.prober.model_list.lock().unwrap() = Some(Ok(krill_native(true)));
    let v = view(&sw).await;
    assert!(v.applied.as_ref().unwrap().catalog.is_none());
    assert!(!v.missing_from_catalog(spec(&v, "gpt-6.1-terra")));
    assert!(v.catalog_changes().is_empty());
}
