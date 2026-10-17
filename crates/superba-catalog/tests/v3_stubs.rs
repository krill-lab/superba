//! v0.3 contract additions of the catalog crate: the curated "Common" list and
//! the feeds.

use superba_catalog::feeds::{FEEDS_SIGNATURE_CONTEXT, FeedStore, HttpFeeds};
use superba_catalog::integrity::SIGNATURE_CONTEXT;
use superba_catalog::{CatalogStore, Curation};
use superba_core::{CodexVersion, DataDir, FeedSource, NetworkSettings};

#[test]
fn curation_reads_the_common_list_in_order() {
    let curation = Curation::parse(
        "format = 1\n\n[common]\npaths = [\"sandbox_mode\", \"approval_policy\"]\n",
    )
    .unwrap();
    assert_eq!(curation.common(), ["sandbox_mode", "approval_policy"]);
    let without = Curation::parse("format = 1\n").unwrap();
    assert!(without.common().is_empty());
    // The shipped curation still parses with or without the section.
    let store = CatalogStore::builtin().expect("built-in catalog");
    let _ = store.curation().common();
}

#[tokio::test]
async fn feeds_use_their_own_signature_context_and_the_data_dir() {
    assert_ne!(FEEDS_SIGNATURE_CONTEXT, SIGNATURE_CONTEXT);
    let data = DataDir::new("/tmp/superba-feeds-contract-unused");
    let store = FeedStore::new(&data);
    assert_eq!(store.dir(), data.feeds_dir());
    let http = HttpFeeds::new(store.clone(), NetworkSettings::default());
    assert_eq!(http.store().dir(), store.dir());
    // Nothing is cached there: both serve the compiled-in snapshot.
    let version = CodexVersion::new(0, 163, 0);
    for source in [&store as &dyn FeedSource] {
        assert!(source.snapshot(false).await.unwrap().serial >= 1);
        assert!(source.prompt(&version).await.unwrap().is_some());
        assert!(source.status().await.unwrap().builtin_serial >= 1);
    }
}

#[test]
fn the_shipped_common_list_names_known_settings() {
    let store = CatalogStore::builtin().expect("built-in catalog");
    let common = store.curation().common();
    assert!(common.len() >= 20, "{common:?}");
    assert_eq!(common[0], "approval_policy");
    let catalog = store.latest().expect("latest");
    for path in common {
        let known = match path.strip_prefix("features.") {
            Some(key) => catalog.features.iter().any(|f| f.key == key),
            None => catalog.config_keys.iter().any(|k| k.path == *path),
        };
        assert!(known, "{path}");
    }
}
