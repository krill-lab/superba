#![allow(missing_docs)]

//! The Claude Code pages (user bug 79): the settings model lists every key
//! of a real file (keys only the file has under Other, edited as JSON),
//! writes edits back keeping every other key and its place (variables into
//! `env`); the model rows merge the lists with ids added by hand; both pages
//! render on the fake service with named controls, the Codex-only id marked,
//! a dropdown per slot and the local router route disabled.

mod a11y_harness;
mod support;

use std::sync::Arc;

use a11y_harness::AppHarness;
use egui::accesskit::Role;
use serde_json::{Value, json};
use superba_catalog::claude::{ClaudeGroup, ClaudeSource};
use superba_core::{
    ClaudeModels, CodexFileDoc, CodexFileKind, FileSchema, Locale, ProfileId, SecretBytes,
    TargetId, Timestamp,
};
use superba_gui::i18n::Tr;
use superba_gui::pages::Page;
use superba_gui::pages::claude::connect::rows;
use superba_gui::pages::claude::model::{Filter, Pending, Rail, SettingsModel};
use support::fake::{FakeOptions, FakeService};

/// Key names of a real user's settings.json (values made up).
const FIXTURE: &str = r#"{
  "$schema": "https://json.schemastore.org/claude-code-settings.json",
  "permissions": { "defaultMode": "acceptEdits" },
  "model": "opus",
  "hooks": {
    "Notification": [{ "hooks": [{ "type": "command", "command": "notify-send hi" }] }],
    "SessionStart": [{ "hooks": [{ "type": "command", "command": "echo start" }] }]
  },
  "enabledPlugins": { "formatter@anthropic-tools": true },
  "modelSettings": { "claude-opus-5-5": { "effortLevel": "high" } },
  "skipDangerousModePermissionPrompt": true,
  "skipWorkflowUsageWarning": true,
  "theme": "dark",
  "idleCompaction": false,
  "switchModelsOnFlag": true,
  "autoMode": { "allow": ["$defaults"], "environment": ["CI=1"] },
  "env": { "MY_OWN_VAR": "x", "DISABLE_TELEMETRY": "1" }
}
"#;

fn doc(text: &str) -> CodexFileDoc {
    CodexFileDoc {
        target: TargetId::local(),
        target_label: "This computer".into(),
        kind: CodexFileKind::ClaudeSettings,
        path: "/home/u/.claude/settings.json".into(),
        contents: Some(SecretBytes::new(text.as_bytes().to_vec())),
        sha256: Some("abc".into()),
        schema: FileSchema::default(),
        owned: vec![superba_core::OwnedKey {
            path: "env.ANTHROPIC_BASE_URL".into(),
            written: None,
        }],
        owns_credential: false,
        profile: Some(ProfileId::new("krill")),
        read_only: None,
        notes: Vec::new(),
        read_at: Timestamp::EPOCH,
    }
}

#[test]
fn every_key_of_a_real_file_has_a_row_and_unknown_keys_go_to_other() {
    let m = SettingsModel::build(&doc(FIXTURE));
    assert!(m.error.is_none());
    let file: Value = serde_json::from_str(FIXTURE).unwrap();
    for key in file.as_object().unwrap().keys() {
        let row = m.row(key).unwrap_or_else(|| panic!("{key} has no row"));
        assert_eq!(row.value.as_ref(), file.get(key), "{key}");
    }
    let internal = m.row("skipWorkflowUsageWarning").unwrap();
    assert_eq!(internal.entry.source, ClaudeSource::File);
    assert_eq!(internal.entry.group, ClaudeGroup::Other);
    let own = m.row("env.MY_OWN_VAR").unwrap();
    assert_eq!(own.entry.source, ClaudeSource::File);
    assert_eq!(own.value, Some(json!("x")));
    // "In the file" lists what is set; the rest are listed by group.
    let changed: Vec<&str> = m
        .listed(
            &Filter {
                rail: Rail::Changed,
                ..Filter::default()
            },
            &Pending::default(),
        )
        .iter()
        .map(|r| r.path())
        .collect();
    assert!(changed.contains(&"hooks"));
    assert!(changed.contains(&"env.DISABLE_TELEMETRY"));
    assert!(!changed.contains(&"env.ANTHROPIC_MODEL"));
    // Search finds Chinese names too.
    let found = m.listed(
        &Filter {
            query: "遥测".into(),
            ..Filter::default()
        },
        &Pending::default(),
    );
    assert!(found.iter().any(|r| r.path() == "env.DISABLE_TELEMETRY"));
    // A managed key is marked.
    assert!(m.row("env.ANTHROPIC_BASE_URL").unwrap().owned);
}

