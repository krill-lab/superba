//! Provider import through the service (`import_scan`, `import_apply`): the
//! other tools' fixture stores in the test home, previews against the profile
//! set, keys into the secret store, conflicts, dry runs, and that nothing is
//! written to Codex and nothing restarts.

#![allow(missing_docs)]

mod fakes;

use std::path::{Path, PathBuf};

use fakes::*;
use superba_core::{
    ConflictAction, ImportApplyRequest, ImportOutcome, ImportScanRequest, ImportSelection,
    ImportSource, KeyRef, ProfileId, SiteKind, StoreStatus, SwitchService,
};

fn fixture(rel: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../superba-import/tests/fixtures")
        .join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn db(path: PathBuf, sql: &str, wal: bool) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(sql).unwrap();
    if wal {
        conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))
            .unwrap();
    }
}

/// cc-switch (schema 20) and AI Toolbox (schema 29) in the world's home.
fn install_both(w: &World) {
    let home = &w.ctx.home_dir;
    db(
        home.join(".cc-switch/cc-switch.db"),
        &fixture("ccs/schema-v20.sql"),
        false,
    );
    std::fs::write(
        home.join(".cc-switch/settings.json"),
        fixture("ccs/settings.json"),
    )
    .unwrap();
    db(
        w.ai_toolbox_db(),
        &fixture("ai-toolbox/schema-v29.sql"),
        true,
    );
}

#[tokio::test]
async fn scan_previews_both_tools_against_the_profile_set() {
    let w = World::new();
    install_both(&w);
    let sw = w.switcher();
    let scan = sw.import_scan(ImportScanRequest::default()).await.unwrap();
    assert_eq!(
        scan.sources.iter().map(|s| s.source).collect::<Vec<_>>(),
        ImportSource::ALL
    );
    let ccs = scan.source(ImportSource::Ccs).unwrap();
    assert_eq!(ccs.stores[0].status, StoreStatus::Read);
    let kimi = ccs.candidates.iter().find(|c| c.name == "Kimi").unwrap();
    assert!(kimi.current && kimi.preselected);
    assert_eq!(kimi.target.as_ref().unwrap().kind, SiteKind::Compatible);
    assert_eq!(kimi.target.as_ref().unwrap().lines.len(), 2);
    let toolbox = scan.source(ImportSource::AiToolbox).unwrap();
    assert!(
        toolbox
            .candidates
            .iter()
            .any(|c| c.name == "Example Relay" && c.preselected)
    );
    // Not importable ones carry their reason.
    let copilot = ccs
        .candidates
        .iter()
        .find(|c| c.name == "GitHub Copilot")
        .unwrap();
    assert!(copilot.not_importable_reason().is_some());
    // Reading wrote nothing anywhere.
    assert!(w.writes().is_empty());
    assert_eq!(*w.profiles.saves.lock().unwrap(), 0);
}

