//! Claude Code's `settings.json` (user bug 79): Superba writes only the keys
//! it owns and keeps every other key with its value and place, takes the key
//! the provider's auth style needs and removes the other one, records the
//! values it replaced and restores them, never writes over a key someone
//! changed without a take-over, refuses a file that changed since the plan or
//! is not JSON, follows `CLAUDE_CONFIG_DIR`, backs the file up (the first
//! time as the original) and creates / removes the file and `env` block
//! when it was the one to create them.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use superba_config::ENV_SUPERBA_SECRET_BACKEND;
use superba_config::claude::{ClaudeConfigStore, ClaudeWriter, json as cjson};
use superba_config::codex_files::EditorStore;
use superba_core::claude::{
    self, AnthropicPreset, ENV_API_KEY, ENV_AUTH_TOKEN, ENV_BASE_URL, KEY_MODEL, KEY_MODEL_PICKER,
    desired, env_path, managed_paths, settings_path_text,
};
use superba_core::{
    AnthropicAuth, AnthropicEndpoint, BackupSource, ClaudeCapability, ClaudeConfig,
    ClaudeFileStatus, ClaudeModels, ClaudePlanKind, ClaudeSlot, CodexFileBackupQuery,
    CodexFileKind, DataDir, Error, LocalContext, MapEnv, ModelSpec, ProfileId, Secret, SlotModel,
    TargetId, Timestamp,
};

const T0: Timestamp = Timestamp::from_millis(1_790_000_000_000);
const KEY: &str = "sk-test-0000000000000000000000000000a1b2";

/// The key names of a real user's settings.json (names only; values made
/// up): everything here must survive every write.
const FIXTURE: &str = r#"{
  "$schema": "https://json.schemastore.org/claude-code-settings.json",
  "permissions": {
    "defaultMode": "acceptEdits",
    "allow": ["Bash(git status)"]
  },
  "model": "opus",
  "hooks": {
    "Notification": [
      { "matcher": "", "hooks": [{ "type": "command", "command": "notify-send done" }] }
    ],
    "SessionStart": [
      { "hooks": [{ "type": "command", "command": "echo hi" }] }
    ]
  },
  "enabledPlugins": { "formatter@anthropic-tools": true },
  "modelSettings": { "claude-opus-5-5": { "effortLevel": "high" } },
  "skipDangerousModePermissionPrompt": true,
  "skipWorkflowUsageWarning": true,
  "theme": "dark",
  "idleCompaction": false,
  "switchModelsOnFlag": true,
  "autoMode": {
    "allow": ["$defaults", "run tests"],
    "environment": ["CI=1"]
  }
}
"#;

struct Env {
    _tmp: tempfile::TempDir,
    ctx: LocalContext,
}

impl Env {
    fn new() -> Self {
        Self::with_env(MapEnv::new())
    }

    fn with_env(env: MapEnv) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = LocalContext::new(
            tmp.path().join("home"),
            DataDir::new(tmp.path().join("data")),
            Arc::new(env.with(ENV_SUPERBA_SECRET_BACKEND, "file")),
        );
        std::fs::create_dir_all(&ctx.home_dir).unwrap();
        Self { _tmp: tmp, ctx }
    }

    fn writer(&self) -> ClaudeWriter {
        ClaudeWriter::new(self.ctx.clone())
    }

    fn path(&self) -> PathBuf {
        self.writer().home().settings_json()
    }

    fn write(&self, text: &str) {
        let p = self.path();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.path()).unwrap()).unwrap()
    }

    fn plan_apply(&self, models: &ClaudeModels, auth: AnthropicAuth) -> superba_core::ClaudePlan {
        let mut e = endpoint();
        e.auth = auth;
        let d = desired(&e, &Secret::new(KEY), models, &|_| None);
        self.writer()
            .plan(
                ClaudePlanKind::Apply,
                Some(d),
                Some(ProfileId::new("krill")),
            )
            .unwrap()
    }

    fn apply(&self, models: &ClaudeModels) -> superba_core::ClaudeApplyReport {
        let plan = self.plan_apply(models, AnthropicAuth::Bearer);
        self.writer().apply(&plan, false, T0).unwrap()
    }

    fn restore(&self) -> superba_core::ClaudeApplyReport {
        let plan = self
            .writer()
            .plan(ClaudePlanKind::Restore, None, None)
            .unwrap();
        self.writer().apply(&plan, false, T0).unwrap()
    }
}