#[test]
fn edits_keep_every_other_key_and_variables_go_into_env() {
    let m = SettingsModel::build(&doc(FIXTURE));
    let mut pending = Pending::default();
    pending
        .0
        .insert("env.API_TIMEOUT_MS".into(), Some(json!("3000000")));
    pending.0.insert("theme".into(), Some(json!("light")));
    pending.0.insert("idleCompaction".into(), None);
    let text = m.render(&pending).unwrap();
    let after: Value = serde_json::from_str(&text).unwrap();
    let before: Value = serde_json::from_str(FIXTURE).unwrap();
    let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
    let mut expected = keys(&before);
    expected.retain(|k| k != "idleCompaction");
    assert_eq!(keys(&after), expected, "order kept, removed key gone");
    assert_eq!(after["theme"], "light");
    assert_eq!(after["env"]["API_TIMEOUT_MS"], "3000000");
    assert_eq!(after["env"]["MY_OWN_VAR"], "x");
    assert_eq!(after["hooks"], before["hooks"]);
    assert_eq!(after["autoMode"], before["autoMode"]);
    // No env block yet: it is created.
    let m = SettingsModel::build(&doc("{\"theme\": \"dark\"}"));
    let mut pending = Pending::default();
    pending
        .0
        .insert("env.DISABLE_TELEMETRY".into(), Some(json!("1")));
    let after: Value = serde_json::from_str(&m.render(&pending).unwrap()).unwrap();
    assert_eq!(
        after,
        json!({"theme": "dark", "env": {"DISABLE_TELEMETRY": "1"}})
    );
    // An invalid file is reported, never rendered over.
    let m = SettingsModel::build(&doc("{ nope"));
    assert!(m.error.is_some());
}

#[test]
fn model_rows_add_ids_by_hand_and_the_named_models() {
    let mut models = ClaudeModels::default();
    models.extra_models.push("qwen3.7-plus".into());
    models.main = Some("kimi-k3".into());
    let r = rows(None, &models);
    let ids: Vec<&str> = r.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["qwen3.7-plus", "kimi-k3"]);
    assert!(r.iter().all(|r| r.manual && r.usable));
}

fn fake() -> Arc<FakeService> {
    Arc::new(FakeService::new(FakeOptions {
        fast: true,
        ..Default::default()
    }))
}

#[test]
fn the_connection_page_shows_the_provider_models_and_the_disabled_router() {
    for locale in [Locale::ZhCn, Locale::En] {
        let tr = Tr::new(locale);
        let mut h = AppHarness::new(fake(), locale, Page::ClaudeCode);
        let t = h.until("models listed", |t| t.has("gpt-6.1-sol"));
        assert!(t.has("deepseek-v4"), "{}", t.dump());
        // The Codex-only id is marked.
        assert!(
            t.has_containing(&tr.t("claude.models.codex_only")),
            "{}",
            t.dump()
        );
        // The provider follows Codex; the local router is not offered yet.
        let combo = t.find(Some(Role::ComboBox), &tr.t("claude.provider.title"));
        assert!(combo.is_some(), "{}", t.dump());
        assert!(t.has_containing(&tr.t("claude.route.router")));
        // Krill's suggested endpoint.
        assert!(t.has("https://api.kbsfdxc.net"), "{}", t.dump());
        // Every control has a name.
        assert!(t.unnamed().is_empty(), "{:?}", t.unnamed());
        // Every slot has a model dropdown, and every model a /model box.
        let opus = tr.f(
            "claude.slots.model_of",
            &[("slot", &tr.t("claude.slot.opus"))],
        );
        assert!(
            t.find(Some(Role::ComboBox), &opus).is_some(),
            "{}",
            t.dump()
        );
        let show = tr.f("claude.picker.show_named", &[("model", &"gpt-6.1-sol")]);
        assert!(
            t.find(Some(Role::CheckBox), &show).is_some(),
            "{}",
            t.dump()
        );
    }
}

#[test]
fn the_settings_page_lists_groups_and_opens_the_file_editor() {
    let tr = Tr::new(Locale::ZhCn);
    let mut h = AppHarness::new(fake(), Locale::ZhCn, Page::ClaudeSettings);
    let t = h.until("settings listed", |t| {
        t.has(&tr.t("claude.settings.group.connection"))
    });
    for g in [
        "connection",
        "models",
        "capabilities",
        "output_context",
        "traffic_privacy",
        "other",
    ] {
        assert!(
            t.has_containing(&tr.t(&format!("claude.settings.group.{g}"))),
            "{g}"
        );
    }
    assert!(t.unnamed().is_empty(), "{:?}", t.unnamed());
    let t = h.click(Some(Role::Button), &tr.t("claude.settings.edit_file"));
    let t = if t.has_containing("settings.json") {
        t
    } else {
        h.settle()
    };
    assert!(t.has_containing("settings.json"), "{}", t.dump());
}
