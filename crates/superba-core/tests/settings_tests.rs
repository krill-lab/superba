#![allow(missing_docs)]

use std::collections::BTreeMap;

use pretty_assertions::assert_eq;
use serde_json::json;
use superba_core::i18n::{core_catalog_source, flatten_toml};
use superba_core::*;

// ---------------------------------------------------------------------------
// values
// ---------------------------------------------------------------------------

#[test]
fn setting_values_render_as_toml_and_parse_back() {
    let table: BTreeMap<String, SettingValue> = [
        ("enabled".to_owned(), SettingValue::Bool(true)),
        ("odd key".to_owned(), SettingValue::Integer(3)),
    ]
    .into_iter()
    .collect();
    let cases = [
        (SettingValue::Bool(false), "false"),
        (SettingValue::Integer(-42), "-42"),
        (SettingValue::float(1.0).unwrap(), "1.0"),
        (SettingValue::float(0.25).unwrap(), "0.25"),
        (SettingValue::float(1e300).unwrap(), "1e300"),
        (SettingValue::float(-2.5e-9).unwrap(), "-2.5e-9"),
        (SettingValue::from("live"), "\"live\""),
        (
            SettingValue::from("a\"b\\c\n\u{1}"),
            "\"a\\\"b\\\\c\\n\\u0001\"",
        ),
        (
            SettingValue::Array(vec!["a".into(), SettingValue::Integer(1)]),
            "[\"a\", 1]",
        ),
        (SettingValue::Table(BTreeMap::new()), "{}"),
        (
            SettingValue::Table(table),
            "{ enabled = true, \"odd key\" = 3 }",
        ),
    ];
    for (value, repr) in cases {
        assert_eq!(value.to_toml_repr(), repr);
        assert_eq!(
            SettingValue::from_toml_repr(repr),
            Some(value.clone()),
            "{repr}"
        );
        assert_eq!(value.to_string(), repr);
    }
    assert_eq!(SettingValue::float(f64::NAN), None);
    assert_eq!(SettingValue::float(f64::INFINITY), None);
    assert_eq!(SettingValue::from_toml_repr("1979-05-27T07:32:00Z"), None);
    assert_eq!(SettingValue::from_toml_repr("a = 1"), None);
    assert_eq!(SettingValue::from_toml_repr(""), None);
    // Integers and floats never compare equal; floats compare by value.
    assert_ne!(SettingValue::Integer(1), SettingValue::float(1.0).unwrap());
    assert_eq!(SettingValue::Integer(7).as_f64(), Some(7.0));
}

#[test]
fn settings_serialize_as_a_json_object_with_null_for_unset() {
    let mut s = CodexSettings::new();
    s.set("web_search", "live").unwrap();
    s.set("features.fast_mode", true).unwrap();
    s.set("tui.animations", false).unwrap();
    s.unset("approval_policy").unwrap();
    s.set("history.max_bytes", 1_000_000i64).unwrap();
    let encoded = serde_json::to_value(&s).unwrap();
    assert_eq!(
        encoded,
        json!({
            "approval_policy": null,
            "features.fast_mode": true,
            "history.max_bytes": 1000000,
            "tui.animations": false,
            "web_search": "live",
        })
    );
    let decoded: CodexSettings = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, s);
    assert_eq!(decoded.get("approval_policy"), Some(&SettingEntry::Unset));
    assert_eq!(decoded.get("approval_policy").unwrap().to_toml_repr(), None);
    assert_eq!(
        decoded.get("web_search").unwrap().to_toml_repr().as_deref(),
        Some("\"live\"")
    );
    assert_eq!(
        decoded.paths().collect::<Vec<_>>(),
        [
            "approval_policy",
            "features.fast_mode",
            "history.max_bytes",
            "tui.animations",
            "web_search"
        ]
    );
    // Values that are not TOML values are rejected.
    assert!(serde_json::from_value::<CodexSettings>(json!({"a": u64::MAX})).is_err());
    let nested: CodexSettings =
        serde_json::from_value(json!({"x.y": [1, {"k": "v"}], "z": 1.5})).unwrap();
    assert_eq!(
        nested.get("x.y").unwrap().to_toml_repr().as_deref(),
        Some("[1, { k = \"v\" }]")
    );
}

