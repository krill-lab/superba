#![allow(missing_docs)]

//! Non-drawing code behind the Codex settings page: the loader against the
//! built-in settings catalog, profile choice, pending edits written back as
//! managed settings, value conversion and the runtime `codex features list`.

use pretty_assertions::assert_eq;
use superba_core::settings::{AuthChoice, CodexSettings, SettingEntry, SettingValue};
use superba_core::{
    AuthMode, KeyRef, ModelDefaults, Profile, ProfileId, ProfileSet, SiteId, catalog,
};
use superba_gui::pages::settings::model::{Change, Kind, Pending, Value};
use superba_gui::pages::settings::source::{self, Input};

fn profile(id: &str, site: &str) -> Profile {
    Profile {
        model_overrides: Default::default(),
        model_picker: Default::default(),
        compaction: Default::default(),
        id: ProfileId::new(id),
        name: format!("{id} profile"),
        builtin: None,
        site_id: SiteId::new(site),
        line_id: None,
        backup_lines: Vec::new(),
        auth_mode: AuthMode::ApiKey,
        auth: AuthChoice::Auto,
        key_ref: Some(KeyRef::Keyring {
            service: "superba-test".into(),
            account: id.into(),
        }),
        base_url_override: None,
        model: ModelDefaults::default(),
        codex_settings: CodexSettings::new(),
    }
}

fn store() -> std::sync::Arc<superba_catalog::CatalogStore> {
    superba_catalog::CatalogStore::builtin().expect("built-in catalog loads")
}

fn krill_set(settings: CodexSettings) -> ProfileSet {
    let mut set = ProfileSet::default();
    let mut p = profile("krill", catalog::KRILL_SITE_ID);
    p.codex_settings = settings;
    set.profiles.push(p);
    set
}

#[test]
fn model_from_the_builtin_catalog_lists_features_and_keys_by_category() {
    let mut settings = CodexSettings::new();
    settings.set("features.fast_mode", false).unwrap();
    settings.unset("web_search").unwrap();
    let input = Input {
        overview: None,
        profiles: Some(krill_set(settings)),
        profile: None,
    };
    let mut config = toml::Table::new();
    config.insert("hide_agent_reasoning".into(), toml::Value::Boolean(true));
    let model = source::build(&store(), &input, Some(&config), superba_catalog::Os::Linux).unwrap();
    assert_eq!(model.profile_id, Some(ProfileId::new("krill")));
    assert!(!model.profile_applied);
    assert!(model.catalog_version.is_some());
    assert!(model.entries.len() > 100, "{} entries", model.entries.len());
    // Categories follow the curation order and each has entries.
    let first = model.categories.first().unwrap();
    assert_eq!(first.id, "model");
    assert!(
        model
            .entries
            .iter()
            .all(|e| model.categories.iter().any(|c| c.id == e.category))
    );
    // A managed feature: the profile's value, flagged as managed, default from the registry.
    let fast = model.entry("features.fast_mode").unwrap();
    assert!(fast.is_feature());
    assert_eq!(fast.kind, Kind::Bool);
    assert!(fast.managed);
    assert_eq!(fast.current, Some(Value::Bool(false)));
    assert!(fast.default.is_some());
    // Krill recommends web_search = "live"; the profile explicitly removes it.
    let web = model.entry("web_search").unwrap();
    assert!(web.managed);
    assert_eq!(web.current, None);
    assert_eq!(web.recommended, Some(Value::Str("live".into())));
    // A key only the file sets is shown as configured, not managed.
    let hide = model.entry("hide_agent_reasoning").unwrap();
    assert!(!hide.managed);
    assert_eq!(hide.current, Some(Value::Bool(true)));
    assert_eq!(hide.file_value, Some(Value::Bool(true)));
    // Keys owned by the connection are read-only.
    let base = model.entry("openai_base_url").unwrap();
    assert!(base.reserved());
    assert!(!base.editable());
    // Map entries and per-profile copies are not listed as settings.
    assert!(
        model
            .entries
            .iter()
            .all(|e| !e.path.contains('<') && !e.path.starts_with("profiles."))
    );
}

#[test]
fn without_a_key_based_profile_the_page_is_read_only() {
    let input = Input {
        overview: None,
        profiles: Some(ProfileSet::default()),
        profile: None,
    };
    let model = source::build(&store(), &input, None, superba_catalog::Os::Linux).unwrap();
    assert_eq!(model.profile_id, None);
    assert_eq!(model.read_only_reason, Some("codex_settings.no_profile"));
}

