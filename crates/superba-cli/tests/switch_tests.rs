mod common;

use common::*;
use superba_core::*;
use superba_switch::messages;

#[tokio::test]
async fn explicit_profile_and_every_answer_become_a_request() {
    let service = FakeService::new();
    let (code, _, _) = invoke(
        service.clone(),
        &[
            "switch",
            "--profile",
            "explicit",
            "--site",
            "ignored",
            "--line",
            "cdn",
            "--auth",
            "apikey",
            "--model",
            "test-model",
            "--migrate-legacy-sessions",
            "--take-over-env",
            "--accept-unknown-version",
            "--accept-shared-home",
            "--keep-key",
            "model",
            "--keep-key",
            "features.fast_mode",
            "--overwrite-key",
            "openai_base_url",
            "--exclude-thread",
            "thread-a",
            "--exclude-thread",
            "thread-b",
            "--no-local",
            "--remote",
            "ssh:a",
            "--remote",
            "ssh:b",
            "--dry-run",
        ],
    )
    .await;
    assert_eq!(code, 0);
    let calls = service.calls();
    let request = &calls.plans[0];
    assert_eq!(
        request.intent,
        SwitchIntent::Profile {
            profile_id: ProfileId::new("explicit"),
            line_id: Some(LineId::new("cdn")),
            auth_mode: Some(AuthMode::ApiKey)
        }
    );
    assert_eq!(
        request.targets,
        TargetSelection {
            local: false,
            remotes: vec![TargetId::new("ssh:a"), TargetId::new("ssh:b")]
        }
    );
    assert_eq!(request.model_override.as_deref(), Some("test-model"));
    assert_eq!(request.legacy_sessions, Some(LegacySessionChoice::Migrate));
    assert_eq!(request.take_over_foreign_env, Some(true));
    assert!(request.accept_unknown_versions && request.accept_shared_home);
    assert_eq!(
        request.key_resolutions,
        [
            ("features.fast_mode".into(), KeyResolution::KeepCurrent),
            ("model".into(), KeyResolution::KeepCurrent),
            ("openai_base_url".into(), KeyResolution::Overwrite)
        ]
        .into()
    );
    assert_eq!(request.migration_exclude, ["thread-a", "thread-b"]);
    assert_eq!(calls.names, ["plan", "plan_settings"]);
}

#[tokio::test]
async fn applied_profile_is_used_and_absent_answers_stay_absent() {
    let service = FakeService::new();
    assert_eq!(invoke(service.clone(), &["switch", "--dry-run"]).await.0, 0);
    assert_eq!(
        service.calls().plans[0],
        SwitchRequest::profile(ProfileId::new("work"))
    );
    assert_eq!(service.calls().names, ["overview", "plan", "plan_settings"]);
    assert_eq!(
        invoke(
            service.clone(),
            &[
                "switch",
                "--skip-legacy-sessions",
                "--keep-env",
                "--dry-run"
            ]
        )
        .await
        .0,
        0
    );
    let request = &service.calls().plans[1];
    assert_eq!(request.legacy_sessions, Some(LegacySessionChoice::Skip));
    assert_eq!(request.take_over_foreign_env, Some(false));
}

#[tokio::test]
async fn site_selection_prefers_matching_active_then_first_user_profile() {
    let service = FakeService::new();
    {
        let mut set = service.profiles.lock().unwrap();
        set.profiles.insert(0, profile("earlier", "krill"));
        let mut builtin = profile("builtin", "custom");
        builtin.builtin = Some(BuiltinProfile::Official);
        set.profiles.push(builtin);
        set.profiles.push(profile("custom-first", "custom"));
        set.profiles.push(profile("custom-second", "custom"));
    }
    assert_eq!(
        invoke(service.clone(), &["switch", "--site", "krill", "--dry-run"])
            .await
            .0,
        0
    );
    assert_eq!(
        service.calls().plans[0].intent,
        SwitchRequest::profile(ProfileId::new("work")).intent
    );
    assert_eq!(
        invoke(
            service.clone(),
            &["switch", "--site", "custom", "--dry-run"]
        )
        .await
        .0,
        0
    );
    assert_eq!(
        service.calls().plans[1].intent,
        SwitchRequest::profile(ProfileId::new("custom-first")).intent
    );
    service.overview.lock().unwrap().active = None;
    assert_eq!(
        invoke(service.clone(), &["switch", "--site", "krill", "--dry-run"])
            .await
            .0,
        0
    );
    assert_eq!(
        service.calls().plans[2].intent,
        SwitchRequest::profile(ProfileId::new("earlier")).intent
    );
}