#[test]
fn settings_refuse_malformed_and_reserved_paths() {
    let mut s = CodexSettings::new();
    for bad in ["", ".a", "a.", "a..b", "a b", "mcp_servers.\"x\"", "ä"] {
        assert!(s.set(bad, true).is_err(), "{bad:?}");
        assert!(!is_valid_setting_path(bad));
    }
    for (path, owner) in [
        ("model_provider", ReservedOwner::Connection),
        ("openai_base_url", ReservedOwner::Connection),
        ("chatgpt_base_url", ReservedOwner::Connection),
        ("model", ReservedOwner::ModelDefaults),
        ("model_reasoning_effort", ReservedOwner::ModelDefaults),
        // v0.3: the Models page and the speed control own these.
        ("model_context_window", ReservedOwner::ModelDefaults),
        (
            "model_auto_compact_token_limit",
            ReservedOwner::ModelDefaults,
        ),
        ("model_catalog_json", ReservedOwner::ModelDefaults),
        ("service_tier", ReservedOwner::ModelDefaults),
        ("model_providers.krill.base_url", ReservedOwner::Superba),
        ("profile", ReservedOwner::Superba),
        ("profiles.work.model", ReservedOwner::Superba),
        ("cli_auth_credentials_store", ReservedOwner::Superba),
    ] {
        assert_eq!(reserved_owner(path), Some(owner), "{path}");
        assert!(s.set(path, "x").is_err(), "{path}");
    }
    for ok in [
        "model_verbosity",
        "model_reasoning_summary",
        "features.token_budget.enabled",
        "mcp_servers.docs-1.enabled",
    ] {
        assert_eq!(reserved_owner(ok), None, "{ok}");
        s.set(ok, true).unwrap();
    }
    assert!(!s.is_empty() && s.len() == 4);
    assert_eq!(
        s.forget("model_verbosity"),
        Some(SettingEntry::Set(true.into()))
    );
    assert!(!s.contains("model_verbosity"));
    // Unchecked inserts exist for migrations of malformed files.
    s.insert_unchecked("a..b", SettingEntry::Unset);
    assert!(s.contains("a..b"));
}

#[test]
fn krill_recommended_preset_matches_the_tutorial_keys() {
    let preset = CodexSettings::krill_recommended();
    assert_eq!(preset.len(), codex::PAT_RECOMMENDED_KEYS.len());
    for (path, lit) in codex::PAT_RECOMMENDED_KEYS {
        assert_eq!(
            preset.get(path),
            Some(&SettingEntry::Set((*lit).into())),
            "{path}"
        );
        assert_eq!(reserved_owner(path), None);
    }
    assert_eq!(
        preset.get("web_search"),
        Some(&SettingEntry::Set("live".into()))
    );
}

// ---------------------------------------------------------------------------
// key auth (user bug 139: key-based profiles always use an API key)
// ---------------------------------------------------------------------------

fn caps(reachable: bool, apikey_ok: bool, path: Option<&str>) -> LineCaps {
    let mut v = json!({
        "site_id": "krill", "line_id": "overseas", "checked_at": 0,
        "reachable": reachable, "latency_ms": 120, "apikey_ok": apikey_ok,
        "blocked_by_edge": false,
    });
    if let Some(p) = path {
        v["codex_path"] = json!(p);
    }
    serde_json::from_value(v).expect("LineCaps from JSON")
}

