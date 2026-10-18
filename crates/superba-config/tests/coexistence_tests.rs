//! GH#5 at the engine level: what cc-switch, AI Toolbox and Codex itself write
//! over Superba's keys (`common::tools`, reproduced from their sources) is seen
//! as drift of the owned keys and of the `auth.json` credential, a switch never
//! writes over it without an answer, restores keep it, a hand-off releases
//! everything Superba owns without touching what changed, and changes made
//! after a release are kept by a later restore too.

mod common;

use std::collections::BTreeMap;

use common::tools::{self, AITB_RELAY, CCS_KIMI};
use common::*;
use pretty_assertions::assert_eq;
use superba_config::toml_keys::ConfigDoc;
use superba_core::codex::config_keys as k;
use superba_core::{
    AUTH_JSON_PATH, AuthShape, ChangeSource, ConfigEngine, Error, KeyResolution, ProviderRestore,
    ResolvedMode, ResolvedProfile,
};

const USER_CONFIG: &str = r#"# my own settings
model = "gpt-5.5"

[mcp_servers.fetch]
command = "uvx"
args = ["mcp-server-fetch"]
"#;

fn get(text: &str, path: &str) -> Option<String> {
    ConfigDoc::parse(text.as_bytes()).unwrap().get_str(path)
}

fn with(mut p: ResolvedProfile, answers: &[(&str, KeyResolution)]) -> ResolvedProfile {
    p.key_resolutions = answers
        .iter()
        .map(|(p, r)| ((*p).to_owned(), *r))
        .collect::<BTreeMap<_, _>>();
    p
}

fn original() -> ResolvedProfile {
    restore(ResolvedMode::Original {
        provider: ProviderRestore::KeepOpenai,
    })
}

fn release(to: ChangeSource) -> ResolvedProfile {
    restore(ResolvedMode::Release { to })
}

/// The user's own config, taken over by a Krill PAT switch.
async fn superba_on_krill() -> Env {
    let env = Env::new();
    env.write_config(USER_CONFIG);
    let e = &env.engine;
    e.apply(&e.plan(&env.home, &pat(OVERSEAS)).await.unwrap())
        .await
        .unwrap();
    env
}

#[tokio::test]
async fn cc_switch_is_drift_of_the_routing_keys_and_a_removed_credential() {
    let env = superba_on_krill().await;
    let (config, auth) = tools::cc_switch(env.config_opt().as_deref(), &CCS_KIMI);
    env.write_config(&config);
    assert!(auth.is_none());
    std::fs::remove_file(env.auth_path()).unwrap();

    let st = env.engine.read_state(&env.home).await.unwrap();
    let paths: Vec<&str> = st.drift.iter().map(|d| d.path.as_str()).collect();
    assert!(paths.contains(&k::MODEL_PROVIDER), "{paths:?}");
    assert!(paths.contains(&k::OPENAI_BASE_URL), "{paths:?}");
    // cc-switch leaves chatgpt_base_url alone: still Superba's, no drift.
    assert!(!paths.contains(&k::CHATGPT_BASE_URL), "{paths:?}");
    let a = st.auth_drift.expect("the credential was removed");
    assert!(matches!(a.written, AuthShape::Pat { .. }));
    assert_eq!(a.live, AuthShape::Missing);

    // A switch does not write over it without an answer (also not when only
    // the credential is unanswered).
    let e = &env.engine;
    assert!(matches!(
        e.plan(&env.home, &pat(OVERSEAS)).await,
        Err(Error::Conflict { .. })
    ));
    let mut answers: Vec<(&str, KeyResolution)> = paths
        .iter()
        .map(|p| (*p, KeyResolution::Overwrite))
        .collect();
    assert!(matches!(
        e.plan(&env.home, &with(pat(OVERSEAS), &answers)).await,
        Err(Error::Conflict { message }) if message.contains(AUTH_JSON_PATH)
    ));
    // "Keep" is no answer for a switch's credential.
    answers.push((AUTH_JSON_PATH, KeyResolution::KeepCurrent));
    assert!(
        e.plan(&env.home, &with(pat(OVERSEAS), &answers))
            .await
            .is_err()
    );
    // Taking it over writes Superba's state again.
    answers.pop();
    answers.push((AUTH_JSON_PATH, KeyResolution::Overwrite));
    let plan = e
        .plan(&env.home, &with(pat(OVERSEAS), &answers))
        .await
        .unwrap();
    e.apply(&plan).await.unwrap();
    assert_eq!(
        get(&env.config(), k::MODEL_PROVIDER).as_deref(),
        Some("openai")
    );
    assert!(env.auth().unwrap().contains(PAT_KEY));
    let st = e.read_state(&env.home).await.unwrap();
    assert!(st.drift.is_empty() && st.auth_drift.is_none());
}

