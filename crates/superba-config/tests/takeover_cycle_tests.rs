//! Each released home starts a fresh takeover cycle; all files are temporary.

mod common;

use common::*;
use superba_config::TomlConfigEngine;
use superba_core::{
    BaselineExtras, ConfigEngine, Error, ProviderRestore, RcLine, ResolvedMode, ResolvedProfile,
    StashScope,
};

const FIRST_AUTH: &str = "{\"OPENAI_API_KEY\": \"first-cycle-test-key\"}\n";
const SECOND_AUTH: &str = "{ \"OPENAI_API_KEY\": \"second-cycle-test-key\" }\n";
const SECOND_CONFIG: &str = "# My new configuration\nmodel_provider = \"mine\"\nopenai_base_url = \"https://my-proxy.example/v1\"\n";

fn original() -> ResolvedProfile {
    restore(ResolvedMode::Original {
        provider: ProviderRestore::Baseline {
            reverse_migrate: false,
        },
    })
}

async fn apply(env: &Env, desired: &ResolvedProfile) {
    let plan = env.engine.plan(&env.home, desired).await.unwrap();
    env.engine.apply(&plan).await.unwrap();
}

fn saved_state(env: &Env) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(env.ctx.data_dir.state_json()).unwrap()).unwrap()
}

fn home_state<'a>(state: &'a serde_json::Value, env: &Env) -> &'a serde_json::Value {
    &state["homes"][env.home.root().to_str().unwrap()]
}

#[tokio::test]
async fn second_takeover_restores_second_cycle_originals_even_without_old_backups() {
    for delete_backups in [false, true] {
        for explicit_snapshot in [false, true] {
            let env = Env::new();
            env.write_config(LEGACY_CONFIG);
            env.write_auth(FIRST_AUTH);
            apply(&env, &pat(OVERSEAS)).await;
            apply(&env, &original()).await;
            assert_eq!(env.config(), LEGACY_CONFIG);
            assert_eq!(env.auth().as_deref(), Some(FIRST_AUTH));
            assert!(
                env.engine
                    .baseline(&env.home)
                    .await
                    .unwrap()
                    .unwrap()
                    .released
            );

            if delete_backups {
                std::fs::remove_dir_all(env.ctx.data_dir.backups_dir()).unwrap();
                // Still listed (reverse migration needs the timeline); only a
                // restore of those generations fails.
                assert_eq!(env.engine.generations(&env.home).await.unwrap().len(), 2);
                // A released home can still render Original after uninstall
                // deleted its first-write backups.
                assert!(
                    env.engine
                        .plan(&env.home, &original())
                        .await
                        .unwrap()
                        .files
                        .iter()
                        .all(|f| matches!(f.change, superba_core::FileChange::Unchanged))
                );
            }
            env.write_config(SECOND_CONFIG);
            env.write_auth(SECOND_AUTH);
            if explicit_snapshot {
                let baseline = env
                    .engine
                    .snapshot_baseline(&env.home, BaselineExtras::default())
                    .await
                    .unwrap();
                assert!(!baseline.released);
                assert_eq!(baseline.provider_key.as_deref(), Some("mine"));
            }
            // Also exercise callers that skip snapshot_baseline entirely.
            apply(&env, &pat(CDN)).await;
            assert!(
                !env.engine
                    .baseline(&env.home)
                    .await
                    .unwrap()
                    .unwrap()
                    .released
            );
            apply(&env, &original()).await;
            assert_eq!(env.config(), SECOND_CONFIG);
            assert_eq!(env.auth().as_deref(), Some(SECOND_AUTH));

            let state = saved_state(&env);
            let hs = home_state(&state, &env);
            let archives = hs["archived_baselines"].as_array().unwrap();
            assert_eq!(archives.len(), 1);
            assert_eq!(archives[0]["baseline"]["provider_key"], "krill");
            for fw in archives[0]["first_write"].as_object().unwrap().values() {
                let path = std::path::Path::new(fw["backup"].as_str().unwrap());
                assert!(
                    path.parent()
                        .unwrap()
                        .file_name()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .starts_with("archive-")
                );
                assert_eq!(path.is_file(), !delete_backups);
                if !delete_backups {
                    let sealed = std::fs::read(path).unwrap();
                    assert!(!String::from_utf8_lossy(&sealed).contains("first-cycle-test-key"));
                }
            }
            let generations: Vec<_> = env
                .engine
                .generations(&env.home)
                .await
                .unwrap()
                .into_iter()
                .map(|g| g.generation)
                .collect();
            assert_eq!(generations, vec![1, 2, 3, 4]);
        }
    }
}