#[test]
fn key_auth_uses_an_api_key_where_the_site_and_line_take_one() {
    let krill = catalog::krill_site();
    let d = key_auth(&krill, None);
    assert_eq!(
        (d.mode, d.reason),
        (Some(AuthMode::ApiKey), AuthReason::Available)
    );
    let d = key_auth(&krill, Some(&caps(true, true, Some("backend_api_codex"))));
    assert_eq!(d.mode, Some(AuthMode::ApiKey));
    let d = key_auth(&krill, Some(&caps(true, true, Some("codex_v1"))));
    assert_eq!(d.mode, Some(AuthMode::ApiKey));
    for down in [
        caps(false, false, None),
        caps(true, false, Some("backend_api_codex")),
        caps(true, true, None),
    ] {
        let d = key_auth(&krill, Some(&down));
        assert_eq!((d.mode, d.reason), (None, AuthReason::LineUnavailable));
    }
    let official = catalog::official_site();
    let d = key_auth(&official, None);
    assert_eq!((d.mode, d.reason), (None, AuthReason::NoKeyMode));
}

#[test]
fn a_former_pat_reads_as_an_api_key() {
    // Profiles, settings and catalogs saved before user bug 139.
    let mode: AuthMode = serde_json::from_value(json!("pat")).unwrap();
    assert_eq!(mode, AuthMode::ApiKey);
    assert_eq!(serde_json::to_value(mode).unwrap(), "apikey");
    let p: Profile = serde_json::from_value(json!({
        "id": "p", "name": "P", "site_id": "krill", "auth_mode": "pat", "auth": "auto",
    }))
    .unwrap();
    assert_eq!(p.auth_mode, AuthMode::ApiKey);
    let caps: SiteCapabilities = serde_json::from_value(json!({
        "auth_modes": ["pat", "apikey"], "codex_paths": ["backend_api_codex"],
    }))
    .unwrap();
    assert_eq!(caps.auth_modes, vec![AuthMode::ApiKey]);
    let settings: Settings = serde_json::from_value(json!({ "default_auth_mode": "pat" })).unwrap();
    assert_eq!(settings.default_auth_mode, AuthMode::ApiKey);
}

// ---------------------------------------------------------------------------
// profiles
// ---------------------------------------------------------------------------

fn profile_with(auth_mode: AuthMode) -> Profile {
    serde_json::from_value(json!({
        "id": "p", "name": "P", "site_id": "krill", "auth_mode": auth_mode,
        "line_id": "overseas",
    }))
    .unwrap()
}

#[test]
fn line_priority_is_primary_then_unique_backups() {
    let mut p = profile_with(AuthMode::ApiKey);
    assert_eq!(p.line_priority(), vec![LineId::new("overseas")]);
    p.set_line_priority(["cdn", "overseas", "cdn", "cn-fast"].map(LineId::new));
    assert_eq!(p.line_id, Some(LineId::new("cdn")));
    assert_eq!(p.backup_lines, ["overseas", "cn-fast"].map(LineId::new));
    p.backup_lines.push(LineId::new("cdn"));
    assert_eq!(
        p.line_priority(),
        ["cdn", "overseas", "cn-fast"].map(LineId::new)
    );
    p.set_line_priority(Vec::new());
    assert_eq!((p.line_id.clone(), p.backup_lines.len()), (None, 0));
    assert!(p.line_priority().is_empty());
}

#[test]
fn preferring_a_line_preserves_the_effective_order_including_implicit_backups() {
    let site = superba_core::catalog::krill_site();
    let mut p = profile_with(AuthMode::ApiKey);
    p.set_line_priority(Vec::new());
    let order = p.ordered_line_ids(&site);
    assert_eq!(order[0], site.default_line.as_ref().unwrap().clone());
    let selected = order.last().unwrap().clone();
    p.prefer_line(&site, selected.clone()).unwrap();
    let expected = std::iter::once(selected.clone())
        .chain(order.into_iter().filter(|id| id != &selected))
        .collect::<Vec<_>>();
    assert_eq!(p.line_priority(), expected);
    let before = p.clone();
    assert!(p.prefer_line(&site, "missing".into()).is_err());
    assert_eq!(p, before);
    p.prefer_line(&site, selected).unwrap();
    assert_eq!(p, before, "selecting the same preference is idempotent");
}