#[tokio::test]
async fn ai_toolbox_merging_its_key_is_a_credential_change_a_reformat_is_not() {
    let env = superba_on_krill().await;
    let pat_form = env.auth().unwrap();
    // Codex (or anyone) rewriting the same credential differently: no change.
    let value: serde_json::Value = serde_json::from_str(&pat_form).unwrap();
    env.write_auth(&format!("{}\n", serde_json::to_string(&value).unwrap()));
    let st = env.engine.read_state(&env.home).await.unwrap();
    assert!(st.auth_drift.is_none());

    let (config, auth) = tools::ai_toolbox(
        env.config_opt().as_deref(),
        env.auth().as_deref(),
        &AITB_RELAY,
    );
    env.write_config(&config);
    env.write_auth(&auth);
    let st = env.engine.read_state(&env.home).await.unwrap();
    // The PAT stays in the file, but Codex now uses AI Toolbox's API key.
    let a = st.auth_drift.expect("AI Toolbox merged its key");
    assert_eq!(
        a.live,
        AuthShape::ApiKey {
            key_last4: "7e1b".into()
        }
    );
    let paths: Vec<&str> = st.drift.iter().map(|d| d.path.as_str()).collect();
    assert_eq!(paths, [k::MODEL_PROVIDER]);
}

#[tokio::test]
async fn a_restore_puts_back_only_what_superba_still_owns() {
    let env = superba_on_krill().await;
    let (config, auth) = tools::ai_toolbox(
        env.config_opt().as_deref(),
        env.auth().as_deref(),
        &AITB_RELAY,
    );
    env.write_config(&config);
    env.write_auth(&auth);
    let e = &env.engine;
    // Unanswered drift of a key is still a question at the engine level (the
    // planner answers "keep" for restores); the credential is kept by default.
    assert!(matches!(
        e.plan(&env.home, &original()).await,
        Err(Error::Conflict { .. })
    ));
    let plan = e
        .plan(
            &env.home,
            &with(
                original(),
                &[(k::MODEL_PROVIDER, KeyResolution::KeepCurrent)],
            ),
        )
        .await
        .unwrap();
    assert!(
        plan.notes
            .iter()
            .any(|n| n.text.key == "config.note.auth_kept"),
        "{:?}",
        plan.notes
    );
    e.apply(&plan).await.unwrap();
    let text = env.config();
    // AI Toolbox's provider, its table, its marker and its credential stay.
    assert_eq!(get(&text, k::MODEL_PROVIDER).as_deref(), Some("custom"));
    assert!(text.starts_with("#:schema none\n"), "{text}");
    assert_eq!(
        get(&text, "model_providers.custom.base_url").as_deref(),
        Some(AITB_RELAY.base_url)
    );
    assert_eq!(env.auth().unwrap(), auth);
    // Superba's own leftovers are gone (they were not there before it).
    assert_eq!(get(&text, k::OPENAI_BASE_URL), None);
    assert_eq!(get(&text, k::CHATGPT_BASE_URL), None);
    assert!(text.contains("[mcp_servers.fetch]"));
    let st = e.read_state(&env.home).await.unwrap();
    assert!(st.drift.is_empty() && st.auth_drift.is_none());
}

#[tokio::test]
async fn a_hand_off_releases_everything_and_is_recorded_with_its_backups() {
    let env = superba_on_krill().await;
    let (config, _) = tools::cc_switch(env.config_opt().as_deref(), &CCS_KIMI);
    env.write_config(&config);
    std::fs::remove_file(env.auth_path()).unwrap();
    let e = &env.engine;
    let to = ChangeSource::CcSwitch {
        provider: Some("Kimi".into()),
    };
    let answers = [
        (k::MODEL_PROVIDER, KeyResolution::KeepCurrent),
        (k::OPENAI_BASE_URL, KeyResolution::KeepCurrent),
    ];
    let plan = e
        .plan(&env.home, &with(release(to.clone()), &answers))
        .await
        .unwrap();
    assert!(plan.releases_ownership && !plan.is_noop());
    let report = e.apply(&plan).await.unwrap();
    let text = env.config();
    // cc-switch's state is untouched; Superba's leftover chatgpt_base_url (and
    // the recommended keys it owned) went back to how the user had them.
    assert_eq!(get(&text, k::MODEL_PROVIDER).as_deref(), Some("custom"));
    assert_eq!(get(&text, "model").as_deref(), Some("kimi-k3"));
    assert_eq!(get(&text, k::CHATGPT_BASE_URL), None);
    assert!(env.auth().is_none());
    let st = e.read_state(&env.home).await.unwrap();
    let h = st.handed_off.expect("recorded");
    assert_eq!(h.to, to);
    assert_eq!(h.generation, report.generation);
    assert!(st.drift.is_empty() && st.auth_drift.is_none());
    // The generation before the hand-off is what Superba had written; it can
    // be restored point in time.
    let generations = e.generations(&env.home).await.unwrap();
    assert_eq!(generations.len(), 2);

    // cc-switch switches again later: not a conflict (Superba owns nothing),
    // but a restore keeps it unless taken over.
    let again = text.replace("kimi-k3", "kimi-k2.7-code");
    env.write_config(&again);
    let st = e.read_state(&env.home).await.unwrap();
    assert!(st.drift.is_empty());
    assert_eq!(
        st.since_release
            .iter()
            .map(|c| c.path.as_str())
            .collect::<Vec<_>>(),
        ["model"]
    );
    let plan = e
        .plan(
            &env.home,
            &with(original(), &[("model", KeyResolution::KeepCurrent)]),
        )
        .await
        .unwrap();
    e.apply(&plan).await.unwrap();
    assert_eq!(
        get(&env.config(), "model").as_deref(),
        Some("kimi-k2.7-code")
    );
}

