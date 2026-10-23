//! Mixed login (router mode with the user's own ChatGPT login): auth.json
//! keeps or gets back the official login, `openai_base_url` points at the
//! router, `chatgpt_base_url` is removed so every ChatGPT call (remote
//! control, Codex Cloud, accounts/check) goes to chatgpt.com with the real
//! account.

mod common;

use common::*;
use pretty_assertions::assert_eq;
use superba_config::toml_keys::ConfigDoc;
use superba_core::codex::config_keys as k;
use superba_core::{AuthMode, ConfigEngine, ResolvedMode, StashScope};

const ROUTER: &str = "http://127.0.0.1:47123";

fn routed(auth: AuthMode) -> superba_core::ResolvedProfile {
    let mut endpoints = superba_core::ResolvedEndpoints::for_base_url(
        &url::Url::parse(&format!("{ROUTER}/backend-api/codex")).unwrap(),
    )
    .unwrap();
    if auth == AuthMode::Pat {
        let mut chatgpt = endpoints.origin.join("/backend-api/");
        let _ = chatgpt.set_scheme("https");
        endpoints.chatgpt_base_url = Some(chatgpt);
        endpoints.authapi_origin = Some(endpoints.origin.clone());
    }
    let mut p = restore(ResolvedMode::Token {
        site_kind: superba_core::SiteKind::Krill,
        auth,
        endpoints,
        key: superba_core::Secret::new("sbr_router_token"),
    });
    p.profile_id = Some(superba_core::ProfileId::new("krill-pat"));
    p
}

fn router_pat() -> superba_core::ResolvedProfile {
    routed(AuthMode::Pat)
}

fn mixed() -> superba_core::ResolvedProfile {
    routed(AuthMode::MixedOfficial)
}

#[tokio::test]
async fn mixed_login_restores_the_stashed_login_and_drops_chatgpt_base_url() {
    let env = Env::new();
    env.write_config(OFFICIAL_CONFIG);
    let login = chatgpt_login("user@example.com", "rt-1", "2026-10-01T00:00:00Z");
    env.write_auth(&login);
    let e = &env.engine;
    // PAT router mode first: the login is stashed, chatgpt_base_url points at
    // the router.
    e.apply(&e.plan(&env.home, &router_pat()).await.unwrap())
        .await
        .unwrap();
    let doc = ConfigDoc::parse(env.config().as_bytes()).unwrap();
    assert!(doc.get_str(k::CHATGPT_BASE_URL).is_some());
    let stash = e
        .login_stash(&StashScope::Local(env.home.root().to_path_buf()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stash.account_id.as_deref(), Some("acct-1"));

    let plan = e.plan(&env.home, &mixed()).await.unwrap();
    assert_eq!(plan.auth_mode, Some(AuthMode::MixedOfficial));
    e.apply(&plan).await.unwrap();
    assert_eq!(env.auth().unwrap(), login, "the official login is back");
    let doc = ConfigDoc::parse(env.config().as_bytes()).unwrap();
    assert_eq!(doc.get_str(k::CHATGPT_BASE_URL), None);
    assert_eq!(
        doc.get_str(k::OPENAI_BASE_URL).as_deref(),
        Some("http://127.0.0.1:47123/backend-api/codex")
    );
    assert_eq!(doc.get_str(k::MODEL_PROVIDER).as_deref(), Some("openai"));
}

#[tokio::test]
async fn mixed_login_keeps_a_live_login_byte_for_byte() {
    let env = Env::new();
    let login = chatgpt_login("user@example.com", "rt-live", "2026-10-15T00:00:00Z");
    env.write_auth(&login);
    let e = &env.engine;
    let plan = e.plan(&env.home, &mixed()).await.unwrap();
    assert!(
        plan.auth_change.is_none(),
        "Codex's rotated tokens are kept"
    );
    e.apply(&plan).await.unwrap();
    assert_eq!(env.auth().unwrap(), login);
}

#[tokio::test]
async fn leaving_mixed_login_stashes_the_login_again() {
    let env = Env::new();
    let login = chatgpt_login("user@example.com", "rt-2", "2026-10-15T00:00:00Z");
    env.write_auth(&login);
    let e = &env.engine;
    e.apply(&e.plan(&env.home, &mixed()).await.unwrap())
        .await
        .unwrap();
    let plan = e.plan(&env.home, &router_pat()).await.unwrap();
    assert!(plan.auth_change.as_ref().unwrap().stash_login);
    e.apply(&plan).await.unwrap();
    assert_eq!(env.auth_json()["personal_access_token"], "sbr_router_token");
}

#[tokio::test]
async fn mixed_login_without_any_login_warns() {
    let env = Env::new();
    let e = &env.engine;
    let plan = e.plan(&env.home, &mixed()).await.unwrap();
    assert!(
        plan.notes
            .iter()
            .any(|n| n.text.key == "config.sign_in_required"
                || n.text.key.ends_with("sign_in_required")),
        "{:?}",
        plan.notes
    );
}
