//! Saved ChatGPT accounts (PR #20) through the service with the real config
//! engine: the overview lists them, a switch to another account is an
//! ordinary plan (a full app restart while the app runs, the chosen login
//! written, the outgoing one saved), this computer only.

#![allow(missing_docs)]

mod fakes;

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use fakes::*;
use superba_core::{
    ConfigEngine, Error, ExecuteOptions, FullRestartReason, NoEvents, OutcomeStatus, RestartKind,
    RestoreTarget, SafeDefaults, SecretBytes, StashScope, SwitchIntent, SwitchRequest,
    SwitchService, TargetId,
};
use superba_switch::{Switcher, Tuning};

fn login(email: &str, account: &str, refresh: &str) -> String {
    let enc = |v: serde_json::Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string().as_bytes())
    };
    let jwt = format!(
        "{}.{}.c2ln",
        enc(serde_json::json!({"alg": "RS256"})),
        enc(serde_json::json!({
            "email": email,
            "exp": 1_900_000_000,
            "https://api.openai.com/auth": {
                "chatgpt_account_id": account,
                "chatgpt_user_id": format!("user-{account}"),
                "chatgpt_plan_type": "plus"
            }
        }))
    );
    serde_json::to_string_pretty(&serde_json::json!({
        "OPENAI_API_KEY": null,
        "tokens": {"id_token": jwt, "access_token": format!("at-{refresh}"),
                   "refresh_token": refresh, "account_id": account},
        "last_refresh": "2026-10-01T00:00:00Z"
    }))
    .unwrap()
}

struct Setup {
    w: World,
    sw: Switcher,
    engine: Arc<superba_config::TomlConfigEngine>,
}

fn setup() -> Setup {
    let w = World::new();
    let engine = Arc::new(superba_config::TomlConfigEngine::new(w.ctx.clone()));
    let mut deps = w.deps();
    deps.config = engine.clone();
    let sw = Switcher::with_tuning(
        deps,
        Tuning {
            poll: Duration::from_millis(10),
            verify_timeout: Duration::from_millis(200),
            launch_verify_timeout: Duration::from_millis(200),
            stable: Duration::ZERO,
            reask_after: Duration::from_secs(60),
            plan_cache: 16,
            windows_app: false,
        },
    );
    std::fs::create_dir_all(w.ctx.codex_home().root()).unwrap();
    Setup { w, sw, engine }
}

fn auth_path(s: &Setup) -> std::path::PathBuf {
    s.w.ctx.codex_home().auth_json()
}

fn use_account(identity: &str) -> SwitchRequest {
    let mut r = SwitchRequest::profile(superba_core::ProfileId::new("official"));
    r.intent = SwitchIntent::Restore {
        target: RestoreTarget::Account {
            identity: identity.to_owned(),
        },
    };
    r
}

/// A live login A, and B added as an account; returns (A, B, id of A, id of B).
async fn two_accounts(s: &Setup) -> (String, String, String, String) {
    let a = login("a@example.com", "acct-a", "rt-a");
    let b = login("b@example.com", "acct-b", "rt-b");
    std::fs::write(auth_path(s), &a).unwrap();
    let id_a = s.sw.chatgpt_save_current().await.unwrap().id;
    let scope = StashScope::Local(s.w.ctx.codex_home().root().to_path_buf());
    let id_b = s
        .engine
        .save_login_account(&scope, &SecretBytes::new(b.as_bytes().to_vec()))
        .await
        .unwrap()
        .id;
    (a, b, id_a, id_b)
}

#[tokio::test]
async fn the_overview_lists_the_saved_accounts_and_the_one_in_use() {
    let s = setup();
    let (_, _, id_a, id_b) = two_accounts(&s).await;
    let o = s.sw.overview().await.unwrap();
    let ids: Vec<(&str, bool)> = o
        .chatgpt_accounts
        .iter()
        .map(|a| (a.id.as_str(), a.active))
        .collect();
    assert_eq!(ids, [(id_a.as_str(), true), (id_b.as_str(), false)]);
    let json = serde_json::to_string(&o).unwrap();
    assert!(
        !json.contains("rt-a") && !json.contains("at-rt"),
        "no tokens"
    );
}

