//! The built-in snapshot: integrity, version lookup and known upstream facts.

use pretty_assertions::assert_eq;
use superba_catalog::format::{INDEX_PATH, to_pretty_json};
use superba_catalog::overlay::ZhState;
use superba_catalog::{
    CatalogStore, KeyKind, MatchKind, Os, SignatureStatus, Stage, StoreOrigin, VersionCatalog,
};
use superba_core::CodexVersion;

fn v(s: &str) -> CodexVersion {
    CodexVersion::parse(s).unwrap()
}

#[test]
fn builtin_loads_and_every_version_parses() {
    let store = CatalogStore::builtin().expect("built-in catalog");
    assert_eq!(store.origin(), &StoreOrigin::BuiltIn);
    let versions: Vec<String> = store.versions().map(ToString::to_string).collect();
    for want in [
        "0.155.0",
        "0.162.0-alpha.17.2",
        "0.162.0",
        "0.162.1",
        "0.163.0",
    ] {
        assert!(
            versions.contains(&want.to_owned()),
            "{want} missing from {versions:?}"
        );
    }
    let mut sorted = store.versions().cloned().collect::<Vec<_>>();
    sorted.sort();
    assert_eq!(sorted, store.versions().cloned().collect::<Vec<_>>());
    for ver in store.versions().cloned().collect::<Vec<_>>() {
        let vc = store.exact(&ver).unwrap().expect("listed version");
        assert_eq!(vc.codex_version, ver.to_string());
        assert!(!vc.config_keys.is_empty() && !vc.features.is_empty());
    }
    assert!(superba_catalog::builtin_compressed_size() < 2 * 1024 * 1024);
}

#[test]
fn builtin_signature_is_valid_when_present() {
    let store = CatalogStore::builtin().unwrap();
    match store.signature() {
        SignatureStatus::Valid { .. } | SignatureStatus::Unsigned => {}
        SignatureStatus::Invalid { reason } => {
            panic!("catalog/dist/index.json.sig does not verify: {reason}")
        }
    }
}

#[test]
fn version_files_round_trip_byte_for_byte() {
    // The serde types are lossless: what the generator wrote is what the app reads.
    let store = CatalogStore::builtin().unwrap();
    for entry in &store.index().versions {
        let bytes = store.file(&entry.path).unwrap();
        let vc: VersionCatalog = serde_json::from_slice(bytes).unwrap();
        assert_eq!(
            to_pretty_json(&vc).unwrap().as_bytes(),
            bytes,
            "{} does not round-trip",
            entry.path
        );
    }
    let idx = store.file(INDEX_PATH).unwrap();
    assert_eq!(to_pretty_json(store.index()).unwrap().as_bytes(), idx);
}

#[test]
fn version_lookup_exact_nearest_newer_older() {
    let store = CatalogStore::builtin().unwrap();
    let latest = store.index().latest.clone();
    let oldest = store.versions().next().unwrap().clone();

    let m = store.for_version(&v("0.163.0")).unwrap();
    assert_eq!(
        (m.kind, m.used.to_string()),
        (MatchKind::Exact, "0.163.0".into())
    );
    assert!(m.notice().is_none());

    // The app-bundled alpha is listed exactly: alphas differ from their release.
    let m = store.for_version(&v("0.162.0-alpha.17.2")).unwrap();
    assert_eq!(m.kind, MatchKind::Exact);
    assert_eq!(m.catalog.source.tag, "rust-v0.162.0-alpha.17.2");

    // Unlisted prerelease: nearest older version.
    let m = store.for_version(&v("0.162.0-alpha.18")).unwrap();
    assert_eq!(
        (m.kind, m.used.to_string()),
        (MatchKind::Nearest, "0.162.0-alpha.17.2".into())
    );
    assert!(m.notice().is_some());

    // An unlisted patch between two listed versions.
    let m = store.for_version(&v("0.159.4")).unwrap();
    assert_eq!(
        (m.kind, m.used.to_string()),
        (MatchKind::Nearest, "0.159.3".into())
    );

    // Newer than everything: latest, with a warning.
    let m = store.for_version(&v("9.0.0")).unwrap();
    assert_eq!(m.kind, MatchKind::NewerThanCatalog);
    assert_eq!(
        m.used.to_string(),
        store.versions().last().unwrap().to_string()
    );
    assert!(m.used.to_string() >= latest);
    let n = m.notice().unwrap();
    assert_eq!(n.severity, superba_core::Severity::Warning);

    // Older than everything: the oldest, with a warning.
    let m = store.for_version(&v("0.100.0")).unwrap();
    assert_eq!(
        (m.kind, m.used.clone()),
        (MatchKind::OlderThanCatalog, oldest)
    );
}

