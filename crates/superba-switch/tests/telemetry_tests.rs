//! "Disable OpenAI telemetry" (user bug 137) through the switch service with the
//! real config engine: a PAT switch leaves `analytics.enabled` alone (with it
//! the desktop app never caches the remote flag that turns its Chinese UI on);
//! the profile's toggle writes `analytics.enabled = false`; turning the toggle
//! off puts back the value from before Superba wrote it at the next switch
//! (removed, or the user's own `false`); so does the one-time migration of
//! profiles saved with the old preset. Only app control and network probes
//! are faked; every file is private to the temporary home.

mod fakes;

use std::sync::Arc;

use fakes::*;
use superba_config::{EnvPlatform, MemoryNativeEnv, ShellEnvPersister, TomlConfigEngine};
use superba_core::{
    CodexSettings, ExecuteOptions, LegacySessionChoice, MapEnv, NoEvents, OutcomeStatus,
    PlanChoice, ProfileId, ProviderRestore, RestoreTarget, SafeDefaults, SwitchIntent,
    SwitchRequest, SwitchService,
};
use superba_switch::Switcher;

const ORIGINAL: &str = "# original\nmodel = \"gpt-5.5\"\n";
const USER_OFF: &str = "# original\nmodel = \"gpt-5.5\"\n\n[analytics]\nenabled = false # mine\n";

struct Rig {
    w: World,
    sw: Switcher,
    codex: std::path::PathBuf,
}

impl Rig {
    fn new(original: &str) -> Self {
        let mut w = World::new();
        let codex = w.tmp.path().join("codex-home");
        w.ctx.env = Arc::new(
            MapEnv::new()
                .with("HOME", w.ctx.home_dir.as_os_str())
                .with("CODEX_HOME", codex.as_os_str())
                .with("SHELL", "/bin/bash")
                .with("SUPERBA_SECRET_BACKEND", "file"),
        );
        std::fs::create_dir_all(&codex).unwrap();
        std::fs::create_dir_all(&w.ctx.home_dir).unwrap();
        std::fs::write(codex.join("config.toml"), original).unwrap();
        w.sessions.report.lock().unwrap().by_provider.clear();
        // A Krill profile as the GUI, CLI and wizard create it.
        w.profiles
            .set
            .lock()
            .unwrap()
            .profiles
            .iter_mut()
            .find(|p| p.id == "work")
            .unwrap()
            .codex_settings = CodexSettings::krill_recommended();
        let engine = Arc::new(TomlConfigEngine::new(w.ctx.clone()));
        let env = Arc::new(ShellEnvPersister::with_platform(
            w.ctx.clone(),
            EnvPlatform::Linux,
            Arc::new(MemoryNativeEnv::new()),
        ));
        let mut deps = w.deps();
        deps.config = engine;
        deps.env = env;
        let sw = Switcher::new(deps);
        Self { w, sw, codex }
    }

    fn config(&self) -> String {
        std::fs::read_to_string(self.codex.join("config.toml")).unwrap()
    }

