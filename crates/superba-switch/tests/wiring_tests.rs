#![allow(missing_docs)]

use superba_core::{LocalContext, catalog};

#[tokio::test]
async fn default_service_wires_all_components_without_touching_the_real_home() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = LocalContext::for_tests(tmp.path());
    assert_eq!(
        ctx.codex_home().root(),
        tmp.path().join("home").join(".codex")
    );
    let service = superba_switch::default_service(ctx);

    let sites = service.sites().await.unwrap();
    let ids: Vec<_> = sites.iter().map(|s| s.id.as_str().to_owned()).collect();
    assert_eq!(ids, [catalog::KRILL_SITE_ID, catalog::OFFICIAL_SITE_ID]);

    // Read-only calls never create the Codex home, whatever the components can do yet.
    let _ = service.overview().await;
    let _ = service.doctor().await;
    assert!(!tmp.path().join("home").join(".codex").exists());
}

#[test]
fn one_settings_catalog_serves_the_planner_the_remote_hosts_and_the_settings_page() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = LocalContext::for_tests(tmp.path());
    let loaded = superba_switch::settings_catalog(&ctx.data_dir).expect("built-in catalog");
    assert!(loaded.notices.is_empty());
    let version = superba_core::CodexVersion::parse("0.162.0").unwrap();
    let source = superba_switch::catalog_source(Some(&loaded));
    let snapshot = source.snapshot(Some(&version)).expect("a snapshot");
    // The page reads the same store the planner's snapshots come from.
    assert_eq!(
        loaded.source.store().index().serial,
        superba_catalog::CatalogStore::builtin()
            .unwrap()
            .index()
            .serial
    );
    assert!(!snapshot.keys.is_empty());
    let remote = superba_switch::remote_catalog(Some(&loaded))(&superba_core::RemoteOs::Linux);
    assert!(remote.snapshot(Some(&version)).is_some());

    // A broken download is ignored with a notice; the built-in catalog stays.
    let tree =
        superba_catalog::remote::cache_tree(&superba_catalog::remote::cache_dir(&ctx.data_dir));
    std::fs::create_dir_all(tree.join("dist")).unwrap();
    std::fs::write(tree.join("dist/index.json"), b"{}").unwrap();
    let loaded = superba_switch::settings_catalog(&ctx.data_dir).expect("built-in catalog");
    assert_eq!(loaded.notices.len(), 1);

    // Without a catalog nothing is filtered.
    assert!(
        superba_switch::catalog_source(None)
            .snapshot(Some(&version))
            .is_none()
    );
}