#[tokio::test]
async fn another_account_restarts_the_running_app_and_is_written_after_the_preview() {
    let s = setup();
    let (a, b, id_a, id_b) = two_accounts(&s).await;
    s.w.set_session(Some(session(None, true)));
    let plan = s.sw.plan(use_account(&id_b)).await.unwrap();
    assert!(!plan.is_blocked(), "{plan:?}");
    assert_eq!(
        plan.primary,
        RestartKind::FullAppRestart {
            reason: FullRestartReason::AccountChanged
        }
    );
    assert_eq!(plan.to.label.key, "switch.destination.chatgpt_account");
    assert!(
        !serde_json::to_string(&plan).unwrap().contains("rt-b"),
        "the plan names the account, never its login"
    );
    // Planning writes nothing.
    assert_eq!(std::fs::read_to_string(auth_path(&s)).unwrap(), a);

    s.w.set_session(None);
    let plan = s.sw.plan(use_account(&id_b)).await.unwrap();
    assert_eq!(plan.primary, RestartKind::None);
    let out =
        s.sw.execute(
            plan,
            ExecuteOptions {
                turn_quiet_ms: 0,
                ..ExecuteOptions::default()
            },
            Arc::new(SafeDefaults),
            Arc::new(NoEvents),
        )
        .await
        .unwrap();
    assert!(
        matches!(
            out.status,
            OutcomeStatus::Switched | OutcomeStatus::WrittenOnly
        ),
        "{out:?}"
    );
    assert_eq!(std::fs::read_to_string(auth_path(&s)).unwrap(), b);
    let accounts = s.sw.chatgpt_accounts().await.unwrap();
    let active: Vec<&str> = accounts
        .iter()
        .filter(|x| x.active)
        .map(|x| x.id.as_str())
        .collect();
    assert_eq!(active, [id_b.as_str()]);
    // The same account again changes nothing; back to A restores A.
    let again = s.sw.plan(use_account(&id_b)).await.unwrap();
    assert!(
        again
            .config
            .as_ref()
            .is_none_or(|c| c.auth_change.is_none())
    );
    let back = s.sw.plan(use_account(&id_a)).await.unwrap();
    s.sw.execute(
        back,
        ExecuteOptions {
            turn_quiet_ms: 0,
            ..ExecuteOptions::default()
        },
        Arc::new(SafeDefaults),
        Arc::new(NoEvents),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(auth_path(&s)).unwrap(), a);
}

#[tokio::test]
async fn an_account_is_for_this_computer_only_and_must_be_saved() {
    let s = setup();
    let (_, _, _, id_b) = two_accounts(&s).await;
    let mut remote = use_account(&id_b);
    remote.targets.remotes = vec![TargetId::new("ssh:devbox")];
    assert!(matches!(
        s.sw.plan(remote).await,
        Err(Error::Invalid { .. })
    ));
    assert!(matches!(
        s.sw.plan(use_account("not-saved")).await,
        Err(Error::NotFound { .. })
    ));
}

#[tokio::test]
async fn the_account_in_use_cannot_be_removed() {
    let s = setup();
    let (_, _, id_a, id_b) = two_accounts(&s).await;
    assert!(matches!(
        s.sw.chatgpt_remove_account(&id_a).await,
        Err(Error::Conflict { .. })
    ));
    s.sw.chatgpt_remove_account(&id_b).await.unwrap();
    assert_eq!(s.sw.chatgpt_accounts().await.unwrap().len(), 1);
    assert!(matches!(
        s.sw.chatgpt_remove_account(&id_b).await,
        Err(Error::NotFound { .. })
    ));
}
