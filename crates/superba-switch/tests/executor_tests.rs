//! `Switcher::execute` over in-memory fakes: the plan 7.2 execution states and the
//! plan 7.6 failure rules — turn waits (cancel = zero writes), the pre-IPC
//! re-check, IPC failure offers, verification recovery, write failures with
//! rollback, full restarts with force-quit and consumer closing, session
//! migration, remote hosts, journal and lock.

#![allow(missing_docs)]

mod fakes;

use std::sync::Arc;
use std::time::Duration;

use fakes::*;
use pretty_assertions::assert_eq;
use superba_core::{
    AuthApiHealth, CodexProcessKind, Decision, DecisionRequest, Error, ExecuteOptions,
    FullRestartReason, KeyChange, LegacySessionChoice, NoEvents, OutcomeStatus, ProfileId,
    QuitOutcome, RendererState, RestartKind, RestoreTarget, SwitchEvent, SwitchIntent,
    SwitchOutcome, SwitchPhase, SwitchPlan, SwitchRequest, SwitchService, TargetId, ThreadRef,
    plan::CheckResult,
};
use superba_switch::Switcher;
use superba_switch::journal::{JournalStatus, StateLock, SwitchJournal};

fn req(profile: &str) -> SwitchRequest {
    SwitchRequest::profile(ProfileId::new(profile))
}

fn allow_full() -> ExecuteOptions {
    ExecuteOptions {
        allow_full_restart: true,
        ..ExecuteOptions::default()
    }
}

async fn plan_with(sw: &Switcher, r: SwitchRequest) -> SwitchPlan {
    sw.plan(r).await.expect("plan")
}

async fn run(
    w: &World,
    r: SwitchRequest,
    opts: ExecuteOptions,
    decider: Arc<ScriptedDecider>,
) -> (SwitchOutcome, Arc<RecordingEvents>) {
    let sw = w.switcher();
    let plan = plan_with(&sw, r).await;
    let events = Arc::new(RecordingEvents::default());
    let outcome = sw
        .execute(plan, opts, decider, events.clone())
        .await
        .expect("execute");
    (outcome, events)
}

fn notice_keys(o: &SwitchOutcome) -> Vec<String> {
    o.notices.iter().map(|n| n.text.key.clone()).collect()
}

fn journal_status(w: &World) -> Vec<JournalStatus> {
    SwitchJournal::new(&w.ctx.data_dir)
        .entries()
        .unwrap()
        .into_iter()
        .map(|e| e.status)
        .collect()
}

// --- hot path ---------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn hot_switch_writes_then_restarts_then_verifies() {
    let w = World::new();
    let (o, ev) = run(
        &w,
        req("work"),
        ExecuteOptions::default(),
        ScriptedDecider::with(&[]),
    )
    .await;
    assert_eq!(o.status, OutcomeStatus::Switched, "{:?}", o.notices);
    assert_eq!(
        w.writes(),
        vec!["config.apply", "env.apply", "app.hot_restart local"]
    );
    assert!(o.restart.is_hot());
    let v = o.verification.as_ref().unwrap();
    assert_eq!(v.process, CheckResult::Passed);
    assert_eq!(v.files, CheckResult::Passed);
    assert_eq!(v.auth, CheckResult::Passed);
    assert_eq!(v.line, CheckResult::Passed);
    assert_eq!(v.renderer, CheckResult::Passed);
    assert_eq!(o.generation, Some(1));
    assert_eq!(
        ev.phases(),
        vec![
            SwitchPhase::TurnCheck,
            SwitchPhase::Applying,
            SwitchPhase::Restarting,
            SwitchPhase::Verifying,
            SwitchPhase::Done
        ]
    );
    assert!(notice_keys(&o).contains(&"switch.done.switched".to_owned()));
    // The journal entry is finished and the selection remembered.
    assert_eq!(journal_status(&w), vec![JournalStatus::Done]);
    assert_eq!(
        w.profiles.set.lock().unwrap().last_selected,
        Some(ProfileId::new("work"))
    );
    // The lock is released afterwards.
    StateLock::acquire(&w.ctx.data_dir).unwrap();
}

#[tokio::test(start_paused = true)]
async fn first_takeover_snapshots_the_baseline_before_writing() {
    let w = World::new();
    *w.config.baseline.lock().unwrap() = None;
    let (o, _) = run(
        &w,
        req("work"),
        ExecuteOptions::default(),
        ScriptedDecider::with(&[]),
    )
    .await;
    assert_eq!(o.status, OutcomeStatus::Switched);
    assert_eq!(
        w.writes()[..2],
        [
            "config.snapshot_baseline".to_owned(),
            "config.apply".to_owned()
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn released_baseline_captures_fresh_env_legacy_lines_and_sessions_before_takeover() {
    for auth in [superba_core::AuthMode::Pat, superba_core::AuthMode::ApiKey] {
        let w = World::new();
        w.set_session(None);
        w.config.baseline.lock().unwrap().as_mut().unwrap().released = true;
        let location = superba_core::EnvLocation::ShellRc {
            path: w.ctx.home_dir.join(".zshenv"),
            shell: superba_core::ShellKind::Zsh,
        };
        let legacy_line = superba_core::RcLine {
            path: w.ctx.home_dir.join(".zshrc"),
            line_no: 7,
            text: format!("export CODEX_AUTHAPI_BASE_URL={CN_FAST}"),
            value: CN_FAST.into(),
        };
        {
            let mut env = w.env.report.lock().unwrap();
            env.persisted = vec![superba_core::PersistedValue {
                location: location.clone(),
                value: Some(CN_FAST.into()),
                managed: false,
            }];
            env.legacy_lines = vec![legacy_line.clone()];
        }
        *w.sessions.report.lock().unwrap() =
            session_report(w.ctx.codex_home().root(), &[("openai", 9), ("custom", 2)]);
        let mut request = req("work");
        request.intent = SwitchIntent::Profile {
            profile_id: "work".into(),
            line_id: None,
            auth_mode: Some(auth),
        };
        let sw = w.switcher();
        let p = plan_with(&sw, request).await;
        assert!(
            w.writes().is_empty(),
            "planning must not replace the released baseline"
        );
        assert!(w.config.baseline.lock().unwrap().as_ref().unwrap().released);
        let o = sw
            .execute(
                p,
                ExecuteOptions::default(),
                ScriptedDecider::with(&[]),
                Arc::new(NoEvents),
            )
            .await
            .unwrap();
        assert_eq!(o.status, OutcomeStatus::Switched);
        let baseline = w.config.baseline.lock().unwrap().clone().unwrap();
        assert!(!baseline.released);
        assert_eq!(baseline.captured_at, superba_core::Timestamp(T0));
        assert_eq!(
            baseline.env,
            vec![superba_core::EnvSnapshot {
                location,
                value: Some(CN_FAST.into())
            }]
        );
        assert_eq!(baseline.legacy_env_lines, vec![legacy_line]);
        assert_eq!(
            baseline.session_distribution,
            std::collections::BTreeMap::from([("openai".into(), 9), ("custom".into(), 2)])
        );
        assert_eq!(
            &w.writes()[..2],
            &["config.snapshot_baseline", "config.apply"]
        );
    }
}

#[tokio::test(start_paused = true)]
async fn restoring_a_released_original_or_official_keeps_the_saved_baseline() {
    for target in [
        RestoreTarget::Original {
            provider: superba_core::ProviderRestore::KeepOpenai,
        },
        RestoreTarget::Official,
    ] {
        let w = World::new();
        w.set_session(None);
        w.config.baseline.lock().unwrap().as_mut().unwrap().released = true;
        let saved = w.config.baseline.lock().unwrap().clone();
        let mut r = req("work");
        r.intent = SwitchIntent::Restore { target };
        let (o, _) = run(&w, r, ExecuteOptions::default(), ScriptedDecider::with(&[])).await;
        assert_eq!(o.status, OutcomeStatus::Switched);
        assert!(!w.calls().contains(&"config.snapshot_baseline".into()));
        assert_eq!(*w.config.baseline.lock().unwrap(), saved);
    }
}

#[tokio::test(start_paused = true)]
async fn restored_pat_generation_is_verified_with_its_saved_credentials() {
    let w = World::new();
    w.config.restore_token(superba_core::AuthMode::Pat);
    let mut r = req("work");
    r.intent = SwitchIntent::Restore {
        target: RestoreTarget::Generation { generation: 1 },
    };
    let sw = w.switcher();
    let p = plan_with(&sw, r).await;
    assert!(p.primary.is_hot());
    w.prober.authapi.lock().unwrap().insert(
        OVERSEAS.into(),
        AuthApiHealth::Unhealthy {
            reason: "expired".into(),
        },
    );
    let o = sw
        .execute(
            p,
            ExecuteOptions::default(),
            ScriptedDecider::with(&[("verification_failed", Decision::KeepWritten)]),
            Arc::new(NoEvents),
        )
        .await
        .unwrap();
    assert!(matches!(
        o.verification.as_ref().unwrap().auth,
        CheckResult::Failed { .. }
    ));
    assert!(
        w.prober.authapi_checks.lock().unwrap().len() >= 2,
        "both planning and execution must check whoami"
    );
}

#[tokio::test(start_paused = true)]
async fn restored_key_generation_on_the_login_page_fails_verification() {
    for auth in [superba_core::AuthMode::Pat, superba_core::AuthMode::ApiKey] {
        let w = World::new();
        w.config.restore_token(auth);
        w.app.renderer.lock().unwrap().on_login_page = true;
        let mut r = req("work");
        r.intent = SwitchIntent::Restore {
            target: RestoreTarget::Generation { generation: 1 },
        };
        let (o, _) = run(
            &w,
            r,
            ExecuteOptions::default(),
            ScriptedDecider::with(&[("verification_failed", Decision::KeepWritten)]),
        )
        .await;
        assert!(matches!(
            o.verification.as_ref().unwrap().renderer,
            CheckResult::Failed { .. }
        ));
    }
}

#[tokio::test(start_paused = true)]
async fn line_and_auth_overrides_are_stored_into_the_profile() {
    let w = World::new();
    let mut r = req("work");
    r.intent = SwitchIntent::Profile {
        profile_id: "work".into(),
        line_id: Some("cdn".into()),
        auth_mode: Some(superba_core::AuthMode::ApiKey),
    };
    let (o, _) = run(&w, r, ExecuteOptions::default(), ScriptedDecider::with(&[])).await;
    assert_eq!(o.status, OutcomeStatus::Switched);
    let set = w.profiles.set.lock().unwrap().clone();
    let p = set.profile(&ProfileId::new("work")).unwrap();
    assert_eq!(p.line_id.as_ref().map(|l| l.as_str()), Some("cdn"));
    assert_eq!(p.auth_mode, superba_core::AuthMode::ApiKey);
}

#[tokio::test(start_paused = true)]
async fn kicked_to_home_returns_to_the_open_thread() {
    let w = World::new();
    *w.app.renderer_after_restart.lock().unwrap() = Some(RendererState {
        url: "app://-/index.html#/".into(),
        on_login_page: false,
        on_home: true,
        current_thread: None,
    });
    let (o, ev) = run(
        &w,
        req("work"),
        ExecuteOptions::default(),
        ScriptedDecider::with(&[]),
    )
    .await;
    assert_eq!(o.status, OutcomeStatus::Switched);
    assert_eq!(
        o.returned_to,
        Some(ThreadRef {
            thread_id: "t1".into(),
            title: Some("Refactor".into())
        })
    );
    assert!(ev.phases().contains(&SwitchPhase::NavRestore));
    assert!(w.calls().contains(&"app.return_to_thread t1".to_owned()));
    assert!(notice_keys(&o).contains(&"switch.done.returned".to_owned()));
}

// --- waiting for turns ----------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn cancel_while_turns_run_writes_nothing() {
    let w = World::new();
    let mut lists = vec![vec![]];
    lists.extend(std::iter::repeat_n(vec![busy("local", "Refactor")], 50));
    w.push_turns(lists);
    let d = ScriptedDecider::with(&[("wait_for_turns", Decision::Cancel)]);
    let (o, ev) = run(&w, req("work"), ExecuteOptions::default(), d.clone()).await;
    assert_eq!(o.status, OutcomeStatus::Cancelled);
    assert!(w.writes().is_empty(), "{:?}", w.writes());
    assert!(ev.phases().contains(&SwitchPhase::WaitingTurn));
    assert_eq!(d.asked_kinds(), vec!["wait_for_turns"]);
    assert!(
        journal_status(&w).is_empty(),
        "no journal entry without writes"
    );
}

#[tokio::test(start_paused = true)]
async fn turns_ending_on_their_own_drop_the_question_and_switch() {
    let w = World::new();
    // Busy for the plan-time check and three polls, then idle.
    w.push_turns(vec![
        vec![busy("local", "plan")],
        vec![busy("local", "a")],
        vec![busy("local", "a")],
        vec![busy("local", "a")],
    ]);
    let d = ScriptedDecider::with(&[]);
    *d.hang_on_wait.lock().unwrap() = true;
    let (o, ev) = run(&w, req("work"), ExecuteOptions::default(), d.clone()).await;
    assert_eq!(o.status, OutcomeStatus::Switched);
    assert_eq!(d.asked_kinds(), vec!["wait_for_turns"]);
    assert!(
        ev.events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, superba_core::SwitchEvent::Turns { .. }))
    );
}

