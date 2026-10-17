//! Owned Codex settings through the public engine, with isolated temporary homes.

mod common;

use std::collections::BTreeMap;

use common::*;
use pretty_assertions::assert_eq;
use superba_core::codex::PAT_RECOMMENDED_KEYS;
use superba_core::{
    ApplyPlan, AuthMode, CodexSettings, ConfigEngine, Error, KeyResolution, ProviderRestore,
    ResolvedMode, ResolvedProfile, SettingEntry, SettingValue,
};

const USER_TABLES: &str = r#"
# 桌面设置：空格和注释必须保留
[desktop]
theme  =  'dark' # user preference

[features]
shell_tool = true # unrelated feature

[tui]
animations = true # unrelated UI setting
"#;

const USER_EDIT: &str = "\n# 用户在切换后修改\n[desktop.layout]\nwidth = 1234\n";
const SETTINGS: &[&str] = &[
    "approval_policy",
    "web_search",
    "features.fast_mode",
    "tui.notifications",
];

fn original_config(with_web_search: bool, crlf: bool) -> String {
    let web_search = if with_web_search {
        "web_search = \"cached\"\n"
    } else {
        ""
    };
    let config = format!("approval_policy = \"never\"\n{web_search}{LEGACY_CONFIG}{USER_TABLES}");
    if crlf {
        config.replace('\n', "\r\n")
    } else {
        config
    }
}

fn profile(mode: AuthMode) -> ResolvedProfile {
    let mut desired = match mode {
        AuthMode::Pat => pat(OVERSEAS),
        AuthMode::ApiKey => apikey(OVERSEAS),
        _ => panic!("expected a token mode"),
    };
    desired.settings = Default::default();
    desired
}

fn settings_profile(mode: AuthMode) -> ResolvedProfile {
    let mut desired = profile(mode);
    desired.settings.set("web_search", "live").unwrap();
    desired.settings.set("features.fast_mode", true).unwrap();
    desired.settings.set("tui.notifications", false).unwrap();
    desired.settings.unset("approval_policy").unwrap();
    desired
}

// Use a separate TOML reader as the oracle, never the engine's patching helpers.
fn value(config: &str, path: &str) -> Option<toml::Value> {
    let parsed: toml::Value = toml::from_str(config).unwrap();
    path.split('.')
        .try_fold(&parsed, |node, part| node.get(part))
        .cloned()
}

fn assert_setting_changes(plan: &ApplyPlan, expected: &[(&str, Option<&str>, Option<&str>)]) {
    // Only routing changes are excluded: unexpected settings and duplicate rows fail.
    let mut actual: Vec<_> = plan
        .key_changes
        .iter()
        .filter(|change| {
            !matches!(
                change.path.as_str(),
                "model_provider" | "openai_base_url" | "chatgpt_base_url"
            )
        })
        .map(|change| {
            (
                change.path.as_str(),
                change.before.as_deref(),
                change.after.as_deref(),
            )
        })
        .collect();
    actual.sort_unstable();
    let mut expected = expected.to_vec();
    expected.sort_unstable();
    assert_eq!(actual, expected);
}

fn assert_restored_settings(config: &str, original: &str, paths: &[&str]) {
    for path in paths {
        assert_eq!(value(config, path), value(original, path), "{path}");
    }
}

