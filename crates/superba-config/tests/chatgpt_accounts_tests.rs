//! Saved ChatGPT accounts (PR #20) on the login stash: back-compat with stashes
//! written before accounts, encryption, exact switching between accounts,
//! the stash keeping the newest tokens and the compare-and-swap of tokens a
//! limits read rotated.

mod common;

use base64::Engine as _;
use common::*;
use superba_core::{ConfigEngine, Error, ResolvedMode, SecretBytes, StashScope};

/// A ChatGPT login for `email` in workspace `account` (Codex's file layout),
/// with its refresh token, `last_refresh` and plan claim.
fn login(email: &str, account: &str, refresh: &str, last_refresh: &str, plan: &str) -> String {
    let enc = |v: serde_json::Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string().as_bytes())
    };
    let jwt = format!(
        "{}.{}.c2ln",
        enc(serde_json::json!({"alg": "RS256", "typ": "JWT"})),
        enc(serde_json::json!({
            "email": email,
            "exp": 1_800_000_000,
            "https://api.openai.com/auth": {
                "chatgpt_account_id": account,
                "chatgpt_user_id": format!("user-{account}"),
                "chatgpt_plan_type": plan
            }
        }))
    );
    serde_json::to_string_pretty(&serde_json::json!({
        "OPENAI_API_KEY": null,
        "tokens": {
            "id_token": jwt,
            "access_token": format!("access-{refresh}"),
            "refresh_token": refresh,
            "account_id": account
        },
        "last_refresh": last_refresh
    }))
    .unwrap()
}

fn scope(env: &Env) -> StashScope {
    StashScope::Local(env.home.root().to_path_buf())
}

fn secret(s: &str) -> SecretBytes {
    SecretBytes::new(s.as_bytes().to_vec())
}

fn account(identity: &str) -> superba_core::ResolvedProfile {
    restore(ResolvedMode::ChatGptAccount {
        identity: identity.to_owned(),
    })
}

