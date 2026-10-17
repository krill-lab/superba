//! v0.3 model keys (`model_catalog_json`, `model_context_window`,
//! `model_auto_compact_token_limit`, `service_tier`): written exactly when the
//! resolved profile sets them, everything else in `config.toml` byte-identical,
//! released and restored like `model`.

mod common;

use std::path::PathBuf;

use common::*;
use superba_core::{
    CodexFiles, ConfigEngine, ModelDefaults, ProviderRestore, RenderInput, ResolvedMode,
    SecretBytes, SpeedTier,
};

fn with_models(mut p: superba_core::ResolvedProfile) -> superba_core::ResolvedProfile {
    p.model = ModelDefaults {
        model: Some("gpt-6-astra".into()),
        reasoning_effort: None,
        speed: Some(SpeedTier::Ultrafast),
        context_window: Some(400_000),
        auto_compact: Some(300_000),
    };
    p.model_catalog = Some(PathBuf::from(
        "/home/用户/.codex/superba/models-0a1b2c3d.json",
    ));
    p
}

/// Lines of `after` that are not in `before` (order kept).
fn added_lines(before: &str, after: &str) -> Vec<String> {
    let old: Vec<&str> = before.lines().collect();
    after
        .lines()
        .filter(|l| !old.contains(l))
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn model_keys_are_written_and_the_rest_stays_byte_identical() {
    let env = Env::new();
    env.write_config(OFFICIAL_CONFIG);
    let e = &env.engine;
    let plan = e
        .plan(&env.home, &with_models(apikey(OVERSEAS)))
        .await
        .unwrap();
    let changed: Vec<&str> = plan.key_changes.iter().map(|k| k.path.as_str()).collect();
    for key in [
        "model_catalog_json",
        "model_context_window",
        "model_auto_compact_token_limit",
        "service_tier",
    ] {
        assert!(changed.contains(&key), "{key} in {changed:?}");
    }
    e.apply(&plan).await.unwrap();
    let cfg = env.config();
    assert!(
        cfg.contains("model_catalog_json = \"/home/用户/.codex/superba/models-0a1b2c3d.json\"")
    );
    assert!(cfg.contains("model_context_window = 400000\n"));
    assert!(cfg.contains("model_auto_compact_token_limit = 300000\n"));
    assert!(cfg.contains("service_tier = \"ultrafast\"\n"));
    // The user's tables and comments are untouched.
    assert!(cfg.contains("# Desktop app settings\n[desktop]\ntheme = \"dark\"\n"));
    assert!(cfg.contains("[mcp_servers.fs]\ncommand = \"mcp-fs\"\n"));
    let added = added_lines(OFFICIAL_CONFIG, &cfg);
    assert!(
        added.iter().all(|l| l.contains('=')),
        "only owned keys added: {added:?}"
    );
    // Restore of the original configuration is byte-exact.
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
    assert_eq!(env.config(), OFFICIAL_CONFIG);
}

#[tokio::test]
async fn standard_is_written_as_default_and_released_keys_restore_the_users_value() {
    let env = Env::new();
    // The user had their own service tier and window before Superba.
    let original =
        "service_tier = \"flex\"\nmodel_context_window = 1000000\n\n[desktop]\ntheme = \"light\"\n";
    env.write_config(original);
    let e = &env.engine;
    let mut p = apikey(OVERSEAS);
    p.model.speed = Some(SpeedTier::Standard);
    e.apply(&e.plan(&env.home, &p).await.unwrap())
        .await
        .unwrap();
    let cfg = env.config();
    assert!(cfg.contains("service_tier = \"default\"\n"), "{cfg}");
    // Not set by the profile: the user's window stays.
    assert!(cfg.contains("model_context_window = 1000000\n"));

    // A profile that sets nothing releases service_tier back to the user's value.
    e.apply(&e.plan(&env.home, &apikey(OVERSEAS)).await.unwrap())
        .await
        .unwrap();
    let cfg = env.config();
    assert!(cfg.contains("service_tier = \"flex\"\n"), "{cfg}");
    assert!(!cfg.contains("model_catalog_json"));

    // The official login releases every model key too.
    let mut p = with_models(apikey(OVERSEAS));
    p.model.model = None;
    e.apply(&e.plan(&env.home, &p).await.unwrap())
        .await
        .unwrap();
    e.apply(
        &e.plan(&env.home, &restore(ResolvedMode::Official))
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    let cfg = env.config();
    assert!(cfg.contains("service_tier = \"flex\"\n"), "{cfg}");
    assert!(cfg.contains("model_context_window = 1000000\n"));
    assert!(!cfg.contains("model_catalog_json"));
    assert!(!cfg.contains("model_auto_compact_token_limit"));
}

#[tokio::test]
async fn crlf_comments_and_cjk_survive_a_round_trip_with_model_keys() {
    let env = Env::new();
    let original = format!(
        "\u{feff}{}",
        "# 我的配置\r\nmodel = \"gpt-5.5\" # 默认模型\r\n\r\n[projects.\"/home/u/项目\"]\r\ntrust_level = \"trusted\"\r\n"
    );
    env.write_config(&original);
    let e = &env.engine;
    e.apply(
        &e.plan(&env.home, &with_models(pat(OVERSEAS)))
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    let cfg = env.config();
    assert!(cfg.starts_with('\u{feff}'));
    assert!(
        !cfg.replace("\r\n", "").contains('\n'),
        "lone LF in {cfg:?}"
    );
    assert!(cfg.contains("[projects.\"/home/u/项目\"]\r\ntrust_level = \"trusted\"\r\n"));
    assert!(cfg.contains("service_tier = \"ultrafast\"\r\n"));
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
async fn a_changed_service_tier_is_drift_like_any_owned_key() {
    let env = Env::new();
    env.write_config(OFFICIAL_CONFIG);
    let e = &env.engine;
    let mut p = apikey(OVERSEAS);
    p.model.speed = Some(SpeedTier::Standard);
    e.apply(&e.plan(&env.home, &p).await.unwrap())
        .await
        .unwrap();
    // The TUI's `/fast` writes the key back.
    let cfg = env
        .config()
        .replace("service_tier = \"default\"", "service_tier = \"fast\"");
    env.write_config(&cfg);
    let state = e.read_state(&env.home).await.unwrap();
    assert_eq!(state.drift.len(), 1);
    assert_eq!(state.drift[0].path, "service_tier");
    let err = e.plan(&env.home, &p).await.unwrap_err();
    assert!(matches!(err, superba_core::Error::Conflict { .. }), "{err}");
}

#[test]
fn pure_render_of_model_keys_for_remote_use() {
    let env = Env::new();
    let files = CodexFiles {
        config_toml: Some(SecretBytes::new(OFFICIAL_CONFIG.as_bytes().to_vec())),
        auth_json: None,
    };
    let desired = with_models(apikey(OVERSEAS));
    let rendered = env
        .engine
        .render(&RenderInput {
            files: &files,
            desired: &desired,
            baseline: None,
            login_stash: None,
            original_files: None,
            last_written: None,
        })
        .unwrap();
    let paths: Vec<&str> = rendered
        .key_changes
        .iter()
        .map(|k| k.path.as_str())
        .collect();
    assert!(paths.contains(&"service_tier"));
    assert!(paths.contains(&"model_catalog_json"));
}