#[test]
fn upstream_facts_of_0_163_0() {
    let store = CatalogStore::builtin().unwrap();
    let c = store.exact(&v("0.163.0")).unwrap().unwrap();
    assert_eq!(c.source.repo, "openai/codex");
    assert!(!c.source.prerelease);

    let fast = c.feature("fast_mode").unwrap();
    assert_eq!((fast.stage, fast.default_enabled), (Stage::Stable, true));

    let cm = c.feature("context_management").unwrap();
    assert_eq!(cm.stage, Stage::UnderDevelopment);

    let secret = c.feature("secret_auth_storage").unwrap();
    assert!(!secret.default_on(Os::Linux) && !secret.default_on(Os::Macos));
    assert!(secret.default_on(Os::Windows));

    let pis = c.feature("prevent_idle_sleep").unwrap();
    assert_eq!(pis.stage, Stage::Experimental);
    assert!(pis.experimental.is_some());

    // Dotted registry keys are nested tables.
    let gtc = c.feature("guardianv2.thread_context").unwrap();
    assert_eq!(gtc.config_path, "features.guardianv2.thread_context");
    assert!(c.feature("artifact").unwrap().not_in_schema);

    assert_eq!(c.canonical_feature("collab"), Some("multi_agent"));
    assert_eq!(c.canonical_feature("codex_hooks"), Some("hooks"));
    assert!(c.legacy.ignored_feature_keys.contains(&"undo".to_owned()));

    let ap = c.key("approval_policy").unwrap();
    assert_eq!(ap.kind, KeyKind::Enum);
    assert!(ap.enum_has_table_form);
    assert!(ap.enum_values.iter().any(|e| e.value == "never"));
    assert_eq!(
        c.type_doc(ap).is_some(),
        ap.definition.is_some() && c.type_docs.contains_key(ap.definition.as_ref().unwrap())
    );

    let hist = c.key("history").unwrap();
    assert_eq!(
        hist.default,
        Some(serde_json::json!({"persistence": "save-all"}))
    );
    assert_eq!(
        hist.schema_default,
        Some(serde_json::json!({"max_bytes": null, "persistence": "save-all"}))
    );
    assert_eq!(
        c.key("history.max_bytes").unwrap().schema_default,
        Some(serde_json::Value::Null)
    );

    let delivery = c
        .key("model_providers.<name>.gateway_oauth.delivery.kind")
        .unwrap();
    assert!(delivery.under_map);
    let kinds: Vec<&str> = delivery
        .enum_values
        .iter()
        .map(|e| e.value.as_str())
        .collect();
    assert_eq!(kinds, ["header", "cookie"]);
}

#[test]
fn alphas_differ_from_their_release() {
    // `instant_interrupt` defaults to false in the bundled alpha and to true in 0.162.0.
    let store = CatalogStore::builtin().unwrap();
    let alpha = store.exact(&v("0.162.0-alpha.17.2")).unwrap().unwrap();
    let rel = store.exact(&v("0.162.0")).unwrap().unwrap();
    assert!(!alpha.feature("instant_interrupt").unwrap().default_enabled);
    assert!(rel.feature("instant_interrupt").unwrap().default_enabled);
}

#[test]
fn explanations_use_official_text_and_flag_missing_chinese() {
    let store = CatalogStore::builtin().unwrap();
    let c = store.exact(&v("0.163.0")).unwrap().unwrap();

    let e = store.explain_feature(&c, "prevent_idle_sleep");
    assert_eq!(
        e.official_name.as_deref(),
        Some("Prevent sleep while running")
    );
    assert!(e.official_en.as_deref().unwrap().contains("awake"));
    assert!(!e.en_missing());

    let e = store.explain_key(&c, "model");
    assert_eq!(
        e.official_en.as_deref(),
        c.key("model").unwrap().description.as_deref()
    );

    let e = store.explain_value(&c, "approval_policy", "never");
    assert!(e.official_en.is_some());

    // A feature without upstream text is flagged, never filled in.
    let e = store.explain_feature(&c, "fast_mode");
    assert!(e.en_missing());
    assert!(e.official_en.is_none());

    // Every overlay entry the UI can reach exists and has a consistent hash.
    for (id, file, entry) in store.overlay().iter() {
        assert!(file.starts_with("overlay/"), "{id} in {file}");
        let expect = superba_catalog::overlay::text_hash(entry.en_name.as_deref(), &entry.en);
        assert_eq!(entry.en_hash, expect, "{id}: en_hash");
        if !entry.has_official_en() {
            assert!(entry.en_source.is_none(), "{id}: source without text");
        } else {
            assert!(
                entry
                    .en_source
                    .as_deref()
                    .unwrap()
                    .starts_with("openai/codex rust-v")
            );
        }
        if entry.zh_state() == ZhState::Missing {
            assert!(entry.zh.is_empty());
        }
    }
}