// ---------------------------------------------------------------------------
// catalog view
// ---------------------------------------------------------------------------

fn v(s: &str) -> CodexVersion {
    s.parse().unwrap()
}

fn feature(key: &str, stage: FeatureStage, default: bool) -> CatalogFeature {
    CatalogFeature {
        key: key.into(),
        config_path: format!("features.{key}"),
        stage,
        default_enabled: default,
        name: None,
        menu_description: None,
    }
}

fn snapshot(version: &str) -> CatalogSnapshot {
    CatalogSnapshot::new(v(version))
        .with_key(CatalogKey::new(
            "mcp_servers.<name>.command",
            SettingKind::String,
        ))
        .with_key(CatalogKey::new("mcp_servers.<name>.<x>", SettingKind::Any))
        .with_key(CatalogKey::new("mcp_servers.<name>.env", SettingKind::Any))
        .with_key(CatalogKey::new("web_search", SettingKind::Enum))
        .with_feature(feature("fast_mode", FeatureStage::Stable, true))
        .with_feature(feature(
            "token_budget",
            FeatureStage::UnderDevelopment,
            false,
        ))
        .with_legacy(LegacyRule {
            path: "features.collab".into(),
            kind: LegacyKind::Alias {
                canonical: "features.multi_agent".into(),
            },
        })
}

#[test]
fn snapshot_lookups_match_patterns_features_and_legacy_keys() {
    let s = snapshot("0.162.0");
    assert_eq!(s.key("web_search").unwrap().kind, SettingKind::Enum);
    assert_eq!(
        s.key("mcp_servers.docs.command").unwrap().path,
        "mcp_servers.<name>.command"
    );
    assert_eq!(
        s.key("mcp_servers.docs.timeout").unwrap().path,
        "mcp_servers.<name>.<x>"
    );
    assert_eq!(s.key("mcp_servers.docs").map(|k| &k.path), None);
    assert_eq!(
        s.open_ancestor("mcp_servers.docs.env.TOKEN").unwrap().path,
        "mcp_servers.<name>.env"
    );
    assert_eq!(s.open_ancestor("web_search.x"), None);
    assert_eq!(
        s.key("features.fast_mode").unwrap().kind,
        SettingKind::Boolean
    );
    assert_eq!(
        s.key("features.fast_mode").unwrap().default,
        Some(SettingValue::Bool(true))
    );
    assert_eq!(
        s.feature_for_path("features.token_budget.enabled")
            .unwrap()
            .key,
        "token_budget"
    );
    assert_eq!(s.feature_for_path("features.token_budgets"), None);
    assert!(s.legacy_rule("features.collab").is_some());
    assert!(s.legacy_rule("features.multi_agent").is_none());
    let json = serde_json::to_value(&s.legacy[0]).unwrap();
    assert_eq!(
        json,
        json!({"path": "features.collab", "kind": "alias", "canonical": "features.multi_agent"})
    );
    assert_eq!(
        serde_json::from_value::<LegacyRule>(json).unwrap(),
        s.legacy[0]
    );
}

