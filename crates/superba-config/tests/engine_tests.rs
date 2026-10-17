//! TomlConfigEngine through the `ConfigEngine` trait, on temp homes.

mod common;

use std::collections::BTreeMap;

use common::*;
use pretty_assertions::assert_eq;
use superba_config::toml_keys::ConfigDoc;
use superba_core::codex::{PAT_RECOMMENDED_KEYS, config_keys as k};
use superba_core::{
    ApplyReport, AuthMode, AuthShape, BaselineExtras, CodexFileKind, CodexFiles, ConfigEngine,
    Error, FileChange, KeyResolution, ModelDefaults, ProviderRestore, RenderInput, ResolvedMode,
    SecretBytes, StashScope,
};

fn key_change<'a>(plan: &'a superba_core::ApplyPlan, path: &str) -> &'a superba_core::KeyChange {
    plan.key_changes
        .iter()
        .find(|c| c.path == path)
        .unwrap_or_else(|| panic!("no change for {path}: {:?}", plan.key_changes))
}

#[tokio::test]
async fn legacy_takeover_pat_apikey_and_byte_exact_original_restore() {
    let env = Env::new();
    env.write_config(LEGACY_CONFIG);
    let e = &env.engine;

    let b = e
        .snapshot_baseline(&env.home, BaselineExtras::default())
        .await
        .unwrap();
    assert_eq!(b.provider_key.as_deref(), Some("krill"));
    assert!(b.provider_table_present);
    assert_eq!(b.auth, AuthShape::Missing);
    // Idempotent.
    let b2 = e
        .snapshot_baseline(&env.home, BaselineExtras::default())
        .await
        .unwrap();
    assert_eq!(b, b2);

    // --- PAT on the overseas line.
    let plan = e.plan(&env.home, &pat(OVERSEAS)).await.unwrap();
    assert_eq!(plan.generation, 1);
    assert_eq!(plan.auth_mode, Some(AuthMode::Pat));
    let c = key_change(&plan, k::MODEL_PROVIDER);
    assert_eq!(c.before.as_deref(), Some("\"krill\""));
    assert_eq!(c.after.as_deref(), Some("\"openai\""));
    assert_eq!(
        key_change(&plan, k::OPENAI_BASE_URL).after.as_deref(),
        Some("\"https://api.krill-code.net/backend-api/codex\"")
    );
    assert_eq!(
        key_change(&plan, k::CHATGPT_BASE_URL).after.as_deref(),
        Some("\"https://api.krill-code.net/backend-api/\"")
    );
    assert_eq!(
        key_change(&plan, "features.context_management")
            .after
            .as_deref(),
        Some("true")
    );
    assert!(plan.key_changes.iter().all(|c| c.path != k::MODEL));
    let ac = plan.auth_change.clone().unwrap();
    assert_eq!(ac.before, AuthShape::Missing);
    assert!(matches!(ac.after, AuthShape::Pat { .. }));
    assert!(!ac.stash_login);
    // Planning never writes.
    assert_eq!(env.config(), LEGACY_CONFIG);
    assert_eq!(env.auth(), None);
    // The serialized plan never contains the key.
    let json = serde_json::to_string(&plan).unwrap();
    assert!(!json.contains(PAT_KEY));

    let report = e.apply(&plan).await.unwrap();
    assert_eq!(report.generation, 1);
    let cfg = env.config();
    assert!(cfg.contains("model_provider = \"openai\""));
    assert!(cfg.contains("openai_base_url = \"https://api.krill-code.net/backend-api/codex\""));
    assert!(cfg.contains("chatgpt_base_url = \"https://api.krill-code.net/backend-api/\""));
    // Everything else is untouched.
    for line in [
        "# Codex 配置（Krill 教程）",
        "model = \"gpt-5.5\"",
        "model_reasoning_effort = \"high\"",
        "[model_providers.krill]",
        "base_url = \"https://api.krill-code.net/codex/v1\"",
        "env = { DOCS_TOKEN = \"tok-123\" } # inline secret",
        "[projects.\"/home/u/项目\"]",
    ] {
        assert!(cfg.contains(line), "missing {line:?} in\n{cfg}");
    }
    assert!(!cfg.contains("[model_providers.openai]"));
    let auth = env.auth_json();
    assert_eq!(auth["personal_access_token"], PAT_KEY);
    assert_eq!(auth["OPENAI_API_KEY"], PAT_KEY);
    #[cfg(unix)]
    assert_eq!(mode_of(&env.auth_path()), 0o600);

    let st = e.read_state(&env.home).await.unwrap();
    assert_eq!(st.model_provider.as_deref(), Some("openai"));
    assert_eq!(st.active.as_ref().unwrap().generation, 1);
    assert_eq!(st.active.as_ref().unwrap().auth_mode, Some(AuthMode::Pat));
    assert!(st.drift.is_empty());
    assert!(!st.journal_pending);
    assert!(matches!(st.auth, AuthShape::Pat { ref key_last4 } if key_last4 == "0001"));

    let creds = e.active_credentials(&env.home).await.unwrap().unwrap();
    assert_eq!(creds.auth_mode, AuthMode::Pat);
    assert_eq!(creds.key.expose(), PAT_KEY);
    assert_eq!(
        creds.chatgpt_base_url.unwrap().as_str(),
        "https://api.krill-code.net/backend-api/"
    );

    // --- API key on the CDN line (/codex/v1): PAT-only keys go back to baseline.
    let plan = e.plan(&env.home, &apikey(CDN)).await.unwrap();
    assert_eq!(key_change(&plan, k::CHATGPT_BASE_URL).after, None);
    assert_eq!(key_change(&plan, "analytics.enabled").after, None);
    e.apply(&plan).await.unwrap();
    let cfg = env.config();
    assert!(cfg.contains("openai_base_url = \"https://api.cdn-krill-ai.com/codex/v1\""));
    assert!(!cfg.contains("chatgpt_base_url"));
    assert!(!cfg.contains("[features]"), "{cfg}");
    assert!(!cfg.contains("[analytics]"), "{cfg}");
    assert!(!cfg.contains("web_search"), "{cfg}");
    let auth = env.auth_json();
    assert_eq!(auth["auth_mode"], "apikey");
    assert_eq!(auth["OPENAI_API_KEY"], API_KEY);
    assert!(auth.get("personal_access_token").is_none());

    // --- Restore the original byte for byte (plan 10.4 option 3).
    let plan = e
        .plan(
            &env.home,
            &restore(ResolvedMode::Original {
                provider: ProviderRestore::Baseline {
                    reverse_migrate: false,
                },
            }),
        )
        .await
        .unwrap();
    assert!(matches!(
        plan.files
            .iter()
            .find(|f| f.kind == superba_core::CodexFileKind::AuthJson)
            .unwrap()
            .change,
        FileChange::Remove
    ));
    e.apply(&plan).await.unwrap();
    assert_eq!(env.config(), LEGACY_CONFIG);
    assert_eq!(env.auth(), None);

    let gens = e.generations(&env.home).await.unwrap();
    assert_eq!(
        gens.iter().map(|g| g.generation).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}

#[tokio::test]
async fn keep_openai_restore_routes_through_original_provider_base_url() {
    let env = Env::new();
    env.write_config(LEGACY_CONFIG);
    env.write_auth("{\"OPENAI_API_KEY\": \"old-tutorial-key-9999\"}");
    let e = &env.engine;
    e.apply(&e.plan(&env.home, &pat(OVERSEAS)).await.unwrap())
        .await
        .unwrap();
    let plan = e
        .plan(
            &env.home,
            &restore(ResolvedMode::Original {
                provider: ProviderRestore::KeepOpenai,
            }),
        )
        .await
        .unwrap();
    e.apply(&plan).await.unwrap();
    let cfg = env.config();
    assert!(cfg.contains("model_provider = \"openai\""), "{cfg}");
    assert!(
        cfg.contains("openai_base_url = \"https://api.krill-code.net/codex/v1\""),
        "{cfg}"
    );
    assert!(!cfg.contains("chatgpt_base_url"));
    assert!(cfg.contains("[model_providers.krill]"));
    // The original auth.json is restored byte for byte.
    assert_eq!(
        env.auth().unwrap(),
        "{\"OPENAI_API_KEY\": \"old-tutorial-key-9999\"}"
    );
    let st = e.read_state(&env.home).await.unwrap();
    assert_eq!(st.active.unwrap().auth_mode, Some(AuthMode::ApiKey));
}

#[tokio::test]
async fn official_login_is_stashed_and_restored_and_config_returns_to_original() {
    let env = Env::new();
    env.write_config(OFFICIAL_CONFIG);
    let login = chatgpt_login("user@example.com", "rt-1", "2026-10-01T00:00:00Z");
    env.write_auth(&login);
    let e = &env.engine;

    let plan = e.plan(&env.home, &pat(OVERSEAS)).await.unwrap();
    assert!(plan.auth_change.as_ref().unwrap().stash_login);
    assert!(matches!(
        plan.auth_change.as_ref().unwrap().before,
        AuthShape::ChatGpt { ref email, .. } if email.as_deref() == Some("user@example.com")
    ));
    e.apply(&plan).await.unwrap();
    let info = e
        .login_stash(&StashScope::Local(env.home.root().to_path_buf()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(info.email.as_deref(), Some("user@example.com"));
    assert_eq!(
        info.last_refresh.unwrap().to_rfc3339(),
        "2026-10-01T00:00:00.000Z"
    );
    assert_eq!(info.id_token_exp.unwrap().as_millis(), 1_800_000_000_000);
    // The stash is encrypted at rest.
    let stash_dir = env.data().join("backups/login-stash");
    for entry in walk(&stash_dir) {
        let bytes = std::fs::read(&entry).unwrap();
        assert!(
            !String::from_utf8_lossy(&bytes).contains("rt-1"),
            "{}",
            entry.display()
        );
    }

    let plan = e
        .plan(&env.home, &restore(ResolvedMode::Official))
        .await
        .unwrap();
    assert_eq!(plan.auth_mode, Some(AuthMode::OfficialChatGpt));
    e.apply(&plan).await.unwrap();
    assert_eq!(env.auth().unwrap(), login);
    assert_eq!(env.config(), OFFICIAL_CONFIG);
    let st = e.read_state(&env.home).await.unwrap();
    assert!(matches!(st.auth, AuthShape::ChatGpt { .. }));
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

#[tokio::test]
async fn live_login_wins_over_the_stash() {
    let env = Env::new();
    env.write_auth(&chatgpt_login(
        "user@example.com",
        "rt-old",
        "2026-10-01T00:00:00Z",
    ));
    let e = &env.engine;
    e.apply(&e.plan(&env.home, &pat(OVERSEAS)).await.unwrap())
        .await
        .unwrap();
    // The user signs in again inside the app while on Krill (newer refresh token).
    let newer = chatgpt_login("user@example.com", "rt-new", "2026-10-15T00:00:00Z");
    env.write_auth(&newer);
    let plan = e
        .plan(&env.home, &restore(ResolvedMode::Official))
        .await
        .unwrap();
    assert!(plan.auth_change.is_none());
    e.apply(&plan).await.unwrap();
    assert_eq!(env.auth().unwrap(), newer);
}

#[tokio::test]
async fn replacing_a_newer_login_refreshes_the_stash() {
    let env = Env::new();
    env.write_auth(&chatgpt_login(
        "user@example.com",
        "rt-old",
        "2026-10-01T00:00:00Z",
    ));
    let e = &env.engine;
    e.apply(&e.plan(&env.home, &pat(OVERSEAS)).await.unwrap())
        .await
        .unwrap();
    let newer = chatgpt_login("user@example.com", "rt-new", "2026-10-15T00:00:00Z");
    env.write_auth(&newer);
    // Switching to Krill again replaces the live (newer) login: it is stashed first.
    e.apply(&e.plan(&env.home, &apikey(OVERSEAS)).await.unwrap())
        .await
        .unwrap();
    let stashed = e
        .load_login_stash(&StashScope::Local(env.home.root().to_path_buf()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stashed.expose(), newer.as_bytes());
    e.apply(
        &e.plan(&env.home, &restore(ResolvedMode::Official))
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(env.auth().unwrap(), newer);
}

#[tokio::test]
async fn signed_out_login_is_not_resurrected() {
    let env = Env::new();
    env.write_auth(&chatgpt_login(
        "user@example.com",
        "rt-1",
        "2026-10-01T00:00:00Z",
    ));
    let e = &env.engine;
    e.apply(&e.plan(&env.home, &pat(OVERSEAS)).await.unwrap())
        .await
        .unwrap();
    e.apply(
        &e.plan(&env.home, &restore(ResolvedMode::Official))
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    assert!(env.auth().is_some());
    // The user signs out in the app: Codex deletes auth.json.
    std::fs::remove_file(env.auth_path()).unwrap();
    e.apply(&e.plan(&env.home, &pat(OVERSEAS)).await.unwrap())
        .await
        .unwrap();
    let plan = e
        .plan(&env.home, &restore(ResolvedMode::Official))
        .await
        .unwrap();
    assert!(
        plan.notes
            .iter()
            .any(|n| n.text.key == "config.note.sign_in_required"),
        "{:?}",
        plan.notes
    );
    e.apply(&plan).await.unwrap();
    assert_eq!(env.auth(), None);
    assert!(
        e.login_stash(&StashScope::Local(env.home.root().to_path_buf()))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn official_without_stash_removes_token_auth_and_warns() {
    let env = Env::new();
    let e = &env.engine;
    e.apply(&e.plan(&env.home, &apikey(OVERSEAS)).await.unwrap())
        .await
        .unwrap();
    let plan = e
        .plan(&env.home, &restore(ResolvedMode::Official))
        .await
        .unwrap();
    assert!(
        plan.notes
            .iter()
            .any(|n| n.text.key == "config.note.sign_in_required")
    );
    e.apply(&plan).await.unwrap();
    assert_eq!(env.auth(), None);
    let cfg = env.config_opt().unwrap_or_default();
    assert!(!cfg.contains("model_provider"), "{cfg}");
    assert!(!cfg.contains("openai_base_url"), "{cfg}");
}

#[tokio::test]
async fn drift_is_a_conflict_until_resolved() {
    let env = Env::new();
    env.write_config(OFFICIAL_CONFIG);
    let e = &env.engine;
    e.apply(&e.plan(&env.home, &pat(OVERSEAS)).await.unwrap())
        .await
        .unwrap();
    // The user edits an owned key by hand.
    let edited = env.config().replace(
        "https://api.krill-code.net/backend-api/codex",
        "https://my-proxy.example/backend-api/codex",
    );
    env.write_config(&edited);
    let st = e.read_state(&env.home).await.unwrap();
    assert_eq!(st.drift.len(), 1);
    assert_eq!(st.drift[0].path, k::OPENAI_BASE_URL);
    assert_eq!(
        st.drift[0].after.as_deref(),
        Some("\"https://my-proxy.example/backend-api/codex\"")
    );

    let err = e.plan(&env.home, &pat(CDN)).await.unwrap_err();
    assert!(matches!(err, Error::Conflict { ref message } if message.contains("openai_base_url")));

    let mut keep = pat(CDN);
    keep.key_resolutions =
        BTreeMap::from([(k::OPENAI_BASE_URL.to_owned(), KeyResolution::KeepCurrent)]);
    let plan = e.plan(&env.home, &keep).await.unwrap();
    assert!(
        plan.key_changes
            .iter()
            .all(|c| c.path != k::OPENAI_BASE_URL)
    );
    e.apply(&plan).await.unwrap();
    assert!(
        env.config()
            .contains("https://my-proxy.example/backend-api/codex")
    );
    assert!(
        env.config()
            .contains("chatgpt_base_url = \"https://api.cdn-krill-ai.com/backend-api/\"")
    );
    // Kept means released: no drift is reported for it any more.
    assert!(e.read_state(&env.home).await.unwrap().drift.is_empty());

    let mut over = pat(OVERSEAS);
    over.key_resolutions =
        BTreeMap::from([(k::OPENAI_BASE_URL.to_owned(), KeyResolution::Overwrite)]);
    e.apply(&e.plan(&env.home, &over).await.unwrap())
        .await
        .unwrap();
    assert!(
        env.config()
            .contains("openai_base_url = \"https://api.krill-code.net/backend-api/codex\"")
    );
}

#[tokio::test]
async fn unparseable_config_is_refused_and_untouched() {
    let env = Env::new();
    let broken = "model = \"gpt\"\n[features\nfast_mode = true\n";
    env.write_config(broken);
    let e = &env.engine;
    let err = e.plan(&env.home, &pat(OVERSEAS)).await.unwrap_err();
    assert!(matches!(err, Error::Parse { .. }), "{err:?}");
    let st = e.read_state(&env.home).await.unwrap();
    assert!(st.parse_error.is_some());
    assert_eq!(env.config(), broken);
    assert_eq!(env.auth(), None);
}

#[tokio::test]
async fn generation_restore_and_rollback() {
    let env = Env::new();
    env.write_config(OFFICIAL_CONFIG);
    let e = &env.engine;
    let r1 = e
        .apply(&e.plan(&env.home, &pat(OVERSEAS)).await.unwrap())
        .await
        .unwrap();
    let after_g1_config = env.config();
    let after_g1_auth = env.auth().unwrap();
    let r2 = e
        .apply(&e.plan(&env.home, &apikey(CDN)).await.unwrap())
        .await
        .unwrap();
    assert_eq!((r1.generation, r2.generation), (1, 2));

    // Rollback of generation 2 restores the exact pre-images.
    e.rollback(&r2).await.unwrap();
    assert_eq!(env.config(), after_g1_config);
    assert_eq!(env.auth().unwrap(), after_g1_auth);
    let st = e.read_state(&env.home).await.unwrap();
    assert_eq!(st.active.unwrap().generation, 1);
    // A second rollback finds the files no longer at the written hash.
    assert!(matches!(e.rollback(&r2).await, Err(Error::Conflict { .. })));

    let r3 = e
        .apply(&e.plan(&env.home, &apikey(CDN)).await.unwrap())
        .await
        .unwrap();
    assert_eq!(r3.generation, 3, "generation numbers are never reused");
    let plan = e
        .plan(
            &env.home,
            &restore(ResolvedMode::Generation { generation: 1 }),
        )
        .await
        .unwrap();
    e.apply(&plan).await.unwrap();
    assert_eq!(env.config(), after_g1_config);
    assert_eq!(env.auth().unwrap(), after_g1_auth);

    // A comment added by another program survives a key-level rollback.
    let r5 = e
        .apply(&e.plan(&env.home, &apikey(OVERSEAS)).await.unwrap())
        .await
        .unwrap();
    env.write_config(&format!("{}\n# user note\n", env.config()));
    e.rollback(&r5).await.unwrap();
    assert!(env.config().contains("# user note"));
    assert_owned_values_equal(&env.config(), &after_g1_config);
    assert_eq!(env.auth().unwrap(), after_g1_auth);
    assert_eq!(
        e.read_state(&env.home)
            .await
            .unwrap()
            .active
            .unwrap()
            .generation,
        4
    );
    assert!(walk(&env.ctx.data_dir.config_journal_dir()).is_empty());
}

const APP_CONFIG: &str = "\n# Settings written by the desktop app\n[desktop]\nfollowUpQueueMode = \"steer\"\n\n[mcp_servers.node_repl]\ncommand  =  'node' # bundled server\nargs = [\"--experimental-repl-await\"]\n";

async fn pat_over_apikey(env: &Env) -> (ApplyReport, String, String) {
    env.write_config(
        "\u{feff}# Existing config\r\nmodel = 'gpt-5.5' # keep this spelling\r\n\r\n[mcp_servers.docs]\r\ncommand  =  'docs-mcp'\r\n",
    );
    let e = &env.engine;
    e.apply(&e.plan(&env.home, &apikey(CDN)).await.unwrap())
        .await
        .unwrap();
    let before_config = env.config();
    let before_auth = env.auth().unwrap();
    let report = e
        .apply(&e.plan(&env.home, &pat(OVERSEAS)).await.unwrap())
        .await
        .unwrap();
    assert_eq!(report.files.len(), 2);
    assert_eq!(report.generation, 2);
    (report, before_config, before_auth)
}

fn assert_owned_values_equal(actual: &str, expected: &str) {
    let actual = ConfigDoc::parse(actual.as_bytes()).unwrap();
    let expected = ConfigDoc::parse(expected.as_bytes()).unwrap();
    for path in [
        k::MODEL_PROVIDER,
        k::OPENAI_BASE_URL,
        k::CHATGPT_BASE_URL,
        k::MODEL,
        k::MODEL_REASONING_EFFORT,
    ]
    .into_iter()
    .chain(PAT_RECOMMENDED_KEYS.iter().map(|(path, _)| *path))
    {
        assert_eq!(actual.get_value(path), expected.get_value(path), "{path}");
    }
}

async fn assert_rollback_conflict_unchanged(env: &Env, report: &ApplyReport, file: &str) {
    let config = std::fs::read(env.config_path()).ok();
    let auth = std::fs::read(env.auth_path()).ok();
    let state = std::fs::read(env.ctx.data_dir.state_json()).unwrap();
    let backups = walk(&env.ctx.data_dir.switch_backups_dir());
    let error = env.engine.rollback(report).await.unwrap_err();
    let Error::Conflict { message } = error else {
        panic!("expected a rollback conflict, got {error:?}");
    };
    assert!(message.contains(file), "{message}");
    assert!(message.contains("no files were rolled back"), "{message}");
    assert_eq!(std::fs::read(env.config_path()).ok(), config);
    assert_eq!(std::fs::read(env.auth_path()).ok(), auth);
    assert_eq!(std::fs::read(env.ctx.data_dir.state_json()).unwrap(), state);
    assert_eq!(walk(&env.ctx.data_dir.switch_backups_dir()), backups);
    assert!(walk(&env.ctx.data_dir.config_journal_dir()).is_empty());
    assert!(
        env.engine
            .recover(&env.home)
            .await
            .unwrap()
            .entries
            .is_empty()
    );
}

#[tokio::test]
async fn rollback_preserves_app_tables_and_semantically_equal_owned_keys() {
    for change_spelling in [false, true] {
        let env = Env::new();
        let (report, before_config, before_auth) = pat_over_apikey(&env).await;
        let mut live = env.config();
        if change_spelling {
            live = live.replace("model_provider = \"openai\"", "model_provider = 'openai'");
            assert_ne!(live, env.config());
        }
        env.write_config(&format!("{live}{APP_CONFIG}"));

        env.engine.rollback(&report).await.unwrap();

        let config = env.config();
        assert_owned_values_equal(&config, &before_config);
        assert!(config.contains(APP_CONFIG), "{config}");
        assert!(config.contains("command  =  'docs-mcp'\r\n"));
        assert!(config.starts_with('\u{feff}'));
        let doc = ConfigDoc::parse(config.as_bytes()).unwrap();
        assert_eq!(
            doc.get_str(k::OPENAI_BASE_URL),
            Some(format!("{CDN}/codex/v1"))
        );
        assert_eq!(doc.get_repr(k::CHATGPT_BASE_URL), None);
        for (path, _) in PAT_RECOMMENDED_KEYS {
            assert_eq!(doc.get_repr(path), None, "{path}");
        }
        assert_eq!(env.auth().unwrap(), before_auth);
        let state = env.engine.read_state(&env.home).await.unwrap();
        assert_eq!(state.active.unwrap().generation, 1);
        assert!(state.drift.is_empty());
        assert!(!state.journal_pending);
        assert!(walk(&env.ctx.data_dir.config_journal_dir()).is_empty());
        assert!(
            env.engine
                .recover(&env.home)
                .await
                .unwrap()
                .entries
                .is_empty()
        );
    }
}

#[tokio::test]
async fn rollback_without_app_edits_restores_exact_preimages() {
    let env = Env::new();
    let (report, before_config, before_auth) = pat_over_apikey(&env).await;

    env.engine.rollback(&report).await.unwrap();

    assert_eq!(
        std::fs::read(env.config_path()).unwrap(),
        before_config.as_bytes()
    );
    assert_eq!(
        std::fs::read(env.auth_path()).unwrap(),
        before_auth.as_bytes()
    );
    assert_eq!(
        env.engine
            .read_state(&env.home)
            .await
            .unwrap()
            .active
            .unwrap()
            .generation,
        1
    );
    assert!(walk(&env.ctx.data_dir.config_journal_dir()).is_empty());
}

#[tokio::test]
async fn rollback_owned_key_edit_leaves_both_files_unchanged() {
    let env = Env::new();
    let (report, _, _) = pat_over_apikey(&env).await;
    let mut doc = ConfigDoc::parse(env.config().as_bytes()).unwrap();
    doc.set_repr(k::OPENAI_BASE_URL, "'https://edited.example/v1'")
        .unwrap();
    std::fs::write(env.config_path(), doc.to_bytes()).unwrap();

    assert_rollback_conflict_unchanged(&env, &report, "config.toml").await;
    assert_eq!(env.auth_json()["personal_access_token"], PAT_KEY);
}

#[tokio::test]
async fn rollback_auth_edit_leaves_both_files_unchanged() {
    for append_app_config in [false, true] {
        let env = Env::new();
        let (report, _, _) = pat_over_apikey(&env).await;
        if append_app_config {
            env.write_config(&format!("{}{APP_CONFIG}", env.config()));
        }
        env.write_auth("{\"OPENAI_API_KEY\": \"edited-by-another-program\"}");

        assert_rollback_conflict_unchanged(&env, &report, "auth.json").await;
    }
}

#[tokio::test]
async fn rollback_first_takeover_keeps_only_app_content_or_removes_empty_file() {
    for app_content in ["", "\n \t\n", "\n# app comment\n", APP_CONFIG] {
        let env = Env::new();
        let report = env
            .engine
            .apply(&env.engine.plan(&env.home, &pat(OVERSEAS)).await.unwrap())
            .await
            .unwrap();
        if !app_content.is_empty() {
            env.write_config(&format!("{}{app_content}", env.config()));
        }

        env.engine.rollback(&report).await.unwrap();

        if app_content.trim().is_empty() {
            assert_eq!(env.config_opt(), None);
        } else {
            let config = env.config();
            assert_eq!(config.trim(), app_content.trim());
            assert_owned_values_equal(&config, "");
        }
        assert_eq!(env.auth(), None);
        let state = env.engine.read_state(&env.home).await.unwrap();
        assert_eq!(state.active, None);
        assert!(!state.journal_pending);
        assert!(walk(&env.ctx.data_dir.config_journal_dir()).is_empty());
    }
}

#[tokio::test]
async fn rollback_unparseable_or_missing_config_leaves_auth_unchanged() {
    for contents in [
        Some(b"[desktop\n".as_slice()),
        Some(b"\xff".as_slice()),
        None,
    ] {
        let env = Env::new();
        let (report, _, _) = pat_over_apikey(&env).await;
        match contents {
            Some(bytes) => std::fs::write(env.config_path(), bytes).unwrap(),
            None => std::fs::remove_file(env.config_path()).unwrap(),
        }
        assert_rollback_conflict_unchanged(&env, &report, "config.toml").await;
    }
}

#[tokio::test]
async fn rollback_missing_or_invalid_backup_leaves_both_files_unchanged() {
    for kind in [CodexFileKind::ConfigToml, CodexFileKind::AuthJson] {
        for invalid in [false, true] {
            let env = Env::new();
            let (report, _, _) = pat_over_apikey(&env).await;
            env.write_config(&format!("{}{APP_CONFIG}", env.config()));
            let file = report.files.iter().find(|f| f.kind == kind).unwrap();
            let backup = file.backup.as_ref().unwrap();
            if invalid {
                std::fs::write(backup, b"invalid sealed backup").unwrap();
            } else {
                std::fs::remove_file(backup).unwrap();
            }
            assert_rollback_conflict_unchanged(
                &env,
                &report,
                file.path.file_name().unwrap().to_str().unwrap(),
            )
            .await;
        }
    }
}

#[tokio::test]
async fn rollback_stale_generation_leaves_both_files_unchanged() {
    for append_app_config in [false, true] {
        let env = Env::new();
        let (report, _, _) = pat_over_apikey(&env).await;
        // The new generation has the same owned values, so comparing keys alone
        // would incorrectly allow the older report to restore its pre-images.
        env.engine
            .apply(&env.engine.plan(&env.home, &pat(OVERSEAS)).await.unwrap())
            .await
            .unwrap();
        if append_app_config {
            env.write_config(&format!("{}{APP_CONFIG}", env.config()));
        }
        assert_rollback_conflict_unchanged(&env, &report, "config.toml").await;
    }
}

#[tokio::test]
async fn rollback_owned_path_changed_to_table_leaves_both_files_unchanged() {
    for table in [
        "\n[model_reasoning_effort]\nvalue = 'high'\n",
        "\n[[model_reasoning_effort]]\nvalue = 'high'\n",
    ] {
        let env = Env::new();
        let (report, _, _) = pat_over_apikey(&env).await;
        env.write_config(&format!("{}{table}", env.config()));
        assert_rollback_conflict_unchanged(&env, &report, "config.toml").await;
    }
}

#[tokio::test]
async fn rollback_released_key_edit_leaves_both_files_unchanged() {
    let env = Env::new();
    pat_over_apikey(&env).await;
    let report = env
        .engine
        .apply(&env.engine.plan(&env.home, &apikey(CDN)).await.unwrap())
        .await
        .unwrap();
    let mut doc = ConfigDoc::parse(env.config().as_bytes()).unwrap();
    doc.set_repr(k::CHATGPT_BASE_URL, "'https://edited.example/backend-api/'")
        .unwrap();
    std::fs::write(env.config_path(), doc.to_bytes()).unwrap();
    assert_rollback_conflict_unchanged(&env, &report, "config.toml").await;
}

#[tokio::test]
async fn first_takeover_without_files_and_full_restore_removes_them() {
    let env = Env::new();
    let e = &env.engine;
    e.apply(&e.plan(&env.home, &pat(OVERSEAS)).await.unwrap())
        .await
        .unwrap();
    #[cfg(unix)]
    assert_eq!(mode_of(&env.config_path()), 0o600);
    let plan = e
        .plan(
            &env.home,
            &restore(ResolvedMode::Original {
                provider: ProviderRestore::KeepOpenai,
            }),
        )
        .await
        .unwrap();
    e.apply(&plan).await.unwrap();
    assert_eq!(env.config_opt(), None);
    assert_eq!(env.auth(), None);
}

#[tokio::test]
async fn model_keys_only_when_the_profile_sets_them() {
    let env = Env::new();
    env.write_config(OFFICIAL_CONFIG);
    let e = &env.engine;
    let mut p = pat(OVERSEAS);
    p.model = ModelDefaults {
        auto_compact: None,
        context_window: None,
        speed: None,
        model: Some("gpt-6".into()),
        reasoning_effort: Some("xhigh".into()),
    };
    e.apply(&e.plan(&env.home, &p).await.unwrap())
        .await
        .unwrap();
    assert!(env.config().contains("model = \"gpt-6\""));
    assert!(env.config().contains("model_reasoning_effort = \"xhigh\""));
    // Back to a profile without model keys: the user's model returns.
    e.apply(&e.plan(&env.home, &apikey(OVERSEAS)).await.unwrap())
        .await
        .unwrap();
    assert!(env.config().contains("model = \"gpt-5.5\""));
    assert!(!env.config().contains("model_reasoning_effort"));
}

#[tokio::test]
async fn existing_config_mode_is_preserved() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let env = Env::new();
        env.write_config(OFFICIAL_CONFIG);
        std::fs::set_permissions(env.config_path(), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        env.write_auth("{\"OPENAI_API_KEY\":\"other-tool-key-12345678\"}");
        std::fs::set_permissions(env.auth_path(), std::fs::Permissions::from_mode(0o644)).unwrap();
        let e = &env.engine;
        e.apply(&e.plan(&env.home, &pat(OVERSEAS)).await.unwrap())
            .await
            .unwrap();
        assert_eq!(mode_of(&env.config_path()), 0o644);
        assert_eq!(mode_of(&env.auth_path()), 0o600, "auth.json is always 0600");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn symlinked_config_is_written_through() {
    let env = Env::new();
    let dotfiles = env.tmp.path().join("dotfiles");
    std::fs::create_dir_all(&dotfiles).unwrap();
    let real = dotfiles.join("codex.toml");
    std::fs::write(&real, OFFICIAL_CONFIG).unwrap();
    std::os::unix::fs::symlink(&real, env.config_path()).unwrap();
    let e = &env.engine;
    e.apply(&e.plan(&env.home, &pat(OVERSEAS)).await.unwrap())
        .await
        .unwrap();
    assert!(
        std::fs::symlink_metadata(env.config_path())
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        std::fs::read_to_string(&real)
            .unwrap()
            .contains("openai_base_url")
    );
    e.apply(
        &e.plan(
            &env.home,
            &restore(ResolvedMode::Original {
                provider: ProviderRestore::KeepOpenai,
            }),
        )
        .await
        .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&real).unwrap(), OFFICIAL_CONFIG);
}

#[tokio::test]
async fn crlf_and_bom_survive_and_restore_exactly() {
    let env = Env::new();
    let original = format!("\u{feff}{}", OFFICIAL_CONFIG.replace('\n', "\r\n"));
    env.write_config(&original);
    let e = &env.engine;
    e.apply(&e.plan(&env.home, &pat(OVERSEAS)).await.unwrap())
        .await
        .unwrap();
    let cfg = env.config();
    assert!(cfg.starts_with('\u{feff}'));
    assert!(
        !cfg.replace("\r\n", "").contains('\n'),
        "lone LF in {cfg:?}"
    );
    e.apply(
        &e.plan(
            &env.home,
            &restore(ResolvedMode::Original {
                provider: ProviderRestore::KeepOpenai,
            }),
        )
        .await
        .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(env.config(), original);
}

#[tokio::test]
async fn pure_render_matches_apply_and_does_no_io() {
    let env = Env::new();
    let e = &env.engine;
    let files = CodexFiles {
        config_toml: Some(SecretBytes::new(LEGACY_CONFIG.as_bytes().to_vec())),
        auth_json: None,
    };
    let desired = pat(OVERSEAS);
    let rendered = e
        .render(&RenderInput {
            files: &files,
            desired: &desired,
            baseline: None,
            login_stash: None,
            original_files: None,
            last_written: None,
        })
        .unwrap();
    let FileChange::Write { contents, .. } = &rendered.config_toml else {
        panic!("expected a config write");
    };
    env.write_config(LEGACY_CONFIG);
    e.apply(&e.plan(&env.home, &desired).await.unwrap())
        .await
        .unwrap();
    assert_eq!(contents.expose(), env.config().as_bytes());
    // Nothing was created by the pure call besides what apply did.
    let st = e.inspect_files(env.home.root(), &files).unwrap();
    assert_eq!(st.model_provider.as_deref(), Some("krill"));
    assert!(st.provider_table_present);
    let b = e
        .capture_baseline(
            env.home.root(),
            &files,
            BaselineExtras::default(),
            superba_core::Timestamp::from_millis(5),
        )
        .unwrap();
    assert_eq!(b.provider_key.as_deref(), Some("krill"));
    assert!(
        b.keys
            .iter()
            .any(|s| s.path == k::OPENAI_BASE_URL && s.value.is_none())
    );
}

#[tokio::test]
async fn render_requires_resolution_for_drift_given_last_written() {
    let env = Env::new();
    let e = &env.engine;
    let files = CodexFiles {
        config_toml: Some(SecretBytes::new(
            b"model_provider = \"openai\"\nopenai_base_url = \"https://hand.example/v1\"\n"
                .to_vec(),
        )),
        auth_json: None,
    };
    let last = vec![superba_core::KeySnapshot {
        path: k::OPENAI_BASE_URL.into(),
        value: Some("\"https://api.krill-code.net/backend-api/codex\"".into()),
    }];
    let desired = apikey(CDN);
    let err = e
        .render(&RenderInput {
            files: &files,
            desired: &desired,
            baseline: None,
            login_stash: None,
            original_files: None,
            last_written: Some(&last),
        })
        .unwrap_err();
    assert!(matches!(err, Error::Conflict { .. }));
    // Generation restores need the engine's snapshot.
    let gen_restore = restore(ResolvedMode::Generation { generation: 1 });
    assert!(matches!(
        e.render(&RenderInput {
            files: &files,
            desired: &gen_restore,
            baseline: None,
            login_stash: None,
            original_files: None,
            last_written: None,
        }),
        Err(Error::Unsupported { .. })
    ));
}

#[tokio::test]
async fn notes_for_profile_override_and_keyring_credentials() {
    let env = Env::new();
    env.write_config("profile = \"work\"\ncli_auth_credentials_store = \"keyring\"\n\n[profiles.work]\nmodel = \"o3\"\n");
    let e = &env.engine;
    let plan = e.plan(&env.home, &apikey(OVERSEAS)).await.unwrap();
    let keys: Vec<_> = plan.notes.iter().map(|n| n.text.key.as_str()).collect();
    assert!(keys.contains(&"config.note.profile_override"), "{keys:?}");
    assert!(keys.contains(&"config.note.credentials_store"), "{keys:?}");
}

#[tokio::test]
async fn original_restore_needs_a_baseline() {
    let env = Env::new();
    let e = &env.engine;
    let err = e
        .plan(
            &env.home,
            &restore(ResolvedMode::Original {
                provider: ProviderRestore::KeepOpenai,
            }),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotFound { .. }));
}

#[tokio::test]
async fn deserialized_plan_cannot_be_applied() {
    let env = Env::new();
    let e = &env.engine;
    let mut plan = e.plan(&env.home, &pat(OVERSEAS)).await.unwrap();
    plan.desired = None;
    assert!(matches!(e.apply(&plan).await, Err(Error::Invalid { .. })));
    assert_eq!(env.auth(), None);
}

#[tokio::test]
async fn signed_out_login_is_not_resurrected_by_an_original_restore_either() {
    let env = Env::new();
    let login = chatgpt_login("user@example.com", "rt-1", "2026-10-01T00:00:00Z");
    env.write_auth(&login);
    let e = &env.engine;
    e.apply(&e.plan(&env.home, &pat(OVERSEAS)).await.unwrap())
        .await
        .unwrap();
    e.apply(
        &e.plan(&env.home, &restore(ResolvedMode::Official))
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    std::fs::remove_file(env.auth_path()).unwrap(); // signed out in the app
    e.apply(&e.plan(&env.home, &apikey(OVERSEAS)).await.unwrap())
        .await
        .unwrap();
    // Two switches later the original login (first-write pre-image) must still not
    // come back.
    e.apply(&e.plan(&env.home, &pat(CDN)).await.unwrap())
        .await
        .unwrap();
    let plan = e
        .plan(
            &env.home,
            &restore(ResolvedMode::Original {
                provider: ProviderRestore::KeepOpenai,
            }),
        )
        .await
        .unwrap();
    e.apply(&plan).await.unwrap();
    assert_eq!(env.auth(), None);

    // Signing in again with the same account lifts the block.
    let again = chatgpt_login("user@example.com", "rt-2", "2026-10-16T00:00:00Z");
    env.write_auth(&again);
    e.apply(&e.plan(&env.home, &pat(OVERSEAS)).await.unwrap())
        .await
        .unwrap();
    e.apply(
        &e.plan(&env.home, &restore(ResolvedMode::Official))
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(env.auth().unwrap(), again);
}

#[tokio::test]
async fn restoring_an_old_login_warns_about_its_age() {
    let env = Env::new();
    // Clock is at 2026-09-21 (1_790_000_000_000 ms); last refresh 30+ days earlier.
    env.write_auth(&chatgpt_login(
        "user@example.com",
        "rt-1",
        "2026-08-01T00:00:00Z",
    ));
    let e = &env.engine;
    e.apply(&e.plan(&env.home, &pat(OVERSEAS)).await.unwrap())
        .await
        .unwrap();
    let plan = e
        .plan(&env.home, &restore(ResolvedMode::Official))
        .await
        .unwrap();
    let note = plan
        .notes
        .iter()
        .find(|n| n.text.key == "config.note.stale_login")
        .expect("stale login warning");
    assert!(
        note.text.render_fallback().contains("51 days"),
        "{}",
        note.text
    );
}

#[tokio::test]
async fn legacy_provider_table_is_kept_and_noted() {
    let env = Env::new();
    env.write_config(LEGACY_CONFIG);
    let e = &env.engine;
    let plan = e.plan(&env.home, &apikey(OVERSEAS)).await.unwrap();
    let note = plan
        .notes
        .iter()
        .find(|n| n.text.key == "config.note.legacy_provider_table")
        .expect("legacy table note");
    assert!(
        note.text
            .render_fallback()
            .contains("[model_providers.krill]")
    );
}

#[tokio::test]
async fn compatible_base_url_on_a_bare_origin_has_no_trailing_slash() {
    let env = Env::new();
    let origin = superba_core::Origin::parse("http://127.0.0.1:18080").unwrap();
    let base = superba_core::CodexPath::AsIs.base_url(&origin);
    // url::Url always serializes a bare origin with a trailing slash.
    assert_eq!(base.as_str(), "http://127.0.0.1:18080/");
    let mut desired = apikey(OVERSEAS);
    if let ResolvedMode::Token { endpoints, .. } = &mut desired.mode {
        endpoints.origin = origin.clone();
        endpoints.openai_base_url = base;
    }
    let plan = env.engine.plan(&env.home, &desired).await.unwrap();
    assert_eq!(
        key_change(&plan, k::OPENAI_BASE_URL).after.as_deref(),
        Some("\"http://127.0.0.1:18080\"")
    );
    // A path keeps everything but a trailing slash.
    if let ResolvedMode::Token { endpoints, .. } = &mut desired.mode {
        endpoints.openai_base_url = url::Url::parse("https://example.com/v1/").unwrap();
    }
    let plan = env.engine.plan(&env.home, &desired).await.unwrap();
    assert_eq!(
        key_change(&plan, k::OPENAI_BASE_URL).after.as_deref(),
        Some("\"https://example.com/v1\"")
    );
}
