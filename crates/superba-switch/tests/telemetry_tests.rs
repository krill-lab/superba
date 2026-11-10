//! "Disable OpenAI telemetry" (user bug 137) through the switch service with the
//! real config engine: a PAT switch leaves `analytics.enabled` alone (with it
//! the desktop app never caches the remote flag that turns its Chinese UI on);
//! the profile's toggle writes `analytics.enabled = false`; turning the toggle
//! off puts back the value from before Superba wrote it at the next switch
//! (removed, or the user's own `false`). Only app control and network probes
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