async fn apply_and_release(mode: AuthMode, with_web_search: bool, crlf: bool) {
    let env = Env::new();
    let original = original_config(with_web_search, crlf);
    env.write_config(&original);
    let mut desired = settings_profile(mode);
    let plan = env.engine.plan(&env.home, &desired).await.unwrap();
    assert_setting_changes(
        &plan,
        &[
            (
                "web_search",
                with_web_search.then_some("\"cached\""),
                Some("\"live\""),
            ),
            ("features.fast_mode", None, Some("true")),
            ("tui.notifications", None, Some("false")),
            ("approval_policy", Some("\"never\""), None),
        ],
    );
    assert_eq!(env.config(), original, "planning must not write");
    assert_eq!(env.auth(), None);
    env.engine.apply(&plan).await.unwrap();

    // The expected bytes contain only the requested setting and routing edits.
    let mut routing = match mode {
        AuthMode::Pat => format!(
            "openai_base_url = \"{OVERSEAS}/backend-api/codex\"\n\
             chatgpt_base_url = \"{OVERSEAS}/backend-api/\"\n"
        ),
        _ => format!("openai_base_url = \"{OVERSEAS}/codex/v1\"\n"),
    };
    if !with_web_search {
        routing.push_str("web_search = \"live\"\n");
    }
    let expected = original_config(with_web_search, false)
        .replace("approval_policy = \"never\"\n", "")
        .replace("web_search = \"cached\"", "web_search = \"live\"")
        .replace("model_provider = \"krill\"", "model_provider = \"openai\"")
        .replace(
            "model_reasoning_effort = \"high\"\n",
            &format!("model_reasoning_effort = \"high\"\n{routing}"),
        )
        .replace(
            "shell_tool = true # unrelated feature\n",
            "shell_tool = true # unrelated feature\nfast_mode = true\n",
        )
        .replace(
            "animations = true # unrelated UI setting\n",
            "animations = true # unrelated UI setting\nnotifications = false\n",
        );
    let expected = if crlf {
        expected.replace('\n', "\r\n")
    } else {
        expected
    };
    let applied = env.config();
    assert_eq!(applied, expected, "all unrelated bytes must survive apply");
    assert_eq!(value(&applied, "web_search"), Some("live".into()));
    assert_eq!(value(&applied, "features.fast_mode"), Some(true.into()));
    assert_eq!(value(&applied, "tui.notifications"), Some(false.into()));
    assert_eq!(value(&applied, "approval_policy"), None);
    if crlf {
        assert!(!applied.replace("\r\n", "").contains('\n'));
        assert!(applied.contains("fast_mode = true\r\n"));
        assert!(applied.contains("notifications = false\r\n"));
    }

    desired.settings.forget("web_search");
    desired.settings.forget("approval_policy");
    let plan = env.engine.plan(&env.home, &desired).await.unwrap();
    assert_setting_changes(
        &plan,
        &[
            (
                "web_search",
                Some("\"live\""),
                with_web_search.then_some("\"cached\""),
            ),
            ("approval_policy", None, Some("\"never\"")),
        ],
    );
    env.engine.apply(&plan).await.unwrap();
    let released = env.config();
    assert_restored_settings(&released, &original, &["web_search", "approval_policy"]);
    assert_eq!(value(&released, "features.fast_mode"), Some(true.into()));
    assert_eq!(value(&released, "tui.notifications"), Some(false.into()));

    // Released keys belong to the user again: later edits need no resolution.
    let edited = released.replace(
        "approval_policy = \"never\"",
        "approval_policy = \"on-request\"",
    );
    let edited = if with_web_search {
        edited.replace("web_search = \"cached\"", "web_search = \"disabled\"")
    } else {
        let newline = if crlf { "\r\n" } else { "\n" };
        format!("web_search = \"disabled\"{newline}{edited}")
    };
    env.write_config(&edited);
    let plan = env.engine.plan(&env.home, &desired).await.unwrap();
    assert!(plan.key_changes.is_empty());
    env.engine.apply(&plan).await.unwrap();
    assert_eq!(env.config(), edited);
    assert!(
        env.engine
            .read_state(&env.home)
            .await
            .unwrap()
            .drift
            .is_empty()
    );
}

#[tokio::test]
async fn pat_settings_preserve_bytes_and_release_cached_baselines() {
    apply_and_release(AuthMode::Pat, true, false).await;
}

#[tokio::test]
async fn apikey_settings_preserve_crlf_and_release_absent_baselines() {
    apply_and_release(AuthMode::ApiKey, false, true).await;
}

async fn drifted_setting() -> (Env, ResolvedProfile) {
    let env = Env::new();
    env.write_config(&original_config(true, false));
    let mut desired = profile(AuthMode::Pat);
    desired.settings.set("web_search", "live").unwrap();
    let plan = env.engine.plan(&env.home, &desired).await.unwrap();
    env.engine.apply(&plan).await.unwrap();
    let edited = env
        .config()
        .replace("web_search = \"live\"", "web_search = \"disabled\"");
    assert_ne!(edited, env.config());
    env.write_config(&edited);
    desired.settings.set("web_search", "cached").unwrap();
    (env, desired)
}

#[tokio::test]
async fn drifted_setting_requires_a_resolution_and_names_the_key() {
    let (env, desired) = drifted_setting().await;
    let before = env.config();
    let before_auth = env.auth();
    let error = env.engine.plan(&env.home, &desired).await.unwrap_err();
    assert!(
        matches!(error, Error::Conflict { ref message } if message.contains("web_search")),
        "{error:?}"
    );
    assert_eq!(env.config(), before);
    assert_eq!(env.auth(), before_auth);
}

