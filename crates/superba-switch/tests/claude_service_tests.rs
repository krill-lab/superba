//! Claude Code through the service (user bug 79), with the real writer:
//! the view of a Krill provider (its line as the suggested endpoint), plan
//! and apply of the owned keys (the key masked in the plan, foreign keys
//! kept, the file backed up), the file editor on settings.json (owned keys
//! marked, a user edit saved with a backup), restore, a provider without an
//! Anthropic endpoint refused, and diagnostics of variables outside the file.

#![allow(missing_docs)]

mod fakes;

use std::sync::Arc;

use fakes::*;
use serde_json::{Value, json};
use superba_core::{
    ClaudeConfig, ClaudeModels, ClaudePlanKind, ClaudeSlot, CodexFileBackupQuery, CodexFileKind,
    CodexFileSave, DataDir, LocalContext, MapEnv, ProfileId, SaveReason, SecretBytes, SlotModel,
    SwitchService, TargetId,
};

fn world(env: MapEnv) -> World {
    let mut w = World::new();
    w.ctx = LocalContext::new(
        w.tmp.path().join("home"),
        DataDir::new(w.tmp.path().join("data")),
        Arc::new(env.with("SUPERBA_SECRET_BACKEND", "file")),
    );
    std::fs::create_dir_all(w.tmp.path().join("home")).unwrap();
    w
}

fn settings_path(w: &World) -> std::path::PathBuf {
    w.tmp
        .path()
        .join("home")
        .join(".claude")
        .join("settings.json")
}

fn config() -> ClaudeConfig {
    let mut models = ClaudeModels {
        main: Some("gpt-6-sol".into()),
        ..ClaudeModels::default()
    };
    models
        .slots
        .insert(ClaudeSlot::Opus, SlotModel::new("gpt-6-sol"));
    let mut c = ClaudeConfig {
        provider: Some(ProfileId::new("work")),
        ..ClaudeConfig::default()
    };
    c.models.insert(ProfileId::new("work"), models);
    c
}

#[tokio::test]
async fn apply_restore_and_the_file_editor_on_settings_json() {
    let w = world(MapEnv::new());
    let sw = w.switcher();
    let path = settings_path(&w);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        r#"{"theme": "dark", "permissions": {"defaultMode": "plan"}}"#,
    )
    .unwrap();

    // Without a configured provider it follows Codex (none applied here).
    let view = sw.claude_view().await.unwrap();
    assert!(view.follows_codex);
    assert_eq!(view.provider, None);
    let view = sw.save_claude_config(config()).await.unwrap();
    assert_eq!(view.provider, Some(ProfileId::new("work")));
    let endpoint = view.endpoint.clone().expect("Krill's suggested endpoint");
    assert_eq!(endpoint.base_text(), OVERSEAS);
    assert!(view.endpoint_suggested);
    assert!(view.applied.is_none());
    assert!(view.pending.contains(&"env.ANTHROPIC_BASE_URL".to_owned()));
    assert_eq!(view.settings_path, path);

    // Plan: the key is masked; apply writes only owned keys.
    let plan = sw.claude_plan(ClaudePlanKind::Apply).await.unwrap();
    let token = plan
        .changes
        .iter()
        .find(|c| c.path == "env.ANTHROPIC_AUTH_TOKEN")
        .unwrap();
    assert!(!token.after.as_deref().unwrap().contains(KEY));
    assert!(!serde_json::to_string(&plan).unwrap().contains(KEY));
    let report = sw.claude_apply(plan, false).await.unwrap();
    assert!(report.backup.is_some());
    let after: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(after["theme"], "dark");
    assert_eq!(after["permissions"], json!({"defaultMode": "plan"}));
    assert_eq!(after["env"]["ANTHROPIC_BASE_URL"], OVERSEAS);
    assert_eq!(after["env"]["ANTHROPIC_AUTH_TOKEN"], KEY);
    assert_eq!(after["env"]["ANTHROPIC_DEFAULT_OPUS_MODEL"], "gpt-6-sol");
    assert_eq!(after["model"], "gpt-6-sol");
    let view = sw.claude_view().await.unwrap();
    assert!(view.pending.is_empty(), "{:?}", view.pending);
    let applied = view.applied.unwrap();
    assert!(applied.drift.is_empty());
    assert!(applied.owned.contains(&"model".to_owned()));

    // The file editor: owned keys are listed (never with the key's value).
    let doc = sw
        .codex_file(TargetId::local(), CodexFileKind::ClaudeSettings)
        .await
        .unwrap();
    assert_eq!(doc.path, path.to_string_lossy());
    let token = doc
        .owned
        .iter()
        .find(|o| o.path == "env.ANTHROPIC_AUTH_TOKEN")
        .unwrap();
    assert_eq!(token.written, None);
    // A user edit of a foreign key is saved with a backup.
    let mut v: Value = serde_json::from_str(&doc.text().unwrap()).unwrap();
    v["theme"] = json!("light");
    let saved = sw
        .save_codex_file(CodexFileSave {
            target: TargetId::local(),
            kind: CodexFileKind::ClaudeSettings,
            contents: SecretBytes::new(serde_json::to_vec_pretty(&v).unwrap()),
            base_sha256: doc.sha256.clone(),
            reason: SaveReason::Edit,
            force: false,
        })
        .await
        .unwrap();
    assert!(saved.backup.is_some());
    assert!(saved.owned_changed.is_empty());
    assert!(saved.runtime.is_none());
    let backups = sw
        .codex_file_backups(CodexFileBackupQuery {
            target: Some(TargetId::local()),
            kind: Some(CodexFileKind::ClaudeSettings),
        })
        .await
        .unwrap();
    assert!(backups.len() >= 3, "{backups:?}");
    assert!(
        backups
            .iter()
            .any(|b| b.source == superba_core::BackupSource::Original)
    );

    // Restore: Superba's keys go, the user's edit stays.
    let plan = sw.claude_plan(ClaudePlanKind::Restore).await.unwrap();
    sw.claude_apply(plan, false).await.unwrap();
    let after: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        after,
        json!({"theme": "light", "permissions": {"defaultMode": "plan"}})
    );
    assert!(sw.claude_view().await.unwrap().applied.is_none());
}