#[tokio::test]
async fn missing_profile_selection_and_conflicting_key_answers_are_usage_errors() {
    let service = FakeService::new();
    service.overview.lock().unwrap().active = None;
    for args in [vec!["switch"], vec!["switch", "--site", "unknown"]] {
        let (code, stdout, stderr) = invoke(service.clone(), &args).await;
        assert_eq!(code, 2);
        assert!(stdout.is_empty() && stderr.contains("spb profile add"));
    }
    assert_eq!(
        invoke(
            service.clone(),
            &[
                "switch",
                "--profile",
                "work",
                "--keep-key",
                "model",
                "--overwrite-key",
                "model"
            ]
        )
        .await
        .0,
        2
    );
    assert!(service.calls().plans.is_empty());
}

#[tokio::test]
async fn profile_use_is_identical_to_explicit_switch() {
    let service = FakeService::new();
    let flags = [
        "--line",
        "cdn",
        "--auth",
        "pat",
        "--model",
        "test-model",
        "--skip-legacy-sessions",
        "--keep-env",
        "--accept-shared-home",
        "--accept-unknown-version",
        "--keep-key",
        "model",
        "--exclude-thread",
        "t",
        "--no-local",
        "--remote",
        "ssh:a",
        "--dry-run",
    ];
    let mut first = vec!["switch", "--profile", "work"];
    first.extend(flags);
    let mut second = vec!["profile", "use", "work"];
    second.extend(flags);
    assert_eq!(invoke(service.clone(), &first).await.0, 0);
    assert_eq!(invoke(service.clone(), &second).await.0, 0);
    assert_eq!(service.calls().plans[0], service.calls().plans[1]);
}

#[tokio::test]
async fn blocked_has_priority_and_never_executes_even_with_all_permissions() {
    let service = FakeService::new();
    service.script(|request| {
        let mut p = plan(request);
        p.primary = RestartKind::Blocked {
            reasons: vec![BlockReason::MissingKey, BlockReason::NoBaseline],
        };
        p.choices.push(PlanChoice::UnknownVersion {
            consumer: "remote".into(),
        });
        Ok(p)
    });
    for json in [false, true] {
        let mut args = vec!["switch", "--yes", "--write-only", "--force", "--dry-run"];
        if json {
            args.push("--json");
        }
        let (code, stdout, stderr) = invoke(service.clone(), &args).await;
        assert_eq!(code, 3);
        if json {
            assert_eq!(one_json(&stdout)["status"], "blocked");
            assert!(stderr.is_empty());
        } else {
            let catalog = messages::catalog(Locale::En);
            assert!(stderr.contains(&catalog.text(&BlockReason::MissingKey.text())));
            assert!(stderr.contains(&catalog.text(&BlockReason::NoBaseline.text())));
        }
    }
    assert!(service.calls().executions.is_empty());
}