#[tokio::test(start_paused = true)]
async fn force_while_waiting_switches_and_force_option_never_asks() {
    let w = World::new();
    w.push_turns(vec![vec![], vec![busy("local", "a")]]);
    let d = ScriptedDecider::with(&[("wait_for_turns", Decision::Force)]);
    let (o, _) = run(&w, req("work"), ExecuteOptions::default(), d.clone()).await;
    assert_eq!(o.status, OutcomeStatus::Switched);

    let w = World::new();
    w.push_turns(vec![vec![busy("local", "a")]; 10]);
    let d = ScriptedDecider::with(&[]);
    let opts = ExecuteOptions {
        force_turns: true,
        ..ExecuteOptions::default()
    };
    let (o, _) = run(&w, req("work"), opts, d.clone()).await;
    assert_eq!(o.status, OutcomeStatus::Switched);
    assert!(d.asked_kinds().is_empty());
}

#[tokio::test(start_paused = true)]
async fn waiting_with_wait_answers_keeps_polling_until_idle() {
    let w = World::new();
    let mut lists = vec![vec![]];
    lists.extend(std::iter::repeat_n(vec![busy("local", "a")], 30));
    w.push_turns(lists);
    let d = ScriptedDecider::with(&[("wait_for_turns", Decision::Wait)]);
    let (o, _) = run(&w, req("work"), ExecuteOptions::default(), d).await;
    assert_eq!(o.status, OutcomeStatus::Switched);
}

#[tokio::test(start_paused = true)]
async fn turn_starting_right_before_the_ipc_can_be_cancelled_with_rollback() {
    let w = World::new();
    // plan-time: idle; TurnCheck: idle; pre-IPC re-check: busy; then busy.
    w.push_turns(vec![
        vec![],
        vec![],
        vec![busy("local", "late")],
        vec![busy("local", "late")],
    ]);
    let d = ScriptedDecider::with(&[("wait_for_turns", Decision::Cancel)]);
    let (o, ev) = run(&w, req("work"), ExecuteOptions::default(), d).await;
    assert_eq!(o.status, OutcomeStatus::RolledBack);
    let calls = w.calls();
    assert!(calls.contains(&"config.rollback".to_owned()));
    assert!(calls.contains(&"env.revert".to_owned()));
    assert!(!calls.iter().any(|c| c.starts_with("app.hot_restart")));
    let phases = ev.phases();
    assert!(phases.contains(&SwitchPhase::RollingBack));
    assert_eq!(journal_status(&w), vec![JournalStatus::RolledBack]);
}

// --- IPC failure (plan 7.6 #2) ------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn ipc_failure_undo_restores_the_files() {
    let w = World::new();
    *w.app.fail_hot_restart.lock().unwrap() = 1;
    let d = ScriptedDecider::with(&[("ipc_failed", Decision::Undo)]);
    let (o, ev) = run(&w, req("work"), ExecuteOptions::default(), d.clone()).await;
    assert_eq!(o.status, OutcomeStatus::RolledBack);
    assert!(ev.phases().contains(&SwitchPhase::FullRestartOffer));
    assert!(w.calls().contains(&"config.rollback".to_owned()));
    assert_eq!(d.asked_kinds(), vec!["ipc_failed"]);
}

#[tokio::test(start_paused = true)]
async fn ipc_failure_keep_written_leaves_the_restart_to_the_user() {
    let w = World::new();
    *w.app.fail_hot_restart.lock().unwrap() = 1;
    let d = ScriptedDecider::with(&[("ipc_failed", Decision::KeepWritten)]);
    let (o, _) = run(&w, req("work"), ExecuteOptions::default(), d).await;
    assert_eq!(o.status, OutcomeStatus::WrittenOnly);
    assert!(!w.calls().contains(&"config.rollback".to_owned()));
    assert_eq!(journal_status(&w), vec![JournalStatus::Done]);
}

#[tokio::test(start_paused = true)]
async fn ipc_failure_full_restart_quits_and_relaunches_with_the_line() {
    let w = World::new();
    *w.app.fail_hot_restart.lock().unwrap() = 1;
    let d = ScriptedDecider::with(&[("ipc_failed", Decision::FullRestart)]);
    let (o, ev) = run(&w, req("work"), ExecuteOptions::default(), d).await;
    assert_eq!(o.status, OutcomeStatus::Switched, "{:?}", o.notices);
    let calls = w.calls();
    assert!(calls.contains(&"app.quit force=false".to_owned()));
    assert!(calls.contains(&format!("app.launch authapi={OVERSEAS}")));
    assert!(ev.phases().contains(&SwitchPhase::Launching));
}

// --- verification failure (plan 7.6 #3) ------------------------------------------------

#[tokio::test(start_paused = true)]
async fn missing_new_app_server_retries_the_ipc_once_then_rolls_back() {
    let w = World::new();
    *w.app.restart_spawns.lock().unwrap() = false;
    let d = ScriptedDecider::with(&[]);
    let (o, ev) = run(&w, req("work"), ExecuteOptions::default(), d.clone()).await;
    assert_eq!(o.status, OutcomeStatus::RolledBack);
    let restarts = w
        .calls()
        .iter()
        .filter(|c| c.starts_with("app.hot_restart"))
        .count();
    // First IPC, automatic retry, restart on the restored files.
    assert_eq!(restarts, 3);
    assert_eq!(d.asked_kinds(), vec!["verification_failed"]);
    assert!(matches!(
        o.verification.as_ref().unwrap().process,
        CheckResult::Failed { .. }
    ));
    let phases = ev.phases();
    assert!(phases.contains(&SwitchPhase::Recovery));
    assert!(phases.contains(&SwitchPhase::RollbackRestart));
    assert!(notice_keys(&o).contains(&"switch.failed.verification".to_owned()));
}

#[tokio::test(start_paused = true)]
async fn login_page_after_restart_fails_verification_and_keep_written_is_honoured() {
    let w = World::new();
    *w.app.renderer_after_restart.lock().unwrap() = Some(RendererState {
        url: "app://-/index.html#/login".into(),
        on_login_page: true,
        on_home: false,
        current_thread: None,
    });
    let d = ScriptedDecider::with(&[("verification_failed", Decision::KeepWritten)]);
    let (o, _) = run(&w, req("work"), ExecuteOptions::default(), d.clone()).await;
    assert_eq!(o.status, OutcomeStatus::WrittenOnly);
    // A login page is about auth, not the process: no automatic IPC retry.
    assert_eq!(
        w.calls()
            .iter()
            .filter(|c| c.starts_with("app.hot_restart"))
            .count(),
        1
    );
    assert!(matches!(
        o.verification.as_ref().unwrap().renderer,
        CheckResult::Failed { .. }
    ));
}