    /// `analytics.enabled` in `config.toml` (`[analytics]` table or dotted key).
    fn analytics(&self) -> Option<bool> {
        let mut section = String::new();
        for line in self.config().lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = name.trim().to_owned();
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let dotted = section.is_empty() && key == "analytics.enabled";
            if dotted || (section == "analytics" && key == "enabled") {
                return value.trim().parse().ok();
            }
        }
        None
    }

    /// Flip the toggle of `work` the way the Connection tab saves it.
    fn set_toggle(&self, off: bool) {
        let mut set = self.w.profiles.set.lock().unwrap();
        let work = set.profiles.iter_mut().find(|p| p.id == "work").unwrap();
        work.codex_settings.set_openai_telemetry_off(off);
        assert_eq!(work.codex_settings.openai_telemetry_off(), off);
    }

    async fn apply(&self, id: &str) {
        let mut request = SwitchRequest::profile(ProfileId::new(id));
        request.legacy_sessions = Some(LegacySessionChoice::Skip);
        request.take_over_foreign_env = Some(true);
        let mut plan = self.sw.plan(request).await.unwrap();
        if matches!(
            plan.choices.as_slice(),
            [PlanChoice::RestoreProvider { .. }]
        ) {
            let mut request = plan.request;
            request.intent = SwitchIntent::Restore {
                target: RestoreTarget::Original {
                    provider: ProviderRestore::Baseline {
                        reverse_migrate: true,
                    },
                },
            };
            plan = self.sw.plan(request).await.unwrap();
        }
        assert!(!plan.is_blocked() && plan.choices.is_empty(), "{plan:?}");
        let out = self
            .sw
            .execute(
                plan,
                ExecuteOptions {
                    write_only: true,
                    ..Default::default()
                },
                Arc::new(SafeDefaults),
                Arc::new(NoEvents),
            )
            .await
            .unwrap();
        assert!(
            matches!(
                out.status,
                OutcomeStatus::WrittenOnly | OutcomeStatus::Switched
            ),
            "{out:?}"
        );
    }
}

#[tokio::test]
async fn a_pat_switch_leaves_openai_telemetry_on_by_default() {
    let rig = Rig::new(ORIGINAL);
    assert!(
        !CodexSettings::krill_recommended().openai_telemetry_off(),
        "the Krill preset must not switch telemetry off"
    );
    rig.apply("work").await;
    let config = rig.config();
    assert!(config.contains("chatgpt_base_url"), "{config}");
    assert!(config.contains("[features]"), "{config}");
    assert!(!config.contains("analytics"), "{config}");
    rig.apply("original").await;
    assert_eq!(rig.config(), ORIGINAL);
}

#[tokio::test]
async fn the_toggle_writes_analytics_off_and_turning_it_off_removes_it_at_the_next_switch() {
    let rig = Rig::new(ORIGINAL);
    // On: written with the profile.
    rig.set_toggle(true);
    rig.apply("work").await;
    assert_eq!(rig.analytics(), Some(false), "{}", rig.config());
    // A profile saved by an earlier Superba (whose preset switched telemetry
    // off) looks exactly like this. Off: the next switch removes the value
    // Superba wrote, and nothing else changes.
    let before = rig.config();
    rig.set_toggle(false);
    rig.apply("work").await;
    let after = rig.config();
    assert_eq!(rig.analytics(), None, "{after}");
    assert!(!after.contains("analytics"), "{after}");
    for kept in [
        "chatgpt_base_url",
        "[features]",
        "fast_mode = true",
        "web_search",
    ] {
        assert!(
            before.contains(kept) && after.contains(kept),
            "{kept}: {after}"
        );
    }
    // Restoring the original configuration is still byte for byte.
    rig.apply("original").await;
    assert_eq!(rig.config(), ORIGINAL);
}

#[tokio::test]
async fn the_user_s_own_analytics_off_survives_the_toggle_and_the_restore() {
    let rig = Rig::new(USER_OFF);
    // Default (toggle off): Superba does not touch the user's key.
    rig.apply("work").await;
    assert_eq!(rig.analytics(), Some(false));
    assert!(
        rig.config().contains("enabled = false # mine"),
        "{}",
        rig.config()
    );
    // Toggle on, then off: the user's value is what comes back.
    rig.set_toggle(true);
    rig.apply("work").await;
    assert_eq!(rig.analytics(), Some(false));
    rig.set_toggle(false);
    rig.apply("work").await;
    assert_eq!(rig.analytics(), Some(false), "{}", rig.config());
    rig.apply("original").await;
    assert_eq!(rig.config(), USER_OFF);
}