#[tokio::test]
async fn keep_current_preserves_the_user_setting_and_releases_ownership() {
    let (env, mut desired) = drifted_setting().await;
    let before = env.config();
    desired
        .key_resolutions
        .insert("web_search".into(), KeyResolution::KeepCurrent);
    let plan = env.engine.plan(&env.home, &desired).await.unwrap();
    assert_setting_changes(&plan, &[]);
    env.engine.apply(&plan).await.unwrap();
    assert_eq!(env.config(), before);
    assert_eq!(value(&env.config(), "web_search"), Some("disabled".into()));

    desired.settings.forget("web_search");
    desired.key_resolutions.clear();
    let plan = env.engine.plan(&env.home, &desired).await.unwrap();
    assert_setting_changes(&plan, &[]);
    env.engine.apply(&plan).await.unwrap();
    assert_eq!(env.config(), before);
}

#[tokio::test]
async fn overwrite_replaces_the_drifted_setting() {
    let (env, mut desired) = drifted_setting().await;
    desired
        .key_resolutions
        .insert("web_search".into(), KeyResolution::Overwrite);
    let plan = env.engine.plan(&env.home, &desired).await.unwrap();
    assert_setting_changes(
        &plan,
        &[("web_search", Some("\"disabled\""), Some("\"cached\""))],
    );
    env.engine.apply(&plan).await.unwrap();
    assert_eq!(value(&env.config(), "web_search"), Some("cached".into()));
    assert!(
        env.engine
            .read_state(&env.home)
            .await
            .unwrap()
            .drift
            .is_empty()
    );
}

#[tokio::test]
async fn scalar_parent_shape_conflict_skips_only_that_setting_with_a_notice() {
    let env = Env::new();
    let original = format!("features.token_budget = true # 用户的布尔值\n{OFFICIAL_CONFIG}");
    env.write_config(&original);
    let mut desired = profile(AuthMode::Pat);
    desired
        .settings
        .set("features.token_budget.enabled", true)
        .unwrap();
    desired.settings.set("web_search", "cached").unwrap();
    let plan = env.engine.plan(&env.home, &desired).await.unwrap();
    assert_setting_changes(&plan, &[("web_search", None, Some("\"cached\""))]);
    assert!(plan.notes.iter().any(|note| {
        note.text.key == "config.note.setting_shape_conflict"
            && note
                .text
                .args
                .iter()
                .any(|(key, value)| key == "key" && value == "features.token_budget.enabled")
    }));
    assert_eq!(env.config(), original);
    env.engine.apply(&plan).await.unwrap();
    let config = env.config();
    assert!(config.contains("features.token_budget = true # 用户的布尔值\n"));
    assert_eq!(value(&config, "features.token_budget"), Some(true.into()));
    assert_eq!(value(&config, "features.token_budget.enabled"), None);
    assert_eq!(value(&config, "web_search"), Some("cached".into()));

    desired.settings = CodexSettings::new();
    let plan = env.engine.plan(&env.home, &desired).await.unwrap();
    env.engine.apply(&plan).await.unwrap();
    assert_eq!(
        value(&env.config(), "features.token_budget"),
        Some(true.into())
    );
}

async fn assert_invalid_setting(path: &str) {
    let env = Env::new();
    env.write_config(OFFICIAL_CONFIG);
    let mut desired = profile(AuthMode::ApiKey);
    desired
        .settings
        .insert_unchecked(path, SettingEntry::Set("invalid".into()));
    let error = env.engine.plan(&env.home, &desired).await.unwrap_err();
    assert!(
        matches!(error, Error::Invalid { ref message } if message.contains(path)),
        "{error:?}"
    );
    assert_eq!(env.config(), OFFICIAL_CONFIG);
    assert_eq!(env.auth(), None);
}

#[tokio::test]
async fn reserved_setting_path_is_rejected_even_when_inserted_unchecked() {
    assert_invalid_setting("openai_base_url").await;
}

#[tokio::test]
async fn malformed_setting_path_is_rejected_even_when_inserted_unchecked() {
    assert_invalid_setting("a..b").await;
}

