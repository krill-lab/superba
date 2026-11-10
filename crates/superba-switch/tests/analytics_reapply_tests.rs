//! User bug 137 after an upgrade: the profile store's one-time migration takes
//! the old Krill preset `analytics.enabled = false` out of profiles, and the
//! window's start writes the applied profile once more, write-only, when
//! Superba's `false` is still in `config.toml` and nothing else would change.
//! Nothing is restarted; when nothing needs it nothing is written; saved but
//! unapplied edits are never written (a notice asks for one switch); a failure
//! is reported with its own reason and retried once at the next start of the
//! window, then given up; `spb`'s recovery never consumes it; the user's own
//! value stays. Real config
//! engine, profile store and env writer; app control is faked (the app runs).
//! Remove after alpha.14+N with the migration.

mod fakes;

use std::sync::Arc;

use fakes::*;
use superba_config::{
    EnvPlatform, JsonProfileStore, MemoryNativeEnv, ShellEnvPersister, TomlConfigEngine,
};
use superba_core::{
    CodexSettings, ExecuteOptions, LegacySessionChoice, MapEnv, NoEvents, OutcomeStatus, ProfileId,
    ProfileSet, ProfileStore, SafeDefaults, Severity, SwitchRequest, SwitchService,
};
use superba_switch::Switcher;

const ORIGINAL: &str = "# original\nmodel = \"gpt-5.5\"\n";
const USER_OFF: &str = "# original\nmodel = \"gpt-5.5\"\n\n[analytics]\nenabled = false # mine\n";

struct Rig {
    w: World,
    sw: Switcher,
    codex: std::path::PathBuf,
    store: JsonProfileStore,
}

impl Rig {
    /// Superba up to alpha.13: `work` (applied) and maybe `spare` carry the
    /// old preset; `work` was applied, so `config.toml` holds its `false`.
    async fn old_superba(original: &str, work_has_preset: bool) -> Self {
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
        let mut old_preset = CodexSettings::krill_recommended();
        old_preset.set_openai_telemetry_off(true);
        let work = set.profiles.iter_mut().find(|p| p.id == "work").unwrap();
        work.codex_settings = if work_has_preset {
            old_preset.clone()
        } else {
            CodexSettings::krill_recommended()
        };
        let mut spare = krill_profile("spare", Some("overseas"), superba_core::AuthMode::Pat);
        spare.codex_settings = old_preset;
        set.profiles.push(spare);
        // Saved as already migrated, so the old switch below keeps the preset.
        set.settings.analytics_preset_dropped = true;
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
        rig.apply_old("work").await;
        // Now the file as alpha.13 left it: no marker.
        let mut set = rig.store.load().await.unwrap();
        set.settings.analytics_preset_dropped = false;
        let json = serde_json::to_vec_pretty(&set).unwrap();
        std::fs::write(rig.w.ctx.data_dir.profiles_json(), json).unwrap();
        rig.w.log.lock().unwrap().clear();
        rig
    }

    async fn apply_old(&self, id: &str) {
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

    fn auth(&self) -> Vec<u8> {
        std::fs::read(self.codex.join("auth.json")).unwrap()
    }

    fn config(&self) -> String {
        std::fs::read_to_string(self.codex.join("config.toml")).unwrap()
    }

    fn analytics(&self) -> Option<bool> {
        let mut section = String::new();
        for line in self.config().lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = name.trim().to_owned();
                continue;
            }
            if let Some((key, value)) = line.split_once('=')
                && section == "analytics"
                && key.trim() == "enabled"
            {
                return value.trim().parse().ok();
            }
        }
        None
    }

    /// Superba's window starts: the start-up recovery, then the re-apply.
    async fn start(&self) -> Vec<(Severity, String)> {
        self.start_notes()
            .await
            .iter()
            .map(|n| (n.severity, n.text.key.clone()))
            .collect()
    }

    async fn start_notes(&self) -> Vec<superba_core::Notice> {
        let report = self.sw.recover().await.unwrap();
        assert!(
            report
                .notices
                .iter()
                .all(|n| !n.text.key.starts_with("switch.analytics.")),
            "the recovery (also run by spb) does not re-apply"
        );
        let notes = self.sw.analytics_reapply().await.unwrap();
        assert!(notes.len() <= 1, "{notes:?}");
        notes
    }

    async fn owed(&self) -> Option<superba_core::AnalyticsReapply> {
        self.store.load().await.unwrap().settings.analytics_reapply
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

/// `config.toml` without its `[analytics]` table and blank lines.
fn without_analytics(config: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_analytics = false;
    for line in config.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_analytics = t == "[analytics]";
        }
        if !in_analytics && !t.is_empty() {
            out.push(line.to_owned());
        }
    }
    out
}

