//! Original restore through the GUI/CLI service with real config and environment
//! writers. Only app control and network probes are faked; every file is private
//! to the temporary home, and native environment stores are in memory.

mod fakes;

use std::sync::Arc;
use std::time::Duration;

use fakes::*;
use superba_config::{EnvPlatform, MemoryNativeEnv, ShellEnvPersister, TomlConfigEngine};
use superba_core::{
    AuthMode, Clock, ConfigEngine, EnvLocation, ExecuteOptions, LegacySessionChoice, MapEnv,
    NoEvents, OutcomeStatus, PlanChoice, ProfileId, ProviderRestore, RestoreTarget, SafeDefaults,
    SwitchIntent, SwitchRequest, SwitchService, Timestamp,
};
use superba_sessions::CodexSessionMigrator;
use superba_switch::Switcher;

const ORIGINAL: &str = "# original\nmodel = \"gpt-5.5\"\n\n[mcp_servers.docs]\ncommand = \"npx\"\nenv = { DOCS_TOKEN = \"fixture\" }\n";
const PROVIDER: &str = "[model_providers.legacy]\nname = \"Legacy\"\nbase_url = \"https://original.example/v1\"\nwire_api = \"responses\"\nenv_key = \"ORIGINAL_KEY\"\n";

async fn apply(sw: &Switcher, id: &str) {
    let mut request = SwitchRequest::profile(ProfileId::new(id));
    request.legacy_sessions = Some(LegacySessionChoice::Skip);
    request.take_over_foreign_env = Some(true);
    let mut plan = sw.plan(request).await.unwrap();
    if matches!(
        plan.choices.as_slice(),
        [PlanChoice::RestoreProvider { .. }]
    ) {
        // The GUI offers the policy even when there are no sessions to move.
        let mut request = plan.request;
        request.intent = SwitchIntent::Restore {
            target: RestoreTarget::Original {
                provider: ProviderRestore::Baseline {
                    reverse_migrate: true,
                },
            },
        };
        plan = sw.plan(request).await.unwrap();
    }
    assert!(!plan.is_blocked() && plan.choices.is_empty(), "{plan:?}");
    let out = sw
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

async fn round_trip(original: &str, switches: &[&str], platform: EnvPlatform, env_value: bool) {
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
    let rc = if env_value {
        "# user's shell\nexport CODEX_AUTHAPI_BASE_URL=https://api-slb.kbsfdxc.net\nexport ORIGINAL_KEY=fixture\n"
    } else {
        "# user's shell\nexport ORIGINAL_KEY=fixture\n"
    };
    for file in [".profile", ".bashrc", ".zshenv"] {
        std::fs::write(w.ctx.home_dir.join(file), rc).unwrap();
    }
    w.sessions.report.lock().unwrap().by_provider.clear();
    {
        let mut set = w.profiles.set.lock().unwrap();
        let pat = set.profiles.iter_mut().find(|p| p.id == "work").unwrap();
        pat.codex_settings = superba_core::CodexSettings::krill_recommended();
        let api = krill_profile("api", Some("overseas"), AuthMode::ApiKey);
        set.profiles.push(api);
    }
    w.secrets
        .map
        .lock()
        .unwrap()
        .insert("profile:api".into(), KEY.into());
    let native = Arc::new(if env_value {
        MemoryNativeEnv::new()
            .with(
                &EnvLocation::WindowsUserEnv,
                "CODEX_AUTHAPI_BASE_URL",
                "https://api-slb.kbsfdxc.net",
            )
            .with(
                &EnvLocation::Launchctl,
                "CODEX_AUTHAPI_BASE_URL",
                "https://api-slb.kbsfdxc.net",
            )
    } else {
        MemoryNativeEnv::new()
    });
    let engine = Arc::new(TomlConfigEngine::new(w.ctx.clone()));
    let env = Arc::new(ShellEnvPersister::with_platform(
        w.ctx.clone(),
        platform,
        native,
    ));
    let mut deps = w.deps();
    deps.config = engine.clone();
    deps.env = env.clone();
    let sw = Switcher::new(deps);
    use superba_core::EnvPersister;
    let before_env = env.inspect("CODEX_AUTHAPI_BASE_URL").await.unwrap();
    for id in switches {
        apply(&sw, id).await;
        let written = std::fs::read_to_string(codex.join("config.toml")).unwrap();
        assert!(written.contains("openai_base_url"), "{written}");
    }
    apply(&sw, "original").await;
    assert_eq!(
        std::fs::read_to_string(codex.join("config.toml")).unwrap(),
        original
    );
    assert!(!codex.join("auth.json").exists());
    assert!(
        engine
            .owned_keys(&w.ctx.codex_home())
            .await
            .unwrap()
            .is_empty()
    );
    let after_env = env.inspect("CODEX_AUTHAPI_BASE_URL").await.unwrap();
    assert_eq!(after_env.persisted, before_env.persisted);
    for file in [".profile", ".bashrc", ".zshenv"] {
        assert_eq!(
            std::fs::read_to_string(w.ctx.home_dir.join(file)).unwrap(),
            rc
        );
    }
    // A second restore must neither recreate keys nor capture the managed file
    // as a fresh original.
    apply(&sw, "original").await;
    assert_eq!(
        std::fs::read_to_string(codex.join("config.toml")).unwrap(),
        original
    );
}

#[tokio::test]
async fn restore_removes_added_keys_from_a_plain_original() {
    round_trip(ORIGINAL, &["work"], EnvPlatform::Linux, false).await;
}

#[tokio::test]
async fn restore_removes_added_url_and_restores_the_original_provider() {
    round_trip(
        &format!("model_provider = \"legacy\"\n{ORIGINAL}\n{PROVIDER}"),
        &["work"],
        EnvPlatform::Linux,
        false,
    )
    .await;
}

#[tokio::test]
async fn restore_keeps_original_urls_and_env_after_multiple_switches() {
    let original = format!(
        "openai_base_url = \"https://original.example/v1\"\nchatgpt_base_url = \"https://chat.example/backend-api/\"\nmodel_provider = \"legacy\"\n{ORIGINAL}\n{PROVIDER}"
    );
    round_trip(
        &original,
        &["work", "api", "work"],
        EnvPlatform::Linux,
        true,
    )
    .await;
}

#[tokio::test]
async fn restore_native_env_values_or_absence_on_windows_and_macos() {
    for platform in [EnvPlatform::Windows, EnvPlatform::MacOs] {
        for present in [false, true] {
            round_trip(ORIGINAL, &["work", "api", "work"], platform, present).await;
        }
    }
}

fn session(home: &std::path::Path, id: &str, provider: &str, at: Timestamp) -> std::path::PathBuf {
    let dir = home.join("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("rollout-{id}.jsonl"));
    let meta = serde_json::json!({
        "type": "session_meta",
        "payload": { "id": id, "model_provider": provider, "timestamp": at.to_rfc3339() }
    });
    std::fs::write(&path, format!("{meta}\n")).unwrap();
    path
}

fn session_provider(path: &std::path::Path) -> String {
    let text = std::fs::read_to_string(path).unwrap();
    let meta: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    meta["payload"]["model_provider"].as_str().unwrap().into()
}

async fn execute_sessions(sw: &Switcher, request: SwitchRequest) {
    let plan = sw.plan(request).await.unwrap();
    assert!(!plan.is_blocked() && plan.choices.is_empty(), "{plan:?}");
    let outcome = sw
        .execute(
            plan,
            ExecuteOptions {
                allow_full_restart: true,
                ..Default::default()
            },
            Arc::new(SafeDefaults),
            Arc::new(NoEvents),
        )
        .await
        .unwrap();
    assert_eq!(outcome.status, OutcomeStatus::Switched, "{outcome:?}");
}

#[tokio::test]
async fn restore_choices_round_trip_config_and_keep_migrated_and_new_sessions_visible() {
    for provider in [
        ProviderRestore::Baseline {
            reverse_migrate: true,
        },
        ProviderRestore::KeepOpenai,
        ProviderRestore::Baseline {
            reverse_migrate: false,
        },
    ] {
        let mut w = World::new();
        w.ctx.env = Arc::new(
            MapEnv::new()
                .with("HOME", w.ctx.home_dir.as_os_str())
                .with("SUPERBA_SECRET_BACKEND", "file"),
        );
        w.set_session(None);
        let home = w.ctx.codex_home();
        std::fs::create_dir_all(home.root()).unwrap();
        let original = format!("model_provider = \"legacy\"\n{ORIGINAL}\n{PROVIDER}");
        std::fs::write(home.config_toml(), &original).unwrap();
        let migrated = session(home.root(), "old-legacy", "legacy", Timestamp(T0 - 10_000));
        let untouched = session(home.root(), "old-openai", "openai", Timestamp(T0 - 5_000));
        let mut deps = w.deps();
        deps.config = Arc::new(TomlConfigEngine::with_clock(w.ctx.clone(), w.clock.clone()));
        deps.sessions =
            Arc::new(CodexSessionMigrator::new(w.ctx.data_dir.clone()).with_clock(w.clock.clone()));
        let sw = Switcher::new(deps);
        sw.save_site(
            superba_switch::new_compatible_site(
                "compatible".into(),
                "Compatible",
                &url::Url::parse("https://compatible.example/v1").unwrap(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
        sw.save_profile(
            superba_core::Profile {
                site_id: "compatible".into(),
                ..krill_profile("compatible", None, AuthMode::ApiKey)
            },
            None,
        )
        .await
        .unwrap();
        w.secrets
            .map
            .lock()
            .unwrap()
            .insert("profile:compatible".into(), KEY.into());

        let mut takeover = SwitchRequest::profile("work".into());
        takeover.legacy_sessions = Some(LegacySessionChoice::Migrate);
        execute_sessions(&sw, takeover).await;
        assert_eq!(session_provider(&migrated), "openai");
        w.clock.advance(Duration::from_secs(1));
        let krill = session(home.root(), "new-krill", "openai", w.clock.now());
        w.clock.advance(Duration::from_secs(1));
        execute_sessions(&sw, SwitchRequest::profile("compatible".into())).await;
        w.clock.advance(Duration::from_secs(1));
        let compatible = session(home.root(), "new-compatible", "openai", w.clock.now());
        w.clock.advance(Duration::from_secs(1));
        execute_sessions(&sw, SwitchRequest::profile("work".into())).await;
        w.clock.advance(Duration::from_secs(1));

        // GUI's original-profile route asks before executing. Its default plan
        // already describes the exact restore and the same reverse migration
        // that the CLI selects when --provider is omitted.
        let pending = sw
            .plan(SwitchRequest::profile("original".into()))
            .await
            .unwrap();
        assert!(matches!(
            pending.choices.as_slice(),
            [PlanChoice::RestoreProvider {
                created_during: 2,
                migrated_in: 1,
                ..
            }]
        ));
        assert_eq!(pending.migration.as_ref().unwrap().items.len(), 3);
        let request = SwitchRequest {
            intent: SwitchIntent::Restore {
                target: RestoreTarget::Original { provider },
            },
            ..pending.request
        };
        execute_sessions(&sw, request).await;
        let restored = std::fs::read_to_string(home.config_toml()).unwrap();
        if provider == ProviderRestore::KeepOpenai {
            assert!(restored.contains("model_provider = \"openai\""));
            assert!(restored.contains("openai_base_url = \"https://original.example/v1\""));
        } else {
            assert_eq!(restored, original);
        }
        let expected = if provider
            == (ProviderRestore::Baseline {
                reverse_migrate: true,
            }) {
            "legacy"
        } else {
            "openai"
        };
        for path in [&migrated, &krill, &compatible] {
            assert_eq!(session_provider(path), expected, "{path:?}");
        }
        assert_eq!(
            session_provider(&untouched),
            "openai",
            "pre-takeover session stays untouched"
        );
    }
}
