//! Override validation, router unions, content names and speed tiers.

use pretty_assertions::assert_eq;
use serde_json::json;
use superba_core::{
    CodexVersion, Error, ModelList, ModelListSource, ModelOverride, ModelSpec, ServiceTierSpec,
    SpeedTier,
};
use superba_models::*;

fn tier(id: &str) -> ServiceTierSpec {
    ServiceTierSpec {
        id: id.into(),
        name: String::new(),
        description: String::new(),
    }
}

fn astra() -> ModelSpec {
    ModelSpec {
        context_window: Some(272_000),
        max_context_window: Some(872_000),
        service_tiers: vec![tier("priority"), tier("ultrafast")],
        ..ModelSpec::new("gpt-6-astra")
    }
}

fn window(w: u64) -> ModelOverride {
    ModelOverride {
        context_window: Some(w),
        ..Default::default()
    }
}

#[test]
fn override_windows_must_be_between_16k_and_the_model_maximum() {
    let spec = astra();
    assert!(matches!(
        validate_override(&spec, &window(15_999)),
        Err(Error::Invalid { .. })
    ));
    assert!(validate_override(&spec, &window(16_000)).is_ok());
    assert!(validate_override(&spec, &window(872_000)).is_ok());
    let err = validate_override(&spec, &window(872_001)).unwrap_err();
    assert!(err.to_string().contains("872000"), "{err}");
    // Unknown maximum: any window from 16K up.
    let open = ModelSpec::new("acme");
    assert!(validate_override(&open, &window(2_000_000)).is_ok());
}