#[tokio::test]
async fn import_saves_sites_profiles_and_keys_but_never_touches_codex() {
    let w = World::new();
    install_both(&w);
    let sw = w.switcher();
    let scan = sw
        .import_scan(ImportScanRequest::source(ImportSource::Ccs))
        .await
        .unwrap();
    let selections = scan.default_selections(ConflictAction::Skip);
    assert!(!selections.is_empty());

    // Dry run: the same answer, nothing written.
    let dry = sw
        .import_apply(ImportApplyRequest {
            selections: selections.clone(),
            dry_run: true,
        })
        .await
        .unwrap();
    assert!(dry.dry_run);
    assert_eq!(*w.profiles.saves.lock().unwrap(), 0);
    assert!(
        w.secrets.map.lock().unwrap().len() == 1,
        "only the world's own key"
    );

    let report = sw
        .import_apply(ImportApplyRequest {
            selections,
            dry_run: false,
        })
        .await
        .unwrap();
    assert_eq!(
        report
            .items
            .iter()
            .map(|i| (&i.name, i.outcome.profile().is_some()))
            .collect::<Vec<_>>(),
        dry.items
            .iter()
            .map(|i| (&i.name, i.outcome.profile().is_some()))
            .collect::<Vec<_>>(),
    );
    assert_eq!(report.imported(), 4, "{report:#?}");
    let set = w.profiles.set.lock().unwrap().clone();
    let kimi = set.profiles.iter().find(|p| p.name == "Kimi").unwrap();
    let site = set
        .custom_sites
        .iter()
        .find(|s| s.id == kimi.site_id)
        .unwrap();
    assert_eq!(site.lines.len(), 2);
    assert_eq!(kimi.model.model.as_deref(), Some("kimi-k3"));
    let key_ref = kimi.key_ref.clone().unwrap();
    assert!(matches!(key_ref, KeyRef::Keyring { .. }));
    assert_eq!(
        w.secrets
            .map
            .lock()
            .unwrap()
            .get(&format!("profile:{}", kimi.id))
            .map(String::as_str),
        Some("sk-test-v20-kimi-000000000000000000")
    );
    // Nothing for Codex: no config, env, session, app or remote writes.
    assert!(w.writes().is_empty(), "{:?}", w.writes());

    // A second import of the same providers collides with the first one and is
    // skipped by default.
    let scan = sw
        .import_scan(ImportScanRequest::source(ImportSource::Ccs))
        .await
        .unwrap();
    let kimi_again = scan.candidates().find(|c| c.name == "Kimi").unwrap();
    let conflict = kimi_again.conflict.as_ref().unwrap();
    assert!(conflict.same_name && conflict.same_base_url);
    assert!(!kimi_again.preselected);
    let report = sw
        .import_apply(ImportApplyRequest {
            selections: vec![ImportSelection::new(
                kimi_again.id.clone(),
                ConflictAction::Skip,
            )],
            dry_run: false,
        })
        .await
        .unwrap();
    assert!(matches!(
        report.items[0].outcome,
        ImportOutcome::Skipped { .. }
    ));
    let profiles_before = w.profiles.set.lock().unwrap().profiles.len();
    // Rename adds a second provider; update keeps the count.
    let report = sw
        .import_apply(ImportApplyRequest {
            selections: vec![ImportSelection::new(
                kimi_again.id.clone(),
                ConflictAction::Rename,
            )],
            dry_run: false,
        })
        .await
        .unwrap();
    match &report.items[0].outcome {
        ImportOutcome::Created { profile_name, .. } => assert_eq!(profile_name, "Kimi (CC Switch)"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        w.profiles.set.lock().unwrap().profiles.len(),
        profiles_before + 1
    );
    let report = sw
        .import_apply(ImportApplyRequest {
            selections: vec![ImportSelection::new(
                kimi_again.id.clone(),
                ConflictAction::Update,
            )],
            dry_run: false,
        })
        .await
        .unwrap();
    assert!(
        matches!(report.items[0].outcome, ImportOutcome::Updated { .. }),
        "{report:#?}"
    );
    assert_eq!(
        w.profiles.set.lock().unwrap().profiles.len(),
        profiles_before + 1
    );
    assert!(w.writes().is_empty());
}

#[tokio::test]
async fn a_keychain_key_is_imported_without_a_key_and_says_so() {
    let w = World::new();
    let mut sql = fixture("ai-toolbox/schema-v29.sql");
    sql.push_str(
        r#"
INSERT INTO codex_provider (id, data, created_at, updated_at) VALUES ('kc01', jsonb('{"name":"Keychain Relay","category":"custom","settings_config":"{\"auth\":{},\"credential_ref\":\"keyring:ai-toolbox-codex-kc01\",\"config\":\"model = \\\"gpt-5.5\\\"\\nmodel_provider = \\\"custom\\\"\\n\\n[model_providers.custom]\\nbase_url = \\\"https://keychain-relay.example.com/v1\\\"\"}","sort_index":20,"is_applied":false,"is_disabled":false}'), 'x', 'x');
"#,
    );
    db(w.ai_toolbox_db(), &sql, true);
    let sw = w.switcher();
    let scan = sw
        .import_scan(ImportScanRequest::source(ImportSource::AiToolbox))
        .await
        .unwrap();
    let c = scan
        .candidates()
        .find(|c| c.name == "Keychain Relay")
        .unwrap();
    assert_eq!(c.key, superba_core::ImportKey::Keychain);
    let report = sw
        .import_apply(ImportApplyRequest {
            selections: vec![ImportSelection::new(c.id.clone(), ConflictAction::Skip)],
            dry_run: false,
        })
        .await
        .unwrap();
    let item = &report.items[0];
    match &item.outcome {
        ImportOutcome::Created {
            key_stored,
            profile,
            ..
        } => {
            assert!(!key_stored);
            let set = w.profiles.set.lock().unwrap().clone();
            assert!(set.profile(profile).unwrap().key_ref.is_none());
        }
        other => panic!("{other:?}"),
    }
    assert!(item.notes.iter().any(|n| n.key == "import.note.keychain"));
    assert_eq!(w.secrets.map.lock().unwrap().len(), 1, "no key was stored");
}

#[tokio::test]
async fn a_krill_line_maps_to_the_krill_preset_and_updates_the_existing_profile() {
    let w = World::new();
    // The world has a Krill profile `work` on the overseas line.
    let home = &w.ctx.home_dir;
    std::fs::create_dir_all(home.join(".cc-switch")).unwrap();
    std::fs::write(
        home.join(".cc-switch/config.json"),
        r#"{"version":2,"codex":{"current":"k1","providers":{
          "k1":{"id":"k1","name":"Krill CDN","settingsConfig":{"auth":{"OPENAI_API_KEY":"nb_test_cdn_0000000000"},
            "config":"model_provider = \"krill\"\nmodel = \"gpt-5.5\"\n\n[model_providers.krill]\nname = \"krill\"\nbase_url = \"https://api.cdn-krill-ai.com/codex/v1\"\nwire_api = \"responses\"\n"}},
          "k2":{"id":"k2","name":"work","settingsConfig":{"auth":{"OPENAI_API_KEY":"nb_test_ovs_0000000000"},
            "config":"model_provider = \"krill\"\n\n[model_providers.krill]\nbase_url = \"https://api.krill-code.net/v1\"\n"}}}}}"#,
    )
    .unwrap();
    let sw = w.switcher();
    let scan = sw
        .import_scan(ImportScanRequest::source(ImportSource::Ccs))
        .await
        .unwrap();
    let cdn = scan.candidates().find(|c| c.name == "Krill CDN").unwrap();
    assert_eq!(cdn.target.as_ref().unwrap().kind, SiteKind::Krill);
    assert!(cdn.conflict.is_none());
    let work = scan.candidates().find(|c| c.name == "work").unwrap();
    let conflict = work.conflict.as_ref().unwrap();
    assert!(conflict.same_name && conflict.same_base_url);
    let report = sw
        .import_apply(ImportApplyRequest {
            selections: vec![
                ImportSelection::new(cdn.id.clone(), ConflictAction::Skip),
                ImportSelection::new(work.id.clone(), ConflictAction::Update),
            ],
            dry_run: false,
        })
        .await
        .unwrap();
    let set = w.profiles.set.lock().unwrap().clone();
    let created = set
        .profile(report.items[0].outcome.profile().unwrap())
        .unwrap();
    assert_eq!(created.site_id.as_str(), "krill");
    assert_eq!(created.line_id.as_ref().unwrap().as_str(), "cdn");
    assert_eq!(
        report.items[1].outcome.profile(),
        Some(&ProfileId::new("work"))
    );
    assert_eq!(
        w.secrets
            .map
            .lock()
            .unwrap()
            .get("profile:work")
            .map(String::as_str),
        Some("nb_test_ovs_0000000000"),
        "update replaced the key"
    );
    assert!(w.writes().is_empty());
}

#[tokio::test]
async fn import_refuses_unknown_candidates() {
    let w = World::new();
    let sw = w.switcher();
    let err = sw
        .import_apply(ImportApplyRequest {
            selections: vec![ImportSelection::new("nope", ConflictAction::Skip)],
            dry_run: true,
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("nope"));
    // Nothing installed: every source is reported missing.
    let scan = sw.import_scan(ImportScanRequest::default()).await.unwrap();
    assert!(scan.sources.iter().all(|s| !s.found()));
}
