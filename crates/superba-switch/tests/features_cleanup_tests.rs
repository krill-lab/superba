//! User bug 144 after an upgrade: Superba up to alpha.16 wrote
//! `features.context_management`, `features.token_budget.enabled` and
//! `features.token_budget.use_history_notes_extension` (the old Krill preset)
//! at every switch, and "Take over again" put a hand edit of them back. The
//! profile store's one-time migration drops them from profiles; the window's
//! next start writes the applied profile once more, write-only, so the values
//! Superba wrote leave `config.toml` (back to the baseline); nothing is
//! restarted; a value the user changed or wrote stays; later switches never
//! write them again and never ask about them. Real config engine, profile
//! store and env writer; app control is faked (the app runs). Remove after
//! alpha.17+N with the migration.

mod fakes;

use std::sync::Arc;

use fakes::*;
use superba_config::{
    EnvPlatform, JsonProfileStore, MemoryNativeEnv, ShellEnvPersister, TomlConfigEngine,
};
use superba_core::codex::RETIRED_PRESET_KEYS;
use superba_core::{
    CodexSettings, ConfigEngine, ExecuteOptions, LegacySessionChoice, MapEnv, NoEvents,
    OutcomeStatus, ProfileId, ProfileSet, ProfileStore, SafeDefaults, SwitchRequest, SwitchService,
};
use superba_switch::Switcher;

const ORIGINAL: &str = "# original\nmodel = \"gpt-5.5\"\n";
const USER_OWN: &str =
    "# original\nmodel = \"gpt-5.5\"\n\n[features]\ncontext_management = false # mine\n";

struct Rig {
    w: World,
    sw: Switcher,
    codex: std::path::PathBuf,
    store: JsonProfileStore,
}

/// The Krill preset up to alpha.16.
fn old_preset() -> CodexSettings {
    let mut s = CodexSettings::krill_recommended();
    for path in RETIRED_PRESET_KEYS {
        s.set(path, true).unwrap();
    }
    s
}

impl Rig {
    /// Superba up to alpha.16 applied `work` (old preset) to a home holding
    /// `original`; then the upgrade loads `profiles.json` (the migration).
    async fn old_superba(original: &str) -> Self {
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
        let store = JsonProfileStore::new(w.ctx.clone());
        let mut set: ProfileSet = w.profiles.set.lock().unwrap().clone();
        let work = set.profiles.iter_mut().find(|p| p.id == "work").unwrap();
        work.codex_settings = old_preset();
        // Saved as already migrated, so the old switch below writes the preset.
        set.settings.features_preset_dropped = true;
        store.save(&set).await.unwrap();
        let engine = Arc::new(TomlConfigEngine::new(w.ctx.clone()));
        let env = Arc::new(ShellEnvPersister::with_platform(
            w.ctx.clone(),
            EnvPlatform::Linux,
            Arc::new(MemoryNativeEnv::new()),
        ));
        let mut deps = w.deps();
        deps.config = engine;
        deps.env = env;
        deps.profiles = Arc::new(store.clone());
        let sw = Switcher::new(deps);
        let rig = Self {
            w,
            sw,
            codex,
            store,
        };
        rig.switch("work").await;
        for path in RETIRED_PRESET_KEYS {
            assert!(rig.has(path), "old Superba wrote {path}: {}", rig.config());
        }
        // Now the file as alpha.16 left it: no marker.
        let mut set = rig.store.load().await.unwrap();
        set.settings.features_preset_dropped = false;
        let json = serde_json::to_vec_pretty(&set).unwrap();
        std::fs::write(rig.w.ctx.data_dir.profiles_json(), json).unwrap();
        rig.w.log.lock().unwrap().clear();
        rig
    }