fn files_under(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(files_under(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

#[tokio::test]
async fn a_login_stashed_before_accounts_existed_reads_as_one_account() {
    let env = Env::new();
    let a = login(
        "a@example.com",
        "acct-a",
        "rt-a",
        "2026-10-01T00:00:00Z",
        "plus",
    );
    env.write_auth(&a);
    let e = &env.engine;
    e.apply(&e.plan(&env.home, &pat(OVERSEAS)).await.unwrap())
        .await
        .unwrap();
    // Rewrite the index as 0.1.1 wrote it: no plan, no first-save time.
    let index = files_under(&env.data().join("backups/login-stash"))
        .into_iter()
        .find(|p| p.ends_with("index.json"))
        .unwrap();
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&index).unwrap()).unwrap();
    let stashed_at = {
        let entries = json["entries"].as_object_mut().unwrap();
        assert_eq!(entries.len(), 1);
        let entry = entries
            .values_mut()
            .next()
            .unwrap()
            .as_object_mut()
            .unwrap();
        entry.remove("plan_type");
        entry.remove("added_at");
        entry["stashed_at"].clone()
    };
    std::fs::write(&index, serde_json::to_vec_pretty(&json).unwrap()).unwrap();

    let accounts = e.login_accounts(&scope(&env)).await.unwrap();
    assert_eq!(accounts.len(), 1, "{accounts:?}");
    let only = &accounts[0];
    assert_eq!(only.email, "a@example.com");
    assert!(only.latest && !only.active);
    assert_eq!(serde_json::to_value(only.added_at).unwrap(), stashed_at);
    assert_eq!(only.plan_type, None, "old entries carry no plan claim");
    // A plain restore of the official login still brings it back.
    e.apply(
        &e.plan(&env.home, &restore(ResolvedMode::Official))
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(env.auth().unwrap(), a);
    let accounts = e.login_accounts(&scope(&env)).await.unwrap();
    assert!(accounts[0].active);
    assert_eq!(accounts[0].plan_type.as_deref(), Some("plus"));
}

#[tokio::test]
async fn accounts_are_encrypted_and_switched_exactly() {
    let env = Env::new();
    let a = login(
        "a@example.com",
        "acct-a",
        "rt-secret-a",
        "2026-10-01T00:00:00Z",
        "plus",
    );
    let b = login(
        "b@example.com",
        "acct-b",
        "rt-secret-b",
        "2026-10-02T00:00:00Z",
        "pro",
    );
    env.write_auth(&a);
    let e = &env.engine;
    // A switch to a key stashes A; B is added as an account meanwhile.
    e.apply(&e.plan(&env.home, &pat(OVERSEAS)).await.unwrap())
        .await
        .unwrap();
    env.clock.advance(std::time::Duration::from_secs(60));
    let added = e
        .save_login_account(&scope(&env), &secret(&b))
        .await
        .unwrap();
    assert_eq!(added.email, "b@example.com");
    assert_eq!(added.plan_type.as_deref(), Some("pro"));
    assert!(
        !added.latest,
        "adding an account keeps the login a restore picks"
    );
    let list = e.login_accounts(&scope(&env)).await.unwrap();
    assert_eq!(
        list.iter().map(|a| a.email.as_str()).collect::<Vec<_>>(),
        ["a@example.com", "b@example.com"],
        "oldest first"
    );
    let (id_a, id_b) = (list[0].id.clone(), list[1].id.clone());
    assert!(list[0].latest && !list[1].latest);
    for file in files_under(&env.data().join("backups/login-stash")) {
        let bytes = String::from_utf8_lossy(&std::fs::read(&file).unwrap()).into_owned();
        assert!(
            !bytes.contains("rt-secret") && !bytes.contains("access-rt"),
            "{}",
            file.display()
        );
    }
    let public = serde_json::to_string(&list).unwrap();
    assert!(!public.contains("rt-secret") && !public.contains("access-"));

    // The plan names only the identity; B is written.
    let plan = e.plan(&env.home, &account(&id_b)).await.unwrap();
    assert!(
        !serde_json::to_string(&plan).unwrap().contains("rt-secret"),
        "a plan never carries the login"
    );
    e.apply(&plan).await.unwrap();
    assert_eq!(env.auth().unwrap(), b);
    assert!(
        !env.config_opt()
            .unwrap_or_default()
            .contains("openai_base_url"),
        "the official login writes no routing keys"
    );

    // B -> A: B (possibly refreshed by Codex) is stashed first.
    let b_refreshed = login(
        "b@example.com",
        "acct-b",
        "rt-secret-b2",
        "2026-10-05T00:00:00Z",
        "pro",
    );
    env.write_auth(&b_refreshed);
    let plan = e.plan(&env.home, &account(&id_a)).await.unwrap();
    assert!(plan.auth_change.as_ref().unwrap().stash_login);
    e.apply(&plan).await.unwrap();
    assert_eq!(env.auth().unwrap(), a);
    assert_eq!(
        e.load_login_account(&scope(&env), &id_b)
            .await
            .unwrap()
            .unwrap()
            .expose(),
        b_refreshed.as_bytes()
    );
    // The live login of the chosen account is kept as is.
    let plan = e.plan(&env.home, &account(&id_a)).await.unwrap();
    assert!(plan.auth_change.is_none());
    // An unknown identity is refused, never replaced by another login.
    assert!(matches!(
        e.plan(&env.home, &account("not-saved")).await,
        Err(Error::NotFound { .. })
    ));
}

#[tokio::test]
async fn stashing_an_older_copy_keeps_the_newer_saved_tokens() {
    let env = Env::new();
    let old = login(
        "b@example.com",
        "acct-b",
        "rt-old",
        "2026-10-01T00:00:00Z",
        "pro",
    );
    let rotated = login(
        "b@example.com",
        "acct-b",
        "rt-rotated",
        "2026-10-09T00:00:00Z",
        "pro",
    );
    let e = &env.engine;
    let saved = e
        .save_login_account(&scope(&env), &secret(&old))
        .await
        .unwrap();
    // A limits read rotated the saved copy; the live file still has the old one.
    e.store_rotated_login(&scope(&env), &secret(&old), &secret(&rotated))
        .await
        .unwrap();
    env.write_auth(&old);
    e.apply(&e.plan(&env.home, &pat(OVERSEAS)).await.unwrap())
        .await
        .unwrap();
    assert_eq!(
        e.load_login_account(&scope(&env), &saved.id)
            .await
            .unwrap()
            .unwrap()
            .expose(),
        rotated.as_bytes(),
        "the switch away must not put the dead refresh token back"
    );
}

#[tokio::test]
async fn rotated_tokens_replace_only_what_still_holds_the_old_ones() {
    let env = Env::new();
    let before = login(
        "a@example.com",
        "acct-a",
        "rt-1",
        "2026-10-01T00:00:00Z",
        "plus",
    );
    let after = login(
        "a@example.com",
        "acct-a",
        "rt-2",
        "2026-10-02T00:00:00Z",
        "plus",
    );
    let newer = login(
        "a@example.com",
        "acct-a",
        "rt-3",
        "2026-10-03T00:00:00Z",
        "plus",
    );
    let other = login(
        "b@example.com",
        "acct-b",
        "rt-b",
        "2026-10-02T00:00:00Z",
        "plus",
    );
    let e = &env.engine;
    let saved = e
        .save_login_account(&scope(&env), &secret(&before))
        .await
        .unwrap();
    env.write_auth(&before);
    e.store_rotated_login(&scope(&env), &secret(&before), &secret(&after))
        .await
        .unwrap();
    assert_eq!(
        env.auth().unwrap(),
        after,
        "the live copy got the new tokens"
    );
    let load = || async {
        e.load_login_account(&scope(&env), &saved.id)
            .await
            .unwrap()
            .unwrap()
    };
    assert_eq!(load().await.expose(), after.as_bytes());

    // Codex refreshed the live file meanwhile, and a newer copy was saved: a
    // late result of a read that started from `before` changes neither.
    env.write_auth(&newer);
    e.save_login_account(&scope(&env), &secret(&newer))
        .await
        .unwrap();
    e.store_rotated_login(&scope(&env), &secret(&before), &secret(&after))
        .await
        .unwrap();
    assert_eq!(env.auth().unwrap(), newer);
    assert_eq!(load().await.expose(), newer.as_bytes());

    // Another identity is refused and changes nothing.
    assert!(
        e.store_rotated_login(&scope(&env), &secret(&newer), &secret(&other))
            .await
            .is_err()
    );
    assert_eq!(env.auth().unwrap(), newer);
    assert_eq!(e.login_accounts(&scope(&env)).await.unwrap().len(), 1);
}

#[tokio::test]
async fn the_account_in_use_cannot_be_removed_and_others_can() {
    let env = Env::new();
    let a = login(
        "a@example.com",
        "acct-a",
        "rt-a",
        "2026-10-01T00:00:00Z",
        "plus",
    );
    let b = login(
        "b@example.com",
        "acct-b",
        "rt-b",
        "2026-10-01T00:00:00Z",
        "plus",
    );
    let e = &env.engine;
    let id_a = e
        .save_login_account(&scope(&env), &secret(&a))
        .await
        .unwrap()
        .id;
    let id_b = e
        .save_login_account(&scope(&env), &secret(&b))
        .await
        .unwrap()
        .id;
    env.write_auth(&a);
    assert!(matches!(
        e.remove_login_account(&scope(&env), &id_a).await,
        Err(Error::Conflict { .. })
    ));
    assert!(e.remove_login_account(&scope(&env), &id_b).await.unwrap());
    let list = e.login_accounts(&scope(&env)).await.unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].active && list[0].latest);
    assert!(
        e.save_login_account(&scope(&env), &secret(r#"{"tokens":{}}"#))
            .await
            .is_err(),
        "a login without an identity is not an account"
    );
}

#[tokio::test]
async fn choosing_a_signed_out_login_again_brings_it_back() {
    let env = Env::new();
    let a = login(
        "a@example.com",
        "acct-a",
        "rt-a",
        "2026-10-01T00:00:00Z",
        "plus",
    );
    let e = &env.engine;
    env.write_auth(&a);
    // Superba leaves A in place (an official restore keeps the live login).
    e.apply(
        &e.plan(&env.home, &restore(ResolvedMode::Official))
            .await
            .unwrap(),
    )
    .await
    .unwrap();
    // The user signs out in the app, then adds A again and picks it.
    std::fs::remove_file(env.auth_path()).unwrap();
    let id = e
        .save_login_account(&scope(&env), &secret(&a))
        .await
        .unwrap()
        .id;
    e.apply(&e.plan(&env.home, &account(&id)).await.unwrap())
        .await
        .unwrap();
    assert_eq!(env.auth().unwrap(), a);
    assert_eq!(e.login_accounts(&scope(&env)).await.unwrap().len(), 1);
}