#[tokio::test]
async fn every_unanswered_choice_prints_its_answer_flags() {
    let cases = [
        (
            PlanChoice::LegacySessions {
                providers: [("legacy".into(), 2)].into(),
            },
            vec!["--migrate-legacy-sessions", "--skip-legacy-sessions"],
        ),
        (
            PlanChoice::ReplacementModel {
                current: "old".into(),
                suggestions: vec!["new".into()],
            },
            vec!["--model"],
        ),
        (
            PlanChoice::ForeignEnv { lines: Vec::new() },
            vec!["--take-over-env", "--keep-env"],
        ),
        (
            PlanChoice::UnknownVersion {
                consumer: "remote".into(),
            },
            vec!["--accept-unknown-version"],
        ),
        (
            PlanChoice::OwnedKeyConflict {
                conflicts: vec![KeyChange {
                    path: "model".into(),
                    before: None,
                    after: Some("test".into()),
                }],
                auth: None,
                source: Default::default(),
            },
            vec!["--take-over", "--keep-key", "hand-off"],
        ),
        (
            PlanChoice::SharedHome {
                targets: vec![TargetId::new("ssh:a")],
            },
            vec!["--accept-shared-home"],
        ),
        (
            PlanChoice::RestoreProvider {
                baseline_provider: "legacy".into(),
                created_during: 2,
                migrated_in: 3,
            },
            vec!["--provider keep-openai"],
        ),
    ];
    for (choice, flags) in cases {
        let service = FakeService::new();
        service.script(move |request| {
            let mut p = plan(request);
            p.choices = vec![choice.clone()];
            Ok(p)
        });
        let (code, stdout, _) = invoke(service.clone(), &["switch", "--yes", "--dry-run"]).await;
        assert_eq!(code, 4);
        for flag in flags {
            assert!(stdout.contains(flag), "missing {flag}");
        }
        let (code, stdout, stderr) = invoke(service.clone(), &["switch", "--json"]).await;
        assert_eq!(code, 4);
        assert_eq!(one_json(&stdout)["status"], "needs_answers");
        assert!(stderr.is_empty() && service.calls().executions.is_empty());
    }
}

fn full_restart(service: &FakeService) {
    service.script(|request| {
        let mut p = plan(request);
        p.primary = RestartKind::FullAppRestart {
            reason: FullRestartReason::NoCdp,
        };
        p.alternatives.push(PlanAlternative {
            label: messages::decision(Decision::KeepWritten),
            request: p.request.clone(),
        });
        Ok(p)
    });
}

#[tokio::test]
async fn dry_run_is_allowed_without_full_restart_confirmation() {
    let service = FakeService::new();
    full_restart(&service);
    let (code, stdout, stderr) = invoke(service.clone(), &["switch", "--dry-run", "--json"]).await;
    assert_eq!(code, 0);
    assert_eq!(one_json(&stdout)["status"], "planned");
    assert!(stderr.is_empty() && service.calls().executions.is_empty());
}

#[tokio::test]
async fn full_restart_requires_confirmation_and_prints_alternatives() {
    let service = FakeService::new();
    full_restart(&service);
    let (code, stdout, _) = invoke(service.clone(), &["switch"]).await;
    assert_eq!(code, 4);
    assert!(
        stdout.contains("--yes")
            && stdout.contains("--write-only")
            && stdout.contains("Alternative")
    );
    let (code, stdout, stderr) = invoke(service.clone(), &["switch", "--json"]).await;
    assert_eq!(code, 4);
    assert_eq!(one_json(&stdout)["status"], "needs_confirmation");
    assert!(stderr.is_empty() && service.calls().executions.is_empty());
}

#[tokio::test]
async fn yes_compatibility_and_write_only_flags_control_execute_options() {
    for flag in ["--yes", "-y", "--allow-full-restart", "--write-only"] {
        let service = FakeService::new();
        full_restart(&service);
        assert_eq!(
            invoke(service.clone(), &["switch", flag, "--force"])
                .await
                .0,
            0
        );
        let calls = service.calls();
        let (id, options) = &calls.executions[0];
        assert_eq!(id, "plan-1");
        assert_eq!(options.write_only, flag == "--write-only");
        assert_eq!(options.allow_full_restart, flag != "--write-only");
        assert!(options.force_turns);
        assert_eq!(
            options.turn_quiet_ms,
            ExecuteOptions::default().turn_quiet_ms
        );
    }
}