fn endpoint() -> AnthropicEndpoint {
    AnthropicEndpoint::new(url::Url::parse("https://relay.example.com").unwrap())
}

fn models() -> ClaudeModels {
    let mut m = ClaudeModels {
        main: Some("gpt-6-sol".into()),
        subagent: Some("gpt-5.6-luna".into()),
        ..ClaudeModels::default()
    };
    m.slots
        .insert(ClaudeSlot::Opus, SlotModel::new("gpt-6-sol"));
    m.slots.insert(
        ClaudeSlot::Haiku,
        SlotModel {
            model: "gpt-5.6-luna".into(),
            name: Some("Luna".into()),
            description: Some("fast".into()),
        },
    );
    m.add_to_picker("gpt-6-sol");
    m
}

fn keys(v: &Value) -> Vec<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

fn fixture_keys() -> Vec<String> {
    keys(&serde_json::from_str(FIXTURE).unwrap())
}

#[test]
fn apply_keeps_every_foreign_key_and_its_place() {
    let env = Env::new();
    env.write(FIXTURE);
    let report = env.apply(&models());
    assert!(report.kept.is_empty());
    let after = env.json();
    let before: Value = serde_json::from_str(FIXTURE).unwrap();
    // Every key of the file is still there, in order, with its value,
    // except `model` (owned) which now names the main model.
    let mut expected = fixture_keys();
    expected.extend(["env".to_owned(), "modelPicker".to_owned()]);
    assert_eq!(keys(&after), expected);
    for k in fixture_keys() {
        if k == KEY_MODEL {
            continue;
        }
        assert_eq!(after[&k], before[&k], "{k}");
    }
    assert_eq!(after["model"], "gpt-6-sol");
    let env_block = &after["env"];
    assert_eq!(env_block[ENV_BASE_URL], "https://relay.example.com");
    assert_eq!(env_block[ENV_AUTH_TOKEN], KEY);
    assert!(env_block.get(ENV_API_KEY).is_none());
    assert_eq!(env_block["ANTHROPIC_DEFAULT_OPUS_MODEL"], "gpt-6-sol");
    assert_eq!(env_block["ANTHROPIC_DEFAULT_HAIKU_MODEL"], "gpt-5.6-luna");
    assert_eq!(env_block["ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME"], "Luna");
    assert_eq!(
        env_block["ANTHROPIC_DEFAULT_HAIKU_MODEL_DESCRIPTION"],
        "fast"
    );
    assert_eq!(
        env_block["ANTHROPIC_DEFAULT_OPUS_MODEL_SUPPORTED_CAPABILITIES"],
        "effort,xhigh_effort,thinking"
    );
    assert_eq!(env_block["CLAUDE_CODE_SUBAGENT_MODEL"], "gpt-5.6-luna");
    assert_eq!(
        after["modelPicker"],
        json!({"options": [{"model": "gpt-6-sol", "behavesAs": "claude-opus-5-5"}]})
    );
    // Only managed paths changed.
    let managed = managed_paths();
    for c in &report.changed {
        assert!(
            managed.contains(&c.strip_prefix("settings.json ").unwrap_or(c).to_string()),
            "{c} is not owned"
        );
    }
    // Two-space indentation and a final newline; mode 0600 (it holds a key).
    let text = std::fs::read_to_string(env.path()).unwrap();
    assert!(text.starts_with("{\n  \"$schema\""), "{text}");
    assert!(text.ends_with("}\n"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(env.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn a_foreign_env_block_keeps_its_other_variables_and_order() {
    let env = Env::new();
    env.write(
        r#"{"env": {"DISABLE_TELEMETRY": "1", "ANTHROPIC_MODEL": "claude-x", "HTTPS_PROXY": "http://p:1"}}"#,
    );
    env.apply(&models());
    let after = env.json();
    let names: Vec<_> = after["env"].as_object().unwrap().keys().cloned().collect();
    // ANTHROPIC_MODEL would override `model`: owned and removed; the others
    // stay where they were, Superba's follow.
    assert_eq!(&names[..2], ["DISABLE_TELEMETRY", "HTTPS_PROXY"]);
    assert!(!names.contains(&"ANTHROPIC_MODEL".to_owned()));
    assert_eq!(after["env"]["HTTPS_PROXY"], "http://p:1");
    // Restore puts ANTHROPIC_MODEL back.
    env.restore();
    let restored = env.json();
    assert_eq!(restored["env"]["ANTHROPIC_MODEL"], "claude-x");
    assert_eq!(restored["env"].as_object().unwrap().len(), 3, "{restored}");
}

#[test]
fn the_other_auth_variable_is_owned_too() {
    let env = Env::new();
    env.write(r#"{"env": {"ANTHROPIC_API_KEY": "sk-user-own"}}"#);
    env.apply(&models());
    let after = env.json();
    assert_eq!(after["env"][ENV_AUTH_TOKEN], KEY);
    assert!(after["env"].get(ENV_API_KEY).is_none(), "{after}");
    // x-api-key style: the API key is written, the token removed.
    let plan = env.plan_apply(&models(), AnthropicAuth::ApiKey);
    env.writer().apply(&plan, false, T0).unwrap();
    let after = env.json();
    assert_eq!(after["env"][ENV_API_KEY], KEY);
    assert!(after["env"].get(ENV_AUTH_TOKEN).is_none());
    // Restore: the user's own key is back, Superba's gone.
    env.restore();
    assert_eq!(
        env.json(),
        json!({"env": {"ANTHROPIC_API_KEY": "sk-user-own"}})
    );
}

#[test]
fn restore_returns_to_the_file_before_superba_and_keeps_later_foreign_edits() {
    let env = Env::new();
    env.write(FIXTURE);
    env.apply(&models());
    // The user adds a key of their own after Superba's apply.
    let mut v = env.json();
    v["verbose"] = json!(true);
    env.write(&String::from_utf8(cjson::print(&v)).unwrap());
    let report = env.restore();
    assert!(report.kept.is_empty());
    let mut expected: Value = serde_json::from_str(FIXTURE).unwrap();
    expected["verbose"] = json!(true);
    assert_eq!(env.json(), expected);
    // Superba owns nothing any more.
    assert!(env.writer().state().unwrap().is_none());
    let file = env.writer().read().unwrap();
    assert!(env.writer().applied(&file).unwrap().is_none());
}

#[test]
fn a_file_superba_created_is_removed_again_by_restore() {
    let env = Env::new();
    assert!(!env.path().exists());
    let plan = env.plan_apply(&models(), AnthropicAuth::Bearer);
    assert_eq!(plan.files.first().and_then(|f| f.base_sha256.clone()), None);
    env.writer().apply(&plan, false, T0).unwrap();
    assert!(env.path().exists());
    let report = env.restore();
    assert_eq!(report.files.first().and_then(|f| f.sha256.clone()), None);
    assert!(
        !env.path().exists(),
        "created by Superba, empty again: removed"
    );
}

#[test]
fn an_env_block_superba_created_goes_when_restored() {
    let env = Env::new();
    env.write(r#"{"theme": "light"}"#);
    env.apply(&models());
    env.restore();
    assert_eq!(env.json(), json!({"theme": "light"}));
}

#[test]
fn drift_is_kept_unless_taken_over_and_stays_drift() {
    let env = Env::new();
    env.write("{}");
    env.apply(&models());
    // Someone points Claude Code elsewhere by hand.
    let mut v = env.json();
    v["env"][ENV_BASE_URL] = json!("https://mine.example.org");
    env.write(&String::from_utf8(cjson::print(&v)).unwrap());
    let file = env.writer().read().unwrap();
    let applied = env.writer().applied(&file).unwrap().unwrap();
    assert_eq!(applied.drift, vec![env_path(ENV_BASE_URL)]);

    // A new endpoint: the plan marks the changed key as drift.
    let mut e = endpoint();
    e.base_url = url::Url::parse("https://other.example.com").unwrap();
    let d = desired(&e, &Secret::new(KEY), &models(), &|_| None);
    let plan = env
        .writer()
        .plan(ClaudePlanKind::Apply, Some(d.clone()), None)
        .unwrap();
    let drift: Vec<_> = plan.drift().map(|c| c.slot.clone()).collect();
    assert_eq!(drift, vec![env_path(ENV_BASE_URL)]);
    // Without take-over it stays the user's...
    let report = env.writer().apply(&plan, false, T0).unwrap();
    assert_eq!(
        report.kept,
        vec![format!("settings.json {}", env_path(ENV_BASE_URL))]
    );
    assert_eq!(env.json()["env"][ENV_BASE_URL], "https://mine.example.org");
    // ...and is still drift the next time.
    let plan = env
        .writer()
        .plan(ClaudePlanKind::Apply, Some(d.clone()), None)
        .unwrap();
    assert_eq!(plan.drift().count(), 1);
    // With take-over it is written.
    env.writer().apply(&plan, true, T0).unwrap();
    assert_eq!(env.json()["env"][ENV_BASE_URL], "https://other.example.com");
    // A restore keeps drift too.
    let mut v = env.json();
    v["env"][ENV_BASE_URL] = json!("https://mine.example.org");
    env.write(&String::from_utf8(cjson::print(&v)).unwrap());
    let report = env.restore();
    assert_eq!(
        report.kept,
        vec![format!("settings.json {}", env_path(ENV_BASE_URL))]
    );
    assert_eq!(
        env.json(),
        json!({"env": {ENV_BASE_URL: "https://mine.example.org"}})
    );
}

#[test]
fn a_file_that_changed_since_the_plan_is_not_replaced() {
    let env = Env::new();
    env.write(r#"{"theme": "dark"}"#);
    let plan = env.plan_apply(&models(), AnthropicAuth::Bearer);
    env.write(r#"{"theme": "light"}"#);
    let err = env.writer().apply(&plan, false, T0).unwrap_err();
    assert!(matches!(err, Error::Conflict { .. }), "{err:?}");
    assert_eq!(env.json(), json!({"theme": "light"}));
}

#[test]
fn invalid_json_is_never_written() {
    let env = Env::new();
    env.write("{ \"theme\": \"dark\", }");
    let file = env.writer().read().unwrap();
    assert!(matches!(
        file.status,
        ClaudeFileStatus::Invalid { line: 1, .. }
    ));
    let d = desired(&endpoint(), &Secret::new(KEY), &models(), &|_| None);
    let err = env
        .writer()
        .plan(ClaudePlanKind::Apply, Some(d), None)
        .unwrap_err();
    assert!(err.to_string().contains("not valid"), "{err}");
    env.write("[1, 2]");
    let file = env.writer().read().unwrap();
    assert_eq!(file.status, ClaudeFileStatus::NotObject);
    // `env` that is not an object is refused too.
    env.write(r#"{"env": "oops"}"#);
    let d = desired(&endpoint(), &Secret::new(KEY), &models(), &|_| None);
    let err = env
        .writer()
        .plan(ClaudePlanKind::Apply, Some(d), None)
        .unwrap_err();
    assert!(err.to_string().contains("env is not an object"), "{err}");
    assert_eq!(env.json(), json!({"env": "oops"}));
}

#[test]
fn claude_config_dir_moves_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("cfg");
    let env = Env::with_env(MapEnv::new().with(claude::ENV_CLAUDE_CONFIG_DIR, dir.as_os_str()));
    assert_eq!(env.path(), dir.join("settings.json"));
    env.apply(&models());
    assert!(dir.join("settings.json").is_file());
    assert!(!env.ctx.home_dir.join(".claude").exists());
    let home = env.writer().home();
    assert!(home.from_env());
    assert_eq!(home.global_state_json(), dir.join(".claude.json"));
}

#[test]
fn default_and_windows_paths() {
    let env = Env::new();
    assert_eq!(
        env.path(),
        env.ctx.home_dir.join(".claude").join("settings.json")
    );
    assert_eq!(
        env.writer().home().global_state_json(),
        env.ctx.home_dir.join(".claude.json")
    );
    assert_eq!(
        settings_path_text(r"C:\Users\me", None, true),
        r"C:\Users\me\.claude\settings.json"
    );
    assert_eq!(
        settings_path_text(r"C:\Users\me\", Some(r"D:\cc\"), true),
        r"D:\cc\settings.json"
    );
    assert_eq!(
        settings_path_text("/home/me", None, false),
        "/home/me/.claude/settings.json"
    );
    assert_eq!(
        claude::managed_settings_dir_for(false, true),
        r"C:\Program Files\ClaudeCode"
    );
    assert_eq!(
        claude::managed_settings_dir_for(true, false),
        "/Library/Application Support/ClaudeCode"
    );
    assert_eq!(
        claude::managed_settings_dir_for(false, false),
        "/etc/claude-code"
    );
}

#[test]
fn backups_hold_the_original_and_each_replaced_file() {
    let env = Env::new();
    env.write(FIXTURE);
    let first = env.apply(&models());
    let backup = first
        .files
        .first()
        .and_then(|f| f.backup.clone())
        .expect("a backup of the replaced file");
    assert_eq!(backup.harness, superba_core::harness::HarnessId::ClaudeCode);
    let mut m = models();
    m.main = Some("gpt-5.6-luna".into());
    env.apply(&m);
    let store = EditorStore::new(env.ctx.clone());
    let list = store
        .list(
            &CodexFileBackupQuery {
                target: Some(TargetId::local()),
                kind: Some(CodexFileKind::ClaudeSettings),
            },
            Path::new("/nonexistent-codex-home"),
            "This computer",
        )
        .unwrap();
    let sources: Vec<_> = list.iter().map(|b| b.source.clone()).collect();
    assert!(sources.contains(&BackupSource::Original), "{sources:?}");
    assert_eq!(
        sources
            .iter()
            .filter(|s| matches!(s, BackupSource::BeforeApply { .. }))
            .count(),
        2
    );
    let original = list
        .iter()
        .find(|b| b.source == BackupSource::Original)
        .unwrap();
    let read = store
        .read(&original.id, Path::new("/nonexistent"), "x")
        .unwrap();
    assert_eq!(read.contents.expose(), FIXTURE.as_bytes());
    // The backups are sealed: the key never sits in them in clear text.
    for entry in walk(&env.ctx.data_dir.editor_backups_dir()) {
        let bytes = std::fs::read(&entry).unwrap();
        assert!(
            !bytes.windows(KEY.len()).any(|w| w == KEY.as_bytes()),
            "{} holds the key",
            entry.display()
        );
    }
    // So is the state.
    let state = std::fs::read(
        env.ctx
            .data_dir
            .harness_dir("claude_code")
            .join("state.enc"),
    )
    .unwrap();
    assert!(!state.windows(KEY.len()).any(|w| w == KEY.as_bytes()));
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

#[test]
fn unused_slots_and_picker_are_removed_and_capabilities_are_rendered() {
    let env = Env::new();
    env.write(r#"{"env": {"ANTHROPIC_DEFAULT_SONNET_MODEL": "claude-sonnet-x"}}"#);
    let mut m = models();
    m.capabilities.insert(
        "gpt-6-sol".into(),
        [ClaudeCapability::Effort, ClaudeCapability::MaxEffort]
            .into_iter()
            .collect(),
    );
    m.capabilities
        .insert("gpt-5.6-luna".into(), Default::default());
    m.replace_builtin = true;
    env.apply(&m);
    let after = env.json();
    // A stale Sonnet pin of the user is owned (removed now, restored later).
    assert!(after["env"].get("ANTHROPIC_DEFAULT_SONNET_MODEL").is_none());
    assert_eq!(
        after["env"]["ANTHROPIC_DEFAULT_OPUS_MODEL_SUPPORTED_CAPABILITIES"],
        "effort,max_effort"
    );
    // No capability: the variable is left out.
    assert!(
        after["env"]
            .get("ANTHROPIC_DEFAULT_HAIKU_MODEL_SUPPORTED_CAPABILITIES")
            .is_none()
    );
    assert_eq!(after["modelPicker"]["replaceBuiltInOptions"], true);
    // Emptying the configuration removes Superba's model keys.
    let empty = ClaudeModels::default();
    env.apply(&empty);
    let after = env.json();
    assert!(after.get(KEY_MODEL_PICKER).is_none());
    assert!(after.get(KEY_MODEL).is_none());
    assert_eq!(
        after["env"].as_object().unwrap().keys().collect::<Vec<_>>(),
        [ENV_BASE_URL, ENV_AUTH_TOKEN]
    );
    // (Superba's variables are added in a fixed order.)
    env.restore();
    assert_eq!(
        env.json(),
        json!({"env": {"ANTHROPIC_DEFAULT_SONNET_MODEL": "claude-sonnet-x"}})
    );
}

#[test]
fn nothing_changes_twice_and_restore_without_state_is_a_no_op() {
    let env = Env::new();
    env.write(FIXTURE);
    let plan = env
        .writer()
        .plan(ClaudePlanKind::Restore, None, None)
        .unwrap();
    assert!(plan.is_empty());
    assert!(!plan.notes.is_empty());
    env.apply(&models());
    let again = env.plan_apply(&models(), AnthropicAuth::Bearer);
    assert!(again.is_empty(), "{:?}", again.changes);
    let report = env.writer().apply(&again, false, T0).unwrap();
    assert!(
        report.files.iter().all(|f| f.backup.is_none()),
        "an unchanged file is not rewritten"
    );
}

#[test]
fn plans_show_keys_masked() {
    let env = Env::new();
    let plan = env.plan_apply(&models(), AnthropicAuth::Bearer);
    let token = plan
        .changes
        .iter()
        .find(|c| c.slot == env_path(ENV_AUTH_TOKEN))
        .unwrap();
    assert!(token.secret);
    let shown = token.after.clone().unwrap();
    assert!(!shown.contains(KEY), "{shown}");
    assert!(shown.ends_with("a1b2"), "{shown}");
    // Debug of the wanted values never prints the key either.
    let debug = format!("{:?}", plan.desired.unwrap());
    assert!(!debug.contains(KEY), "{debug}");
    // Serializing a plan leaves the values out.
    let json = serde_json::to_string(&env.plan_apply(&models(), AnthropicAuth::Bearer)).unwrap();
    assert!(!json.contains(KEY));
}

#[test]
fn pure_planning_marks_drift_only_for_keys_superba_wrote() {
    let current = json!({"env": {"ANTHROPIC_BASE_URL": "https://user.example"}});
    let d = desired(
        &endpoint(),
        &Secret::new(KEY),
        &ClaudeModels::default(),
        &|_| None,
    );
    // Never written by Superba: an ordinary change (shown in the plan).
    let env = Env::new();
    env.write(&current.to_string());
    let changes = env
        .writer()
        .plan(ClaudePlanKind::Apply, Some(d), None)
        .unwrap()
        .changes;
    let base = changes
        .iter()
        .find(|c| c.slot == env_path(ENV_BASE_URL))
        .unwrap();
    assert!(!base.drift);
    assert!(
        base.before
            .as_deref()
            .unwrap()
            .contains("https://user.example")
    );
}

#[test]
fn the_config_store_round_trips() {
    let env = Env::new();
    let store = ClaudeConfigStore::new(&env.ctx);
    assert_eq!(store.load().unwrap(), ClaudeConfig::default());
    let mut c = ClaudeConfig {
        provider: Some(ProfileId::new("krill")),
        ..ClaudeConfig::default()
    };
    c.models.insert(ProfileId::new("krill"), models());
    store.save(&c).unwrap();
    assert_eq!(store.load().unwrap(), c);
    // No key in it.
    let text = std::fs::read_to_string(env.ctx.data_dir.claude_config_json()).unwrap();
    assert!(!text.contains("sk-"), "{text}");
}

#[test]
fn presets_build_endpoints() {
    let krill = claude::anthropic_preset(claude::PRESET_KRILL_CODEX).unwrap();
    let origin = url::Url::parse("https://api.krill-code.net").unwrap();
    assert_eq!(
        krill.endpoint(Some(&origin)).unwrap().base_text(),
        "https://api.krill-code.net/codex"
    );
    let ds: AnthropicPreset = claude::anthropic_preset("deepseek").unwrap();
    let e = ds.endpoint(None).unwrap();
    assert_eq!(e.base_text(), "https://api.deepseek.com/anthropic");
    assert_eq!(
        e.join("v1/messages").as_str(),
        "https://api.deepseek.com/anthropic/v1/messages"
    );
    assert_eq!(e.auth, AnthropicAuth::Bearer);
    // Spec metadata feeds the capability suggestion.
    let mut spec = ModelSpec::new("gpt-x");
    spec.reasoning_levels = vec![
        superba_core::ReasoningLevel {
            effort: "high".into(),
            description: None,
        },
        superba_core::ReasoningLevel {
            effort: "xhigh".into(),
            description: None,
        },
    ];
    let caps = claude::suggested_capabilities("gpt-x", Some(&spec));
    assert_eq!(
        claude::capabilities_text(&caps),
        "effort,xhigh_effort,thinking"
    );
}
