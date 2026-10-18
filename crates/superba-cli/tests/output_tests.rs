mod common;

use common::*;
use std::sync::Arc;
use superba_core::*;
use superba_switch::messages;

#[tokio::test]
async fn status_json_is_exactly_the_overview_and_human_status_has_required_fields() {
    let service = FakeService::new();
    let (code, stdout, stderr) = invoke(service.clone(), &["status", "--json"]).await;
    assert_eq!(code, 0);
    assert_eq!(one_json(&stdout), serde_json::to_value(overview()).unwrap());
    assert!(stderr.is_empty());
    assert_eq!(service.calls().names, ["overview"]);
    let (_, stdout, stderr) = invoke(service, &["status"]).await;
    for value in [
        "Codex home: /fake/codex",
        "Applied profile: work",
        "Site: krill",
        "Line: Overseas (overseas)",
        "Generation: 7",
        "fake-app",
        "26.1013",
        "PID: 424242",
        "CDP port: 9234",
        "Launched by Superba: yes",
        "Hot switch: Ready",
        "Auth line: Overseas (overseas)",
        "/fake/bin/codex",
        "0.162.0",
        "PATH rank",
        "CLI processes: 1",
        "Stale processes: 1",
        "ssh:a",
        "Test host",
        "[warning]",
    ] {
        assert!(stdout.contains(value), "missing {value}");
    }
    assert!(stderr.is_empty());
}

#[tokio::test]
async fn unavailable_hot_switch_uses_the_shared_reason() {
    let service = FakeService::new();
    service.overview.lock().unwrap().app.hot_switch = HotSwitchStatus::Unavailable {
        reason: FullRestartReason::NoCdp,
    };
    let (_, stdout, _) = invoke(service, &["status", "--locale", "zh-CN"]).await;
    assert!(
        stdout.contains(&messages::catalog(Locale::ZhCn).text(&FullRestartReason::NoCdp.text()))
    );
}

#[tokio::test]
async fn lines_listing_and_probe_have_distinct_calls_and_json_shapes() {
    let service = FakeService::new();
    let (code, stdout, stderr) = invoke(service.clone(), &["lines", "--json"]).await;
    assert_eq!(code, 0);
    assert_eq!(
        one_json(&stdout),
        serde_json::to_value(catalog::krill_site().lines).unwrap()
    );
    assert_eq!(service.calls().names, ["sites"]);
    assert!(stderr.is_empty());
    let (_, stdout, _) = invoke(service.clone(), &["lines"]).await;
    assert!(
        stdout.contains("Overseas")
            && stdout.contains("(default)")
            && stdout.contains(catalog::KRILL_ORIGIN_OVERSEAS)
    );
    let (code, stdout, stderr) = invoke(service.clone(), &["lines", "--probe", "--json"]).await;
    assert_eq!(code, 0);
    assert_eq!(
        one_json(&stdout),
        serde_json::to_value(&*service.line_caps.lock().unwrap()).unwrap()
    );
    assert_eq!(service.calls().probes, [SiteId::new("krill")]);
    assert!(stderr.is_empty());
    let (_, stdout, _) = invoke(service.clone(), &["lines", "--probe"]).await;
    for value in [
        "123",
        "PAT ok",
        "API key ok",
        "Codex path",
        "Edge blocked",
        "No problems found",
    ] {
        assert!(stdout.contains(value));
    }
    let (code, stdout, _) = invoke(service, &["lines", "--site", "missing", "--json"]).await;
    assert_eq!(code, 1);
    assert_eq!(one_json(&stdout)["error"]["code"], "not_found");
}