#[test]
fn static_catalog_resolves_exact_versions_only() {
    let cat = StaticCatalog::new([
        snapshot("0.162.0"),
        snapshot("0.150.0"),
        snapshot("0.162.0-alpha.17.2"),
    ]);
    let get = |ver: Option<&str>| {
        let s = cat.snapshot(ver.map(v).as_ref()).unwrap();
        (s.codex_version.to_string(), s.matched.clone())
    };
    assert_eq!(
        get(Some("0.162.0-alpha.17.2")),
        ("0.162.0-alpha.17.2".into(), CatalogMatch::Exact)
    );
    // An alpha that is not listed uses the nearest older version, not its release.
    assert_eq!(
        get(Some("0.162.0-alpha.18")),
        ("0.162.0-alpha.17.2".into(), CatalogMatch::Approximate)
    );
    assert_eq!(
        get(Some("0.155.3")),
        ("0.150.0".into(), CatalogMatch::Approximate)
    );
    assert_eq!(
        get(Some("0.170.0")),
        ("0.162.0".into(), CatalogMatch::Outdated)
    );
    assert_eq!(
        get(Some("0.100.0")),
        ("0.150.0".into(), CatalogMatch::BelowFloor)
    );
    assert_eq!(get(None), ("0.162.0".into(), CatalogMatch::VersionUnknown));
    assert!(StaticCatalog::default().snapshot(None).is_none());
    assert!(CatalogMatch::Approximate.is_inexact() && !CatalogMatch::Exact.is_inexact());
}

#[test]
fn feature_stages_parse_every_spelling() {
    for (text, stage) in [
        ("under development", FeatureStage::UnderDevelopment),
        ("under_development", FeatureStage::UnderDevelopment),
        ("underDevelopment", FeatureStage::UnderDevelopment),
        ("experimental", FeatureStage::Experimental),
        ("beta", FeatureStage::Experimental),
        ("Stable", FeatureStage::Stable),
        ("deprecated", FeatureStage::Deprecated),
        ("removed", FeatureStage::Removed),
    ] {
        assert_eq!(FeatureStage::parse(text), Some(stage), "{text}");
    }
    assert_eq!(FeatureStage::parse("preview"), None);
    assert!(FeatureStage::Stable.in_default_view());
    assert!(FeatureStage::Experimental.in_default_view());
    assert!(!FeatureStage::UnderDevelopment.in_default_view());
    assert_eq!(
        serde_json::to_value(FeatureStage::UnderDevelopment).unwrap(),
        "under_development"
    );
}

// ---------------------------------------------------------------------------
// reports
// ---------------------------------------------------------------------------

#[test]
fn features_list_parser_tolerates_spaces_colours_and_noise() {
    let out = "\
WARNING: under-development features enabled: token_budget\n\
Feature              Stage              Enabled\n\
apply_patch_freeform stable             true\n\
\u{1b}[1mcontext_management\u{1b}[0m   under development  false\n\
fast_mode            stable             true   \n\
\n\
guardianv2.thread_context removed false\n\
new_thing            preview            on\n\
weird line without a bool\n";
    let list = FeaturesList::parse(out);
    let rows: Vec<_> = list
        .entries
        .iter()
        .map(|e| (e.key.as_str(), e.stage, e.stage_text.as_str(), e.enabled))
        .collect();
    assert_eq!(
        rows,
        vec![
            (
                "apply_patch_freeform",
                Some(FeatureStage::Stable),
                "stable",
                true
            ),
            (
                "context_management",
                Some(FeatureStage::UnderDevelopment),
                "under development",
                false
            ),
            ("fast_mode", Some(FeatureStage::Stable), "stable", true),
            (
                "guardianv2.thread_context",
                Some(FeatureStage::Removed),
                "removed",
                false
            ),
            ("new_thing", None, "preview", true),
        ]
    );
    assert_eq!(list.other_lines.len(), 3, "{:?}", list.other_lines);
    assert_eq!(list.get("fast_mode").map(|e| e.enabled), Some(true));
    assert_eq!(FeaturesList::parse(""), FeaturesList::default());
}

