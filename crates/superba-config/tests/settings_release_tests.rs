//! Releasing and re-taking Codex settings keeps the user's latest own value.

mod common;

use common::*;
use pretty_assertions::assert_eq;
use superba_config::toml_keys::ConfigDoc;
use superba_core::{
    BaselineExtras, CodexSettings, ConfigEngine, ProviderRestore, ResolvedMode, ResolvedProfile,
};

fn with_settings(mut p: ResolvedProfile, pairs: &[(&str, &str)]) -> ResolvedProfile {
    let mut s = CodexSettings::new();
    for (path, value) in pairs {
        s.set(path, *value).unwrap();
    }
    p.settings = s;
    p
}

fn value(env: &Env, path: &str) -> Option<String> {
    ConfigDoc::parse(env.config().as_bytes())
        .unwrap()
        .get_repr(path)
}

async fn apply(env: &Env, p: &ResolvedProfile) {
    let plan = env.engine.plan(&env.home, p).await.unwrap();
    env.engine.apply(&plan).await.unwrap();
}

#[tokio::test]
async fn a_retaken_setting_is_released_to_the_users_latest_value() {
    let env = Env::new();
    env.write_config("model_verbosity = \"low\"\n\n[desktop]\ntheme = \"dark\"\n");
    env.engine
        .snapshot_baseline(&env.home, BaselineExtras::default())
        .await
        .unwrap();

    // Superba takes the key over, then releases it: the original comes back.
    apply(
        &env,
        &with_settings(apikey(OVERSEAS), &[("model_verbosity", "high")]),
    )
    .await;
    assert_eq!(value(&env, "model_verbosity").as_deref(), Some("\"high\""));
    apply(&env, &with_settings(apikey(OVERSEAS), &[])).await;
    assert_eq!(value(&env, "model_verbosity").as_deref(), Some("\"low\""));

    // The user changes it while Superba does not own it.
    let edited = env.config().replace("\"low\"", "\"medium\"");
    env.write_config(&edited);

    // Taken over and released again: the user's latest value, not the first one.
    apply(
        &env,
        &with_settings(apikey(OVERSEAS), &[("model_verbosity", "high")]),
    )
    .await;
    assert_eq!(value(&env, "model_verbosity").as_deref(), Some("\"high\""));
    apply(&env, &with_settings(apikey(OVERSEAS), &[])).await;
    assert_eq!(
        value(&env, "model_verbosity").as_deref(),
        Some("\"medium\"")
    );

    // Restore original ends with it as well; the app's table is untouched.
    apply(
        &env,
        &with_settings(apikey(OVERSEAS), &[("model_verbosity", "high")]),
    )
    .await;
    apply(
        &env,
        &restore(ResolvedMode::Original {
            provider: ProviderRestore::KeepOpenai,
        }),
    )
    .await;
    assert_eq!(
        value(&env, "model_verbosity").as_deref(),
        Some("\"medium\"")
    );
    assert!(env.config().contains("[desktop]\ntheme = \"dark\""));
}

#[tokio::test]
async fn a_setting_absent_before_the_takeover_is_removed_at_every_release() {
    let env = Env::new();
    env.write_config("[desktop]\ntheme = \"dark\"\n");
    env.engine
        .snapshot_baseline(&env.home, BaselineExtras::default())
        .await
        .unwrap();
    apply(
        &env,
        &with_settings(apikey(OVERSEAS), &[("model_verbosity", "high")]),
    )
    .await;
    apply(&env, &with_settings(apikey(OVERSEAS), &[])).await;
    assert_eq!(value(&env, "model_verbosity"), None);
    apply(
        &env,
        &with_settings(apikey(OVERSEAS), &[("model_verbosity", "low")]),
    )
    .await;
    apply(&env, &with_settings(apikey(OVERSEAS), &[])).await;
    assert_eq!(value(&env, "model_verbosity"), None);
}