#[tokio::test]
async fn sessions_remote_doctor_and_recover_json_match_contracts() {
    let service = FakeService::new();
    for args in [
        vec!["sessions", "--json"],
        vec!["sessions", "scan", "--json"],
    ] {
        let (code, stdout, stderr) = invoke(service.clone(), &args).await;
        assert_eq!(code, 0);
        let sessions = service.sessions.lock().unwrap().clone();
        assert_eq!(
            one_json(&stdout),
            serde_json::json!({ "report": sessions.0, "history": sessions.1 })
        );
        assert!(stderr.is_empty());
    }
    let (_, stdout, _) = invoke(service.clone(), &["remote", "list", "--json"]).await;
    assert_eq!(
        one_json(&stdout),
        serde_json::to_value(&service.v3.lock().unwrap().hosts).unwrap()
    );
    let (_, stdout, _) = invoke(service.clone(), &["doctor", "--json"]).await;
    assert_eq!(one_json(&stdout), serde_json::json!([]));
    let (_, stdout, _) = invoke(service.clone(), &["doctor"]).await;
    assert_eq!(stdout.trim(), "No problems found");
    let (_, stdout, _) = invoke(service.clone(), &["recover", "--json"]).await;
    assert_eq!(
        one_json(&stdout),
        serde_json::to_value(&*service.recovery.lock().unwrap()).unwrap()
    );
}

#[tokio::test]
async fn sessions_remote_doctor_and_recover_human_details_are_complete() {
    let service = FakeService::new();
    let (_, stdout, _) = invoke(service.clone(), &["sessions"]).await;
    for value in [
        "legacy | 6 | 5 | 4 | 3 | 2",
        "Inconsistent sessions: 1",
        "migration-1",
        "1970-01-01",
        "legacy: 6",
        "Rolled back",
    ] {
        assert!(stdout.contains(value));
    }
    service.v3.lock().unwrap().hosts[0].probe_error = Some(Error::other("test probe error"));
    let (_, stdout, _) = invoke(service.clone(), &["remote", "list"]).await;
    for value in ["ssh:dev", "dev", "yes", "test probe error"] {
        assert!(stdout.contains(value), "{value}: {stdout}");
    }
    service.findings.lock().unwrap().push(DoctorFinding {
        id: "test".into(),
        severity: Severity::Danger,
        title: messages::text("switch.doctor.journal_pending"),
        detail: Some(messages::text("switch.doctor.app_not_ours")),
        fixable: true,
    });
    let (_, stdout, _) = invoke(service.clone(), &["doctor"]).await;
    for value in [
        "danger",
        "interrupted switch",
        "not opened from Superba",
        "yes",
    ] {
        assert!(stdout.contains(value));
    }
    let (_, stdout, _) = invoke(service, &["recover"]).await;
    for value in ["journal-1", "Completed", "/fake/config.toml"] {
        assert!(stdout.contains(value));
    }
}

#[tokio::test]
async fn locale_flag_wins_over_injected_environment_for_cli_and_shared_text() {
    let service = FakeService::new();
    service.events.lock().unwrap().push(SwitchEvent::Phase {
        phase: SwitchPhase::Applying,
        at: Timestamp::EPOCH,
    });
    let rt = runtime(service.clone());
    let (_, stdout, stderr) = invoke_runtime(&rt, &["switch", "--locale", "zh-CN"]).await;
    assert!(stdout.contains("目标配置"));
    assert!(stdout.contains(
        &messages::catalog(Locale::ZhCn).text(&messages::outcome(OutcomeStatus::Switched))
    ));
    assert!(
        stderr.contains(
            &messages::catalog(Locale::ZhCn).text(&messages::phase(SwitchPhase::Applying))
        )
    );
    let mut rt = runtime(service);
    rt.env = Arc::new(MapEnv::new().with("SUPERBA_LOCALE", "zh_CN.UTF-8"));
    assert!(invoke_runtime(&rt, &["status"]).await.1.contains("主目录"));
    assert!(
        invoke_runtime(&rt, &["status", "--locale", "en"])
            .await
            .1
            .contains("Codex home")
    );
}

