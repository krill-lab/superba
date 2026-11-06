//! Managed accounts reuse the official switch planner and remain local.
#![allow(missing_docs)]
mod fakes;

use fakes::{ScriptedDecider, World};
use std::sync::Arc;
use superba_core::{
    Decision, ExecuteOptions, NoEvents, OutcomeStatus, ProfileId, ResolvedMode, SecretBytes,
    SwitchRequest, SwitchService, TargetId, Timestamp, plan::CheckResult,
};

fn login(id: &str) -> SecretBytes {
    SecretBytes::new(serde_json::to_vec(&serde_json::json!({
        "auth_mode":"chatgpt", "tokens":{"account_id":id,"id_token":{"email":format!("{id}@example.test"),"sub":id},"access_token":"fake-access","refresh_token":"fake-refresh"}
    })).unwrap())
}

#[tokio::test]
async fn selected_chatgpt_account_is_resolved_into_the_official_plan_without_writes() {
    let world = World::new();
    let credential = login("selected");
    let store = superba_config::chatgpt::Accounts::new(world.ctx.clone());
    let account = store.put(&credential, Timestamp(1)).unwrap();
    let mut request = SwitchRequest::profile(ProfileId::new("official"));
    request.chatgpt_account_id = Some(account.id.clone());
    let plan = world.switcher().plan(request).await.unwrap();
    assert!(
        world.writes().is_empty(),
        "planning must not write the live account"
    );
    let mode = &plan.config.as_ref().unwrap().desired.as_ref().unwrap().mode;
    assert!(
        matches!(mode, ResolvedMode::ChatGptAccount { account_id, auth_json } if account_id == &account.id && auth_json == &credential)
    );
    assert_eq!(plan.to.profile_id.as_ref().unwrap().as_str(), "official");
    let serialized = serde_json::to_string(&plan).unwrap();
    assert!(!serialized.contains("fake-access") && !serialized.contains("fake-refresh"));
}

#[tokio::test]
async fn a_managed_login_cannot_be_used_as_an_api_key_or_sent_to_a_remote() {
    let world = World::new();
    let mut wrong_provider = SwitchRequest::profile(ProfileId::new("work"));
    wrong_provider.chatgpt_account_id = Some("a".into());
    assert!(world.switcher().plan(wrong_provider).await.is_err());
    let mut remote = SwitchRequest::profile(ProfileId::new("official"));
    remote.chatgpt_account_id = Some("a".into());
    remote.targets.remotes.push(TargetId::new("ssh:test"));
    assert!(world.switcher().plan(remote).await.is_err());
    assert!(world.writes().is_empty());
}

#[tokio::test(start_paused = true)]
async fn rotated_tokens_verify_only_for_the_exact_selected_chatgpt_account() {
    for live_id in ["selected", "different-account"] {
        let world = World::new();
        let credential = login("selected");
        let store = superba_config::chatgpt::Accounts::new(world.ctx.clone());
        let account = store.put(&credential, Timestamp(1)).unwrap();
        let mut request = SwitchRequest::profile(ProfileId::new("official"));
        request.chatgpt_account_id = Some(account.id);
        let switcher = world.switcher();
        let plan = switcher.plan(request).await.unwrap();

        // The fake reports a changed auth hash after apply. The real fixture
        // identifies whether this was token rotation or an unexpected account.
        let mut rotated: serde_json::Value =
            serde_json::from_slice(login(live_id).expose()).unwrap();
        rotated["tokens"]["access_token"] = "fake-rotated-access".into();
        std::fs::create_dir_all(world.ctx.codex_home().root()).unwrap();
        std::fs::write(
            world.ctx.codex_home().auth_json(),
            serde_json::to_vec(&rotated).unwrap(),
        )
        .unwrap();
        *world.config.tamper_after_apply.lock().unwrap() = true;
        let outcome = switcher
            .execute(
                plan,
                ExecuteOptions {
                    allow_full_restart: true,
                    ..ExecuteOptions::default()
                },
                ScriptedDecider::with(&[("verification_failed", Decision::KeepWritten)]),
                Arc::new(NoEvents),
            )
            .await
            .unwrap();
        let files = &outcome.verification.as_ref().unwrap().files;
        if live_id == "selected" {
            assert_eq!(outcome.status, OutcomeStatus::Switched, "{outcome:?}");
            assert_eq!(*files, CheckResult::Passed);
        } else {
            assert!(matches!(files, CheckResult::Failed { .. }), "{outcome:?}");
        }
    }
}