#[tokio::test]
async fn a_restore_whose_changes_were_all_made_elsewhere_still_releases_ownership() {
    let env = superba_on_krill().await;
    let e = &env.engine;
    // The user's own model stays in the file; the app's model picker changes it.
    let picked = tools::codex_model_picker(&env.config(), "gpt-5.6-sol");
    env.write_config(&picked);
    // Superba does not own `model` (the profile sets none): no drift.
    let st = e.read_state(&env.home).await.unwrap();
    assert!(st.drift.is_empty());

    // Everything Superba owns was changed by hand: restoring keeps all of it
    // and writes nothing, but Superba no longer owns anything afterwards.
    let mut doc = ConfigDoc::parse(picked.as_bytes()).unwrap();
    assert!(st.active.is_some());
    let mut owned = vec![
        k::MODEL_PROVIDER.to_owned(),
        k::OPENAI_BASE_URL.to_owned(),
        k::CHATGPT_BASE_URL.to_owned(),
    ];
    owned.extend(
        superba_core::CodexSettings::krill_recommended()
            .paths()
            .map(str::to_owned),
    );
    for p in &owned {
        doc.remove(p);
    }
    let hand = String::from_utf8(doc.to_bytes()).unwrap();
    env.write_config(&hand);
    env.write_auth("{\n  \"OPENAI_API_KEY\": \"sk-own-key-0000\"\n}");
    let st = e.read_state(&env.home).await.unwrap();
    let answers: Vec<(&str, KeyResolution)> = st
        .drift
        .iter()
        .map(|d| (d.path.as_str(), KeyResolution::KeepCurrent))
        .collect();
    let plan = e
        .plan(&env.home, &with(original(), &answers))
        .await
        .unwrap();
    assert!(plan.writes_no_files(), "{:?}", plan.key_changes);
    assert!(plan.releases_ownership);
    e.apply(&plan).await.unwrap();
    assert_eq!(env.config(), hand);
    let st = e.read_state(&env.home).await.unwrap();
    assert!(st.drift.is_empty() && st.auth_drift.is_none());
}

#[tokio::test]
async fn codex_signing_in_is_a_credential_change_that_a_switch_stashes() {
    let env = superba_on_krill().await;
    let login = chatgpt_login("user@example.com", "rt-1", "2026-10-15T00:00:00Z");
    env.write_auth(&login);
    let e = &env.engine;
    let st = e.read_state(&env.home).await.unwrap();
    assert!(matches!(
        st.auth_drift.as_ref().map(|a| &a.live),
        Some(AuthShape::ChatGpt { .. })
    ));
    // A switch replaces a login after stashing it (no question).
    let plan = e.plan(&env.home, &apikey(OVERSEAS)).await.unwrap();
    assert!(plan.auth_change.as_ref().is_some_and(|c| c.stash_login));
}

#[tokio::test]
async fn a_hand_off_when_superba_owns_nothing_writes_nothing() {
    let env = superba_on_krill().await;
    let e = &env.engine;
    // Back to the official login: Superba owns nothing any more.
    e.apply(
        &e.plan(&env.home, &restore(ResolvedMode::Official))
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    // AI Toolbox takes Codex over afterwards.
    let (config, auth) = tools::ai_toolbox(
        env.config_opt().as_deref(),
        env.auth().as_deref(),
        &AITB_RELAY,
    );
    env.write_config(&config);
    env.write_auth(&auth);
    let plan = e
        .plan(
            &env.home,
            &release(ChangeSource::AiToolbox { provider: None }),
        )
        .await
        .unwrap();
    assert!(plan.writes_no_files(), "{:?}", plan.key_changes);
    e.apply(&plan).await.unwrap();
    assert_eq!((env.config(), env.auth().unwrap()), (config, auth));
    assert!(e.read_state(&env.home).await.unwrap().handed_off.is_some());
}
