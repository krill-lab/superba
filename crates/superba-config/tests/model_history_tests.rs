mod common;

use common::Env;
use serde_json::json;
use std::collections::BTreeMap;
use superba_core::ConfigEngine;

#[tokio::test]
async fn repair_preserves_other_preferences_effort_and_tier_and_is_idempotent() {
    let e = Env::new();
    let path = e.home.root().join(".codex-global-state.json");
    let before = json!({
        "unrelated": {"workspace": ["one", "two"]},
        "electron-persisted-atom-state": {
            "other-setting": true,
            "composer-recent-model-configurations-v1": [
                {"model":"krill/gpt-6.1-sol","reasoningEffort":"high","serviceTier":"priority","future":42},
                {"model":"other/model","reasoningEffort":"low"},
                {"model":"krill/unknown"}
            ]
        }
    });
    std::fs::write(&path, serde_json::to_vec(&before).unwrap()).unwrap();
    assert_eq!(
        e.engine.recent_models(&e.home).await.unwrap(),
        ["krill/gpt-6.1-sol", "other/model", "krill/unknown"]
    );
    let replacements = BTreeMap::from([("krill/gpt-6.1-sol".into(), "gpt-6.1-sol".into())]);
    assert_eq!(
        e.engine
            .repair_recent_models(&e.home, &replacements)
            .await
            .unwrap(),
        1
    );
    let after: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut expected = before;
    expected["electron-persisted-atom-state"]["composer-recent-model-configurations-v1"][0]["model"] =
        json!("gpt-6.1-sol");
    assert_eq!(after, expected);
    assert_eq!(
        e.engine
            .repair_recent_models(&e.home, &replacements)
            .await
            .unwrap(),
        0
    );
    let backup_root = e.ctx.data_dir.root().join("backups/model-history");
    let home_dir = std::fs::read_dir(backup_root)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let backup = std::fs::read_dir(home_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let encrypted = std::fs::read(backup).unwrap();
    assert!(encrypted.starts_with(b"SPBV1\0"));
    assert!(!String::from_utf8_lossy(&encrypted).contains("other/model"));
}

#[tokio::test]
async fn malformed_and_missing_preferences_are_never_overwritten() {
    let e = Env::new();
    let path = e.home.root().join(".codex-global-state.json");
    let replacements = BTreeMap::from([("krill/a".into(), "a".into())]);
    assert!(e.engine.recent_models(&e.home).await.unwrap().is_empty());
    assert_eq!(
        e.engine
            .repair_recent_models(&e.home, &replacements)
            .await
            .unwrap(),
        0
    );
    assert!(!path.exists());
    for invalid in [
        b"not JSON".as_slice(),
        br#"{"electron-persisted-atom-state":{"composer-recent-model-configurations-v1":{}}}"#,
    ] {
        std::fs::write(&path, invalid).unwrap();
        assert!(e.engine.recent_models(&e.home).await.is_err());
        assert!(
            e.engine
                .repair_recent_models(&e.home, &replacements)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), invalid);
    }
}

#[tokio::test]
async fn duplicate_repaired_entries_keep_distinct_effort_and_tier() {
    let e = Env::new();
    let path = e.home.root().join(".codex-global-state.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&json!({"electron-persisted-atom-state":{
            "composer-recent-model-configurations-v1":[
                {"model":"krill/a","reasoningEffort":"high","serviceTier":"priority"},
                {"model":"a","reasoningEffort":"high","serviceTier":"priority"},
                {"model":"a","reasoningEffort":"xhigh","serviceTier":"priority"}
            ]
        }}))
        .unwrap(),
    )
    .unwrap();
    e.engine
        .repair_recent_models(&e.home, &BTreeMap::from([("krill/a".into(), "a".into())]))
        .await
        .unwrap();
    let after: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let recent = after["electron-persisted-atom-state"]["composer-recent-model-configurations-v1"]
        .as_array()
        .unwrap();
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0]["reasoningEffort"], "high");
    assert_eq!(recent[1]["reasoningEffort"], "xhigh");
}
