//! The Claude Code settings catalog (user bug 79): every official key and
//! documented variable plus the keys only the installed Claude Code knows,
//! each with Chinese text and a group; structured keys get their editors;
//! keys of a real user's file (names only) all render.

use superba_catalog::claude::{
    COMMON, ClaudeCatalog, ClaudeEditor, ClaudeGroup, ClaudeKind, ClaudeSource,
};

#[test]
fn every_key_and_variable_is_listed_with_chinese_text_and_a_group() {
    let c = ClaudeCatalog::builtin();
    let official = c
        .entries()
        .iter()
        .filter(|e| !e.env && e.source == ClaudeSource::Schemastore)
        .count();
    let binary = c
        .entries()
        .iter()
        .filter(|e| e.source == ClaudeSource::Binary)
        .count();
    let env = c.entries().iter().filter(|e| e.env).count();
    assert_eq!(official, 142);
    assert!(binary >= 40, "{binary}");
    assert_eq!(env, 340);
    for e in c.entries() {
        assert!(
            e.name_zh.as_deref().is_some_and(|n| !n.is_empty()),
            "{}",
            e.path
        );
        assert!(e.zh.as_deref().is_some_and(|n| !n.is_empty()), "{}", e.path);
        assert!(e.en.as_deref().is_some_and(|n| !n.is_empty()), "{}", e.path);
    }
    for g in ClaudeGroup::ALL {
        assert!(
            c.entries().iter().any(|e| e.group == g),
            "group {g:?} is empty"
        );
    }
    for p in COMMON {
        assert!(c.entry(p).is_some(), "common {p} is not in the catalog");
    }
}

#[test]
fn keys_of_a_real_file_all_render() {
    let c = ClaudeCatalog::builtin();
    // Names from a real settings.json (values are not needed).
    for key in [
        "$schema",
        "permissions",
        "model",
        "hooks",
        "enabledPlugins",
        "modelSettings",
        "skipDangerousModePermissionPrompt",
        "theme",
        "idleCompaction",
        "switchModelsOnFlag",
        "autoMode",
    ] {
        assert!(c.entry(key).is_some(), "{key} is not in the catalog");
    }
    // An internal key the catalog leaves out still renders as a file key.
    assert!(c.entry("skipWorkflowUsageWarning").is_none());
    let e = ClaudeCatalog::file_entry("skipWorkflowUsageWarning");
    assert_eq!(e.source, ClaudeSource::File);
    assert_eq!(e.group, ClaudeGroup::Other);
    assert_eq!(e.editor, ClaudeEditor::Json);
    let v = ClaudeCatalog::file_entry("env.MY_OWN_VAR");
    assert!(v.env);
    assert_eq!(v.kind, ClaudeKind::Text);
}

#[test]
fn structured_keys_get_their_editors() {
    let c = ClaudeCatalog::builtin();
    let editor = |k: &str| c.entry(k).unwrap().editor;
    assert_eq!(editor("env"), ClaudeEditor::Env);
    assert_eq!(editor("hooks"), ClaudeEditor::Hooks);
    assert_eq!(editor("enabledPlugins"), ClaudeEditor::BoolMap);
    assert_eq!(editor("modelSettings"), ClaudeEditor::ModelSettings);
    for k in [
        "permissions",
        "statusLine",
        "sandbox",
        "autoMode",
        "attribution",
        "modelPicker",
    ] {
        assert_eq!(editor(k), ClaudeEditor::Object, "{k}");
    }
    assert_eq!(editor("model"), ClaudeEditor::Value);
    // permissions: its fields come with the schema ($refs resolved).
    let perms = c.entry("permissions").unwrap().schema.clone().unwrap();
    let modes = perms["properties"]["defaultMode"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(modes.contains(&"acceptEdits".to_owned()), "{modes:?}");
    assert!(perms["properties"]["allow"]["items"]["examples"].is_array());
    assert!(c.hook_events().contains(&"PreToolUse".to_owned()));
    assert!(c.hook_events().contains(&"SessionStart".to_owned()));
    assert!(c.hook_events().contains(&"Notification".to_owned()));
}

#[test]
fn value_kinds_follow_the_schema() {
    let c = ClaudeCatalog::builtin();
    let kind = |k: &str| c.entry(k).unwrap().kind.clone();
    assert_eq!(kind("idleCompaction"), ClaudeKind::Bool);
    assert!(
        matches!(kind("theme"), ClaudeKind::Enum { open: true, ref values } if values.contains(&"dark".to_owned()))
    );
    assert!(
        matches!(kind("cleanupPeriodDays"), ClaudeKind::Number { integer: true, min: Some(m), .. } if m == 1.0)
    );
    assert_eq!(
        kind("env.CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"),
        ClaudeKind::Flag
    );
    assert_eq!(kind("env.ANTHROPIC_BASE_URL"), ClaudeKind::Text);
    assert_eq!(
        c.entry("env.ANTHROPIC_BASE_URL").unwrap().group,
        ClaudeGroup::Connection
    );
    assert_eq!(
        c.entry("env.ANTHROPIC_DEFAULT_OPUS_MODEL_SUPPORTED_CAPABILITIES")
            .unwrap()
            .group,
        ClaudeGroup::Capabilities
    );
    assert!(matches!(
        kind("availableModelsMatch"),
        ClaudeKind::Enum { .. }
    ));
    assert_eq!(kind("deniedModels"), ClaudeKind::List);
}