#[test]
fn legacy_migrations_produce_settings_edits() {
    let m = LegacyMigration {
        legacy_path: "features.web_search_request".into(),
        legacy_value: Some("true".into()),
        canonical: Some("web_search".into()),
        canonical_value: Some("live".into()),
        source: MigrationSource::Config,
        conflict: false,
    };
    assert_eq!(
        m.edits(),
        vec![
            ("features.web_search_request".into(), SettingEntry::Unset),
            ("web_search".into(), SettingEntry::Set("live".into())),
        ]
    );
    let conflicting = LegacyMigration {
        conflict: true,
        ..m.clone()
    };
    assert_eq!(conflicting.edits().len(), 1);
    let ignored = LegacyMigration {
        canonical: None,
        canonical_value: None,
        ..m
    };
    assert_eq!(
        ignored.text().render_fallback(),
        "features.web_search_request is ignored by Codex and can be removed"
    );
}

#[test]
fn settings_plan_round_trips_through_json() {
    let mut apply = CodexSettings::new();
    apply.set("web_search", "live").unwrap();
    let plan = SettingsPlan {
        home: Some(TargetId::local()),
        consumers: vec![SettingsConsumer::new(
            ConsumerKind::DesktopApp,
            "ChatGPT",
            Some(v("0.162.0-alpha.17.2")),
        )],
        apply,
        skipped: vec![SkippedSetting {
            path: "model".into(),
            entry: SettingEntry::Set("gpt-5".into()),
            reason: SkipReason::Reserved {
                owner: ReservedOwner::ModelDefaults,
            },
        }],
        support: BTreeMap::new(),
        migrations: Vec::new(),
        migrated: BTreeMap::new(),
        notes: Vec::new(),
    };
    let json = serde_json::to_value(&plan).unwrap();
    assert_eq!(json["skipped"][0]["reason"]["owner"], "model_defaults");
    assert_eq!(serde_json::from_value::<SettingsPlan>(json).unwrap(), plan);
}

// ---------------------------------------------------------------------------
// texts
// ---------------------------------------------------------------------------

fn all_texts() -> Vec<LocalizedText> {
    let mut texts: Vec<LocalizedText> = Vec::new();
    for r in [AuthReason::LineUnavailable, AuthReason::NoKeyMode] {
        texts.push(r.text());
    }
    for s in [
        FeatureStage::UnderDevelopment,
        FeatureStage::Experimental,
        FeatureStage::Stable,
        FeatureStage::Deprecated,
        FeatureStage::Removed,
    ] {
        texts.push(s.text());
    }
    for s in [
        KeySupport::Supported,
        KeySupport::Deprecated,
        KeySupport::Removed,
        KeySupport::Legacy,
        KeySupport::Missing,
        KeySupport::Unknown,
    ] {
        texts.push(s.text());
    }
    for r in [
        SkipReason::InvalidPath,
        SkipReason::Reserved {
            owner: ReservedOwner::Connection,
        },
        SkipReason::Reserved {
            owner: ReservedOwner::ModelDefaults,
        },
        SkipReason::Reserved {
            owner: ReservedOwner::Superba,
        },
        SkipReason::NotSupported {
            consumers: vec!["CLI 0.150.0".into()],
        },
        SkipReason::Removed,
        SkipReason::InvalidValue {
            expected: "boolean".into(),
        },
        SkipReason::ShapeConflict {
            parent: "features.token_budget".into(),
        },
        SkipReason::Overlap {
            other: "tui".into(),
        },
    ] {
        texts.push(r.text());
    }
    let m = LegacyMigration {
        legacy_path: "features.collab".into(),
        legacy_value: None,
        canonical: Some("features.multi_agent".into()),
        canonical_value: None,
        source: MigrationSource::Profile,
        conflict: false,
    };
    texts.push(m.text());
    texts.push(
        LegacyMigration {
            canonical: None,
            ..m
        }
        .text(),
    );
    for r in [
        ChangeReason::Connection,
        ChangeReason::Model,
        ChangeReason::KeptModel,
        ChangeReason::Setting,
        ChangeReason::Reset,
        ChangeReason::Released,
        ChangeReason::Migration {
            legacy: "features.collab".into(),
        },
        ChangeReason::Restore,
    ] {
        texts.push(r.text());
    }
    texts
}

