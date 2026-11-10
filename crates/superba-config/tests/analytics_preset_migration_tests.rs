//! The one-time migration of user bug 137 in the profile store: a
//! `profiles.json` from before it (no `analytics_preset_dropped`) loses
//! `analytics.enabled = false` from built-in Krill profiles only; the run is
//! recorded, so a value the user sets again afterwards survives every later
//! load; a new profile set needs no migration. Remove after alpha.14+N.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use superba_config::{ENV_SUPERBA_SECRET_BACKEND, JsonProfileStore};
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

fn profile(id: &str, site: &str, analytics: Option<bool>) -> Profile {
    let mut settings = CodexSettings::krill_recommended();
    if let Some(v) = analytics {
        settings.set("analytics.enabled", v).unwrap();
    }
    Profile {
        id: ProfileId::new(id),
        name: id.into(),
        builtin: None,
        site_id: SiteId::new(site),
        line_id: None,
        backup_lines: Vec::new(),
        disabled_lines: Default::default(),
        auth_mode: AuthMode::Pat,
        auth: Default::default(),
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

/// A `profiles.json` as Superba up to alpha.13 wrote it.
fn write_old_file(ctx: &LocalContext) {
    let mut set = ProfileSet::default();
    set.profiles.extend([
        // The old Krill preset.
        profile("krill-old", catalog::KRILL_SITE_ID, Some(false)),
        // A Krill profile whose user turned telemetry back on.
        profile("krill-on", catalog::KRILL_SITE_ID, Some(true)),
        // Another site: never the Krill preset.
        profile("other", "example", Some(false)),
    ]);
    set.settings.analytics_preset_dropped = false;
    let json = serde_json::to_string_pretty(&set).unwrap();
    assert!(!json.contains("analytics_preset_dropped"), "old file shape");
    fs::create_dir_all(ctx.data_dir.root()).unwrap();
    fs::write(ctx.data_dir.profiles_json(), json).unwrap();
}

fn analytics(set: &ProfileSet, id: &str) -> Option<SettingEntry> {
    set.profile(&ProfileId::new(id))
        .unwrap()
        .codex_settings
        .get("analytics.enabled")
        .cloned()
}

#[tokio::test]
async fn only_krill_profiles_with_the_old_preset_lose_it_and_the_run_is_recorded() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(tmp.path());
    write_old_file(&ctx);
    let store = JsonProfileStore::new(ctx.clone());
    let set = store.load().await.unwrap();
    assert_eq!(analytics(&set, "krill-old"), None);
    // The rest of the old preset stays.
    assert_eq!(
        set.profile(&ProfileId::new("krill-old"))
            .unwrap()
            .codex_settings,
        CodexSettings::krill_recommended()
    );
    assert_eq!(
        analytics(&set, "krill-on"),
        Some(SettingEntry::Set(SettingValue::Bool(true)))
    );
    assert_eq!(
        analytics(&set, "other"),
        Some(SettingEntry::Set(SettingValue::Bool(false)))
    );
    assert!(set.settings.analytics_preset_dropped);
    // The switch service owes a re-apply for the changed profile only.
    assert_eq!(
        set.settings.analytics_reapply,
        Some(superba_core::AnalyticsReapply {
            profiles: vec![ProfileId::new("krill-old")],
            ..Default::default()
        })
    );
    // Recorded on disk at once.
    let on_disk = fs::read_to_string(ctx.data_dir.profiles_json()).unwrap();
    assert!(
        on_disk.contains("\"analytics_preset_dropped\": true"),
        "{on_disk}"
    );
    let disk: ProfileSet = serde_json::from_str(&on_disk).unwrap();
    assert_eq!(analytics(&disk, "krill-old"), None);
    assert!(analytics(&disk, "other").is_some());
}

#[tokio::test]
async fn it_runs_once_so_telemetry_switched_off_again_survives_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(tmp.path());
    write_old_file(&ctx);
    let store = JsonProfileStore::new(ctx.clone());
    let mut set = store.load().await.unwrap();
    // The user wants telemetry off after all: the toggle on the Connection tab.
    let p = set
        .profiles
        .iter_mut()
        .find(|p| p.id == "krill-old")
        .unwrap();
    p.codex_settings.set_openai_telemetry_off(true);
    store.save(&set).await.unwrap();
    // Superba restarts (a fresh store reads the file again), twice.
    for _ in 0..2 {
        let set = JsonProfileStore::new(ctx.clone()).load().await.unwrap();
        assert!(
            set.profile(&ProfileId::new("krill-old"))
                .unwrap()
                .codex_settings
                .openai_telemetry_off()
        );
    }
}

#[tokio::test]
async fn a_new_profile_set_needs_no_migration() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(tmp.path());
    let store = JsonProfileStore::new(ctx.clone());
    // No file yet: nothing to migrate, and nothing is written by a load.
    let mut set = store.load().await.unwrap();
    assert!(set.settings.analytics_preset_dropped);
    assert_eq!(set.settings.analytics_reapply, None);
    assert!(!ctx.data_dir.profiles_json().exists());
    let mut p = profile("krill", catalog::KRILL_SITE_ID, None);
    p.codex_settings.set_openai_telemetry_off(true);
    set.profiles.push(p);
    store.save(&set).await.unwrap();
    let set = JsonProfileStore::new(ctx).load().await.unwrap();
    assert!(
        set.profile(&ProfileId::new("krill"))
            .unwrap()
            .codex_settings
            .openai_telemetry_off()
    );
}

/// Recording the run fails (here: `profiles.json` is a link into a folder
/// Superba cannot write). The load still answers with the migrated set and
/// logs; once writing works again, the next save records the run with the
/// value the user set in between, and later loads keep that value.
#[cfg(unix)]
#[tokio::test]
async fn a_failed_record_neither_fails_the_load_nor_loses_a_later_choice() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(tmp.path());
    write_old_file(&ctx);
    // Move the file into a read-only folder and link it back.
    let ro = tmp.path().join("ro");
    fs::create_dir_all(&ro).unwrap();
    let real = ro.join("profiles.json");
    fs::rename(ctx.data_dir.profiles_json(), &real).unwrap();
    std::os::unix::fs::symlink(&real, ctx.data_dir.profiles_json()).unwrap();
    let before = fs::read(&real).unwrap();
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(ro.join("probe"), b"").is_ok() {
        // Running with CAP_DAC_OVERRIDE (root): the folder cannot be made
        // read-only for this process, so the failure cannot be staged.
        eprintln!("skipped: permissions are not enforced for this user");
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }

    let store = JsonProfileStore::new(ctx.clone());
    let mut set = store.load().await.expect("the load does not fail");
    assert!(set.settings.analytics_preset_dropped);
    assert_eq!(analytics(&set, "krill-old"), None);
    assert_eq!(
        analytics(&set, "krill-on"),
        Some(SettingEntry::Set(SettingValue::Bool(true)))
    );
    assert_eq!(fs::read(&real).unwrap(), before, "nothing was written");

    // Writing works again; the user switches telemetry off and it is saved.
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).unwrap();
    set.profiles
        .iter_mut()
        .find(|p| p.id == "krill-old")
        .unwrap()
        .codex_settings
        .set_openai_telemetry_off(true);
    store.save(&set).await.unwrap();
    for _ in 0..2 {
        let set = JsonProfileStore::new(ctx.clone()).load().await.unwrap();
        assert!(set.settings.analytics_preset_dropped);
        assert_eq!(
            analytics(&set, "krill-old"),
            Some(SettingEntry::Set(SettingValue::Bool(false)))
        );
    }
}
