#![allow(missing_docs)]
//! The context window and compaction threshold Codex uses per model (custom,
//! all-models, listed), what is applied now, and the bounds the Models page,
//! `spb models` and `save_models` share (user bug 7).

use std::collections::BTreeMap;

use pretty_assertions::assert_eq;
use superba_core::*;

fn spec(slug: &str, window: Option<u64>, max: Option<u64>) -> ModelSpec {
    ModelSpec {
        context_window: window,
        max_context_window: max,
        effective_context_window_percent: Some(95),
        ..ModelSpec::new(slug)
    }
}

fn custom(window: Option<u64>, compact: Option<u64>) -> ModelOverride {
    ModelOverride {
        context_window: window,
        auto_compact: compact,
        ..ModelOverride::default()
    }
}

fn all(window: Option<u64>, compact: Option<u64>) -> ModelDefaults {
    ModelDefaults {
        context_window: window,
        auto_compact: compact,
        ..ModelDefaults::default()
    }
}

#[test]
fn the_listed_values_are_the_default_with_compaction_at_90_percent_of_the_window() {
    let m = spec("gpt-5.5", Some(272_000), Some(272_000));
    let l = model_limits(&m, None, &ModelDefaults::default());
    assert_eq!(l.window, Some(272_000));
    assert_eq!(l.window_source, LimitSource::Model);
    assert_eq!(l.max, Some(272_000));
    assert_eq!(l.compact_limit, None);
    assert_eq!(l.compact, Some(244_800));
    assert_eq!(l.compact_source, LimitSource::Model);
    assert!(!l.clamped);
    assert_eq!(l.usable_window(), Some(258_400));
    assert_eq!(l.compact_percent(), Some(90));
    // Only a maximum listed: Codex resolves the window to it.
    let l = model_limits(
        &spec("x", None, Some(400_000)),
        None,
        &ModelDefaults::default(),
    );
    assert_eq!(l.window, Some(400_000));
    // Nothing known.
    let l = model_limits(&ModelSpec::new("y"), None, &ModelDefaults::default());
    assert_eq!((l.window, l.compact), (None, None));
}

#[test]
fn a_custom_value_wins_over_the_all_models_value_which_is_capped_per_model() {
    let m = spec("gpt-5.5", Some(272_000), Some(272_000));
    let d = all(Some(1_000_000), Some(300_000));
    // All models: the window is capped at the model's maximum, the threshold
    // clamped to 90 % of it.
    let l = model_limits(&m, None, &d);
    assert_eq!(l.window, Some(272_000));
    assert_eq!(l.window_source, LimitSource::AllModels);
    assert_eq!(l.compact_limit, Some(300_000));
    assert_eq!(l.compact, Some(244_800));
    assert_eq!(l.compact_source, LimitSource::AllModels);
    assert!(l.clamped);
    // The model's own values win.
    let o = custom(Some(200_000), Some(150_000));
    let l = model_limits(&m, Some(&o), &d);
    assert_eq!(l.pair(), (Some(200_000), Some(150_000)));
    assert_eq!(
        (l.window_source, l.compact_source),
        (LimitSource::Custom, LimitSource::Custom)
    );
    assert_eq!(l.compact_percent(), Some(75));
    assert_eq!(l.usable_window(), Some(190_000));
    // Its own window with the all-models threshold.
    let o = custom(Some(200_000), None);
    let l = model_limits(&m, Some(&o), &d);
    assert_eq!(l.compact, Some(180_000));
    assert_eq!(l.compact_source, LimitSource::AllModels);
}

#[test]
fn bounds_need_a_sane_window_and_a_threshold_below_it() {
    assert_eq!(
        check_window(15_999, None),
        Err(LimitError::WindowTooSmall { min: 16_000 })
    );
    assert_eq!(check_window(16_000, Some(272_000)), Ok(()));
    assert_eq!(
        check_window(272_001, Some(272_000)),
        Err(LimitError::WindowOverMax { max: 272_000 })
    );
    // No model maximum (all models, unknown models): the sanity bound.
    assert_eq!(check_window(10_000_000, None), Ok(()));
    assert_eq!(
        check_window(10_000_001, None),
        Err(LimitError::WindowOverMax { max: 10_000_000 })
    );
    assert_eq!(
        check_compact(7_999, None),
        Err(LimitError::CompactTooSmall { min: 8_000 })
    );
    assert_eq!(check_compact(199_999, Some(200_000)), Ok(()));
    let e = check_compact(200_000, Some(200_000)).unwrap_err();
    assert_eq!(e, LimitError::CompactNotBelowWindow { window: 200_000 });
    assert_eq!(e.field(), LimitField::Compact);
    assert!(
        e.message().contains("below the context window"),
        "{}",
        e.message()
    );
}