#[tokio::test]
async fn every_outcome_status_maps_to_its_exit_code_and_one_json_value() {
    for (status, expected, serialized) in [
        (OutcomeStatus::Switched, 0, "switched"),
        (OutcomeStatus::WrittenOnly, 0, "written_only"),
        (OutcomeStatus::Cancelled, 5, "cancelled"),
        (OutcomeStatus::RolledBack, 1, "rolled_back"),
        (OutcomeStatus::Failed, 1, "failed"),
        (OutcomeStatus::Partial, 1, "partial"),
    ] {
        let service = FakeService::new();
        *service.outcome.lock().unwrap() = Ok(outcome(status));
        service.events.lock().unwrap().push(SwitchEvent::Phase {
            phase: SwitchPhase::Applying,
            at: Timestamp::EPOCH,
        });
        let (code, stdout, stderr) = invoke(service.clone(), &["switch", "--json"]).await;
        assert_eq!(code, expected);
        let value = one_json(&stdout);
        assert_eq!(value["status"], serialized);
        assert_eq!(value["outcome"]["status"], serialized);
        assert!(stderr.is_empty());
        assert_eq!(service.calls().executions.len(), 1);
    }
}

#[tokio::test]
async fn planning_execution_and_read_errors_use_shared_exit_mapping() {
    for (error, expected) in [
        (
            Error::Blocked {
                reasons: vec![BlockReason::MissingKey],
            },
            3,
        ),
        (Error::Cancelled, 5),
        (Error::invalid("scripted input failure"), 1),
        (Error::other("scripted failure"), 1),
    ] {
        for stage in ["plan", "execute", "overview"] {
            let service = FakeService::new();
            let error = error.clone();
            match stage {
                "plan" => {
                    let error = error.clone();
                    service.script(move |_| Err(error.clone()));
                }
                "execute" => *service.outcome.lock().unwrap() = Err(error.clone()),
                _ => *service.error.lock().unwrap() = Some(error.clone()),
            }
            let command = if stage == "overview" {
                "status"
            } else {
                "switch"
            };
            let (code, stdout, stderr) = invoke(service.clone(), &[command, "--json"]).await;
            assert_eq!(code, expected, "{stage}");
            assert_eq!(one_json(&stdout), serde_json::json!({ "error": error }));
            assert!(stderr.is_empty());
            if stage != "execute" {
                assert!(service.calls().executions.is_empty());
            }
        }
    }
}

#[tokio::test]
async fn waiting_and_close_consumers_hints_are_on_stderr() {
    let service = FakeService::new();
    *service.decisions.lock().unwrap() = vec![
        DecisionRequest::WaitForTurns {
            busy: vec![thread()],
            waited_ms: 0,
        },
        DecisionRequest::CloseConsumers {
            processes: vec![process()],
        },
    ];
    let (_, stdout, stderr) = invoke(service.clone(), &["switch"]).await;
    assert!(
        stderr.contains("--wait-turn") && stderr.contains("--force") && stderr.contains("424243")
    );
    assert!(!stdout.contains("--wait-turn"));
    assert_eq!(
        service.calls().answers,
        [Decision::Cancel, Decision::Cancel]
    );
}

#[tokio::test]
async fn hot_countdown_waits_before_execute_and_is_quiet_in_json() {
    for json in [false, true] {
        let service = FakeService::new();
        service.script(|request| {
            let mut p = plan(request);
            p.primary = RestartKind::AppServerHot {
                host_id: "local".into(),
                kill_remote: false,
            };
            Ok(p)
        });
        let mut args = vec!["switch", "--countdown", "1"];
        if json {
            args.push("--json");
        }
        let started = std::time::Instant::now();
        let (result, ()) = tokio::join!(invoke(service.clone(), &args), async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            assert!(service.calls().executions.is_empty());
        });
        assert_eq!(result.0, 0);
        assert!(started.elapsed() >= std::time::Duration::from_secs(1));
        if json {
            one_json(&result.1);
            assert!(result.2.is_empty());
        } else {
            assert!(result.2.contains("in 1 s"));
        }
        assert_eq!(service.calls().executions.len(), 1);
    }
}

#[tokio::test]
async fn interrupted_switches_are_recovered_before_a_real_switch() {
    let service = FakeService::new();
    let (code, _, stderr) = invoke(service.clone(), &["switch", "--countdown", "0"]).await;
    assert_eq!(code, 0);
    let names = service.calls().names;
    let recover = names.iter().position(|n| *n == "recover").expect("recover");
    let plan = names.iter().position(|n| *n == "plan").expect("plan");
    assert!(recover < plan, "{names:?}");
    assert!(stderr.contains("Recovered 1 interrupted"), "{stderr}");

    // A dry run never writes, so it never recovers either.
    let service = FakeService::new();
    assert_eq!(invoke(service.clone(), &["switch", "--dry-run"]).await.0, 0);
    assert!(!service.calls().names.contains(&"recover"));
}

