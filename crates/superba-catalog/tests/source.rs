//! The settings engine's view of the catalog (`superba_core::CatalogSource`).

use superba_catalog::{Os, SnapshotSource};
use superba_core::{
    CatalogMatch, CatalogSource, CodexVersion, FeatureStage, LegacyKind, SettingKind, SettingValue,
};

fn v(s: &str) -> CodexVersion {
    CodexVersion::parse(s).unwrap()
}

#[test]
fn snapshot_matching_mirrors_the_store() {
    let src = SnapshotSource::builtin().unwrap();
    let s = src.snapshot(Some(&v("0.162.0-alpha.17.2"))).unwrap();
    assert_eq!(
        (s.codex_version.clone(), s.matched.clone()),
        (v("0.162.0-alpha.17.2"), CatalogMatch::Exact)
    );
    let s = src.snapshot(Some(&v("0.162.0-alpha.18"))).unwrap();
    assert_eq!(s.matched, CatalogMatch::Approximate);
    assert_eq!(s.requested, Some(v("0.162.0-alpha.18")));
    assert_eq!(
        src.snapshot(Some(&v("9.9.9"))).unwrap().matched,
        CatalogMatch::Outdated
    );
    assert_eq!(
        src.snapshot(Some(&v("0.1.0"))).unwrap().matched,
        CatalogMatch::BelowFloor
    );
    let s = src.snapshot(None).unwrap();
    assert_eq!(s.matched, CatalogMatch::VersionUnknown);
    assert_eq!(s.codex_version.to_string(), src.store().index().latest);
    // Cached per (version, request).
    let a = src.snapshot(Some(&v("0.163.0"))).unwrap();
    let b = src.snapshot(Some(&v("0.163.0"))).unwrap();
    assert!(std::sync::Arc::ptr_eq(&a, &b));
}

#[test]
fn snapshot_contents_of_0_163_0() {
    let src = SnapshotSource::builtin().unwrap().for_os(Os::Linux);
    let s = src.snapshot(Some(&v("0.163.0"))).unwrap();

    let ap = s.key("approval_policy").unwrap();
    assert_eq!(ap.kind, SettingKind::Enum);
    assert!(ap.enum_has_table_form);
    assert!(ap.description.is_some());
    assert_eq!(
        s.key("history.persistence").unwrap().default,
        Some(SettingValue::String("save-all".into()))
    );
    assert!(
        s.key("mcp_servers.docs.command").is_some(),
        "map entries resolve through <name>"
    );
    assert_eq!(
        s.key("windows.sandbox").unwrap().platform_hint.as_deref(),
        Some("windows")
    );

    let fast = &s.features["fast_mode"];
    assert_eq!(
        (fast.stage, fast.default_enabled),
        (FeatureStage::Stable, true)
    );
    assert_eq!(
        s.key("features.fast_mode").unwrap().default,
        Some(SettingValue::Bool(true))
    );
    assert_eq!(
        s.features["context_management"].stage,
        FeatureStage::UnderDevelopment
    );
    let pis = &s.features["prevent_idle_sleep"];
    assert_eq!(pis.name.as_deref(), Some("Prevent sleep while running"));
    assert!(
        s.key("features.artifact").is_some(),
        "registry-only feature gets a key"
    );
    assert_eq!(
        s.feature_for_path("features.token_budget.enabled")
            .unwrap()
            .key,
        "token_budget"
    );

    // ultrafast_mode appeared in 0.162.0-alpha.17 (after the catalog floor).
    assert_eq!(
        s.key("features.ultrafast_mode").unwrap().since,
        Some(v("0.162.0-alpha.17.2"))
    );
    assert_eq!(s.key("model").unwrap().since, None);

    match &s.legacy_rule("features.collab").unwrap().kind {
        LegacyKind::Alias { canonical } => assert_eq!(canonical, "features.multi_agent"),
        other => panic!("{other:?}"),
    }
    match &s
        .legacy_rule("experimental_use_unified_exec_tool")
        .unwrap()
        .kind
    {
        LegacyKind::Alias { canonical } => assert_eq!(canonical, "features.unified_exec"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        s.legacy_rule("features.undo").unwrap().kind,
        LegacyKind::Ignored
    );
    match &s.legacy_rule("features.web_search_request").unwrap().kind {
        LegacyKind::Replace {
            canonical,
            when,
            value,
        } => {
            assert_eq!(canonical, "web_search");
            assert_eq!(when, &Some(SettingValue::Bool(true)));
            assert_eq!(value, &SettingValue::String("live".into()));
        }
        other => panic!("{other:?}"),
    }
    // The curated rule replaces the generated alias for the same path.
    assert!(matches!(
        s.legacy_rule("features.web_search").unwrap().kind,
        LegacyKind::Replace { .. }
    ));
}

#[test]
fn per_os_defaults() {
    let src = SnapshotSource::builtin().unwrap();
    let win = src
        .for_os(Os::Windows)
        .snapshot(Some(&v("0.163.0")))
        .unwrap();
    let mac = src.for_os(Os::Macos).snapshot(Some(&v("0.163.0"))).unwrap();
    assert!(win.features["secret_auth_storage"].default_enabled);
    assert!(!mac.features["secret_auth_storage"].default_enabled);
    assert_eq!(src.for_os(Os::Macos).os(), Os::Macos);
}