#[tokio::test]
async fn outcome_shows_every_check_remote_result_stale_process_and_notice() {
    let service = FakeService::new();
    let mut result = outcome(OutcomeStatus::Partial);
    result.verification = Some(Verification {
        process: CheckResult::Passed,
        files: CheckResult::Failed {
            detail: "test mismatch".into(),
        },
        auth: CheckResult::Skipped {
            reason: "test reason".into(),
        },
        line: CheckResult::Pending,
        renderer: CheckResult::Passed,
    });
    result.remotes.push(RemoteOutcome {
        target_id: TargetId::new("ssh:a"),
        status: OutcomeStatus::Failed,
        error: Some(Error::other("test remote failure")),
    });
    result.stale_processes.push(process());
    result.notices.push(Notice::warning(messages::text(
        "switch.warn.daemon_running",
    )));
    *service.outcome.lock().unwrap() = Ok(result);
    let (code, stdout, _) = invoke(service, &["switch"]).await;
    assert_eq!(code, 1);
    for value in [
        "Partly switched",
        "Elapsed: 1.2 s",
        "Verify core: passed",
        "Verify files: failed: test mismatch",
        "Verify auth: skipped: test reason",
        "Verify line: pending",
        "ssh:a",
        "test remote failure",
        "Stale processes: 1",
        "424243",
        "1970-01-01",
        "[warning]",
    ] {
        assert!(stdout.contains(value), "missing {value}");
    }
}

#[tokio::test]
async fn plan_preview_covers_diffs_and_never_serializes_secret_payloads() {
    let service = FakeService::new();
    service.script(|request| {
        let mut p = plan(request);
        p.primary = RestartKind::FullAppRestart {
            reason: FullRestartReason::SessionMigration,
        };
        p.restarts = vec![ConsumerRestart {
            consumer: ConsumerKind::CliProcesses,
            restart: RestartKind::NextLaunch,
            pids: vec![424_243, 424_244],
        }];
        p.warnings.push(Notice::warning(messages::text(
            "switch.warn.daemon_running",
        )));
        p.config = Some(ApplyPlan {
            id: "config-1".into(),
            codex_home: "/fake/codex".into(),
            generation: 8,
            profile_id: Some(ProfileId::new("work")),
            site_id: Some(SiteId::new("krill")),
            line_id: Some(LineId::new("overseas")),
            auth_mode: Some(AuthMode::Pat),
            files: vec![PlannedFile {
                kind: CodexFileKind::AuthJson,
                path: "/fake/codex/auth.json".into(),
                before_sha256: None,
                change: FileChange::Write {
                    sha256: "test-hash".into(),
                    contents: SecretBytes::new(FAKE_KEY.as_bytes().to_vec()),
                },
            }],
            key_changes: vec![KeyChange {
                path: "model".into(),
                before: Some("old-model".into()),
                after: Some("new-model".into()),
            }],
            auth_change: Some(AuthChange {
                before: AuthShape::Missing,
                after: AuthShape::Pat {
                    key_last4: "1111".into(),
                },
                stash_login: false,
            }),
            notes: vec![],
            desired: Some(ResolvedProfile {
                model_catalog: None,
                profile_id: Some(ProfileId::new("work")),
                site_id: Some(SiteId::new("krill")),
                line_id: Some(LineId::new("overseas")),
                mode: ResolvedMode::Token {
                    site_kind: SiteKind::Krill,
                    auth: AuthMode::Pat,
                    endpoints: ResolvedEndpoints::for_line(
                        &Origin::parse("https://example.invalid").unwrap(),
                        CodexPath::BackendApiCodex,
                        AuthMode::Pat,
                    )
                    .unwrap(),
                    key: Secret::new(FAKE_KEY),
                },
                model: ModelDefaults::default(),
                key_resolutions: Default::default(),
                settings: Default::default(),
            }),
            releases_ownership: false,
        });
        p.env = Some(EnvPlan {
            var: "CODEX_AUTHAPI_BASE_URL".into(),
            op: EnvOp::Set {
                value: "https://example.invalid".into(),
                take_over_foreign: false,
            },
            edits: vec![EnvEdit {
                location: EnvLocation::ShellRc {
                    path: "/fake/rc".into(),
                    shell: ShellKind::Sh,
                },
                action: EnvEditAction::WriteBlock,
                preview: "export CODEX_AUTHAPI_BASE_URL=https://example.invalid".into(),
                before: None,
                after: None,
            }],
            scope_notes: vec![],
        });
        p.migration = Some(MigrationPlan {
            id: "migration-1".into(),
            codex_home: "/fake/codex".into(),
            request: MigrationRequest {
                to_provider: "openai".into(),
                selectors: vec![MigrationSelector::Provider {
                    provider: "legacy".into(),
                }],
                include_archived: true,
                exclude: vec![],
            },
            thread_count: 2,
            rollout_count: 3,
            by_source_provider: [("legacy".into(), 2)].into(),
            backup_dir: "/fake/backups".into(),
            backup_bytes: 10,
            items: vec![],
        });
        p.alternatives.push(PlanAlternative {
            label: messages::decision(Decision::KeepWritten),
            request: p.request.clone(),
        });
        p.active_threads.push(thread());
        Ok(p)
    });
    let (code, stdout, stderr) = invoke(service.clone(), &["switch", "--dry-run"]).await;
    assert_eq!(code, 0);
    for value in [
        "Destination:",
        "Full restart",
        "Codex in terminals",
        "2 processes",
        "[warning]",
        "model: old-model → new-model",
        "auth.json:",
        "1111",
        "Environment: CODEX_AUTHAPI_BASE_URL → https://example.invalid in /fake/rc",
        // No items: the session count falls back to max(rows, rollout files).
        "Migration: 3 sessions (2 database rows, 3 rollout files) → openai",
        "Source legacy: 2",
        "Alternative:",
        "Example thread",
    ] {
        assert!(stdout.contains(value), "missing {value}");
    }
    assert!(!stdout.contains(FAKE_KEY) && stderr.is_empty());
    assert!(!stdout.contains("export CODEX_AUTHAPI_BASE_URL"));
    let (_, verbose, _) = invoke(service.clone(), &["switch", "--dry-run", "--verbose"]).await;
    assert!(verbose.contains("/fake/rc:\nexport CODEX_AUTHAPI_BASE_URL=https://example.invalid"));
    let (code, stdout, stderr) = invoke(service.clone(), &["switch", "--dry-run", "--json"]).await;
    assert_eq!(code, 0);
    let value = one_json(&stdout);
    assert_eq!(value["status"], "planned");
    assert!(value["plan"]["config"].get("desired").is_none());
    assert!(
        value["plan"]["config"]["files"][0]["change"]
            .get("contents")
            .is_none()
    );
    assert!(!stdout.contains(FAKE_KEY) && stderr.is_empty());
    assert!(service.calls().executions.is_empty());
}