#[tokio::test(start_paused = true)]
async fn file_changed_after_writing_fails_the_hash_check() {
    let w = World::new();
    *w.config.tamper_after_apply.lock().unwrap() = true;
    let d = ScriptedDecider::with(&[("verification_failed", Decision::Rollback)]);
    let (o, _) = run(&w, req("work"), ExecuteOptions::default(), d).await;
    assert_eq!(o.status, OutcomeStatus::RolledBack);
    match &o.verification.as_ref().unwrap().files {
        CheckResult::Failed { detail } => assert!(detail.contains("auth.json"), "{detail}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn config_rewritten_outside_owned_keys_still_verifies() {
    // e2e: the desktop app writes `[desktop]` / `[mcp_servers.*]` to config.toml
    // right after the restart; the hash changes, Superba's keys do not.
    let w = World::new();
    *w.config.config_rewritten_after_apply.lock().unwrap() = Some(Vec::new());
    let (o, ev) = run(
        &w,
        req("work"),
        ExecuteOptions::default(),
        ScriptedDecider::with(&[]),
    )
    .await;
    assert_eq!(o.status, OutcomeStatus::Switched);
    assert_eq!(o.verification.as_ref().unwrap().files, CheckResult::Passed);
    assert!(!w.calls().contains(&"config.rollback".to_owned()));
    assert!(ev.events.lock().unwrap().iter().any(|e| matches!(
        e,
        SwitchEvent::Log { text, .. } if text.key == "switch.verify.config_changed_elsewhere"
    )));
}

#[tokio::test(start_paused = true)]
async fn owned_keys_changed_after_writing_fail_the_files_check() {
    let w = World::new();
    *w.config.config_rewritten_after_apply.lock().unwrap() = Some(vec![KeyChange {
        path: "openai_base_url".into(),
        before: Some("\"https://api.krill-code.net/backend-api/codex\"".into()),
        after: Some("\"https://elsewhere.example\"".into()),
    }]);
    let d = ScriptedDecider::with(&[("verification_failed", Decision::Rollback)]);
    let (o, _) = run(&w, req("work"), ExecuteOptions::default(), d).await;
    assert_eq!(o.status, OutcomeStatus::RolledBack);
    match &o.verification.as_ref().unwrap().files {
        CheckResult::Failed { detail } => {
            assert!(detail.contains("openai_base_url"), "{detail}")
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn unhealthy_whoami_after_the_switch_is_reported() {
    let w = World::new();
    let sw = w.switcher();
    let plan = plan_with(&sw, req("work")).await;
    // The auth address breaks between planning and verification.
    w.prober.authapi.lock().unwrap().insert(
        OVERSEAS.into(),
        AuthApiHealth::Unhealthy {
            reason: "edge".into(),
        },
    );
    let d = ScriptedDecider::with(&[("verification_failed", Decision::Rollback)]);
    let o = sw
        .execute(plan, ExecuteOptions::default(), d, Arc::new(NoEvents))
        .await
        .unwrap();
    assert_eq!(o.status, OutcomeStatus::RolledBack);
    assert!(matches!(
        o.verification.as_ref().unwrap().auth,
        CheckResult::Failed { .. }
    ));
}

// --- write failures (plan 7.6 #1) -------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn env_write_failure_rolls_the_config_back_and_restarts_nothing() {
    let w = World::new();
    *w.env.fail_apply.lock().unwrap() = Some(Error::Io {
        path: "/fake/.zshenv".into(),
        kind: "PermissionDenied".into(),
        message: "denied".into(),
        open_files_limit: None,
    });
    let (o, ev) = run(
        &w,
        req("work"),
        ExecuteOptions::default(),
        ScriptedDecider::with(&[]),
    )
    .await;
    assert_eq!(o.status, OutcomeStatus::RolledBack);
    assert_eq!(
        w.calls()
            .into_iter()
            .filter(|c| !c.starts_with("prober") && !c.starts_with("config.plan"))
            .collect::<Vec<_>>(),
        vec!["config.apply", "env.apply", "config.rollback"]
    );
    assert!(ev.phases().contains(&SwitchPhase::RollingBack));
    assert!(notice_keys(&o).contains(&"switch.failed.apply".to_owned()));
    assert_eq!(journal_status(&w), vec![JournalStatus::RolledBack]);
}

#[tokio::test(start_paused = true)]
async fn config_write_failure_changes_nothing() {
    let w = World::new();
    *w.config.fail_apply.lock().unwrap() = Some(Error::Conflict {
        message: "raced".into(),
    });
    let (o, _) = run(
        &w,
        req("work"),
        ExecuteOptions::default(),
        ScriptedDecider::with(&[]),
    )
    .await;
    assert_eq!(o.status, OutcomeStatus::RolledBack);
    assert!(
        !w.calls()
            .iter()
            .any(|c| c == "env.apply" || c.starts_with("app."))
    );
}

#[tokio::test(start_paused = true)]
async fn failed_rollback_is_reported_as_failed() {
    let w = World::new();
    *w.env.fail_apply.lock().unwrap() = Some(Error::other("env broken"));
    *w.config.fail_rollback.lock().unwrap() = Some(Error::Conflict {
        message: "edited meanwhile".into(),
    });
    let (o, _) = run(
        &w,
        req("work"),
        ExecuteOptions::default(),
        ScriptedDecider::with(&[]),
    )
    .await;
    assert_eq!(o.status, OutcomeStatus::Failed);
    assert!(notice_keys(&o).contains(&"switch.failed.rollback".to_owned()));
    assert_eq!(journal_status(&w), vec![JournalStatus::RollingBack]);
    *w.config.fail_rollback.lock().unwrap() = None;
    let report = w.switcher().recover().await.unwrap();
    assert_eq!(
        report.entries[0].action,
        superba_core::JournalAction::RolledBack
    );
    assert_eq!(journal_status(&w), vec![JournalStatus::RolledBack]);
}

#[tokio::test(start_paused = true)]
async fn crash_during_committed_undo_is_recovered_as_rollback() {
    let w = World::new();
    // Verification fails after the config and env transaction committed.
    w.app.renderer.lock().unwrap().on_login_page = true;
    *w.env.hang_on_revert.lock().unwrap() = true;
    let sw = w.switcher();
    let p = plan_with(&sw, req("work")).await;
    {
        let undo = sw.execute(
            p,
            ExecuteOptions::default(),
            ScriptedDecider::with(&[("verification_failed", Decision::Rollback)]),
            Arc::new(NoEvents),
        );
        tokio::pin!(undo);
        tokio::select! {
            result = &mut undo => panic!("undo unexpectedly finished: {result:?}"),
            () = w.env.revert_started.notified() => {}
        }
        assert_eq!(journal_status(&w), vec![JournalStatus::RollingBack]);
        assert!(!w.calls().contains(&"config.rollback".into()));
        // Dropping the pending execution simulates a crash during undo.
    }
    *w.env.hang_on_revert.lock().unwrap() = false;
    w.set_session(None);
    let report = sw.recover().await.unwrap();
    assert_eq!(
        report.entries[0].action,
        superba_core::JournalAction::RolledBack
    );
    assert_eq!(journal_status(&w), vec![JournalStatus::RolledBack]);
    assert_eq!(
        w.config.state.lock().unwrap().config_sha256.as_deref(),
        Some("cfg-0")
    );
}

// --- full restart -----------------------------------------------------------------------

fn no_cdp_world() -> World {
    let w = World::new();
    w.set_session(Some(session(Some(OVERSEAS), false)));
    w.app
        .processes
        .lock()
        .unwrap()
        .push(app_server(150, 100, T0 - 10));
    w
}

fn unhealthy_authapi_world() -> World {
    let w = World::new();
    w.set_session(Some(session(Some(CN_FAST), true)));
    w.prober.authapi.lock().unwrap().insert(
        CN_FAST.into(),
        AuthApiHealth::Unhealthy {
            reason: "timeout".into(),
        },
    );
    w
}

fn home_renderer() -> RendererState {
    RendererState {
        url: "app://-/index.html#/".into(),
        on_login_page: false,
        on_home: true,
        current_thread: None,
    }
}

fn assert_no_nav_restore(w: &World, o: &SwitchOutcome, ev: &RecordingEvents) {
    assert_eq!(o.returned_to, None);
    assert!(!ev.phases().contains(&SwitchPhase::NavRestore));
    assert!(
        !w.calls()
            .iter()
            .any(|c| c.starts_with("app.return_to_thread"))
    );
    assert!(!notice_keys(o).contains(&"switch.done.returned".to_owned()));
}

#[tokio::test(start_paused = true)]
async fn full_restart_needs_permission() {
    let w = no_cdp_world();
    let sw = w.switcher();
    let plan = plan_with(&sw, req("work")).await;
    let err = sw
        .execute(
            plan,
            ExecuteOptions::default(),
            ScriptedDecider::with(&[]),
            Arc::new(NoEvents),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "blocked");
    assert!(w.writes().is_empty());
}

#[tokio::test(start_paused = true)]
async fn full_restart_quits_writes_and_relaunches_through_superba() {
    let w = no_cdp_world();
    let (o, ev) = run(&w, req("work"), allow_full(), ScriptedDecider::with(&[])).await;
    assert_eq!(o.status, OutcomeStatus::Switched, "{:?}", o.notices);
    assert_eq!(
        o.restart,
        RestartKind::FullAppRestart {
            reason: FullRestartReason::NoCdp
        }
    );
    assert_eq!(
        w.writes(),
        vec![
            "app.quit force=false".to_owned(),
            "config.apply".to_owned(),
            "env.apply".to_owned(),
            format!("app.launch authapi={OVERSEAS}"),
        ]
    );
    let launch = w.app.launches.lock().unwrap()[0].clone();
    assert_eq!(launch.cdp, superba_core::CdpMode::Auto);
    assert_eq!(
        ev.phases(),
        vec![
            SwitchPhase::TurnCheck,
            SwitchPhase::Quitting,
            SwitchPhase::Applying,
            SwitchPhase::Launching,
            SwitchPhase::Verifying,
            SwitchPhase::Done
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn full_restart_returns_to_the_open_thread() {
    let w = unhealthy_authapi_world();
    let before = w.app.renderer.lock().unwrap().clone();
    let thread = before.current_thread.clone().unwrap();
    w.app
        .renderer_states
        .lock()
        .unwrap()
        .extend([Ok(before), Ok(home_renderer())]);

    let (o, ev) = run(&w, req("work"), allow_full(), ScriptedDecider::with(&[])).await;

    assert_eq!(o.status, OutcomeStatus::Switched, "{:?}", o.notices);
    assert_eq!(
        o.restart,
        RestartKind::FullAppRestart {
            reason: FullRestartReason::AuthApiUnhealthy
        }
    );
    assert_eq!(o.returned_to, Some(thread.clone()));
    assert_eq!(
        ev.phases(),
        vec![
            SwitchPhase::TurnCheck,
            SwitchPhase::Quitting,
            SwitchPhase::Applying,
            SwitchPhase::Launching,
            SwitchPhase::Verifying,
            SwitchPhase::NavRestore,
            SwitchPhase::Done,
        ]
    );
    assert!(
        w.calls()
            .contains(&format!("app.return_to_thread {}", thread.thread_id))
    );
    assert!(notice_keys(&o).contains(&"switch.done.returned".to_owned()));
}

#[tokio::test(start_paused = true)]
async fn full_restart_waits_for_renderer_before_returning() {
    let w = unhealthy_authapi_world();
    let before = w.app.renderer.lock().unwrap().clone();
    let thread = before.current_thread.clone().unwrap();
    let loading = RendererState {
        url: "about:blank".into(),
        on_home: false,
        ..home_renderer()
    };
    w.app.renderer_states.lock().unwrap().extend([
        Ok(before),
        Ok(loading.clone()), // Verification can pass before navigation is ready.
        Ok(loading.clone()),
        Ok(RendererState {
            on_login_page: true,
            ..home_renderer()
        }),
        Err(Error::Cdp {
            message: "renderer still starting".into(),
        }),
        Ok(loading),
        Ok(home_renderer()),
    ]);
    let started = tokio::time::Instant::now();

    let (o, ev) = run(&w, req("work"), allow_full(), ScriptedDecider::with(&[])).await;

    assert_eq!(o.status, OutcomeStatus::Switched, "{:?}", o.notices);
    assert_eq!(o.returned_to, Some(thread.clone()));
    assert!(ev.phases().contains(&SwitchPhase::NavRestore));
    assert!(
        w.calls()
            .contains(&format!("app.return_to_thread {}", thread.thread_id))
    );
    assert!(notice_keys(&o).contains(&"switch.done.returned".to_owned()));
    assert!(started.elapsed() >= Duration::from_secs(2));
    assert!(started.elapsed() < Duration::from_millis(2500));
    assert!(w.app.renderer_states.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn full_restart_without_an_open_thread_does_not_navigate() {
    let w = unhealthy_authapi_world();
    *w.app.renderer.lock().unwrap() = home_renderer();

    let (o, ev) = run(&w, req("work"), allow_full(), ScriptedDecider::with(&[])).await;

    assert_eq!(o.status, OutcomeStatus::Switched);
    assert_no_nav_restore(&w, &o, &ev);
}

#[tokio::test(start_paused = true)]
async fn full_restart_with_failed_verification_does_not_navigate() {
    let w = unhealthy_authapi_world();
    let before = w.app.renderer.lock().unwrap().clone();
    w.app
        .renderer_states
        .lock()
        .unwrap()
        .extend([Ok(before), Ok(home_renderer())]);
    *w.config.tamper_after_apply.lock().unwrap() = true;

    let (o, ev) = run(&w, req("work"), allow_full(), ScriptedDecider::with(&[])).await;

    assert_eq!(o.status, OutcomeStatus::WrittenOnly);
    assert!(matches!(
        o.verification.as_ref().unwrap().files,
        CheckResult::Failed { .. }
    ));
    assert_no_nav_restore(&w, &o, &ev);
}

#[tokio::test(start_paused = true)]
async fn full_restart_stops_waiting_when_renderer_never_becomes_ready() {
    let w = unhealthy_authapi_world();
    let before = w.app.renderer.lock().unwrap().clone();
    w.app.renderer_states.lock().unwrap().extend([
        Ok(before),
        Ok(RendererState {
            url: "about:blank".into(),
            on_home: false,
            ..home_renderer()
        }),
    ]);
    let started = tokio::time::Instant::now();

    let (o, ev) = tokio::time::timeout(
        Duration::from_secs(16),
        run(&w, req("work"), allow_full(), ScriptedDecider::with(&[])),
    )
    .await
    .expect("renderer readiness must not prevent the switch from finishing");

    assert_eq!(o.status, OutcomeStatus::Switched);
    assert!(o.verification.as_ref().unwrap().passed());
    assert!(started.elapsed() >= Duration::from_secs(15));
    assert_no_nav_restore(&w, &o, &ev);
}

#[tokio::test(start_paused = true)]
async fn full_restart_leaves_an_already_open_thread_alone() {
    let w = unhealthy_authapi_world();
    let before = w.app.renderer.lock().unwrap().clone();
    let after = RendererState {
        url: "app://-/index.html#/thread/t2".into(),
        current_thread: Some(ThreadRef {
            thread_id: "t2".into(),
            title: Some("Another conversation".into()),
        }),
        ..before.clone()
    };
    w.app
        .renderer_states
        .lock()
        .unwrap()
        .extend([Ok(before), Ok(after)]);
    let started = tokio::time::Instant::now();

    let (o, ev) = run(&w, req("work"), allow_full(), ScriptedDecider::with(&[])).await;

    assert_eq!(o.status, OutcomeStatus::Switched);
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_no_nav_restore(&w, &o, &ev);
}

#[tokio::test(start_paused = true)]
async fn full_restart_ignores_failure_to_read_the_open_thread() {
    let w = unhealthy_authapi_world();
    w.app.renderer_states.lock().unwrap().extend([
        Err(Error::Cdp {
            message: "renderer unavailable before quit".into(),
        }),
        Ok(home_renderer()),
    ]);

    let (o, ev) = run(&w, req("work"), allow_full(), ScriptedDecider::with(&[])).await;

    assert_eq!(o.status, OutcomeStatus::Switched);
    assert_no_nav_restore(&w, &o, &ev);
}

#[tokio::test(start_paused = true)]
async fn full_restart_in_safe_mode_does_not_navigate() {
    let w = unhealthy_authapi_world();
    w.profiles.set.lock().unwrap().settings.safe_mode = true;
    let before = w.app.renderer.lock().unwrap().clone();
    w.app
        .renderer_states
        .lock()
        .unwrap()
        .extend([Ok(before), Ok(home_renderer())]);

    let (o, ev) = run(&w, req("work"), allow_full(), ScriptedDecider::with(&[])).await;

    assert_eq!(o.status, OutcomeStatus::Switched);
    assert!(
        w.app
            .session
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .cdp
            .is_none()
    );
    assert_eq!(w.app.renderer_states.lock().unwrap().len(), 1);
    assert_no_nav_restore(&w, &o, &ev);
}

#[tokio::test(start_paused = true)]
async fn full_restart_without_cdp_before_quit_does_not_remember_a_thread() {
    let w = no_cdp_world();
    let before = w.app.renderer.lock().unwrap().clone();
    w.app
        .renderer_states
        .lock()
        .unwrap()
        .extend([Ok(before), Ok(home_renderer())]);

    let (o, ev) = run(&w, req("work"), allow_full(), ScriptedDecider::with(&[])).await;

    assert_eq!(o.status, OutcomeStatus::Switched);
    // Only verification reads the renderer in the new session.
    assert_eq!(w.app.renderer_states.lock().unwrap().len(), 1);
    assert_no_nav_restore(&w, &o, &ev);
}

/// User bug 76: on Windows a ChatGPT Superba did not open has no debugging
/// port, so it cannot be asked to quit (closing its windows leaves it in the
/// notification area). The plan says up front that it is ended by force, and a
/// confirmation of that plan (the GUI's) ends it without a graceful wait and
/// without asking again.
#[tokio::test(start_paused = true)]
async fn on_windows_a_confirmed_app_without_cdp_is_ended_at_once() {
    let w = no_cdp_world();
    let sw = w.switcher_on(true);
    let plan = plan_with(&sw, req("work")).await;
    assert!(plan.needs_confirmation(), "{:?}", plan.primary);
    assert!(plan.quits_by_force(), "{:?}", plan.warnings);
    let d = ScriptedDecider::with(&[]);
    let opts = ExecuteOptions {
        force_quit: true,
        ..allow_full()
    };
    let ev = Arc::new(RecordingEvents::default());
    let o = sw.execute(plan, opts, d.clone(), ev.clone()).await.unwrap();
    assert_eq!(o.status, OutcomeStatus::Switched, "{:?}", o.notices);
    assert!(d.asked_kinds().is_empty(), "{:?}", d.asked_kinds());
    let calls = w.calls();
    assert!(
        !calls.contains(&"app.quit force=false".to_owned()),
        "{calls:?}"
    );
    assert!(
        calls.contains(&"app.quit force=true".to_owned()),
        "{calls:?}"
    );
    assert!(!ev.phases().contains(&SwitchPhase::ForceQuitConfirm));
}

/// Without that confirmation (`spb` without --force) nothing waits for a
/// quit that cannot come: the question is asked at once, and cancelling
/// leaves ChatGPT as it was (its windows are not closed).
#[tokio::test(start_paused = true)]
async fn on_windows_an_app_without_cdp_is_asked_about_at_once() {
    let w = no_cdp_world();
    let sw = w.switcher_on(true);
    let plan = plan_with(&sw, req("work")).await;
    let d = ScriptedDecider::with(&[("force_quit", Decision::Cancel)]);
    let o = sw
        .execute(
            plan,
            allow_full(),
            d.clone(),
            Arc::new(RecordingEvents::default()),
        )
        .await
        .unwrap();
    assert_eq!(o.status, OutcomeStatus::Cancelled);
    assert_eq!(d.asked_kinds(), vec!["force_quit"]);
    let calls = w.calls();
    assert!(
        !calls.iter().any(|c| c.starts_with("app.quit ")),
        "{calls:?}"
    );
    assert!(!calls.contains(&"config.apply".to_owned()));
}

/// The notice and the shortcut are for Windows without CDP only: an app
/// Superba opened is asked to quit, and other systems quit it gracefully.
#[tokio::test(start_paused = true)]
async fn the_force_quit_notice_needs_windows_and_an_app_without_cdp() {
    let w = no_cdp_world();
    let plan = plan_with(&w.switcher_on(false), req("work")).await;
    assert!(plan.needs_confirmation());
    assert!(!plan.quits_by_force(), "{:?}", plan.warnings);
    let w = unhealthy_authapi_world();
    let sw = w.switcher_on(true);
    let plan = plan_with(&sw, req("work")).await;
    assert!(plan.needs_confirmation(), "{:?}", plan.primary);
    assert!(!plan.quits_by_force(), "{:?}", plan.warnings);
    // force_quit asked for an app with CDP: it is still asked to quit first.
    let d = ScriptedDecider::with(&[]);
    let opts = ExecuteOptions {
        force_quit: true,
        ..allow_full()
    };
    sw.execute(plan, opts, d, Arc::new(RecordingEvents::default()))
        .await
        .unwrap();
    assert!(w.calls().contains(&"app.quit force=false".to_owned()));
}

#[tokio::test(start_paused = true)]
async fn cancelled_full_restart_does_not_navigate() {
    let w = unhealthy_authapi_world();
    w.app
        .quit_outcomes
        .lock()
        .unwrap()
        .push_back(QuitOutcome::TimedOut);
    let d = ScriptedDecider::with(&[("force_quit", Decision::Cancel)]);

    let (o, ev) = run(&w, req("work"), allow_full(), d).await;

    assert_eq!(o.status, OutcomeStatus::Cancelled);
    assert!(w.app.launches.lock().unwrap().is_empty());
    assert_no_nav_restore(&w, &o, &ev);
}

#[tokio::test(start_paused = true)]
async fn full_restart_with_failed_launch_does_not_navigate() {
    let w = unhealthy_authapi_world();
    *w.app.fail_launch.lock().unwrap() = Some(Error::Process {
        message: "exec failed".into(),
    });

    let (o, ev) = run(&w, req("work"), allow_full(), ScriptedDecider::with(&[])).await;

    assert_eq!(o.status, OutcomeStatus::WrittenOnly);
    assert!(o.verification.is_none());
    assert_no_nav_restore(&w, &o, &ev);
}

#[tokio::test(start_paused = true)]
async fn quit_timeout_asks_to_force_and_cancel_writes_nothing() {
    let w = no_cdp_world();
    w.app
        .quit_outcomes
        .lock()
        .unwrap()
        .push_back(QuitOutcome::TimedOut);
    let d = ScriptedDecider::with(&[("force_quit", Decision::Cancel)]);
    let (o, ev) = run(&w, req("work"), allow_full(), d.clone()).await;
    assert_eq!(o.status, OutcomeStatus::Cancelled);
    assert!(ev.phases().contains(&SwitchPhase::ForceQuitConfirm));
    assert!(!w.calls().contains(&"config.apply".to_owned()));
    assert_eq!(d.asked_kinds(), vec!["force_quit"]);
}

#[tokio::test(start_paused = true)]
async fn quit_timeout_force_kills_then_continues() {
    let w = no_cdp_world();
    w.app
        .quit_outcomes
        .lock()
        .unwrap()
        .push_back(QuitOutcome::TimedOut);
    let d = ScriptedDecider::with(&[("force_quit", Decision::Force)]);
    let (o, _) = run(&w, req("work"), allow_full(), d).await;
    assert_eq!(o.status, OutcomeStatus::Switched);
    assert!(w.calls().contains(&"app.quit force=true".to_owned()));
}

fn left(pid: u32, role: &str) -> superba_core::LingeringProcess {
    superba_core::LingeringProcess {
        pid,
        ppid: Some(1),
        image: Some(std::path::PathBuf::from(format!("C:/app/{role}.exe"))),
        role: role.into(),
        arch: Some("arm64".into()),
        started_at: Some(superba_core::Timestamp::from_millis(T0 - 60_000)),
        end_error: None,
    }
}

fn still_running(pids: &[(u32, &str)]) -> superba_core::QuitReport {
    superba_core::QuitReport {
        steps: vec![
            superba_core::QuitStep::ClosedWindows,
            superba_core::QuitStep::Waited { ms: 5_000 },
            superba_core::QuitStep::Ended {
                failed_names: Vec::new(),
                ended: 14,
                failed: 1,
            },
        ],
        left: pids.iter().map(|(p, r)| left(*p, r)).collect(),
    }
}

fn logged(ev: &RecordingEvents, key: &str) -> Vec<superba_core::LocalizedText> {
    ev.events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            SwitchEvent::Log { text, .. } if text.key == key => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// User bug 53: the graceful quit timed out; the question names the app's
/// processes still running and what was tried, and so does the log.
#[tokio::test(start_paused = true)]
async fn quit_timeout_names_the_processes_still_running() {
    let w = no_cdp_world();
    w.app
        .quit_outcomes
        .lock()
        .unwrap()
        .push_back(QuitOutcome::TimedOut);
    w.app
        .quit_reports
        .lock()
        .unwrap()
        .push_back(still_running(&[(100, "main"), (101, "renderer")]));
    let d = ScriptedDecider::with(&[("force_quit", Decision::Cancel)]);
    let (o, ev) = run(&w, req("work"), allow_full(), d.clone()).await;
    assert_eq!(o.status, OutcomeStatus::Cancelled);
    let asked = d.asked.lock().unwrap().clone();
    let superba_core::DecisionRequest::ForceQuit { pids, report } = &asked[0] else {
        panic!("{asked:?}");
    };
    assert_eq!(pids, &vec![100, 101]);
    assert_eq!(report.left.len(), 2);
    let lines = logged(&ev, "switch.quit.still_running");
    assert_eq!(lines.len(), 1);
    let line = lines[0].render_fallback();
    assert!(
        line.contains("pid 100 main.exe (main, arm64, parent 1"),
        "{line}"
    );
    assert!(line.contains("pid 101 renderer.exe"), "{line}");
    assert!(line.contains("closed its windows"), "{line}");
}

/// User bug 53: the forced quit timed out too; "end these processes" ends
/// exactly the listed ones and the switch goes on.
#[tokio::test(start_paused = true)]
async fn forced_quit_timeout_ends_the_listed_processes_when_the_user_asks() {
    let w = no_cdp_world();
    w.app
        .quit_outcomes
        .lock()
        .unwrap()
        .extend([QuitOutcome::TimedOut, QuitOutcome::TimedOut]);
    w.app.quit_reports.lock().unwrap().extend([
        still_running(&[(100, "main")]),
        still_running(&[(100, "main"), (102, "gpu-process")]),
    ]);
    let d = ScriptedDecider::with(&[
        ("force_quit", Decision::Force),
        ("app_still_running", Decision::Force),
    ]);
    let (o, ev) = run(&w, req("work"), allow_full(), d.clone()).await;
    assert_eq!(o.status, OutcomeStatus::Switched);
    assert_eq!(d.asked_kinds(), vec!["force_quit", "app_still_running"]);
    assert!(
        w.calls()
            .contains(&"app.end_app_processes 100,102".to_owned()),
        "{:?}",
        w.calls()
    );
    assert_eq!(logged(&ev, "switch.quit.still_running").len(), 2);
}

/// "Try again" runs the forced quit once more.
#[tokio::test(start_paused = true)]
async fn forced_quit_timeout_can_be_retried() {
    let w = no_cdp_world();
    w.app.quit_outcomes.lock().unwrap().extend([
        QuitOutcome::TimedOut,
        QuitOutcome::TimedOut,
        QuitOutcome::Killed,
    ]);
    let d = ScriptedDecider::with(&[
        ("force_quit", Decision::Force),
        ("app_still_running", Decision::Continue),
    ]);
    let (o, _) = run(&w, req("work"), allow_full(), d.clone()).await;
    assert_eq!(o.status, OutcomeStatus::Switched);
    let quits: Vec<String> = w
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("app.quit force="))
        .collect();
    assert_eq!(
        quits,
        vec![
            "app.quit force=false",
            "app.quit force=true",
            "app.quit force=true"
        ]
    );
}

/// Stopping at "still running" cancels the switch before anything is written.
#[tokio::test(start_paused = true)]
async fn forced_quit_timeout_cancelled_writes_nothing() {
    let w = no_cdp_world();
    w.app
        .quit_outcomes
        .lock()
        .unwrap()
        .extend([QuitOutcome::TimedOut, QuitOutcome::TimedOut]);
    let d = ScriptedDecider::with(&[("force_quit", Decision::Force)]);
    let (o, _) = run(&w, req("work"), allow_full(), d.clone()).await;
    assert_eq!(o.status, OutcomeStatus::Cancelled);
    assert_eq!(d.asked_kinds(), vec!["force_quit", "app_still_running"]);
    assert!(!w.calls().contains(&"config.apply".to_owned()));
}

/// Processes that cannot be ended: after the last round the switch fails
/// with an error that names them (never a bare "timed out").
#[tokio::test(start_paused = true)]
async fn processes_that_cannot_be_ended_fail_the_switch_naming_them() {
    let w = no_cdp_world();
    w.app
        .quit_outcomes
        .lock()
        .unwrap()
        .extend([QuitOutcome::TimedOut, QuitOutcome::TimedOut]);
    let mut stuck = still_running(&[(100, "main")]);
    stuck.left[0].end_error = Some("terminate: Access is denied. (os error 5)".into());
    for _ in 0..6 {
        w.app.quit_reports.lock().unwrap().push_back(stuck.clone());
        w.app.end_reports.lock().unwrap().push_back(stuck.clone());
    }
    let d = ScriptedDecider::with(&[
        ("force_quit", Decision::Force),
        ("app_still_running", Decision::Force),
        ("app_still_running", Decision::Force),
        ("app_still_running", Decision::Force),
    ]);
    let sw = w.switcher();
    let plan = plan_with(&sw, req("work")).await;
    let events = Arc::new(RecordingEvents::default());
    let error = sw
        .execute(plan, allow_full(), d.clone(), events)
        .await
        .expect_err("the switch fails")
        .to_string();
    assert!(error.contains("pid 100 main.exe"), "{error}");
    assert!(error.contains("Access is denied"), "{error}");
    assert!(!w.calls().contains(&"config.apply".to_owned()));
}

#[tokio::test(start_paused = true)]
async fn write_only_skips_quit_and_launch() {
    let w = no_cdp_world();
    let opts = ExecuteOptions {
        write_only: true,
        ..ExecuteOptions::default()
    };
    let (o, _) = run(&w, req("work"), opts, ScriptedDecider::with(&[])).await;
    assert_eq!(o.status, OutcomeStatus::WrittenOnly);
    assert_eq!(w.writes(), vec!["config.apply", "env.apply"]);
}

#[tokio::test(start_paused = true)]
async fn failed_launch_keeps_the_new_configuration_and_says_so() {
    let w = no_cdp_world();
    *w.app.fail_launch.lock().unwrap() = Some(Error::Process {
        message: "exec failed".into(),
    });
    let (o, _) = run(&w, req("work"), allow_full(), ScriptedDecider::with(&[])).await;
    assert_eq!(o.status, OutcomeStatus::WrittenOnly);
    assert!(notice_keys(&o).contains(&"switch.failed.launch".to_owned()));
}

// --- session migration ------------------------------------------------------------------

fn legacy_world() -> World {
    let w = World::new();
    w.config.state.lock().unwrap().model_provider = Some("krill".into());
    let home = w.ctx.codex_home();
    *w.sessions.report.lock().unwrap() = session_report(home.root(), &[("krill", 12)]);
    w
}

fn migrate_req() -> SwitchRequest {
    let mut r = req("work");
    r.legacy_sessions = Some(LegacySessionChoice::Migrate);
    r
}

#[tokio::test(start_paused = true)]
async fn full_restart_after_migration_does_not_navigate_when_app_was_not_running() {
    let w = legacy_world();
    w.set_session(None);
    *w.app.renderer.lock().unwrap() = home_renderer();

    let (o, ev) = run(&w, migrate_req(), allow_full(), ScriptedDecider::with(&[])).await;

    assert_eq!(o.status, OutcomeStatus::Switched);
    assert!(w.app.launches.lock().unwrap().is_empty());
    assert!(notice_keys(&o).contains(&"switch.done.not_running".to_owned()));
    assert_no_nav_restore(&w, &o, &ev);
}

#[tokio::test(start_paused = true)]
async fn takeover_migration_stops_everything_then_migrates_and_relaunches() {
    let w = legacy_world();
    w.app
        .processes
        .lock()
        .unwrap()
        .push(cli_process(302, CodexProcessKind::CliDaemon, T0 - 1));
    let (o, _) = run(&w, migrate_req(), allow_full(), ScriptedDecider::with(&[])).await;
    assert_eq!(o.status, OutcomeStatus::Switched, "{:?}", o.notices);
    let calls: Vec<String> = w
        .calls()
        .into_iter()
        .filter(|c| !c.starts_with("prober") && !c.starts_with("config.plan"))
        .collect();
    assert_eq!(
        calls,
        vec![
            "app.quit force=false".to_owned(),
            "app.stop_daemon".to_owned(),
            "config.apply".to_owned(),
            "env.apply".to_owned(),
            "sessions.migrate".to_owned(),
            format!("app.launch authapi={OVERSEAS}"),
        ]
    );
    let entry = &SwitchJournal::new(&w.ctx.data_dir).entries().unwrap()[0];
    assert!(entry.migration.is_some(), "receipt journaled");
}

#[tokio::test(start_paused = true)]
async fn open_terminals_must_be_closed_and_cancel_reopens_the_app() {
    let w = legacy_world();
    w.app
        .processes
        .lock()
        .unwrap()
        .push(cli_process(300, CodexProcessKind::CliTui, T0 - 1));
    let d = ScriptedDecider::with(&[("close_consumers", Decision::Cancel)]);
    let (o, _) = run(&w, migrate_req(), allow_full(), d.clone()).await;
    assert_eq!(o.status, OutcomeStatus::Cancelled);
    assert_eq!(d.asked_kinds(), vec!["close_consumers"]);
    let calls = w.calls();
    assert!(!calls.contains(&"config.apply".to_owned()));
    // The app was quit already: it is reopened as it was (same auth address).
    assert!(calls.contains(&format!("app.launch authapi={OVERSEAS}")));
}

/// GH#2: "stop them for me" asks each process to exit, then the switch goes on.
#[tokio::test(start_paused = true)]
async fn stop_them_for_me_asks_each_process_to_exit_then_migrates() {
    let w = legacy_world();
    {
        let mut ps = w.app.processes.lock().unwrap();
        ps.push(cli_process(300, CodexProcessKind::CliTui, T0 - 1));
        let mut ide = cli_process(301, CodexProcessKind::IdeAppServer, T0 - 1);
        ide.ide = Some(superba_core::IdeKind::VsCode);
        ps.push(ide);
    }
    let d = ScriptedDecider::with(&[("close_consumers", Decision::StopProcesses)]);
    let (o, ev) = run(&w, migrate_req(), allow_full(), d.clone()).await;
    assert_eq!(o.status, OutcomeStatus::Switched, "{:?}", o.notices);
    assert_eq!(d.asked_kinds(), vec!["close_consumers"]);
    let calls = w.calls();
    let pos = |s: &str| {
        calls
            .iter()
            .position(|c| c == s)
            .unwrap_or_else(|| panic!("{s}: {calls:?}"))
    };
    assert!(pos("app.stop_process pid=300 force=false") < pos("config.apply"));
    assert!(pos("app.stop_process pid=301 force=false") < pos("config.apply"));
    assert!(
        !calls.iter().any(|c| c.contains("force=true")),
        "nothing ended by force: {calls:?}"
    );
    // The question listed both processes; every stop is in the log.
    match &d.asked.lock().unwrap()[0] {
        DecisionRequest::CloseConsumers { processes, .. } => {
            let pids: Vec<u32> = processes.iter().map(|p| p.pid).collect();
            assert_eq!(pids, vec![300, 301]);
        }
        other => panic!("{other:?}"),
    }
    let logs: Vec<String> = ev
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            SwitchEvent::Log { text, .. } => Some(text.key.clone()),
            _ => None,
        })
        .collect();
    assert!(logs.contains(&"switch.stop.asking".to_owned()), "{logs:?}");
    assert_eq!(
        logs.iter().filter(|k| *k == "switch.stop.result").count(),
        2,
        "{logs:?}"
    );
}

/// What does not exit is ended only after a second, explicit answer.
#[tokio::test(start_paused = true)]
async fn processes_that_do_not_exit_are_ended_only_after_a_second_answer() {
    let w = legacy_world();
    w.app
        .processes
        .lock()
        .unwrap()
        .push(cli_process(300, CodexProcessKind::CliTui, T0 - 1));
    // Windows: a console program cannot be asked to exit.
    w.app
        .stop_answers
        .lock()
        .unwrap()
        .insert(300, superba_core::StopOutcome::ForceOnly);
    let d = ScriptedDecider::with(&[
        ("close_consumers", Decision::StopProcesses),
        ("force_stop", Decision::Force),
    ]);
    let (o, _) = run(&w, migrate_req(), allow_full(), d.clone()).await;
    assert_eq!(o.status, OutcomeStatus::Switched, "{:?}", o.notices);
    assert_eq!(d.asked_kinds(), vec!["close_consumers", "force_stop"]);
    match &d.asked.lock().unwrap()[1] {
        DecisionRequest::ForceStop { processes, .. } => {
            assert_eq!(
                processes.iter().map(|p| p.pid).collect::<Vec<_>>(),
                vec![300]
            );
        }
        other => panic!("{other:?}"),
    }
    let calls = w.calls();
    let pos = |s: &str| {
        calls
            .iter()
            .position(|c| c == s)
            .unwrap_or_else(|| panic!("{s}: {calls:?}"))
    };
    assert!(
        pos("app.stop_process pid=300 force=false") < pos("app.stop_process pid=300 force=true")
    );
    assert!(pos("app.stop_process pid=300 force=true") < pos("config.apply"));
}

/// User bug 74: a forced stop that leaves processes running names each of
/// them — pid, image, architecture, parent and why Windows refused — the way a
/// quit report does (bug 53), and the hint for "access denied"; the processes
/// are listed again, nothing is written while one still runs.
#[tokio::test(start_paused = true)]
async fn a_forced_stop_names_the_processes_still_running_and_why() {
    let w = legacy_world();
    w.app
        .processes
        .lock()
        .unwrap()
        .push(cli_process(300, CodexProcessKind::CliTui, T0 - 1));
    w.app
        .stop_answers
        .lock()
        .unwrap()
        .insert(300, superba_core::StopOutcome::ForceOnly);
    w.app.force_left.lock().unwrap().insert(
        300,
        vec![superba_core::LingeringProcess {
            pid: 300,
            ppid: Some(12),
            image: Some(r"C:\Users\me\AppData\Roaming\npm\codex.exe".into()),
            role: "codex".into(),
            arch: Some("x64".into()),
            started_at: None,
            end_error: Some("open: Access is denied. (os error 5)".into()),
        }],
    );
    let d = ScriptedDecider::with(&[
        ("close_consumers", Decision::StopProcesses),
        ("force_stop", Decision::Force),
        ("close_consumers", Decision::Cancel),
    ]);
    let (o, ev) = run(&w, migrate_req(), allow_full(), d.clone()).await;
    assert_eq!(o.status, OutcomeStatus::Cancelled);
    assert!(!w.calls().contains(&"config.apply".to_owned()));
    let logs: Vec<superba_core::LocalizedText> = ev
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            SwitchEvent::Log { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    let line = logs
        .iter()
        .find(|t| t.key == "switch.quit.with_hint")
        .map(|t| superba_switch::messages::catalog(superba_core::Locale::En).text(t))
        .unwrap_or_else(|| panic!("no still-running line: {logs:?}"));
    for needle in [
        "codex in a terminal (pid 300): still running after Superba ended it (1)",
        "pid 300 codex.exe (codex, x64, parent 12, cannot end it: open: Access is denied. (os error 5))",
        "Windows refused to end some of them (access denied)",
    ] {
        assert!(line.contains(needle), "{needle:?} missing in {line}");
    }
}

/// Cancelling the force question ends nothing and writes nothing.
#[tokio::test(start_paused = true)]
async fn cancelling_the_force_question_ends_nothing_and_reopens_the_app() {
    let w = legacy_world();
    w.app
        .processes
        .lock()
        .unwrap()
        .push(cli_process(300, CodexProcessKind::CliTui, T0 - 1));
    w.app
        .stop_answers
        .lock()
        .unwrap()
        .insert(300, superba_core::StopOutcome::StillRunning);
    let d = ScriptedDecider::with(&[
        ("close_consumers", Decision::StopProcesses),
        ("force_stop", Decision::Cancel),
    ]);
    let (o, _) = run(&w, migrate_req(), allow_full(), d.clone()).await;
    assert_eq!(o.status, OutcomeStatus::Cancelled);
    let calls = w.calls();
    assert!(!calls.iter().any(|c| c.contains("force=true")), "{calls:?}");
    assert!(!calls.contains(&"config.apply".to_owned()));
    assert!(calls.contains(&format!("app.launch authapi={OVERSEAS}")));
    assert!(
        w.app.processes.lock().unwrap().iter().any(|p| p.pid == 300),
        "the terminal codex still runs"
    );
}

/// "I closed them" after a failed stop lists again (the user closed it).
#[tokio::test(start_paused = true)]
async fn check_again_after_a_failed_stop_lists_the_processes_again() {
    let w = legacy_world();
    w.app
        .processes
        .lock()
        .unwrap()
        .push(cli_process(300, CodexProcessKind::CliTui, T0 - 1));
    w.app
        .stop_answers
        .lock()
        .unwrap()
        .insert(300, superba_core::StopOutcome::StillRunning);
    let d = ScriptedDecider::with(&[
        ("close_consumers", Decision::StopProcesses),
        ("force_stop", Decision::Continue),
        ("close_consumers", Decision::Cancel),
    ]);
    let (o, _) = run(&w, migrate_req(), allow_full(), d.clone()).await;
    assert_eq!(o.status, OutcomeStatus::Cancelled);
    assert_eq!(
        d.asked_kinds(),
        vec!["close_consumers", "force_stop", "close_consumers"]
    );
}

/// GH#4: after a local switch the outcome says what every consumer runs.
#[tokio::test(start_paused = true)]
async fn the_outcome_reports_which_consumers_still_run_the_previous_configuration() {
    let w = World::new();
    w.app.processes.lock().unwrap().push(cli_process(
        300,
        CodexProcessKind::CliTui,
        T0 - 3_600_000,
    ));
    let (o, _) = run(
        &w,
        req("work"),
        ExecuteOptions::default(),
        ScriptedDecider::with(&[]),
    )
    .await;
    assert_eq!(o.status, OutcomeStatus::Switched, "{:?}", o.notices);
    let view = o.runtime.expect("runtime view after a local switch");
    assert!(view.busy_checked);
    let tui = view
        .consumers
        .iter()
        .find(|c| c.pid == 300)
        .expect("the terminal codex is listed");
    assert!(tui.is_previous(), "{tui:?}");
    assert_eq!(tui.advice, superba_core::RestartAdvice::Reopen);
    let app = view
        .consumers
        .iter()
        .find(|c| c.role == superba_core::ConsumerRole::DesktopApp)
        .expect("the app is listed");
    assert_eq!(app.effect, superba_core::RuntimeEffect::Current, "{app:?}");
}

#[tokio::test(start_paused = true)]
async fn migration_failure_rolls_everything_back_and_reopens_the_app() {
    let w = legacy_world();
    *w.sessions.fail_migrate.lock().unwrap() = Some(Error::Locked {
        what: "state_5.sqlite".into(),
    });
    let (o, _) = run(&w, migrate_req(), allow_full(), ScriptedDecider::with(&[])).await;
    assert_eq!(o.status, OutcomeStatus::RolledBack);
    let calls = w.calls();
    let pos = |s: &str| calls.iter().position(|c| c == s).unwrap();
    assert!(pos("env.revert") < pos("config.rollback"), "reverse order");
    assert!(calls.iter().any(|c| c.starts_with("app.launch")));
}

#[tokio::test(start_paused = true)]
async fn migration_progress_reaches_the_events_and_ends_done() {
    let w = legacy_world();
    let (o, ev) = run(&w, migrate_req(), allow_full(), ScriptedDecider::with(&[])).await;
    assert_eq!(o.status, OutcomeStatus::Switched, "{:?}", o.notices);
    let reports: Vec<superba_core::SessionProgress> = ev
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            SwitchEvent::Sessions { progress } => Some(progress.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(reports.len(), 3);
    assert!(reports.windows(2).all(|p| p[1].done >= p[0].done));
    let last = reports.last().unwrap();
    assert_eq!(last.stage, superba_core::SessionStage::Done);
    assert_eq!((last.done, last.total), (12, 12));
}

#[tokio::test(start_paused = true)]
async fn stopping_the_migration_undoes_the_switch_and_reopens_the_app() {
    let w = legacy_world();
    let sw = w.switcher();
    let plan = plan_with(&sw, migrate_req()).await;
    let events = Arc::new(RecordingEvents::default());
    events.stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let o = sw
        .execute(
            plan,
            allow_full(),
            ScriptedDecider::with(&[]),
            events.clone(),
        )
        .await
        .expect("execute");
    assert_eq!(o.status, OutcomeStatus::Cancelled, "{:?}", o.notices);
    assert!(notice_keys(&o).contains(&"switch.sessions.stopped".to_owned()));
    let calls = w.calls();
    let pos = |s: &str| calls.iter().position(|c| c == s).unwrap();
    assert!(calls.contains(&"sessions.stopped".to_owned()));
    assert!(pos("env.revert") < pos("config.rollback"), "reverse order");
    assert!(calls.iter().any(|c| c.starts_with("app.launch")));
    // No receipt to roll back: the migration undid itself.
    assert!(!calls.iter().any(|c| c.starts_with("sessions.rollback")));
    let steps: Vec<_> = events
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            SwitchEvent::Step {
                step: superba_core::SwitchStep::MigrateSessions,
                status,
                ..
            } => Some(status.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(steps.last(), Some(&superba_core::StepStatus::Skipped));
    assert_eq!(journal_status(&w), vec![JournalStatus::RolledBack]);
}

#[tokio::test(start_paused = true)]
async fn write_only_does_not_migrate_sessions() {
    let w = legacy_world();
    let opts = ExecuteOptions {
        write_only: true,
        ..ExecuteOptions::default()
    };
    let (o, _) = run(&w, migrate_req(), opts, ScriptedDecider::with(&[])).await;
    assert_eq!(o.status, OutcomeStatus::WrittenOnly);
    assert!(!w.calls().contains(&"sessions.migrate".to_owned()));
    assert!(notice_keys(&o).contains(&"switch.warn.write_only_migration".to_owned()));
}

// --- app not running ------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn app_not_running_writes_and_says_it_applies_next_time() {
    let w = World::new();
    w.set_session(None);
    let (o, ev) = run(
        &w,
        req("work"),
        ExecuteOptions::default(),
        ScriptedDecider::with(&[]),
    )
    .await;
    assert_eq!(o.status, OutcomeStatus::Switched);
    assert_eq!(o.restart, RestartKind::None);
    assert_eq!(w.writes(), vec!["config.apply", "env.apply"]);
    assert!(notice_keys(&o).contains(&"switch.done.not_running".to_owned()));
    assert!(!ev.phases().contains(&SwitchPhase::TurnCheck));
}

#[tokio::test(start_paused = true)]
async fn restart_app_intent_launches_a_stopped_app() {
    let w = World::new();
    w.set_session(None);
    let mut r = req("work");
    r.intent = SwitchIntent::RestartApp { install_id: None };
    let (o, _) = run(&w, r, ExecuteOptions::default(), ScriptedDecider::with(&[])).await;
    assert_eq!(o.status, OutcomeStatus::Switched);
    assert!(w.calls().iter().any(|c| c.starts_with("app.launch")));
}

// --- refusals ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn blocked_plans_and_open_questions_are_refused() {
    let w = World::new();
    w.secrets.map.lock().unwrap().clear();
    let sw = w.switcher();
    let plan = plan_with(&sw, req("work")).await;
    let err = sw
        .execute(
            plan,
            ExecuteOptions::default(),
            ScriptedDecider::with(&[]),
            Arc::new(NoEvents),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "blocked");

    let w = World::new();
    w.app.installs.lock().unwrap()[0].bundled_codex_version = None;
    let sw = w.switcher();
    let plan = plan_with(&sw, req("work")).await;
    let err = sw
        .execute(
            plan,
            ExecuteOptions::default(),
            ScriptedDecider::with(&[]),
            Arc::new(NoEvents),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid");
    assert!(w.writes().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_plan_executes_once_and_only_on_its_service() {
    let w = World::new();
    let sw = w.switcher();
    let plan = plan_with(&sw, req("work")).await;
    sw.execute(
        plan.clone(),
        ExecuteOptions::default(),
        ScriptedDecider::with(&[]),
        Arc::new(NoEvents),
    )
    .await
    .unwrap();
    let again = sw
        .execute(
            plan.clone(),
            ExecuteOptions::default(),
            ScriptedDecider::with(&[]),
            Arc::new(NoEvents),
        )
        .await
        .unwrap_err();
    assert_eq!(again.code(), "not_found");
    let other = w.switcher();
    assert_eq!(
        other
            .execute(
                plan,
                ExecuteOptions::default(),
                ScriptedDecider::with(&[]),
                Arc::new(NoEvents)
            )
            .await
            .unwrap_err()
            .code(),
        "not_found"
    );
}

#[tokio::test(start_paused = true)]
async fn another_superba_holding_the_state_lock_is_reported() {
    let w = World::new();
    let sw = w.switcher();
    let plan = plan_with(&sw, req("work")).await;
    let _held = StateLock::acquire(&w.ctx.data_dir).unwrap();
    let err = sw
        .execute(
            plan,
            ExecuteOptions::default(),
            ScriptedDecider::with(&[]),
            Arc::new(NoEvents),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "locked");
    assert!(w.writes().is_empty());
}

// --- remote hosts -------------------------------------------------------------------------

fn remote_req(keys: &[&str]) -> SwitchRequest {
    let mut r = req("work");
    r.targets.remotes = keys.iter().map(|k| TargetId::remote(k)).collect();
    r
}

#[tokio::test(start_paused = true)]
async fn full_restart_uses_the_fresh_app_session_for_connected_remotes() {
    let w = no_cdp_world();
    *w.remote.hosts.lock().unwrap() = vec![remote_host("devbox", Some("h-dev"))];
    let (o, _) = run(
        &w,
        remote_req(&["devbox"]),
        allow_full(),
        ScriptedDecider::with(&[]),
    )
    .await;
    assert_eq!(o.status, OutcomeStatus::Switched);
    assert_eq!(o.remotes[0].status, OutcomeStatus::Switched);
    let calls = w.calls();
    let launched = calls
        .iter()
        .position(|c| c.starts_with("app.launch "))
        .unwrap();
    let applied = calls
        .iter()
        .position(|c| c == "remote.apply devbox")
        .unwrap();
    let restarted = calls
        .iter()
        .position(|c| c == "app.hot_restart h-dev")
        .unwrap();
    assert!(launched < applied && applied < restarted, "{calls:#?}");
    assert_eq!(*w.app.hot_restart_sessions.lock().unwrap(), vec![200]);
}

#[tokio::test(start_paused = true)]
async fn remote_hosts_switch_in_parallel_and_a_failure_is_partial() {
    let w = World::new();
    *w.remote.hosts.lock().unwrap() = vec![
        remote_host("devbox", Some("h-dev")),
        remote_host("gpu", Some("h-gpu")),
        remote_host("old", Some("h-old")),
    ];
    w.remote.fail_apply.lock().unwrap().insert(
        "gpu".into(),
        Error::Ssh {
            host: "gpu".into(),
            message: "timeout".into(),
        },
    );
    w.remote
        .verify_ok
        .lock()
        .unwrap()
        .insert("old".into(), false);
    let (o, _) = run(
        &w,
        remote_req(&["devbox", "gpu", "old"]),
        ExecuteOptions::default(),
        ScriptedDecider::with(&[]),
    )
    .await;
    assert_eq!(o.status, OutcomeStatus::Partial);
    let status = |k: &str| {
        o.remotes
            .iter()
            .find(|r| r.target_id == TargetId::remote(k))
            .unwrap()
            .status
    };
    assert_eq!(status("devbox"), OutcomeStatus::Switched);
    assert_eq!(status("gpu"), OutcomeStatus::Failed);
    assert_eq!(status("old"), OutcomeStatus::Failed);
    let calls = w.calls();
    assert!(calls.contains(&"app.hot_restart h-dev".to_owned()));
    // A failed verification rolls that host back and restarts it again.
    assert!(calls.contains(&"remote.rollback old".to_owned()));
    assert_eq!(
        calls
            .iter()
            .filter(|c| *c == "app.hot_restart h-old")
            .count(),
        2
    );
    assert!(notice_keys(&o).contains(&"switch.done.partial".to_owned()));
    let journal = SwitchJournal::new(&w.ctx.data_dir);
    let before = journal.entries().unwrap();
    assert_eq!(before[0].status, JournalStatus::Partial);
    w.log.lock().unwrap().clear();
    assert!(w.switcher().recover().await.unwrap().entries.is_empty());
    assert!(!w.calls().iter().any(|c| c.starts_with("remote.rollback")));
    assert_eq!(journal.entries().unwrap(), before);
}

#[tokio::test(start_paused = true)]
async fn unknown_remote_activity_asks_and_cancel_writes_nothing() {
    let w = World::new();
    *w.remote.hosts.lock().unwrap() = vec![remote_host("devbox", Some("h-dev"))];
    w.remote.incomplete.lock().unwrap().push("devbox".into());
    let d = ScriptedDecider::with(&[]);
    let (o, _) = run(
        &w,
        remote_req(&["devbox"]),
        ExecuteOptions::default(),
        d.clone(),
    )
    .await;
    assert_eq!(o.status, OutcomeStatus::Cancelled);
    assert_eq!(d.asked_kinds(), vec!["unknown_remote_activity"]);
    assert!(w.writes().is_empty());
}

#[tokio::test(start_paused = true)]
async fn remote_rollback_uses_the_journal_of_the_last_switch() {
    let w = World::new();
    *w.remote.hosts.lock().unwrap() = vec![remote_host("devbox", Some("h-dev"))];
    let (o, _) = run(
        &w,
        remote_req(&["devbox"]),
        ExecuteOptions::default(),
        ScriptedDecider::with(&[]),
    )
    .await;
    assert_eq!(o.status, OutcomeStatus::Switched);
    let mut r = req("work");
    r.intent = SwitchIntent::RollbackRemote {
        target_id: TargetId::remote("devbox"),
    };
    let (o, _) = run(
        &w,
        r.clone(),
        ExecuteOptions::default(),
        ScriptedDecider::with(&[]),
    )
    .await;
    assert_eq!(o.status, OutcomeStatus::Switched, "{:?}", o.remotes);
    assert!(w.calls().contains(&"remote.rollback devbox".to_owned()));
    // Nothing left to roll back afterwards.
    let p = plan_with(&w.switcher(), r).await;
    assert!(p.is_blocked());
}

// --- stale CLI processes (plan 7.7) --------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn terminals_started_before_the_switch_are_reported_stale() {
    let w = World::new();
    w.app
        .processes
        .lock()
        .unwrap()
        .push(cli_process(300, CodexProcessKind::CliTui, T0 - 60_000));
    let (o, _) = run(
        &w,
        req("work"),
        ExecuteOptions::default(),
        ScriptedDecider::with(&[]),
    )
    .await;
    assert_eq!(
        o.stale_processes.iter().map(|p| p.pid).collect::<Vec<_>>(),
        vec![300]
    );
    assert!(notice_keys(&o).contains(&"switch.banner.stale".to_owned()));
}

// --- restore -----------------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn restore_official_with_the_app_running_is_a_confirmed_full_restart() {
    let w = World::new();
    let mut r = req("work");
    r.intent = SwitchIntent::Restore {
        target: RestoreTarget::Official,
    };
    let (o, _) = run(&w, r, allow_full(), ScriptedDecider::with(&[])).await;
    assert_eq!(o.status, OutcomeStatus::Switched, "{:?}", o.notices);
    assert_eq!(
        o.restart,
        RestartKind::FullAppRestart {
            reason: FullRestartReason::CrossAuthFamily
        }
    );
    assert!(notice_keys(&o).contains(&"switch.done.restored".to_owned()));
    // The official login keeps the persisted auth address (no env edit).
    assert!(!w.calls().contains(&"env.apply".to_owned()));
}

#[tokio::test(start_paused = true)]
async fn uninstall_restores_the_baseline_and_removes_launcher_entries_and_backups() {
    let w = World::new();
    w.set_session(None);
    let backups = w.ctx.data_dir.backups_dir();
    std::fs::create_dir_all(backups.join("switch")).unwrap();
    let profiles = w.profiles.set.lock().unwrap().clone();
    let saved_keys = w.secrets.map.lock().unwrap().clone();
    let mut r = req("work");
    r.intent = SwitchIntent::Uninstall {
        delete_backups: true,
        provider: Some(superba_core::ProviderRestore::KeepOpenai),
    };
    let (o, _) = run(&w, r, ExecuteOptions::default(), ScriptedDecider::with(&[])).await;
    assert_eq!(o.status, OutcomeStatus::Switched, "{:?}", o.notices);
    assert_eq!(
        w.config.last_desired.lock().unwrap().as_deref(),
        Some("original:KeepOpenai")
    );
    // The auth address goes back to its baseline, and a CODEX_CLI_PATH Superba
    // may have persisted for the pin is removed last.
    let ops = w.env.plans.lock().unwrap().clone();
    assert!(
        ops.iter()
            .any(|op| matches!(op, superba_core::EnvOp::RestoreBaseline { .. })),
        "{ops:?}"
    );
    assert!(
        w.env
            .applied
            .lock()
            .unwrap()
            .iter()
            .any(|(var, op)| var == superba_core::codex::ENV_CODEX_CLI_PATH
                && *op == superba_core::EnvOp::Clear)
    );
    assert!(
        w.calls()
            .contains(&"app.set_launcher_entry false".to_owned())
    );
    assert!(!backups.exists());
    assert!(w.config.baseline.lock().unwrap().as_ref().unwrap().released);
    assert_eq!(*w.profiles.set.lock().unwrap(), profiles);
    assert_eq!(*w.secrets.map.lock().unwrap(), saved_keys);
    assert!(w.secrets.deleted.lock().unwrap().is_empty());

    // The next takeover captures the user's current state before applying. The
    // config engine can then recreate first-write backups for the new baseline.
    w.config.state.lock().unwrap().model_provider = Some("custom".into());
    *w.sessions.report.lock().unwrap() =
        session_report(w.ctx.codex_home().root(), &[("openai", 8)]);
    w.log.lock().unwrap().clear();
    let (o, _) = run(
        &w,
        req("work"),
        ExecuteOptions::default(),
        ScriptedDecider::with(&[]),
    )
    .await;
    assert_eq!(o.status, OutcomeStatus::Switched);
    assert_eq!(
        &w.writes()[..2],
        &["config.snapshot_baseline", "config.apply"]
    );
    let baseline = w.config.baseline.lock().unwrap().clone().unwrap();
    assert!(!baseline.released);
    assert_eq!(baseline.provider_key.as_deref(), Some("custom"));
    assert_eq!(baseline.session_distribution.get("openai"), Some(&8));
}

#[tokio::test(start_paused = true)]
async fn unobservable_app_servers_skip_the_process_check_instead_of_rolling_back() {
    let w = World::new();
    // Process listing never shows app-servers on this system.
    w.app
        .processes
        .lock()
        .unwrap()
        .retain(|p| !matches!(p.kind, CodexProcessKind::AppServer));
    *w.app.restart_spawns.lock().unwrap() = false;
    let (o, _) = run(
        &w,
        req("work"),
        ExecuteOptions::default(),
        ScriptedDecider::with(&[]),
    )
    .await;
    assert_eq!(o.status, OutcomeStatus::Switched, "{:?}", o.notices);
    assert!(matches!(
        o.verification.as_ref().unwrap().process,
        CheckResult::Skipped { .. }
    ));
}

// --- a relaunch that fails (user bug 40) -----------------------------------------

#[tokio::test(start_paused = true)]
async fn a_failed_relaunch_is_reported_at_once_and_not_as_a_plain_written_switch() {
    let w = no_cdp_world();
    *w.app.fail_launch.lock().unwrap() = Some(Error::NotFound {
        what: r"desktop app install path:\\?\C:\Program Files\WindowsApps\OpenAI.Codex_1_x64__p"
            .into(),
    });
    let (o, ev) = run(&w, req("work"), allow_full(), ScriptedDecider::with(&[])).await;
    // The configuration stays (complete and journaled) ...
    assert_eq!(o.status, OutcomeStatus::WrittenOnly);
    assert_eq!(journal_status(&w), vec![JournalStatus::Done]);
    // ... the step failed, and the activity log says why before the run ends.
    let events = ev.events.lock().unwrap().clone();
    let failed_step = events.iter().position(|e| {
        matches!(
            e,
            SwitchEvent::Step {
                step: superba_core::SwitchStep::LaunchApp,
                status: superba_core::StepStatus::Failed { .. },
                ..
            }
        )
    });
    let danger_log = events.iter().position(|e| {
        matches!(e, SwitchEvent::Log { level: superba_core::Severity::Danger, text } if text.key == "switch.failed.launch")
    });
    let done = events.iter().position(|e| {
        matches!(
            e,
            SwitchEvent::Phase {
                phase: SwitchPhase::Done,
                ..
            }
        )
    });
    assert!(
        failed_step.is_some() && danger_log.is_some() && done.is_some(),
        "{events:?}"
    );
    assert!(failed_step < danger_log && danger_log < done);
    // The outcome tells the user to open the app, not to "restart" it.
    let keys = notice_keys(&o);
    assert!(
        keys.contains(&"switch.failed.launch".to_owned()),
        "{keys:?}"
    );
    assert!(
        keys.contains(&"switch.done.launch_failed".to_owned()),
        "{keys:?}"
    );
    assert!(
        !keys.contains(&"switch.done.written_only".to_owned()),
        "{keys:?}"
    );
}
