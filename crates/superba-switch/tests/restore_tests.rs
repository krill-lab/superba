//! Original restore through the GUI/CLI service with real config and environment
//! writers. Only app control and network probes are faked; every file is private
//! to the temporary home, and native environment stores are in memory.

mod fakes;

use std::sync::Arc;

use fakes::*;
use superba_config::{EnvPlatform, MemoryNativeEnv, ShellEnvPersister, TomlConfigEngine};
use superba_core::{
    AuthMode, ConfigEngine, EnvLocation, ExecuteOptions, LegacySessionChoice, MapEnv, NoEvents,
    OutcomeStatus, ProfileId, SafeDefaults, SwitchRequest, SwitchService,
};
use superba_switch::Switcher;

const ORIGINAL: &str = "# original\nmodel = \"gpt-5.5\"\n\n[mcp_servers.docs]\ncommand = \"npx\"\nenv = { DOCS_TOKEN = \"fixture\" }\n";
const PROVIDER: &str = "[model_providers.legacy]\nname = \"Legacy\"\nbase_url = \"https://original.example/v1\"\nwire_api = \"responses\"\nenv_key = \"ORIGINAL_KEY\"\n";

async fn apply(sw: &Switcher, id: &str) {
    let mut request = SwitchRequest::profile(ProfileId::new(id));
    request.legacy_sessions = Some(LegacySessionChoice::Skip);
    request.take_over_foreign_env = Some(true);
    let plan = sw.plan(request).await.unwrap();
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
        "# user's shell\nexport CODEX_AUTHAPI_BASE_URL=https://api-slb.krill-code.net\nexport ORIGINAL_KEY=fixture\n"
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
                "https://api-slb.krill-code.net",
            )
            .with(
                &EnvLocation::Launchctl,
                "CODEX_AUTHAPI_BASE_URL",
                "https://api-slb.krill-code.net",
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