#[tokio::test]
async fn streamed_verification_and_success_notices_are_not_repeated_in_summary() {
    let service = FakeService::new();
    service.recovery.lock().unwrap().entries.clear();
    let mut result = outcome(OutcomeStatus::Switched);
    result.verification = Some(Verification {
        process: CheckResult::Passed,
        files: CheckResult::Passed,
        auth: CheckResult::Passed,
        line: CheckResult::Passed,
        renderer: CheckResult::Passed,
    });
    let summary = Notice::info(
        messages::text("switch.done.switched")
            .arg_text("destination", messages::text("switch.destination.official"))
            .arg("seconds", "1.2"),
    );
    let warning = Notice::warning(messages::text("switch.warn.daemon_running"));
    result.notices.extend([summary, warning.clone()]);
    service.events.lock().unwrap().extend(
        [
            SwitchStep::VerifyProcess,
            SwitchStep::VerifyFiles,
            SwitchStep::VerifyAuth,
            SwitchStep::VerifyLine,
            SwitchStep::VerifyRenderer,
        ]
        .into_iter()
        .map(|step| SwitchEvent::Step {
            step,
            status: StepStatus::Done,
            at: Timestamp::EPOCH,
        }),
    );
    service.events.lock().unwrap().push(SwitchEvent::Log {
        level: warning.severity,
        text: warning.text,
    });
    *service.outcome.lock().unwrap() = Ok(result.clone());
    let (code, stdout, stderr) = invoke(service.clone(), &["switch"]).await;
    assert_eq!(code, 0);
    assert_eq!(stdout.matches("Switched").count(), 1);
    assert!(stdout.contains("Switched to Official ChatGPT login in 1.2 s"));
    assert!(!stdout.contains("[info] Switched") && !stdout.contains("Elapsed:"));
    assert!(!stdout.contains("Stale processes: 0") && !stdout.contains("[warning]"));
    assert_eq!(stdout.matches("Restart:").count(), 1);
    for label in [
        "Verify core",
        "Verify files",
        "Verify auth",
        "Verify line",
        "Verify window",
    ] {
        assert!(!stdout.contains(label), "{stdout}");
        assert_eq!(stderr.matches(label).count(), 1, "{stderr}");
    }
    let (_, stdout, stderr) = invoke(service, &["switch", "--json"]).await;
    assert_eq!(
        one_json(&stdout),
        serde_json::json!({ "status": result.status, "outcome": result })
    );
    assert!(stderr.is_empty());
}