#[test]
fn every_settings_text_is_in_both_core_catalogs() {
    let texts = all_texts();
    let prefixes = [
        "auth_choice.",
        "auth_reason.",
        "feature_stage.",
        "settings.",
    ];
    for locale in Locale::ALL {
        let entries = flatten_toml(core_catalog_source(locale)).unwrap();
        let used: std::collections::BTreeSet<&str> = texts
            .iter()
            .map(|t| t.key.as_str())
            .filter(|k| prefixes.iter().any(|p| k.starts_with(p)))
            .collect();
        let defined: std::collections::BTreeSet<&str> = entries
            .keys()
            .map(String::as_str)
            .filter(|k| prefixes.iter().any(|p| k.starts_with(p)))
            .collect();
        assert_eq!(used, defined, "{locale}");
        let catalog = Catalog::new(locale);
        for t in &texts {
            let rendered = catalog.text(t);
            assert!(!rendered.contains('{'), "{} in {locale}: {rendered}", t.key);
            if locale == Locale::En && entries.contains_key(&t.key) {
                assert_eq!(entries[&t.key], t.fallback, "fallback of {}", t.key);
            }
        }
    }
    // No user-facing "context management" wording is left in the core catalogs.
    for locale in Locale::ALL {
        let src = core_catalog_source(locale);
        assert!(
            !src.to_lowercase().contains("context management"),
            "{locale}"
        );
        assert!(!src.contains("上下文管理"), "{locale}");
    }
    assert_eq!(AuthMode::ApiKey.text().render_fallback(), "API key");
}

#[test]
fn router_model_list_defaults_to_codex_and_ignores_the_alpha7_prefix_flag() {
    use superba_core::{CatalogMode, RouterCatalogSettings, Settings};
    assert_eq!(RouterCatalogSettings::default().mode, CatalogMode::Codex);
    // alpha.7 wrote `show_prefixed` (true by default); no mode is derived from it.
    for old in [
        r#"{"show_prefixed":true,"provider_slugs":{"office":"office"}}"#,
        r#"{"show_prefixed":false}"#,
        "{}",
    ] {
        let s: RouterCatalogSettings = serde_json::from_str(old).unwrap();
        assert_eq!(s.mode, CatalogMode::Codex, "{old}");
    }
    let saved = Settings::default();
    assert_eq!(saved.router.catalog.mode, CatalogMode::Codex);
    for mode in CatalogMode::ALL {
        let s = RouterCatalogSettings {
            mode,
            ..Default::default()
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(
            json.contains(&format!("\"mode\":\"{}\"", mode.as_str())),
            "{json}"
        );
        assert!(!json.contains("show_prefixed"));
        let back: RouterCatalogSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
        assert_eq!(CatalogMode::parse(mode.as_str()), Some(mode));
    }
    assert_eq!(CatalogMode::parse("provider"), None);
}

#[test]
fn disabled_lines_keep_their_place_but_leave_the_enabled_order() {
    let site = superba_core::catalog::krill_site();
    let mut p = profile_with(AuthMode::ApiKey);
    let order = p.ordered_line_ids(&site);
    let off = order[0].clone();
    p.set_line_disabled(&site, &off, true).unwrap();
    assert!(p.is_line_disabled(&off));
    assert_eq!(p.ordered_line_ids(&site), order, "the order is unchanged");
    assert_eq!(p.enabled_line_ids(&site), order[1..]);
    assert!(
        p.prefer_line(&site, off.clone()).is_err(),
        "never preferred"
    );
    p.set_line_disabled(&site, &order[1], true).unwrap();
    let before = p.clone();
    assert!(p.set_line_disabled(&site, &order[2], true).is_err());
    assert_eq!(p, before, "the last enabled line stays");
    assert!(p.set_line_disabled(&site, &"missing".into(), true).is_err());
    p.set_line_disabled(&site, &off, false).unwrap();
    assert_eq!(p.enabled_line_ids(&site), [off, order[2].clone()]);
}