#[test]
fn official_overlay_text_matches_the_latest_catalog() {
    let store = CatalogStore::builtin().unwrap();
    let c = store.latest().unwrap();
    for k in &c.config_keys {
        if c.features.iter().any(|f| f.config_path == k.path) {
            continue;
        }
        let id = superba_catalog::EntryId::Config(k.path.clone());
        let e = store
            .overlay()
            .get(&id)
            .unwrap_or_else(|| panic!("no overlay entry {id}"));
        assert_eq!(e.en, k.description.clone().unwrap_or_default(), "{id}");
    }
    for f in &c.features {
        let id = superba_catalog::EntryId::Feature(f.key.clone());
        let e = store
            .overlay()
            .get(&id)
            .unwrap_or_else(|| panic!("no overlay entry {id}"));
        let want = f
            .experimental
            .as_ref()
            .map(|x| x.menu_description.clone())
            .unwrap_or_default();
        assert_eq!(e.en, want, "{id}");
    }
}

#[test]
fn categories_cover_keys_and_have_labels() {
    let store = CatalogStore::builtin().unwrap();
    let cur = store.curation();
    let en =
        superba_core::i18n::flatten_toml(superba_catalog::i18n_source(superba_core::Locale::En))
            .unwrap();
    let zh =
        superba_core::i18n::flatten_toml(superba_catalog::i18n_source(superba_core::Locale::ZhCn))
            .unwrap();
    assert_eq!(
        en.keys().collect::<std::collections::BTreeSet<_>>(),
        zh.keys().collect::<std::collections::BTreeSet<_>>(),
        "en and zh-CN locale keys differ"
    );
    for cat in &cur.categories {
        let key = format!("catalog.category.{}", cat.id);
        assert_eq!(en.get(&key), Some(&cat.label), "{key}");
        assert!(zh.contains_key(&key), "{key} missing in zh-CN");
    }
    let c = store.latest().unwrap();
    for k in &c.config_keys {
        let id = cur.category_of_key(&k.path);
        assert!(
            cur.category(id).is_some(),
            "{} -> unknown category {id}",
            k.path
        );
    }
    for top in [
        "model",
        "approval_policy",
        "tui.theme",
        "mcp_servers.<name>.command",
    ] {
        assert_ne!(cur.category_of_key(top), "other", "{top}");
    }
    assert_eq!(cur.category_of_key("features.fast_mode"), "model");
    assert_eq!(cur.category_of_feature("token_budget"), "context");
    assert_eq!(
        cur.platforms_of("windows.sandbox"),
        Some(&[Os::Windows][..])
    );
    assert_eq!(
        cur.platforms_of("computer_use.macos.bundle_ids"),
        Some(&[Os::Macos][..])
    );
    assert_eq!(cur.platforms_of("model"), None);
}

#[test]
fn stage_labels_have_i18n_keys() {
    let en =
        superba_core::i18n::flatten_toml(superba_catalog::i18n_source(superba_core::Locale::En))
            .unwrap();
    for s in [
        Stage::UnderDevelopment,
        Stage::Experimental,
        Stage::Stable,
        Stage::Deprecated,
        Stage::Removed,
        Stage::Unknown,
    ] {
        assert!(
            en.contains_key(&format!("catalog.stage.{}", s.id())),
            "{s:?}"
        );
        assert_eq!(
            superba_core::FeatureStage::parse(s.cli_str()).is_some(),
            s != Stage::Unknown
        );
    }
}

#[test]
fn coverage_counts_add_up() {
    let store = CatalogStore::builtin().unwrap();
    let cov = store.coverage(Os::Linux).unwrap();
    let t = cov.total;
    assert_eq!(t.total, t.en_official + t.en_missing);
    assert_eq!(t.total, t.zh_current + t.zh_stale + t.zh_missing);
    assert_eq!(t.total + cov.retired, store.overlay().len());
    assert!(cov.to_markdown().contains("| **all** |"));
}

/// Release gate (v0.3 policy): every live entry of the shipped catalog has a
/// Chinese name and explanation written against its current English text, so
/// the Settings page never shows the "no Chinese description yet" fallback.
#[test]
fn every_shipped_entry_has_a_current_chinese_name_and_explanation() {
    let store = CatalogStore::builtin().unwrap();
    let mut gaps = Vec::new();
    let mut live = 0;
    for (id, _file, entry) in store.overlay().iter() {
        if entry.retired.is_some() {
            continue;
        }
        live += 1;
        let named = entry
            .zh_name
            .as_deref()
            .is_some_and(|n| !n.trim().is_empty());
        if !named || entry.zh.trim().is_empty() || entry.zh_state() != ZhState::Current {
            gaps.push(id.to_owned());
        }
    }
    assert!(live >= 1000, "the shipped overlay has {live} live entries");
    assert!(
        gaps.is_empty(),
        "{} entries without current Chinese: {gaps:?}",
        gaps.len()
    );
}