#[tokio::test]
async fn the_applied_profile_is_written_again_once_without_restarting_the_app() {
    let rig = Rig::old_superba(ORIGINAL, true).await;
    assert_eq!(rig.analytics(), Some(false), "{}", rig.config());
    let before = rig.config();
    let auth = rig.auth();
    let notes = rig.start().await;
    assert_eq!(
        notes,
        [(Severity::Info, "switch.analytics.reapplied".to_owned())]
    );
    assert_eq!(rig.analytics(), None, "{}", rig.config());
    // Only the key went (its table with it): every other line, the line in
    // use (base URLs) and auth.json are as they were.
    assert_eq!(without_analytics(&before), without_analytics(&rig.config()));
    assert!(!rig.config().contains("analytics"), "{}", rig.config());
    assert_eq!(rig.auth(), auth, "auth.json byte for byte");
    // The running app was left alone: no restart, quit, launch or stop.
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
    assert_eq!(rig.owed().await, None);
    // Done: the next start does nothing.
    let before = rig.config();
    assert!(rig.start().await.is_empty());
    assert_eq!(rig.config(), before);
}

#[tokio::test]
async fn nothing_is_written_when_the_applied_profile_did_not_carry_the_preset() {
    // Only `spare` (not applied) had the preset; `config.toml` has no false.
    let rig = Rig::old_superba(ORIGINAL, false).await;
    let before = rig.config();
    assert_eq!(rig.analytics(), None, "{before}");
    assert!(rig.start().await.is_empty());
    assert_eq!(rig.config(), before);
    assert_eq!(rig.owed().await, None, "nothing is owed any more");
    let set = rig.store.load().await.unwrap();
    let spare = set.profile(&ProfileId::new("spare")).unwrap();
    assert!(!spare.codex_settings.openai_telemetry_off(), "migrated");
}

#[tokio::test]
async fn the_user_s_own_false_stays() {
    let rig = Rig::old_superba(USER_OFF, true).await;
    assert_eq!(rig.analytics(), Some(false));
    let before = rig.config();
    // Released to the user's own `false`: nothing visible changes, so no notice.
    assert!(rig.start().await.is_empty());
    assert_eq!(rig.config(), before);
    assert_eq!(rig.owed().await, None);
    rig.apply_old("original").await;
    assert_eq!(rig.config(), USER_OFF);
}

#[tokio::test]
async fn a_failure_is_reported_retried_once_at_the_next_start_and_then_given_up() {
    let rig = Rig::old_superba(ORIGINAL, true).await;
    // The key is gone: planning the applied profile fails.
    rig.w.secrets.map.lock().unwrap().clear();
    let notes = rig.start().await;
    assert_eq!(
        notes,
        [(
            Severity::Danger,
            "switch.analytics.reapply_retry".to_owned()
        )]
    );
    assert_eq!(rig.analytics(), Some(false), "unchanged");
    assert!(rig.owed().await.is_some_and(|o| o.failed_once));
    // Next start: one more try, then it gives up.
    let notes = rig.start().await;
    assert_eq!(
        notes,
        [(
            Severity::Danger,
            "switch.analytics.reapply_given_up".to_owned()
        )]
    );
    assert_eq!(rig.owed().await, None);
    // And never again.
    assert!(rig.start().await.is_empty());
    assert!(rig.start().await.is_empty());
    assert_eq!(rig.analytics(), Some(false));
}

#[tokio::test]
async fn a_retry_that_works_clears_the_debt() {
    let rig = Rig::old_superba(ORIGINAL, true).await;
    let saved = rig.w.secrets.map.lock().unwrap().clone();
    rig.w.secrets.map.lock().unwrap().clear();
    assert_eq!(rig.start().await.len(), 1);
    *rig.w.secrets.map.lock().unwrap() = saved;
    assert_eq!(
        rig.start().await,
        [(Severity::Info, "switch.analytics.reapplied".to_owned())]
    );
    assert_eq!(rig.analytics(), None, "{}", rig.config());
    assert_eq!(rig.owed().await, None);
}