async fn rollback_first_apply(with_user_edit: bool) {
    let env = Env::new();
    let original = original_config(true, false);
    env.write_config(&original);
    let plan = env
        .engine
        .plan(&env.home, &settings_profile(AuthMode::Pat))
        .await
        .unwrap();
    let report = env.engine.apply(&plan).await.unwrap();
    if with_user_edit {
        env.write_config(&format!("{}{USER_EDIT}", env.config()));
    }
    env.engine.rollback(&report).await.unwrap();
    let config = env.config();
    assert_restored_settings(&config, &original, SETTINGS);
    if with_user_edit {
        assert!(config.contains(USER_EDIT));
        assert_eq!(
            toml::from_str::<toml::Value>(&config).unwrap(),
            toml::from_str::<toml::Value>(&format!("{original}{USER_EDIT}")).unwrap(),
        );
    } else {
        assert_eq!(config, original);
    }
    assert_eq!(env.auth(), None);
}

#[tokio::test]
async fn rollback_first_apply_restores_exact_preimages() {
    rollback_first_apply(false).await;
}

#[tokio::test]
async fn rollback_first_apply_restores_settings_and_preserves_user_edits() {
    rollback_first_apply(true).await;
}

async fn rollback_second_apply(with_user_edit: bool) {
    let env = Env::new();
    env.write_config(&original_config(true, false));
    let mut first = profile(AuthMode::ApiKey);
    first.settings.set("tui.notifications", false).unwrap();
    first.settings.unset("approval_policy").unwrap();
    let plan = env.engine.plan(&env.home, &first).await.unwrap();
    env.engine.apply(&plan).await.unwrap();
    let before = env.config();
    let before_auth = env.auth();

    let mut second = profile(AuthMode::Pat);
    second
        .settings
        .set("notify", SettingValue::Array(vec!["a".into(), "b".into()]))
        .unwrap();
    let plan = env.engine.plan(&env.home, &second).await.unwrap();
    assert_setting_changes(
        &plan,
        &[
            ("tui.notifications", Some("false"), None),
            ("approval_policy", None, Some("\"never\"")),
            ("notify", None, Some("[\"a\", \"b\"]")),
        ],
    );
    let report = env.engine.apply(&plan).await.unwrap();
    if with_user_edit {
        // Force a key-level rollback, which needs the settings in both generations.
        env.write_config(&format!("{}{USER_EDIT}", env.config()));
    }
    env.engine.rollback(&report).await.unwrap();
    let config = env.config();
    assert_restored_settings(
        &config,
        &before,
        &["tui.notifications", "approval_policy", "notify"],
    );
    if with_user_edit {
        assert!(config.contains(USER_EDIT));
        assert_eq!(
            toml::from_str::<toml::Value>(&config).unwrap(),
            toml::from_str::<toml::Value>(&format!("{before}{USER_EDIT}")).unwrap(),
        );
    } else {
        assert_eq!(config, before);
    }
    assert_eq!(env.auth(), before_auth);
    let state = env.engine.read_state(&env.home).await.unwrap();
    assert_eq!(state.active.unwrap().generation, 1);
    assert!(state.drift.is_empty());
}

#[tokio::test]
async fn rollback_second_apply_restores_exact_previous_generation() {
    rollback_second_apply(false).await;
}

#[tokio::test]
async fn rollback_second_apply_restores_both_generations_settings_and_user_edits() {
    rollback_second_apply(true).await;
}

async fn restore_settings(mode: ResolvedMode) {
    let env = Env::new();
    let original = original_config(true, false);
    env.write_config(&original);
    let plan = env
        .engine
        .plan(&env.home, &settings_profile(AuthMode::Pat))
        .await
        .unwrap();
    env.engine.apply(&plan).await.unwrap();
    env.write_config(&format!("{}{USER_EDIT}", env.config()));
    let plan = env.engine.plan(&env.home, &restore(mode)).await.unwrap();
    assert_setting_changes(
        &plan,
        &[
            ("web_search", Some("\"live\""), Some("\"cached\"")),
            ("features.fast_mode", Some("true"), None),
            ("tui.notifications", Some("false"), None),
            ("approval_policy", None, Some("\"never\"")),
        ],
    );
    env.engine.apply(&plan).await.unwrap();
    let config = env.config();
    assert_restored_settings(&config, &original, SETTINGS);
    assert!(config.contains(USER_EDIT));
    assert_eq!(env.auth(), None);

    // Prove the restored settings were released, rather than still tracked.
    let edited = config.replace("web_search = \"cached\"", "web_search = \"disabled\"");
    assert_ne!(edited, config);
    env.write_config(&edited);
    assert!(
        env.engine
            .read_state(&env.home)
            .await
            .unwrap()
            .drift
            .is_empty()
    );
    let plan = env
        .engine
        .plan(&env.home, &profile(AuthMode::ApiKey))
        .await
        .unwrap();
    assert_setting_changes(&plan, &[]);
    env.engine.apply(&plan).await.unwrap();
    assert_eq!(value(&env.config(), "web_search"), Some("disabled".into()));
}