#[tokio::test]
async fn known_busy_turns_refuse_before_countdown_and_never_execute() {
    for json in [false, true] {
        let service = FakeService::new();
        service.recovery.lock().unwrap().entries.clear();
        service.script(|request| {
            let mut plan = plan(request);
            plan.primary = RestartKind::AppServerHot {
                host_id: "local".into(),
                kill_remote: false,
            };
            plan.active_threads.push(thread());
            Ok(plan)
        });
        let mut args = vec!["switch", "--countdown", "30"];
        if json {
            args.push("--json");
        }
        let (code, stdout, stderr) = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            invoke(service.clone(), &args),
        )
        .await
        .expect("known busy turns must be refused before countdown");
        assert_eq!(code, 5);
        assert!(service.calls().executions.is_empty());
        assert!(!stderr.contains("Switching to") && !stderr.contains("Waiting for"));
        if json {
            let value = one_json(&stdout);
            assert_eq!(value["status"], "cancelled");
            assert_eq!(value["outcome"]["status"], "cancelled");
            assert!(stderr.is_empty());
        } else {
            assert!(
                stdout.contains("Example thread")
                    && stdout.contains("Cancelled; nothing was changed")
            );
            assert!(stderr.contains("--wait-turn") && stderr.contains("--force"));
        }
        for flag in ["--wait-turn", "--force"] {
            assert_eq!(invoke(service.clone(), &["switch", flag]).await.0, 0);
        }
        assert_eq!(service.calls().executions.len(), 2);
        assert_eq!(invoke(service.clone(), &["switch", "--dry-run"]).await.0, 0);
        assert_eq!(service.calls().executions.len(), 2);
    }
}

#[tokio::test]
async fn alternatives_are_runnable_commands_preserving_targets_and_answers() {
    let service = FakeService::new();
    service.script(|request| {
        let mut plan = plan(request);
        plan.primary = RestartKind::FullAppRestart {
            reason: FullRestartReason::AuthApiUnhealthy,
        };
        let mut alternative = plan.request.clone();
        if let SwitchIntent::Profile { auth_mode, .. } = &mut alternative.intent {
            *auth_mode = Some(AuthMode::ApiKey);
        }
        plan.alternatives.push(PlanAlternative {
            label: messages::text("switch.action.hot_with_apikey"),
            request: alternative,
        });
        Ok(plan)
    });
    let args = [
        "switch",
        "--profile",
        "work",
        "--line",
        "c",
        "--auth",
        "pat",
        "--model",
        "test-model",
        "--no-local",
        "--remote",
        "ssh:a",
        "--skip-legacy-sessions",
        "--keep-env",
        "--accept-shared-home",
        "--accept-unknown-version",
        "--keep-key",
        "features.fast_mode",
        "--overwrite-key",
        "model",
        "--exclude-thread",
        "thread-a",
        "--wait-turn",
        "--countdown",
        "3",
        "--dry-run",
    ];
    let (_, stdout, _) = invoke(service.clone(), &args).await;
    let command = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Alternative: "))
        .and_then(|rest| rest.split_once(" — ").map(|(_, command)| command))
        .unwrap();
    assert!(
        command.starts_with("spb switch --profile work --line c --auth apikey"),
        "{command}"
    );
    let words = command.split_whitespace().skip(1).collect::<Vec<_>>();
    assert_eq!(invoke(service.clone(), &words).await.0, 0);
    let calls = service.calls();
    let mut expected = calls.plans[0].clone();
    if let SwitchIntent::Profile { auth_mode, .. } = &mut expected.intent {
        *auth_mode = Some(AuthMode::ApiKey);
    }
    assert_eq!(calls.plans[1], expected);
    assert!(calls.executions.is_empty());
}