#[tokio::test]
async fn a_provider_without_an_anthropic_endpoint_is_refused() {
    let w = world(MapEnv::new());
    // A compatible site on a host no preset knows.
    {
        let mut set = w.profiles.set.lock().unwrap();
        let mut p = set
            .profiles
            .iter()
            .find(|p| p.id == ProfileId::new("work"))
            .unwrap()
            .clone();
        p.id = ProfileId::new("other");
        p.site_id = "custom".into();
        p.line_id = None;
        p.base_url_override = Some(url::Url::parse("https://llm.example.org/v1").unwrap());
        set.profiles.push(p);
        set.custom_sites.push(
            superba_net::new_compatible_site(
                "custom".into(),
                "Custom",
                &url::Url::parse("https://llm.example.org/v1").unwrap(),
            )
            .unwrap(),
        );
    }
    let sw = w.switcher();
    let mut c = config();
    c.provider = Some(ProfileId::new("other"));
    let view = sw.save_claude_config(c).await.unwrap();
    assert_eq!(view.provider, Some(ProfileId::new("other")), "{view:?}");
    assert_eq!(view.endpoint, None);
    let err = sw.claude_plan(ClaudePlanKind::Apply).await.unwrap_err();
    assert!(err.to_string().contains("0.1.2"), "{err}");
}

#[tokio::test]
async fn diagnostics_name_variables_outside_the_file() {
    let w = world(
        MapEnv::new()
            .with("ANTHROPIC_API_KEY", "sk-shell")
            .with("CLAUDE_CODE_USE_BEDROCK", "1"),
    );
    std::fs::write(
        w.tmp.path().join("home").join(".zshrc"),
        "export ANTHROPIC_MODEL=claude-x\n# export ANTHROPIC_BASE_URL=commented\n",
    )
    .unwrap();
    let sw = w.switcher();
    let d = sw.claude_diagnostics().await.unwrap();
    let names: Vec<&str> = d.env_conflicts.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"ANTHROPIC_API_KEY"), "{names:?}");
    assert!(names.contains(&"CLAUDE_CODE_USE_BEDROCK"), "{names:?}");
    assert!(names.contains(&"ANTHROPIC_MODEL"), "{names:?}");
    assert!(!names.contains(&"ANTHROPIC_BASE_URL"), "{names:?}");
    assert!(d.findings.iter().any(|f| f.id == "claude_env_conflict"));
    assert!(d.findings.iter().any(|f| f.id == "claude_settings_absent"));
    // No value of a variable is reported.
    assert!(!serde_json::to_string(&d).unwrap().contains("sk-shell"));
}