#[tokio::test]
async fn original_keep_openai_restores_and_releases_all_settings() {
    restore_settings(ResolvedMode::Original {
        provider: ProviderRestore::KeepOpenai,
    })
    .await;
}

#[tokio::test]
async fn official_restores_and_releases_all_settings() {
    restore_settings(ResolvedMode::Official).await;
}

#[tokio::test]
async fn a_changed_recommended_setting_keeps_the_others() {
    let env = Env::new();
    env.write_config(OFFICIAL_CONFIG);
    let mut desired = pat(OVERSEAS);
    desired.settings.set("web_search", "cached").unwrap();
    let plan = env.engine.plan(&env.home, &desired).await.unwrap();
    let expected: Vec<_> = PAT_RECOMMENDED_KEYS
        .iter()
        .map(|(path, literal)| {
            (
                *path,
                if *path == "web_search" {
                    SettingValue::from("cached")
                } else {
                    (*literal).into()
                }
                .to_toml_repr(),
            )
        })
        .collect();
    let rows: Vec<_> = expected
        .iter()
        .map(|(path, repr)| (*path, None, Some(repr.as_str())))
        .collect();
    assert_setting_changes(&plan, &rows);
    env.engine.apply(&plan).await.unwrap();
    let config = env.config();
    assert_eq!(value(&config, "web_search"), Some("cached".into()));
    for (path, repr) in expected {
        let expected: toml::Table = toml::from_str(&format!("value = {repr}")).unwrap();
        assert_eq!(
            value(&config, path).as_ref(),
            expected.get("value"),
            "{path}"
        );
    }
}

#[tokio::test]
async fn array_and_inline_table_settings_round_trip_through_apply_and_release() {
    for with_notify in [false, true] {
        let env = Env::new();
        let original = format!(
            "{}{}",
            if with_notify {
                "notify = [\"original\"]\n"
            } else {
                ""
            },
            original_config(false, false)
        );
        env.write_config(&original);
        let mut desired = profile(AuthMode::ApiKey);
        desired
            .settings
            .set("notify", SettingValue::Array(vec!["a".into(), "b".into()]))
            .unwrap();
        desired
            .settings
            .set(
                "features.token_budget",
                SettingValue::Table(BTreeMap::from([("enabled".into(), true.into())])),
            )
            .unwrap();
        let plan = env.engine.plan(&env.home, &desired).await.unwrap();
        assert_setting_changes(
            &plan,
            &[
                (
                    "notify",
                    with_notify.then_some("[\"original\"]"),
                    Some("[\"a\", \"b\"]"),
                ),
                ("features.token_budget", None, Some("{ enabled = true }")),
            ],
        );
        env.engine.apply(&plan).await.unwrap();
        let config = env.config();
        assert_eq!(
            value(&config, "notify"),
            Some(toml::Value::Array(vec!["a".into(), "b".into()]))
        );
        assert_eq!(
            value(&config, "features.token_budget.enabled"),
            Some(true.into())
        );
        assert!(config.contains("token_budget = { enabled = true }"));

        desired.settings = CodexSettings::new();
        let plan = env.engine.plan(&env.home, &desired).await.unwrap();
        assert_setting_changes(
            &plan,
            &[
                (
                    "notify",
                    Some("[\"a\", \"b\"]"),
                    with_notify.then_some("[\"original\"]"),
                ),
                ("features.token_budget", Some("{ enabled = true }"), None),
            ],
        );
        env.engine.apply(&plan).await.unwrap();
        let config = env.config();
        assert_restored_settings(&config, &original, &["notify", "features.token_budget"]);
        assert_eq!(value(&config, "features.shell_tool"), Some(true.into()));
    }
}