#[tokio::test]
async fn saved_but_unapplied_edits_are_not_written_and_a_switch_is_asked_for() {
    let rig = Rig::old_superba(ORIGINAL, true).await;
    // The user saved an edit of the applied profile without applying it.
    let mut set = rig.store.load().await.unwrap();
    let work = set.profiles.iter_mut().find(|p| p.id == "work").unwrap();
    work.codex_settings.set("web_search", "cached").unwrap();
    rig.store.save(&set).await.unwrap();
    let before = rig.config();
    assert!(before.contains("web_search = \"live\""), "{before}");
    let notes = rig.start().await;
    assert_eq!(
        notes,
        [(Severity::Warning, "switch.analytics.switch_once".to_owned())]
    );
    assert_eq!(rig.config(), before, "nothing written");
    assert_eq!(rig.owed().await, None, "asked once, not again");
    assert!(rig.start().await.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn a_failed_write_says_why() {
    use std::os::unix::fs::PermissionsExt;
    let rig = Rig::old_superba(ORIGINAL, true).await;
    let before = rig.config();
    let mode = |m| std::fs::set_permissions(&rig.codex, std::fs::Permissions::from_mode(m));
    mode(0o555).unwrap();
    if std::fs::write(rig.codex.join("probe"), b"").is_ok() {
        mode(0o755).unwrap();
        eprintln!("skipped: permissions are not enforced for this user");
        return;
    }
    let notes = rig.start_notes().await;
    mode(0o755).unwrap();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(notes[0].severity, Severity::Danger);
    assert_eq!(notes[0].text.key, "switch.analytics.reapply_retry");
    let shown = notes[0].text.render_fallback();
    assert!(!shown.contains("needs an answer"), "{shown}");
    let reason = notes[0]
        .text
        .nested
        .iter()
        .find(|(name, _)| name == "reason")
        .map(|(_, v)| v.render_fallback())
        .unwrap_or_default();
    eprintln!("reason: {reason}");
    // The write's own error, not a generic one.
    assert!(reason.contains("Permission denied"), "{reason}");
    assert_eq!(rig.config(), before, "unchanged");
    assert!(rig.owed().await.is_some_and(|o| o.failed_once));
}

#[tokio::test]
async fn an_unreadable_state_keeps_the_debt_and_says_so() {
    let rig = Rig::old_superba(ORIGINAL, true).await;
    rig.sw.recover().await.unwrap();
    // Superba's own state cannot be read: which profile is applied is unknown.
    let state = rig.w.ctx.data_dir.state_json();
    let saved = std::fs::read(&state).unwrap();
    std::fs::write(&state, b"{ not json").unwrap();
    let notes = rig.sw.analytics_reapply().await.unwrap();
    let keys: Vec<_> = notes
        .iter()
        .map(|n| (n.severity, n.text.key.clone()))
        .collect();
    assert_eq!(
        keys,
        [(
            Severity::Danger,
            "switch.analytics.reapply_retry".to_owned()
        )]
    );
    assert!(rig.owed().await.is_some_and(|o| o.failed_once), "kept");
    assert_eq!(rig.analytics(), Some(false), "nothing written");
    // Readable again: the retry at the next start works.
    std::fs::write(&state, saved).unwrap();
    assert_eq!(
        rig.start().await,
        [(Severity::Info, "switch.analytics.reapplied".to_owned())]
    );
    assert_eq!(rig.analytics(), None);
}

#[tokio::test]
async fn a_value_the_user_changed_since_is_left_alone() {
    let rig = Rig::old_superba(ORIGINAL, true).await;
    let config = rig.config().replace("enabled = false", "enabled = true");
    std::fs::write(rig.codex.join("config.toml"), &config).unwrap();
    assert!(rig.start().await.is_empty());
    assert_eq!(rig.config(), config);
    assert_eq!(rig.owed().await, None);
}

#[tokio::test]
async fn spb_s_recovery_does_not_consume_it() {
    let rig = Rig::old_superba(ORIGINAL, true).await;
    for _ in 0..3 {
        let report = rig.sw.recover().await.unwrap();
        assert!(
            report
                .notices
                .iter()
                .all(|n| !n.text.key.starts_with("switch.analytics."))
        );
    }
    assert_eq!(rig.analytics(), Some(false));
    assert!(rig.owed().await.is_some_and(|o| !o.failed_once));
}

#[tokio::test]
async fn router_mode_asks_for_a_switch_instead() {
    let rig = Rig::old_superba(ORIGINAL, true).await;
    let mut set = rig.store.load().await.unwrap();
    set.settings.router.enabled = true;
    rig.store.save(&set).await.unwrap();
    let before = rig.config();
    assert_eq!(
        rig.start().await,
        [(Severity::Warning, "switch.analytics.switch_once".to_owned())]
    );
    assert_eq!(rig.config(), before, "nothing written");
    assert_eq!(rig.owed().await, None);
}
