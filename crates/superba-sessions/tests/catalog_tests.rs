//! The crate's message catalogs: parity between locales and coverage of every key
//! the crate emits.

#![allow(missing_docs)]

use std::collections::BTreeSet;

use superba_core::{
    Catalog, Locale, MigrationSelector, ProviderSessions, SessionReport, Timestamp,
};
use superba_sessions::{LEGACY_PROVIDER_IDS, catalog_source, takeover_request};

fn keys(src: &str) -> BTreeSet<String> {
    superba_core::i18n::flatten_toml(src)
        .unwrap()
        .into_keys()
        .collect()
}

fn placeholders(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else {
            break;
        };
        out.insert(rest[start + 1..start + end].to_owned());
        rest = &rest[start + end + 1..];
    }
    out
}

#[test]
fn locales_have_the_same_keys_and_placeholders() {
    let en = superba_core::i18n::flatten_toml(catalog_source(Locale::En)).unwrap();
    let zh = superba_core::i18n::flatten_toml(catalog_source(Locale::ZhCn)).unwrap();
    assert_eq!(
        en.keys().collect::<BTreeSet<_>>(),
        zh.keys().collect::<BTreeSet<_>>()
    );
    for (k, v) in &en {
        assert_eq!(placeholders(v), placeholders(&zh[k]), "{k}");
    }
    let mut catalog = Catalog::new(Locale::ZhCn);
    catalog
        .add_toml(Locale::En, catalog_source(Locale::En))
        .unwrap();
    catalog
        .add_toml(Locale::ZhCn, catalog_source(Locale::ZhCn))
        .unwrap();
    assert!(catalog.get("sessions.warn.db_on_share").is_some());
}

#[test]
fn every_emitted_key_is_in_the_catalog() {
    let src = include_str!("../src/text.rs");
    let en = keys(catalog_source(Locale::En));
    let mut found = 0;
    for part in src.split('"').skip(1).step_by(2) {
        if part.starts_with("sessions.") {
            assert!(en.contains(part), "{part} missing from locales/en.toml");
            found += 1;
        }
    }
    assert!(found >= 8);
}

#[test]
fn takeover_request_selects_present_legacy_ids_only() {
    let mut report = SessionReport {
        codex_home: "/h/.codex".into(),
        scanned_at: Timestamp(0),
        sqlite_paths: vec![],
        by_provider: Default::default(),
        inconsistent: 0,
        warnings: vec![],
    };
    assert!(takeover_request(&report).is_none());
    report.by_provider.insert(
        "openai".into(),
        ProviderSessions {
            distinct: 3,
            ..Default::default()
        },
    );
    assert!(takeover_request(&report).is_none());
    for id in LEGACY_PROVIDER_IDS {
        report.by_provider.insert(
            (*id).into(),
            ProviderSessions {
                distinct: 1,
                ..Default::default()
            },
        );
    }
    let req = takeover_request(&report).unwrap();
    assert_eq!(req.to_provider, "openai");
    assert!(req.include_archived);
    assert_eq!(
        req.selectors,
        LEGACY_PROVIDER_IDS
            .iter()
            .map(|p| MigrationSelector::Provider {
                provider: (*p).into()
            })
            .collect::<Vec<_>>()
    );
}