#[tokio::test]
async fn official_releases_ownership_but_original_keeps_the_legacy_baseline() {
    let env = Env::new();
    env.write_config(LEGACY_CONFIG);
    env.write_auth(FIRST_AUTH);
    apply(&env, &pat(OVERSEAS)).await;
    apply(&env, &restore(ResolvedMode::Official)).await;
    let baseline = env.engine.baseline(&env.home).await.unwrap().unwrap();
    assert!(baseline.released);
    assert_eq!(baseline.provider_key.as_deref(), Some("krill"));
    assert!(
        home_state(&saved_state(&env), &env)["history"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["last_written"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    apply(&env, &original()).await;
    assert_eq!(env.config(), LEGACY_CONFIG);
    assert_eq!(env.auth().as_deref(), Some(FIRST_AUTH));
    assert_eq!(
        env.engine.baseline(&env.home).await.unwrap().unwrap(),
        baseline
    );
    assert!(
        home_state(&saved_state(&env), &env)["archived_baselines"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn released_state_roundtrips_and_old_state_defaults_to_unreleased() {
    let env = Env::new();
    env.write_config(LEGACY_CONFIG);
    apply(&env, &pat(OVERSEAS)).await;
    apply(&env, &original()).await;
    let restarted = TomlConfigEngine::new(env.ctx.clone());
    assert!(
        restarted
            .baseline(&env.home)
            .await
            .unwrap()
            .unwrap()
            .released
    );

    let mut state = saved_state(&env);
    let hs = &mut state["homes"][env.home.root().to_str().unwrap()];
    hs["baseline"].as_object_mut().unwrap().remove("released");
    hs.as_object_mut().unwrap().remove("archived_baselines");
    std::fs::write(
        env.ctx.data_dir.state_json(),
        serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();
    assert!(
        !restarted
            .baseline(&env.home)
            .await
            .unwrap()
            .unwrap()
            .released
    );
}

#[tokio::test]
async fn recapture_uses_fresh_extras_stashes_login_and_keeps_signed_out_identities() {
    let env = Env::new();
    env.write_config(LEGACY_CONFIG);
    apply(&env, &pat(OVERSEAS)).await;
    apply(&env, &original()).await;
    let mut state = saved_state(&env);
    state["homes"][env.home.root().to_str().unwrap()]["signed_out"] =
        serde_json::json!(["previously-signed-out"]);
    std::fs::write(
        env.ctx.data_dir.state_json(),
        serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();
    let login = chatgpt_login("second@example.com", "second-login", "2026-10-01T00:00:00Z");
    env.write_config(SECOND_CONFIG);
    env.write_auth(&login);
    let line = RcLine {
        path: env.ctx.home_dir.join(".zshrc"),
        line_no: 2,
        text: format!("export CODEX_AUTHAPI_BASE_URL={CDN}"),
        value: CDN.into(),
    };
    let extras = BaselineExtras {
        legacy_env_lines: vec![line.clone()],
        ..Default::default()
    };
    let baseline = env
        .engine
        .snapshot_baseline(&env.home, extras)
        .await
        .unwrap();
    assert_eq!(baseline.legacy_env_lines, vec![line]);
    assert!(baseline.login_stashed);
    assert!(!baseline.released);
    let stashed = env
        .engine
        .load_login_stash(&StashScope::Local(env.home.root().to_path_buf()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stashed.expose(), login.as_bytes());
    assert_eq!(
        home_state(&saved_state(&env), &env)["signed_out"],
        serde_json::json!(["previously-signed-out"])
    );
}

#[tokio::test]
async fn only_five_archived_baselines_are_retained_without_reusing_backup_paths() {
    let env = Env::new();
    env.write_config(LEGACY_CONFIG);
    for cycle in 0..7 {
        env.write_auth(&format!("{{\"OPENAI_API_KEY\": \"cycle-{cycle}\"}}"));
        apply(&env, &pat(OVERSEAS)).await;
        apply(&env, &original()).await;
    }
    let state = saved_state(&env);
    let archives = home_state(&state, &env)["archived_baselines"]
        .as_array()
        .unwrap();
    assert_eq!(archives.len(), 5);
    let mut paths = std::collections::BTreeSet::new();
    for archive in archives {
        for fw in archive["first_write"].as_object().unwrap().values() {
            if let Some(path) = fw["backup"].as_str() {
                assert!(paths.insert(path));
                assert!(std::path::Path::new(path).is_file());
            }
        }
    }
}

#[tokio::test]
async fn missing_owned_original_backup_has_actionable_error() {
    let env = Env::new();
    env.write_config(LEGACY_CONFIG);
    env.write_auth(FIRST_AUTH);
    apply(&env, &pat(OVERSEAS)).await;
    std::fs::remove_dir_all(env.ctx.data_dir.first_write_backups_dir()).unwrap();
    let error = env.engine.plan(&env.home, &original()).await.unwrap_err();
    assert!(matches!(error, Error::NotFound { .. }));
    let message = error.to_string();
    assert!(message.contains("deleted"));
    assert!(message.contains("generation"));
    assert!(message.contains("official"));
}

#[tokio::test]
async fn rolling_back_a_restore_reinstates_ownership_without_replacing_the_baseline() {
    let env = Env::new();
    env.write_config(LEGACY_CONFIG);
    env.write_auth(FIRST_AUTH);
    apply(&env, &pat(OVERSEAS)).await;
    for desired in [restore(ResolvedMode::Official), original()] {
        let plan = env.engine.plan(&env.home, &desired).await.unwrap();
        let restored = env.engine.apply(&plan).await.unwrap();
        assert!(
            env.engine
                .baseline(&env.home)
                .await
                .unwrap()
                .unwrap()
                .released
        );
        env.engine.rollback(&restored).await.unwrap();
        let baseline = env.engine.baseline(&env.home).await.unwrap().unwrap();
        assert!(!baseline.released);
        assert_eq!(baseline.provider_key.as_deref(), Some("krill"));
    }
    apply(&env, &pat(CDN)).await;
    apply(&env, &original()).await;
    assert_eq!(env.config(), LEGACY_CONFIG);
    assert_eq!(env.auth().as_deref(), Some(FIRST_AUTH));
    assert!(
        home_state(&saved_state(&env), &env)["archived_baselines"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn rolling_back_a_second_takeover_releases_its_new_baseline() {
    let env = Env::new();
    env.write_config(LEGACY_CONFIG);
    env.write_auth(FIRST_AUTH);
    apply(&env, &pat(OVERSEAS)).await;
    apply(&env, &original()).await;
    env.write_config(SECOND_CONFIG);
    env.write_auth(SECOND_AUTH);
    let plan = env.engine.plan(&env.home, &pat(CDN)).await.unwrap();
    let applied = env.engine.apply(&plan).await.unwrap();
    env.engine.rollback(&applied).await.unwrap();
    assert!(
        env.engine
            .baseline(&env.home)
            .await
            .unwrap()
            .unwrap()
            .released
    );
    assert_eq!(env.config(), SECOND_CONFIG);
    assert_eq!(env.auth().as_deref(), Some(SECOND_AUTH));
    env.write_auth("{\"OPENAI_API_KEY\":\"third-cycle-test-key\"}");
    apply(&env, &pat(OVERSEAS)).await;
    apply(&env, &original()).await;
    assert_eq!(env.config(), SECOND_CONFIG);
    assert_eq!(
        env.auth().as_deref(),
        Some("{\"OPENAI_API_KEY\":\"third-cycle-test-key\"}")
    );
}

#[tokio::test]
async fn deleted_config_or_auth_snapshot_stays_listed_but_cannot_be_restored() {
    for snapshot in ["config_snapshot", "auth_snapshot"] {
        let env = Env::new();
        env.write_config(LEGACY_CONFIG);
        apply(&env, &pat(OVERSEAS)).await;
        apply(&env, &pat(CDN)).await;
        let state = saved_state(&env);
        let history = home_state(&state, &env)["history"].as_array().unwrap();
        std::fs::remove_file(history[0][snapshot].as_str().unwrap()).unwrap();
        let generations = env.engine.generations(&env.home).await.unwrap();
        assert_eq!(generations.len(), 2);
        let error = env
            .engine
            .plan(
                &env.home,
                &restore(ResolvedMode::Generation { generation: 1 }),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, Error::NotFound { .. }));
        assert!(
            error
                .to_string()
                .contains("snapshot of generation 1 was deleted or pruned")
        );
    }
}