    /// A switch with no answers given (no take-over, no keep).
    async fn switch(&self, id: &str) {
        let mut request = SwitchRequest::profile(ProfileId::new(id));
        request.legacy_sessions = Some(LegacySessionChoice::Skip);
        request.take_over_foreign_env = Some(true);
        let plan = self.sw.plan(request).await.unwrap();
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
        assert!(matches!(
            out.status,
            OutcomeStatus::WrittenOnly | OutcomeStatus::Switched
        ));
    }

    fn config(&self) -> String {
        std::fs::read_to_string(self.codex.join("config.toml")).unwrap()
    }

    fn write_config(&self, config: &str) {
        std::fs::write(self.codex.join("config.toml"), config).unwrap();
    }

    fn has(&self, path: &str) -> bool {
        superba_config::toml_keys::ConfigDoc::parse(self.config().as_bytes())
            .unwrap()
            .get_repr(path)
            .is_some()
    }

    /// Superba's window starts: the start-up recovery, then the once-per-start
    /// re-apply call that carries the clean-up.
    async fn start(&self) {
        self.sw.recover().await.unwrap();
        let notes = self.sw.analytics_reapply().await.unwrap();
        assert!(notes.is_empty(), "the clean-up is silent: {notes:?}");
        self.sw.analytics_reapply_delivered().await.unwrap();
    }

    async fn owed(&self) -> Vec<ProfileId> {
        self.store.load().await.unwrap().settings.features_cleanup
    }

    fn app_calls(&self) -> Vec<String> {
        self.w
            .log
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.starts_with("app."))
            .cloned()
            .collect()
    }
}

/// `config.toml` without the old preset's feature lines, `[features.token_budget]`
/// and blank lines.
fn without_retired(config: &str) -> Vec<String> {
    config
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty()
                && t != "[features.token_budget]"
                && !t.starts_with("context_management")
                && !t.starts_with("enabled")
                && !t.starts_with("use_history_notes_extension")
        })
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn the_values_superba_wrote_leave_config_toml_once_without_restarting_the_app() {
    let rig = Rig::old_superba(ORIGINAL).await;
    let before = rig.config();
    assert_eq!(rig.owed().await, [ProfileId::new("work")]);
    rig.start().await;
    for path in RETIRED_PRESET_KEYS {
        assert!(!rig.has(path), "{path}: {}", rig.config());
    }
    assert!(!rig.config().contains("token_budget"), "{}", rig.config());
    // Everything else stays, the rest of the preset included.
    assert_eq!(without_retired(&before), without_retired(&rig.config()));
    assert!(
        rig.config().contains("fast_mode = true"),
        "{}",
        rig.config()
    );
    let disruptive: Vec<_> = rig
        .app_calls()
        .into_iter()
        .filter(|c| {
            [
                "app.hot_restart",
                "app.quit",
                "app.launch",
                "app.end_app_processes",
                "app.stop_process",
                "app.stop_daemon",
            ]
            .iter()
            .any(|x| c.starts_with(x))
        })
        .collect();
    assert!(disruptive.is_empty(), "{disruptive:?}");
    assert!(rig.owed().await.is_empty());
    // Done: the next start and the next switch write nothing about them.
    let after = rig.config();
    rig.start().await;
    assert_eq!(rig.config(), after);
    rig.switch("work").await;
    for path in RETIRED_PRESET_KEYS {
        assert!(!rig.has(path), "{path}: {}", rig.config());
    }
}

#[tokio::test]
async fn the_user_s_value_from_before_superba_comes_back() {
    let rig = Rig::old_superba(USER_OWN).await;
    rig.start().await;
    assert!(
        rig.config().contains("context_management = false # mine"),
        "{}",
        rig.config()
    );
    assert!(
        !rig.has("features.token_budget.enabled"),
        "{}",
        rig.config()
    );
}

