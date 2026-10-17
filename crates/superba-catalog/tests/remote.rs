//! Remote updates: disabled by default, signed, rollback-protected, verified before
//! they replace the cache; the loader prefers a verified newer cache only.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use superba_catalog::format::{INDEX_PATH, Index, SIGNATURE_PATH, to_pretty_json};
use superba_catalog::integrity::{TrustedKey, sha256_hex};
use superba_catalog::remote::{
    RefreshOutcome, RemoteConfig, Transport, cache_tree, refresh_with_keys,
};
use superba_catalog::{CatalogError, CatalogStore, StoreOrigin, TrustPolicy};
use url::Url;

mod common;
use common::{sign_files, test_key};

#[derive(Default)]
struct MemTransport {
    files: HashMap<String, Vec<u8>>,
    requests: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl Transport for MemTransport {
    async fn get(&self, url: &Url, max_bytes: u64) -> superba_catalog::Result<Vec<u8>> {
        self.requests.lock().unwrap().push(url.to_string());
        match self.files.get(url.as_str()) {
            Some(b) if b.len() as u64 <= max_bytes => Ok(b.clone()),
            Some(_) => Err(CatalogError::Network {
                url: url.to_string(),
                message: "too large".into(),
            }),
            None => Err(CatalogError::Network {
                url: url.to_string(),
                message: "HTTP 404".into(),
            }),
        }
    }
}

const BASE: &str = "https://catalog.example.test/catalog/";

fn config() -> RemoteConfig {
    RemoteConfig {
        enabled: true,
        base_url: BASE.into(),
        min_interval_secs: 3600,
    }
}

/// The built-in tree with one overlay entry changed (a human note) and the index
/// re-serialized with `serial`, signed by `key`.
fn remote_tree(serial: u64, key: &ed25519_dalek::SigningKey) -> BTreeMap<String, Vec<u8>> {
    let mut files = superba_catalog::builtin_files().unwrap();
    files.remove(SIGNATURE_PATH);
    let overlay = "overlay/features.toml".to_owned();
    let text = String::from_utf8(files[&overlay].clone()).unwrap();
    let text = text.replacen(
        "[\"feature:fast_mode\"]\nen = \"\"\n",
        "[\"feature:fast_mode\"]\nen = \"\"\nen_note = \"Remote note.\"\n",
        1,
    );
    assert!(text.contains("Remote note."), "fixture anchor not found");
    files.insert(overlay.clone(), text.into_bytes());
    let mut index: Index = serde_json::from_slice(&files[INDEX_PATH]).unwrap();
    index.serial = serial;
    for f in &mut index.files {
        if f.path == overlay {
            f.sha256 = sha256_hex(&files[&overlay]);
            f.size = files[&overlay].len() as u64;
        }
    }
    files.insert(
        INDEX_PATH.into(),
        to_pretty_json(&index).unwrap().into_bytes(),
    );
    sign_files(&mut files, &[key]);
    files
}

fn transport_for(files: &BTreeMap<String, Vec<u8>>) -> MemTransport {
    MemTransport {
        files: files
            .iter()
            .map(|(p, b)| (format!("{BASE}{p}"), b.clone()))
            .collect(),
        requests: Mutex::new(Vec::new()),
    }
}

fn builtin_serial() -> u64 {
    CatalogStore::builtin().unwrap().index().serial
}

fn run<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

#[allow(clippy::too_many_arguments)]
fn do_refresh(
    cfg: &RemoteConfig,
    t: &MemTransport,
    dir: &std::path::Path,
    now: u64,
    force: bool,
    tk: TrustedKey,
) -> superba_catalog::Result<RefreshOutcome> {
    let current = CatalogStore::builtin().unwrap();
    run(refresh_with_keys(
        cfg,
        t,
        dir,
        &current,
        now,
        force,
        &[tk],
        &[],
    ))
}

#[test]
fn disabled_by_default_and_makes_no_request() {
    assert!(!RemoteConfig::default().enabled);
    let (_, tk) = test_key(1);
    let t = MemTransport::default();
    let dir = tempfile::tempdir().unwrap();
    let out = do_refresh(&RemoteConfig::default(), &t, dir.path(), 0, true, tk).unwrap();
    assert_eq!(out, RefreshOutcome::Disabled);
    assert!(t.requests.lock().unwrap().is_empty());
    assert!(!dir.path().join("state.json").exists());
}

#[test]
fn newer_signed_catalog_is_cached_and_loaded() {
    let (sk, tk) = test_key(11);
    let serial = builtin_serial() + 5;
    let tree = remote_tree(serial, &sk);
    let t = transport_for(&tree);
    let dir = tempfile::tempdir().unwrap();

    let out = do_refresh(&config(), &t, dir.path(), 1000, false, tk).unwrap();
    assert_eq!(
        out,
        RefreshOutcome::Updated {
            serial,
            downloaded: 1
        }
    );
    // Only the changed overlay was downloaded besides the index and signature.
    let reqs = t.requests.lock().unwrap().clone();
    assert_eq!(reqs.len(), 3, "{reqs:?}");

    // The cache verifies with the test key...
    let cached = CatalogStore::from_files_with_keys(
        StoreOrigin::Cache(cache_tree(dir.path())),
        superba_catalog::store::read_tree(&cache_tree(dir.path())).unwrap(),
        TrustPolicy::RequireSignature,
        &[tk],
        &[],
    )
    .unwrap();
    assert_eq!(cached.index().serial, serial);
    let latest = cached.latest().unwrap();
    assert_eq!(
        cached
            .explain_feature(&latest, "fast_mode")
            .en_note
            .as_deref(),
        Some("Remote note.")
    );
    // ...but the production loader trusts only the embedded keys, so it falls back to
    // the built-in snapshot and says why.
    let loaded = superba_catalog::load(Some(dir.path())).unwrap();
    assert_eq!(loaded.store.origin(), &StoreOrigin::BuiltIn);
    assert_eq!(loaded.notices.len(), 1);

    // Within the interval nothing is requested.
    t.requests.lock().unwrap().clear();
    let out = do_refresh(&config(), &t, dir.path(), 1001, false, tk).unwrap();
    assert_eq!(out, RefreshOutcome::NotDue);
    assert!(t.requests.lock().unwrap().is_empty());
}

#[test]
fn same_index_is_up_to_date() {
    let (_, tk) = test_key(12);
    let mut files = superba_catalog::builtin_files().unwrap();
    // Re-sign the built-in index with the test key.
    files.remove(SIGNATURE_PATH);
    let (sk, _) = test_key(12);
    sign_files(&mut files, &[&sk]);
    let t = transport_for(&files);
    let dir = tempfile::tempdir().unwrap();
    let out = do_refresh(&config(), &t, dir.path(), 5, true, tk).unwrap();
    assert_eq!(
        out,
        RefreshOutcome::UpToDate {
            serial: builtin_serial()
        }
    );
    assert!(!cache_tree(dir.path()).exists());
}

#[test]
fn rollback_bad_signature_and_tampering_are_rejected_without_touching_the_cache() {
    let (sk, tk) = test_key(13);
    let dir = tempfile::tempdir().unwrap();

    // Older serial.
    if builtin_serial() > 1 {
        let tree = remote_tree(builtin_serial() - 1, &sk);
        let err =
            do_refresh(&config(), &transport_for(&tree), dir.path(), 0, true, tk).unwrap_err();
        assert!(matches!(err, CatalogError::Rollback { .. }), "{err}");
    }

    // Signed by an untrusted key.
    let (evil, _) = test_key(66);
    let tree = remote_tree(builtin_serial() + 1, &evil);
    let err = do_refresh(&config(), &transport_for(&tree), dir.path(), 0, true, tk).unwrap_err();
    assert!(matches!(err, CatalogError::Signature(_)), "{err}");

    // A file that does not match the signed index.
    let mut tree = remote_tree(builtin_serial() + 1, &sk);
    let f = tree.get_mut("overlay/features.toml").unwrap();
    f.extend_from_slice(b"\n");
    let err = do_refresh(&config(), &transport_for(&tree), dir.path(), 0, true, tk).unwrap_err();
    assert!(
        matches!(
            err,
            CatalogError::Integrity { .. } | CatalogError::Network { .. }
        ),
        "{err}"
    );

    // Unsigned.
    let mut tree = remote_tree(builtin_serial() + 1, &sk);
    tree.remove(SIGNATURE_PATH);
    let err = do_refresh(&config(), &transport_for(&tree), dir.path(), 0, true, tk).unwrap_err();
    assert!(matches!(err, CatalogError::Network { .. }), "{err}");

    assert!(!cache_tree(dir.path()).exists());
}

#[test]
fn base_url_must_be_https_or_loopback_http() {
    let mut c = config();
    for ok in [
        "https://raw.githubusercontent.com/krill-lab/superba/main/catalog",
        "http://127.0.0.1:8080/catalog/",
        "http://[::1]:9/",
        "http://localhost:1/x/",
    ] {
        c.base_url = ok.into();
        let u = c.base().unwrap();
        assert!(u.as_str().ends_with('/'), "{u}");
    }
    for bad in [
        "http://example.com/catalog/",
        "ftp://example.com/",
        "https://user:pw@example.com/",
        "https://example.com/c/?x=1",
        "not a url",
    ] {
        c.base_url = bad.into();
        assert!(c.base().is_err(), "{bad}");
    }
}

#[test]
fn load_without_cache_uses_builtin() {
    let dir = tempfile::tempdir().unwrap();
    let loaded = superba_catalog::load(Some(dir.path())).unwrap();
    assert_eq!(loaded.store.origin(), &StoreOrigin::BuiltIn);
    assert!(loaded.notices.is_empty());
    let loaded = superba_catalog::load(None).unwrap();
    assert_eq!(loaded.store.origin(), &StoreOrigin::BuiltIn);
}