/// A Krill profile saved by Superba up to alpha.13 (the preset switched
/// telemetry off) was applied; after the one-time migration the next switch
/// removes the value Superba wrote and keeps a user's own value. Remove after
/// alpha.14+N with the migration.
async fn migrated_old_profile(original: &str) -> Rig {
    let rig = Rig::new(original);
    rig.set_toggle(true);
    rig.apply("work").await;
    assert_eq!(rig.analytics(), Some(false), "{}", rig.config());
    let dropped = {
        let mut set = rig.w.profiles.set.lock().unwrap();
        superba_config::drop_old_analytics_preset(&mut set)
    };
    assert_eq!(dropped, [ProfileId::new("work")]);
    rig.apply("work").await;
    rig
}

#[tokio::test]
async fn after_the_migration_the_next_switch_removes_the_value_superba_wrote() {
    let rig = migrated_old_profile(ORIGINAL).await;
    assert_eq!(rig.analytics(), None, "{}", rig.config());
    assert!(!rig.config().contains("analytics"), "{}", rig.config());
    rig.apply("original").await;
    assert_eq!(rig.config(), ORIGINAL);
}

#[tokio::test]
async fn after_the_migration_the_next_switch_keeps_the_user_s_own_value() {
    let rig = migrated_old_profile(USER_OFF).await;
    assert_eq!(rig.analytics(), Some(false), "{}", rig.config());
    rig.apply("original").await;
    assert_eq!(rig.config(), USER_OFF);
}

/// The recorded run survives the Preferences page: a copy of the settings
/// without the marker (an older window's copy, a hand-made request) is saved
/// through the service, yet the next start does not migrate again and the
/// user's own "Disable OpenAI telemetry" stays. Real profile store. Remove
/// after alpha.14+N with the migration.
#[tokio::test]
async fn saving_settings_without_the_marker_keeps_the_migration_recorded() {
    use superba_config::JsonProfileStore;
    use superba_core::{ProfileStore, Settings};

    let w = World::new();
    let store = JsonProfileStore::new(w.ctx.clone());
    // profiles.json as Superba up to alpha.13 wrote it: the old preset, no marker.
    let mut old = w.profiles.set.lock().unwrap().clone();
    old.profiles
        .iter_mut()
        .find(|p| p.id == "work")
        .unwrap()
        .codex_settings
        .set_openai_telemetry_off(true);
    old.settings.analytics_preset_dropped = false;
    std::fs::create_dir_all(w.ctx.data_dir.root()).unwrap();
    std::fs::write(
        w.ctx.data_dir.profiles_json(),
        serde_json::to_vec_pretty(&old).unwrap(),
    )
    .unwrap();
    let mut deps = w.deps();
    deps.profiles = Arc::new(store.clone());
    let sw = Switcher::new(deps);

    // First start: migrated.
    let set = sw.profile_set().await.unwrap();
    assert!(set.settings.analytics_preset_dropped);
    let mut work = set.profile(&ProfileId::new("work")).unwrap().clone();
    assert!(!work.codex_settings.openai_telemetry_off());
    // The user wants telemetry off after all.
    work.codex_settings.set_openai_telemetry_off(true);
    sw.save_profile(work, None).await.unwrap();
    // Preferences saved from a copy without the marker.
    let stale = Settings {
        analytics_preset_dropped: false,
        analytics_reapply: None,
        countdown_secs: 3,
        ..set.settings.clone()
    };
    sw.save_settings(stale).await.unwrap();

    // Next start (a fresh store reads the file).
    let set = JsonProfileStore::new(w.ctx.clone()).load().await.unwrap();
    assert_eq!(set.settings.countdown_secs, 3, "the settings were saved");
    assert!(set.settings.analytics_preset_dropped);
    // The re-apply still owed (it runs at start-up recovery) is kept too.
    assert_eq!(
        set.settings
            .analytics_reapply
            .as_ref()
            .map(|r| r.profiles.clone())
            .unwrap_or_default(),
        [ProfileId::new("work")]
    );
    assert!(
        set.profile(&ProfileId::new("work"))
            .unwrap()
            .codex_settings
            .openai_telemetry_off(),
        "the user's own choice survives"
    );
}