#[test]
fn compaction_is_reported_after_the_90_percent_clamp() {
    let spec = astra();
    let check = validate_override(
        &spec,
        &ModelOverride {
            context_window: Some(400_000),
            auto_compact: Some(380_000),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(check.effective_window, Some(400_000));
    assert_eq!(check.effective_auto_compact, Some(360_000));
    assert!(check.clamped);
    assert_eq!(check.notes.len(), 1);
    assert_eq!(check.notes[0].text.key, "models_note.compact_clamped");

    let check = validate_override(
        &spec,
        &ModelOverride {
            auto_compact: Some(200_000),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(check.effective_window, Some(272_000));
    assert_eq!(check.effective_auto_compact, Some(200_000));
    assert!(!check.clamped);

    // Nothing set: Codex's default, 90 % of the window.
    let check = validate_override(&spec, &ModelOverride::default()).unwrap();
    assert_eq!(check.effective_auto_compact, Some(244_800));
    assert!(
        validate_override(
            &spec,
            &ModelOverride {
                auto_compact: Some(0),
                ..Default::default()
            }
        )
        .is_err()
    );
}

#[test]
fn empty_names_are_rejected() {
    for ov in [
        ModelOverride {
            display_name: Some("  ".into()),
            ..Default::default()
        },
        ModelOverride {
            upstream_name: Some(String::new()),
            ..Default::default()
        },
    ] {
        assert!(validate_override(&astra(), &ov).is_err());
    }
}

#[test]
fn union_keeps_the_first_member_per_slug_and_names_the_member() {
    let a = ModelList {
        source: ModelListSource::Snapshot {
            codex_version: CodexVersion::new(0, 163, 0),
        },
        models: vec![
            ModelSpec {
                description: Some("Frontier".into()),
                raw: Some(json!({"slug": "gpt-6-astra", "description": "Frontier", "x": 1})),
                ..ModelSpec::new("gpt-6-astra")
            },
            ModelSpec::new("gpt-5.5"),
        ],
        fallback_reason: None,
        sources: Vec::new(),
    };
    let b = ModelList {
        source: ModelListSource::OfficialReadOnly,
        models: vec![ModelSpec::new("gpt-5.5"), ModelSpec::new("other")],
        fallback_reason: None,
        sources: Vec::new(),
    };
    let u = union(&[("Krill".into(), a), ("Site B".into(), b)]).unwrap();
    assert_eq!(
        u.source,
        ModelListSource::Union {
            members: vec!["Krill".into(), "Site B".into()]
        }
    );
    let slugs: Vec<&str> = u.models.iter().map(|m| m.slug.as_str()).collect();
    assert_eq!(slugs, ["gpt-6-astra", "gpt-5.5", "other"]);
    assert_eq!(u.models[0].description.as_deref(), Some("Krill · Frontier"));
    assert_eq!(
        u.models[0].raw.as_ref().unwrap()["description"],
        "Krill · Frontier"
    );
    assert_eq!(u.models[0].raw.as_ref().unwrap()["x"], 1);
    assert_eq!(u.models[1].description.as_deref(), Some("Krill"));
    assert_eq!(u.models[2].description.as_deref(), Some("Site B"));
    assert!(union(&[]).unwrap().models.is_empty());
}

#[test]
fn content_names_are_the_first_8_hex_digits_of_sha256() {
    // sha256("{}") = 44136fa3...
    let (sha8, name) = content_name(b"{}").unwrap();
    assert_eq!(sha8, "44136fa3");
    assert_eq!(name, "models-44136fa3.json");
}

#[test]
fn speed_tiers_come_from_the_catalog_tiers() {
    assert_eq!(
        speed_tiers_of(&astra(), false).unwrap(),
        [SpeedTier::Standard, SpeedTier::Fast, SpeedTier::Ultrafast]
    );
    assert_eq!(
        speed_tiers_of(&astra(), true).unwrap(),
        [SpeedTier::Standard, SpeedTier::Fast, SpeedTier::Ultrafast]
    );
    assert_eq!(
        speed_tiers_of(&ModelSpec::new("plain"), true).unwrap(),
        [SpeedTier::Standard]
    );
}

#[test]
fn tier_suffixes_split_into_model_and_tier() {
    assert_eq!(
        split_tier_suffix("gpt-6-astra-ultrafast"),
        Some(("gpt-6-astra", SpeedTier::Ultrafast))
    );
    assert_eq!(
        split_tier_suffix("gpt-6-luna-fast"),
        Some(("gpt-6-luna", SpeedTier::Fast))
    );
    assert_eq!(split_tier_suffix("gpt-6-luna"), None);
    assert_eq!(split_tier_suffix("-fast"), None);
}

#[test]
fn codex_entries_become_specs_leniently() {
    let e = json!({
        "slug": "gpt-6-astra",
        "display_name": "GPT-6-Astra",
        "visibility": "hide",
        "supported_in_api": false,
        "priority": 2,
        "context_window": 272000,
        "max_context_window": 872000,
        "effective_context_window_percent": 95,
        "auto_compact_token_limit": -1,
        "supported_reasoning_levels": [{"effort": "low", "description": "Fast"}, {"effort": "high"}],
        "default_reasoning_level": "low",
        "service_tiers": [{"id": "priority", "name": "Fast", "description": "2x"}],
        "input_modalities": ["text", "image"],
        "future_field": {"a": 1}
    });
    let s = ModelSpec::from_codex_entry(&e).unwrap();
    assert_eq!(s.slug, "gpt-6-astra");
    assert_eq!(s.visibility, superba_core::ModelVisibility::Hide);
    assert!(!s.supported_in_api);
    assert_eq!(s.priority, Some(2));
    assert_eq!(s.context_window, Some(272_000));
    assert_eq!(
        s.auto_compact_token_limit, None,
        "negative counts are dropped"
    );
    assert_eq!(s.reasoning_levels.len(), 2);
    assert_eq!(s.reasoning_levels[1].description, None);
    assert_eq!(s.speed_tiers(), [SpeedTier::Standard, SpeedTier::Fast]);
    assert_eq!(s.raw.as_ref(), Some(&e));
    assert!(ModelSpec::from_codex_entry(&json!({"slug": " "})).is_none());
    assert!(ModelSpec::from_codex_entry(&json!("gpt")).is_none());
}