#[tokio::test]
async fn a_changed_verification_result_is_still_shown_after_streamed_success() {
    let service = FakeService::new();
    let mut result = outcome(OutcomeStatus::Failed);
    result.verification = Some(Verification {
        process: CheckResult::Failed {
            detail: "process stopped after verification".into(),
        },
        files: CheckResult::Passed,
        auth: CheckResult::Passed,
        line: CheckResult::Passed,
        renderer: CheckResult::Passed,
    });
    *service.outcome.lock().unwrap() = Ok(result);
    service.events.lock().unwrap().push(SwitchEvent::Step {
        step: SwitchStep::VerifyProcess,
        status: StepStatus::Done,
        at: Timestamp::EPOCH,
    });
    let (code, stdout, _) = invoke(service, &["switch"]).await;
    assert_eq!(code, 1);
    assert!(stdout.contains("Verify core: failed: process stopped after verification"));
}

#[tokio::test]
async fn line_names_match_across_status_lines_and_switch_in_both_locales() {
    let service = FakeService::new();
    service.script(|request| {
        let mut plan = plan(request);
        let site = catalog::krill_site();
        plan.to.label = messages::text("switch.destination.profile")
            .arg_text(
                "line",
                site.line(&LineId::new("overseas")).unwrap().name.clone(),
            )
            .arg_text("site", site.name)
            .arg_text("mode", AuthMode::Pat.text());
        Ok(plan)
    });
    for (locale, expected) in [
        ("en", "Overseas (overseas)"),
        ("zh-CN", "海外线路（overseas）"),
    ] {
        for args in [
            vec!["status"],
            vec!["lines"],
            vec!["lines", "--probe"],
            vec!["switch", "--dry-run"],
        ] {
            let mut args = args;
            args.extend(["--locale", locale]);
            let (code, stdout, _) = invoke(service.clone(), &args).await;
            assert_eq!(code, 0);
            assert!(stdout.contains(expected), "{stdout}");
        }
    }
}

#[tokio::test]
async fn missing_resources_have_next_steps_and_json_errors_stay_unchanged() {
    for (what, hint) in [
        ("profile missing", "spb profile list"),
        ("line missing of site krill", "spb lines"),
        ("site missing", "spb site list"),
    ] {
        let service = FakeService::new();
        let error = Error::NotFound { what: what.into() };
        let planned_error = error.clone();
        service.script(move |_| Err(planned_error.clone()));
        let (code, _, stderr) = invoke(service.clone(), &["switch"]).await;
        assert_eq!(code, 1);
        assert!(stderr.contains(hint), "{stderr}");
        assert_eq!(stderr.matches("error:").count(), 1);
        assert_eq!(stderr.to_lowercase().matches("not found:").count(), 1);
        let (_, stdout, stderr) = invoke(service, &["switch", "--json"]).await;
        assert_eq!(one_json(&stdout), serde_json::json!({"error": error}));
        assert!(stderr.is_empty());
    }
    for locale in ["en", "zh-CN"] {
        let (_, _, stderr) = invoke(
            FakeService::new(),
            &[
                "key",
                "set",
                "--profile",
                "missing",
                "--stdin",
                "--locale",
                locale,
            ],
        )
        .await;
        assert!(stderr.starts_with("error: "));
        assert!(stderr.contains("spb profile list"));
    }
}