#[test]
fn an_override_is_checked_against_the_window_the_model_ends_up_with() {
    let m = spec("gpt-5.5", Some(272_000), Some(272_000));
    let none = ModelDefaults::default();
    assert_eq!(
        check_model_override(Some(&m), &custom(Some(200_000), Some(150_000)), &none),
        Ok(())
    );
    assert_eq!(
        check_model_override(Some(&m), &custom(Some(400_000), None), &none),
        Err(LimitError::WindowOverMax { max: 272_000 })
    );
    // Its own threshold against the listed window, then against the
    // all-models window (capped at the model's maximum).
    assert_eq!(
        check_model_override(Some(&m), &custom(None, Some(272_000)), &none),
        Err(LimitError::CompactNotBelowWindow { window: 272_000 })
    );
    assert_eq!(
        check_model_override(
            Some(&m),
            &custom(None, Some(150_000)),
            &all(Some(128_000), None)
        ),
        Err(LimitError::CompactNotBelowWindow { window: 128_000 })
    );
    // The all-models row on its own.
    assert_eq!(
        check_all_models(&all(Some(1_000_000), Some(800_000))),
        Ok(())
    );
    assert_eq!(
        check_all_models(&all(Some(100_000), Some(100_000))),
        Err(LimitError::CompactNotBelowWindow { window: 100_000 })
    );
    assert_eq!(check_all_models(&all(None, Some(300_000))), Ok(()));
}

#[test]
fn applied_values_come_from_the_catalog_file_and_the_all_models_keys() {
    let m = spec("gpt-5.5", Some(272_000), Some(272_000));
    // No file: the listed values.
    let applied = AppliedModels::default();
    assert_eq!(applied.limits(&m).pair(), (Some(272_000), Some(244_800)));
    // The file's entry.
    let applied = AppliedModels {
        catalog: Some("/c/models-1.json".into()),
        models: BTreeMap::from([(
            "gpt-5.5".to_owned(),
            AppliedModel {
                hidden: false,
                context_window: Some(200_000),
                max_context_window: Some(272_000),
                auto_compact: Some(150_000),
            },
        )]),
        ..AppliedModels::default()
    };
    assert_eq!(applied.limits(&m).pair(), (Some(200_000), Some(150_000)));
    // A model the file does not list keeps the listed values.
    let other = spec("gpt-6-astra", Some(400_000), Some(400_000));
    assert_eq!(
        applied.limits(&other).pair(),
        (Some(400_000), Some(360_000))
    );
    // The all-models keys win (window capped per model).
    let applied = AppliedModels {
        context_window: Some(1_000_000),
        ..AppliedModels::default()
    };
    assert_eq!(applied.limits(&m).pair(), (Some(272_000), Some(244_800)));
    // Saved and applied agree when the saved profile asks for the same.
    let saved = model_limits(&m, None, &all(Some(1_000_000), None));
    assert_eq!(saved.pair(), applied.limits(&m).pair());
    // A catalog entry read back from the file.
    let (slug, e) = AppliedModel::from_entry(&serde_json::json!({
        "slug": "gpt-5.5", "context_window": 200000, "max_context_window": 272000,
        "auto_compact_token_limit": null
    }))
    .unwrap();
    assert_eq!(slug, "gpt-5.5");
    assert_eq!(
        e,
        AppliedModel {
            hidden: false,
            context_window: Some(200_000),
            max_context_window: Some(272_000),
            auto_compact: None
        }
    );
    assert!(AppliedModel::from_entry(&serde_json::json!({"slug": " "})).is_none());
}

#[test]
fn applied_catalog_visibility_tracks_upstream_and_user_hiding() {
    for (visibility, supported, hidden) in [
        ("list", true, false),
        ("hide", true, true),
        ("none", true, true),
        ("list", false, true),
        ("future", true, false),
    ] {
        let (_, entry) = AppliedModel::from_entry(&serde_json::json!({
            "slug": "gpt-test", "visibility": visibility, "supported_in_api": supported
        }))
        .unwrap();
        assert_eq!(entry.hidden, hidden, "{visibility}, {supported}");
    }
}

