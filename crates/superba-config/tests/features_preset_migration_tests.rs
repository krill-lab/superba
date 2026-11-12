//! The one-time migration of user bug 144 in the profile store: a
//! `profiles.json` from before it (no `features_preset_dropped`) loses every
//! value it stores for the feature keys of the old Krill preset
//! (`features.context_management`, `features.token_budget.enabled`,
//! `features.token_budget.use_history_notes_extension`), `true` or `false`, in
//! every profile; the rest of the settings stays; the profiles it changed are
//! owed one clean-up write; the run is recorded once. Remove after
//! alpha.17+N.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use superba_config::{ENV_SUPERBA_SECRET_BACKEND, JsonProfileStore};
use superba_core::codex::{FORMER_PRESET_KEYS, RETIRED_PRESET_KEYS};
use superba_core::{
    AuthMode, CodexSettings, DataDir, LocalContext, MapEnv, Profile, ProfileId, ProfileSet,
    ProfileStore, SettingEntry, SettingValue, SiteId, catalog,
};

fn context(root: &Path) -> LocalContext {
    LocalContext::new(
        root.join("home"),
        DataDir::new(root.join("data")),
        Arc::new(MapEnv::new().with(ENV_SUPERBA_SECRET_BACKEND, "file")),
    )
}

fn profile(id: &str, site: &str, settings: CodexSettings) -> Profile {
    Profile {
        id: ProfileId::new(id),
        name: id.into(),
        builtin: None,
        site_id: SiteId::new(site),
        line_id: None,
        backup_lines: Vec::new(),
        disabled_lines: Default::default(),
        auth_mode: AuthMode::ApiKey,
        key_ref: None,
        base_url_override: None,
        model: Default::default(),
        codex_settings: settings,
        model_overrides: Default::default(),
        model_picker: Default::default(),
        compaction: Default::default(),
        anthropic: None,
        model_source: None,
    }
}

/// The Krill preset up to alpha.16.
fn old_preset() -> CodexSettings {
    let mut s = kept_preset();
    for path in RETIRED_PRESET_KEYS {
        s.set(path, true).unwrap();
    }
    s
}

/// What the migration leaves of it: the keys the preset still holds and
/// the ones new profiles no longer get but stored profiles keep.
fn kept_preset() -> CodexSettings {
    let mut s = CodexSettings::krill_recommended();
    for path in FORMER_PRESET_KEYS {
        s.set(path, true).unwrap();
    }
    s
}

/// A `profiles.json` as Superba up to alpha.16 wrote it.
fn write_old_file(ctx: &LocalContext) {
    let mut set = ProfileSet::default();
    // The user turned the features off on the Settings page.
    let mut off = old_preset();
    for path in RETIRED_PRESET_KEYS {
        off.set(path, false).unwrap();
    }
    // Another site whose user set one of them, and removes another one.
    let mut other = CodexSettings::new();
    other.set("features.context_management", true).unwrap();
    other.unset("features.token_budget.enabled").unwrap();
    other.set("features.memories", true).unwrap();
    set.profiles.extend([
        profile("krill-old", catalog::KRILL_SITE_ID, old_preset()),
        profile("krill-off", catalog::KRILL_SITE_ID, off),
        profile("other", "example", other),
        profile(
            "clean",
            catalog::KRILL_SITE_ID,
            CodexSettings::krill_recommended(),
        ),
    ]);
    set.settings.features_preset_dropped = false;
    let json = serde_json::to_string_pretty(&set).unwrap();
    assert!(!json.contains("features_preset_dropped"), "old file shape");
    fs::create_dir_all(ctx.data_dir.root()).unwrap();
    fs::write(ctx.data_dir.profiles_json(), json).unwrap();
}

fn settings(set: &ProfileSet, id: &str) -> CodexSettings {
    set.profile(&ProfileId::new(id))
        .unwrap()
        .codex_settings
        .clone()
}

#[tokio::test]
async fn every_stored_value_of_the_old_feature_keys_goes_and_the_run_is_recorded() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(tmp.path());
    write_old_file(&ctx);
    let set = JsonProfileStore::new(ctx.clone()).load().await.unwrap();
    // `true` and `false` alike; the rest of the preset stays.
    assert_eq!(settings(&set, "krill-old"), kept_preset());
    assert_eq!(settings(&set, "krill-off"), kept_preset());
    // Any site; an entry that removes a key and other features stay.
    let other = settings(&set, "other");
    assert_eq!(other.get("features.context_management"), None);
    assert_eq!(
        other.get("features.token_budget.enabled"),
        Some(&SettingEntry::Unset)
    );
    assert_eq!(
        other.get("features.memories"),
        Some(&SettingEntry::Set(SettingValue::Bool(true)))
    );
    assert!(set.settings.features_preset_dropped);
    // One clean-up write is owed for the profiles it changed.
    assert_eq!(
        set.settings.features_cleanup,
        ["krill-old", "krill-off", "other"].map(ProfileId::new)
    );
    // Recorded on disk at once.
    let on_disk = fs::read_to_string(ctx.data_dir.profiles_json()).unwrap();
    assert!(
        on_disk.contains("\"features_preset_dropped\": true"),
        "{on_disk}"
    );
    let disk: ProfileSet = serde_json::from_str(&on_disk).unwrap();
    for path in RETIRED_PRESET_KEYS {
        assert_eq!(settings(&disk, "krill-old").get(path), None, "{path}");
    }
}

#[tokio::test]
async fn it_runs_once_so_a_value_the_user_sets_again_stays() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(tmp.path());
    write_old_file(&ctx);
    let store = JsonProfileStore::new(ctx.clone());
    let mut set = store.load().await.unwrap();
    // The user turns context management on, on the Settings page.
    set.profiles
        .iter_mut()
        .find(|p| p.id == "krill-old")
        .unwrap()
        .codex_settings
        .set("features.context_management", true)
        .unwrap();
    set.settings.features_cleanup.clear();
    store.save(&set).await.unwrap();
    for _ in 0..2 {
        let set = JsonProfileStore::new(ctx.clone()).load().await.unwrap();
        assert_eq!(
            settings(&set, "krill-old").get("features.context_management"),
            Some(&SettingEntry::Set(SettingValue::Bool(true)))
        );
        assert!(set.settings.features_cleanup.is_empty());
    }
}

#[tokio::test]
async fn a_new_profile_set_and_a_new_krill_profile_have_none_of_them() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(tmp.path());
    let set = JsonProfileStore::new(ctx.clone()).load().await.unwrap();
    assert!(set.settings.features_preset_dropped);
    assert!(set.settings.features_cleanup.is_empty());
    assert!(!ctx.data_dir.profiles_json().exists());
    let preset = CodexSettings::krill_recommended();
    for path in RETIRED_PRESET_KEYS {
        assert_eq!(preset.get(path), None, "{path}");
    }
}