#[test]
fn pick_profile_prefers_the_requested_then_the_applied_then_the_first_key_profile() {
    let mut set = krill_set(CodexSettings::new());
    set.profiles.push(profile("acme", "acme"));
    let first = source::pick_profile(&set, None, None).unwrap();
    assert_eq!(first.id.as_str(), "krill");
    let wanted = ProfileId::new("acme");
    assert_eq!(
        source::pick_profile(&set, None, Some(&wanted))
            .unwrap()
            .id
            .as_str(),
        "acme"
    );
    // Built-in profiles are never edited here.
    let official = ProfileId::new(catalog::PROFILE_OFFICIAL);
    assert_eq!(
        source::pick_profile(&set, None, Some(&official))
            .unwrap()
            .id
            .as_str(),
        "krill"
    );
}

#[test]
fn updated_profile_writes_pending_edits_as_managed_settings() {
    let set = krill_set(CodexSettings::new());
    let input = Input {
        overview: None,
        profiles: Some(set.clone()),
        profile: None,
    };
    let mut config = toml::Table::new();
    config.insert("web_search".into(), toml::Value::String("cached".into()));
    let model = source::build(&store(), &input, Some(&config), superba_catalog::Os::Linux).unwrap();
    let mut pending = Pending::default();
    pending.set_path(&model, "features.fast_mode", Change::Set(Value::Bool(true)));
    // Removing a key the file sets is an edit; removing an absent one is not.
    pending.set_path(&model, "web_search", Change::Unset);
    pending.set_path(&model, "hide_agent_reasoning", Change::Unset);
    assert_eq!(pending.len(), 2);
    pending.set_path(
        &model,
        "tool_output_token_limit",
        Change::Set(Value::Int(9000)),
    );
    let p = source::updated_profile(&set, &model, &pending).unwrap();
    assert_eq!(
        p.codex_settings.get("features.fast_mode"),
        Some(&SettingEntry::Set(SettingValue::Bool(true)))
    );
    assert_eq!(
        p.codex_settings.get("web_search"),
        Some(&SettingEntry::Unset)
    );
    assert_eq!(
        p.codex_settings.get("tool_output_token_limit"),
        Some(&SettingEntry::Set(SettingValue::Integer(9000)))
    );
    // Reserved paths are refused by the profile's settings.
    let mut bad = Pending::default();
    bad.set_path(
        &model,
        "openai_base_url",
        Change::Set(Value::Str("x".into())),
    );
    assert!(source::updated_profile(&set, &model, &bad).is_err());
}

#[test]
fn setting_values_convert_both_ways() {
    for v in [
        Value::Bool(true),
        Value::Int(-3),
        Value::Float(0.5),
        Value::Str("live".into()),
    ] {
        let s = source::to_setting(&v).unwrap();
        assert_eq!(source::from_setting(&s), v);
    }
    assert!(source::to_setting(&Value::Float(f64::NAN)).is_none());
}

#[test]
fn runtime_features_set_effective_values_and_note_differences() {
    let input = Input {
        overview: None,
        profiles: Some(krill_set(CodexSettings::new())),
        profile: None,
    };
    let mut model = source::build(&store(), &input, None, superba_catalog::Os::Linux).unwrap();
    let listed = superba_core::settings::FeaturesList::parse(
        "fast_mode  stable  false\nbrand_new_feature  under development  true\n",
    );
    source::apply_runtime(&mut model, "CLI", &listed);
    assert_eq!(
        model.entry("features.fast_mode").unwrap().effective,
        Some(false)
    );
    assert_eq!(model.notes.len(), 1);
    assert_eq!(model.notes[0].key, "codex_settings.note.runtime_differs");
}

#[cfg(unix)]
#[test]
fn features_list_runs_codex_in_a_private_home_and_cleans_up() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("cache");
    let fake = dir.path().join("codex");
    let seen = dir.path().join("seen");
    // The fake prints the copied config's first line and its CODEX_HOME.
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nhead -n1 \"$CODEX_HOME/config.toml\" > '{0}'\necho \"$CODEX_HOME\" >> '{0}'\n\
             printf 'fast_mode  stable  true\\nundo  removed  false\\n'\n",
            seen.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let listed = source::features_list(&fake, Some("model = \"x\"\n"), &cache).unwrap();
    assert_eq!(listed.entries.len(), 2);
    assert!(listed.entries[0].enabled);
    let seen = std::fs::read_to_string(&seen).unwrap();
    let mut lines = seen.lines();
    assert_eq!(lines.next(), Some("model = \"x\""));
    let home = lines.next().unwrap();
    assert!(home.starts_with(cache.to_str().unwrap()), "{home}");
    assert!(!std::path::Path::new(home).exists(), "private home removed");
}