/// The report: the user deletes the lines (or sets them to `false`) by hand.
/// Before the fix every switch asked, and "Take over again" put Superba's
/// `true` back. Now the hand edit stays, at start and at every later switch,
/// and no switch asks about it.
#[tokio::test]
async fn a_hand_edit_stays_through_the_start_and_later_switches() {
    for edit in ["remove", "false"] {
        let rig = Rig::old_superba(ORIGINAL).await;
        let mut config = rig.config();
        config = match edit {
            "remove" => config
                .replace("context_management = true\n", "")
                .replace("[features.token_budget]\nenabled = true\n", "")
                .replace("use_history_notes_extension = true\n", ""),
            _ => config
                .replace(
                    "context_management = true\n",
                    "context_management = false\n",
                )
                .replace(
                    "enabled = true\nuse_history_notes_extension = true\n",
                    "enabled = false\nuse_history_notes_extension = false\n",
                ),
        };
        rig.write_config(&config);
        let edited = rig.config();
        rig.start().await;
        assert_eq!(rig.config(), edited, "{edit}: kept at start");
        // No longer Superba's: not reported as changed outside Superba.
        let state = TomlConfigEngine::new(rig.w.ctx.clone())
            .read_state(&rig.w.ctx.codex_home())
            .await
            .unwrap();
        assert!(
            state
                .drift
                .iter()
                .all(|k| !RETIRED_PRESET_KEYS.contains(&k.path.as_str())),
            "{edit}: {:?}",
            state.drift
        );
        // A plain switch (no take-over, no keep) needs no answer and keeps it.
        rig.switch("work").await;
        rig.switch("work").await;
        for path in RETIRED_PRESET_KEYS {
            assert_eq!(
                rig.has(path),
                edit == "false",
                "{edit} {path}: {}",
                rig.config()
            );
        }
        if edit == "false" {
            assert!(
                rig.config().contains("context_management = false"),
                "{}",
                rig.config()
            );
            assert!(
                !rig.config().contains("= true\nuse_history"),
                "{}",
                rig.config()
            );
        }
    }
}

#[tokio::test]
async fn nothing_is_written_when_the_applied_profile_did_not_carry_them() {
    let rig = Rig::old_superba(ORIGINAL).await;
    // Another profile is applied now (a new-style preset: none of them).
    let mut set = rig.store.load().await.unwrap();
    let work = set.profiles.iter_mut().find(|p| p.id == "work").unwrap();
    let mut clean = work.clone();
    clean.id = ProfileId::new("clean");
    clean.codex_settings = CodexSettings::krill_recommended();
    set.profiles.push(clean);
    // Saved as migrated already, with nothing owed.
    set.settings.features_cleanup.clear();
    rig.store.save(&set).await.unwrap();
    rig.switch("clean").await;
    let before = rig.config();
    for path in RETIRED_PRESET_KEYS {
        assert!(!rig.has(path), "released by the switch: {path}: {before}");
    }
    rig.start().await;
    assert_eq!(rig.config(), before);
}

/// `spb` only (the window never starts): the next switch removes the values
/// Superba wrote and keeps the user's, without asking.
#[tokio::test]
async fn without_the_window_the_next_switch_does_it() {
    let rig = Rig::old_superba(ORIGINAL).await;
    rig.switch("work").await;
    for path in RETIRED_PRESET_KEYS {
        assert!(!rig.has(path), "{path}: {}", rig.config());
    }
    let rig = Rig::old_superba(ORIGINAL).await;
    let edited = rig.config().replace(
        "context_management = true\n",
        "context_management = false\n",
    );
    rig.write_config(&edited);
    rig.switch("work").await;
    assert!(
        rig.config().contains("context_management = false"),
        "{}",
        rig.config()
    );
    assert!(
        !rig.has("features.token_budget.enabled"),
        "{}",
        rig.config()
    );
    let state = TomlConfigEngine::new(rig.w.ctx.clone())
        .read_state(&rig.w.ctx.codex_home())
        .await
        .unwrap();
    assert!(state.drift.is_empty(), "{:?}", state.drift);
}