fn catalog_view(models: &[(&str, ModelVisibility)], applied: &[&str]) -> ModelsView {
    ModelsView {
        router_catalog: None,
        profile_id: ProfileId::new("krill"),
        site_id: SiteId::new("krill"),
        read_only: false,
        list: ModelList {
            source: ModelListSource::Upstream {
                path: "/backend-api/codex/models".into(),
                fetched_at: Timestamp(0),
                etag: None,
            },
            models: models
                .iter()
                .map(|(slug, visibility)| ModelSpec {
                    visibility: *visibility,
                    ..ModelSpec::new(*slug)
                })
                .collect(),
            fallback_reason: None,
            sources: Vec::new(),
        },
        defaults: ModelDefaults::default(),
        overrides: BTreeMap::new(),
        picker: ModelPicker::CodexDefault,
        picker_available: false,
        speed: SpeedSupport::default(),
        catalog_file: CatalogFileStatus::NotNeeded,
        notes: Vec::new(),
        per_model: true,
        applied: Some(AppliedModels {
            catalog: Some("/home/u/.codex/superba/models-0a1b2c3d.json".into()),
            models: applied
                .iter()
                .map(|s| ((*s).to_owned(), AppliedModel::default()))
                .collect(),
            ..AppliedModels::default()
        }),
    }
}

/// GH#26 / user bug 132: what the provider's current list has that Codex's
/// static catalog file lacks, and the reverse, only where Codex reads such a
/// file of a direct provider's own list.
#[test]
fn catalog_changes_compare_the_providers_list_with_the_static_file() {
    use ModelVisibility::{Hide, List};
    let mut view = catalog_view(
        &[
            ("gpt-6-astra", List),
            ("gpt-6.1-terra", List),
            ("codex-auto-review", Hide),
            ("gpt-6-luna", List),
        ],
        &[
            "gpt-6-astra",
            "gpt-6-astra-fast",
            "gpt-6-sol",
            "gpt-5.5",
            "gpt-5.5-ultrafast",
            "gpt-6-retired",
        ],
    );
    view.overrides.insert(
        "gpt-6-luna".into(),
        ModelOverride {
            hidden: true,
            ..ModelOverride::default()
        },
    );
    // The configured models stay in the file (the builder adds them), and so
    // do the tier names of listed or configured models.
    view.defaults.model = Some("gpt-6-sol".into());
    view.applied.as_mut().unwrap().model = Some("gpt-5.5".into());
    let m = |slug: &str| view.list.model(slug).unwrap().clone();
    assert!(view.missing_from_catalog(&m("gpt-6.1-terra")));
    assert!(!view.missing_from_catalog(&m("gpt-6-astra")));
    assert!(
        !view.missing_from_catalog(&m("codex-auto-review")),
        "hidden upstream"
    );
    assert!(
        !view.missing_from_catalog(&m("gpt-6-luna")),
        "hidden by the user"
    );
    let mut unsupported = m("gpt-6.1-terra");
    unsupported.supported_in_api = false;
    assert!(!view.missing_from_catalog(&unsupported));
    assert_eq!(
        view.catalog_changes(),
        ModelCatalogChanges {
            missing: vec!["gpt-6.1-terra".into()],
            removed: vec!["gpt-6-retired".into()],
        }
    );

    // Nothing to compare: no catalog file, a fallback list, the router's live
    // list, or no per-model values.
    let terra = m("gpt-6.1-terra");
    let cases: [fn(&mut ModelsView); 5] = [
        |v| v.applied.as_mut().unwrap().catalog = None,
        |v| v.applied = None,
        |v| {
            v.list.source = ModelListSource::Snapshot {
                codex_version: CodexVersion::new(0, 168, 0),
            }
        },
        |v| v.router_catalog = Some(RouterCatalogView::default()),
        |v| v.per_model = false,
    ];
    for (i, change) in cases.into_iter().enumerate() {
        let mut v = view.clone();
        change(&mut v);
        assert!(!v.missing_from_catalog(&terra), "case {i}");
        assert!(v.catalog_changes().is_empty(), "case {i}");
    }
}
