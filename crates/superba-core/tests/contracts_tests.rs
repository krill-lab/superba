#![allow(missing_docs)]

use std::collections::BTreeMap;
use std::sync::Arc;

use superba_core::traits::{
    AppController, ConfigEngine, EnvPersister, LineProber, ProfileStore, RemoteHosts, SecretStore,
    SessionMigrator, SwitchDecider, SwitchEvents, SwitchService,
};
use superba_core::*;

#[test]
fn all_component_traits_support_shared_send_sync_trait_objects() {
    fn assert_shared_trait<T: ?Sized + Send + Sync>() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Arc<T>>();
    }
    // Each `dyn` instantiation fails to compile if the trait loses object safety;
    // the generic bounds check the trait itself, not an added `dyn Trait + Send`.
    assert_shared_trait::<dyn ConfigEngine>();
    assert_shared_trait::<dyn EnvPersister>();
    assert_shared_trait::<dyn SecretStore>();
    assert_shared_trait::<dyn ProfileStore>();
    assert_shared_trait::<dyn LineProber>();
    assert_shared_trait::<dyn SessionMigrator>();
    assert_shared_trait::<dyn AppController>();
    assert_shared_trait::<dyn RemoteHosts>();
    assert_shared_trait::<dyn SwitchEvents>();
    assert_shared_trait::<dyn SwitchDecider>();
    assert_shared_trait::<dyn SwitchService>();
    assert_shared_trait::<dyn Clock>();
    assert_shared_trait::<dyn EnvSource>();
}

fn decision_cases() -> [(DecisionRequest, Decision, &'static [Decision]); 6] {
    [
        (
            DecisionRequest::WaitForTurns {
                busy: vec![],
                waited_ms: 900_000,
            },
            Decision::Wait,
            &[Decision::Wait, Decision::Force, Decision::Cancel],
        ),
        (
            DecisionRequest::ForceQuit {
                pids: vec![123, 456],
                report: Default::default(),
            },
            Decision::Cancel,
            &[Decision::Force, Decision::Cancel],
        ),
        (
            DecisionRequest::AppStillRunning {
                report: Default::default(),
            },
            Decision::Cancel,
            &[Decision::Force, Decision::Continue, Decision::Cancel],
        ),
        (
            DecisionRequest::IpcFailed {
                error: Error::Cdp {
                    message: "bridge disconnected".into(),
                },
            },
            Decision::Undo,
            &[Decision::FullRestart, Decision::Undo, Decision::KeepWritten],
        ),
        (
            DecisionRequest::VerificationFailed {
                verification: Verification {
                    files: CheckResult::Failed {
                        detail: "hash mismatch".into(),
                    },
                    ..Verification::default()
                },
            },
            Decision::Rollback,
            &[
                Decision::Rollback,
                Decision::FullRestart,
                Decision::KeepWritten,
            ],
        ),
        (
            DecisionRequest::UnknownRemoteActivity {
                targets: vec![TargetId::remote("build")],
            },
            Decision::Cancel,
            &[Decision::Continue, Decision::Cancel],
        ),
    ]
}

#[test]
fn every_decision_request_has_the_expected_allowed_answers_and_safe_default() {
    for (request, safe, allowed) in decision_cases() {
        assert_eq!(request.allowed(), allowed, "{request:?}");
        assert_eq!(request.safe_default(), safe, "{request:?}");
        assert!(
            request.allowed().contains(&request.safe_default()),
            "{request:?}"
        );
    }
}

#[tokio::test]
async fn safe_defaults_decider_works_through_a_shared_trait_object() {
    let decider: Arc<dyn SwitchDecider> = Arc::new(SafeDefaults);
    for (request, safe, _) in decision_cases() {
        assert_eq!(decider.decide(request).await, safe);
    }
}

fn switch_plan(primary: RestartKind) -> SwitchPlan {
    SwitchPlan {
        id: "plan-test".into(),
        created_at: Timestamp(123),
        request: SwitchRequest::profile("work".into()),
        from: None,
        to: PlanDestination {
            profile_id: Some("work".into()),
            site_id: Some("krill".into()),
            line_id: Some("overseas".into()),
            auth_mode: Some(AuthMode::Pat),
            label: LocalizedText::new("test.destination", "Work"),
        },
        primary,
        restarts: vec![],
        config: None,
        env: None,
        certificate_env: None,
        migration: None,
        remotes: vec![],
        active_threads: vec![],
        model_check: None,
        choices: vec![],
        alternatives: vec![],
        warnings: vec![],
        cleanup: vec![],
    }
}

fn migration_plan() -> MigrationPlan {
    MigrationPlan {
        id: "migration-test".into(),
        codex_home: "/fixtures/codex".into(),
        request: MigrationRequest {
            to_provider: "openai".into(),
            selectors: vec![MigrationSelector::Provider {
                provider: "krill".into(),
            }],
            include_archived: true,
            exclude: vec![],
        },
        thread_count: 3,
        rollout_count: 3,
        by_source_provider: BTreeMap::from([("krill".into(), 3)]),
        backup_dir: "/fixtures/backups/migrate-test".into(),
        backup_bytes: 512,
        items: vec![],
    }
}

#[test]
fn restart_kinds_define_countdown_confirmation_blocking_and_turn_interruption() {
    for (kind, hot, confirm, blocked, interrupts) in [
        (
            RestartKind::Blocked {
                reasons: vec![BlockReason::MissingKey],
            },
            false,
            false,
            true,
            false,
        ),
        (RestartKind::None, false, false, false, false),
        (
            RestartKind::AppServerHot {
                host_id: "local".into(),
                kill_remote: false,
            },
            true,
            false,
            false,
            true,
        ),
        (
            RestartKind::AppServerHot {
                host_id: "remote".into(),
                kill_remote: true,
            },
            true,
            false,
            false,
            true,
        ),
        (
            RestartKind::FullAppRestart {
                reason: FullRestartReason::NoCdp,
            },
            false,
            true,
            false,
            true,
        ),
        (RestartKind::NextLaunch, false, false, false, false),
        (RestartKind::DaemonRestart, false, true, false, true),
    ] {
        assert_eq!(kind.is_hot(), hot, "{kind:?}");
        assert_eq!(kind.needs_confirmation(), confirm, "{kind:?}");
        assert_eq!(kind.is_blocked(), blocked, "{kind:?}");
        assert_eq!(kind.interrupts_turns(), interrupts, "{kind:?}");
        let plan = switch_plan(kind);
        assert_eq!(plan.countdown_allowed(), hot);
        assert_eq!(plan.needs_confirmation(), confirm);
        assert_eq!(plan.is_blocked(), blocked);
    }
}

#[test]
fn unresolved_choices_disable_the_hot_countdown_until_replanned() {
    let mut plan = switch_plan(RestartKind::AppServerHot {
        host_id: "local".into(),
        kill_remote: false,
    });
    assert!(plan.countdown_allowed());
    plan.choices.push(PlanChoice::UnknownVersion {
        consumer: "remote".into(),
    });
    assert!(!plan.countdown_allowed());
    assert!(!plan.needs_confirmation());
    plan.choices.clear();
    assert!(plan.countdown_allowed());
}

#[test]
fn migration_always_requires_confirmation_and_disables_countdown() {
    for kind in [
        RestartKind::None,
        RestartKind::AppServerHot {
            host_id: "local".into(),
            kill_remote: false,
        },
        RestartKind::NextLaunch,
        RestartKind::FullAppRestart {
            reason: FullRestartReason::SessionMigration,
        },
        RestartKind::DaemonRestart,
    ] {
        let mut plan = switch_plan(kind);
        plan.migration = Some(migration_plan());
        assert!(!plan.countdown_allowed());
        assert!(plan.needs_confirmation());
        plan.choices.push(PlanChoice::UnknownVersion {
            consumer: "local".into(),
        });
        assert!(!plan.countdown_allowed());
        assert!(plan.needs_confirmation());
    }
}

#[test]
fn verification_passes_only_when_each_check_passes_or_is_skipped() {
    // A verification that never ran must not count as passed.
    assert!(!Verification::default().passed());
    assert!(!CheckResult::Pending.ok());
    let skipped = CheckResult::Skipped {
        reason: "not applicable".into(),
    };
    let results = [
        CheckResult::Passed,
        skipped.clone(),
        CheckResult::Failed {
            detail: "failed".into(),
        },
    ];
    // All 3^5 combinations ensure a failure in any one check prevents success.
    for combination in 0..243 {
        let indices = [
            combination % 3,
            combination / 3 % 3,
            combination / 9 % 3,
            combination / 27 % 3,
            combination / 81 % 3,
        ];
        let verification = Verification {
            process: results[indices[0]].clone(),
            files: results[indices[1]].clone(),
            auth: results[indices[2]].clone(),
            line: results[indices[3]].clone(),
            renderer: results[indices[4]].clone(),
        };
        assert_eq!(
            verification.passed(),
            !indices.contains(&2),
            "{verification:?}"
        );
    }
    assert!(CheckResult::Passed.ok());
    assert!(skipped.ok());
    assert!(!results[2].ok());
}

#[test]
fn line_capabilities_require_reachability_a_path_and_the_requested_auth_support() {
    for reachable in [false, true] {
        for path in [
            None,
            Some(CodexPath::BackendApiCodex),
            Some(CodexPath::CodexV1),
            Some(CodexPath::AsIs),
        ] {
            for (pat_ok, apikey_ok) in [(false, false), (true, false), (false, true), (true, true)]
            {
                let caps = LineCaps {
                    site_id: "krill".into(),
                    line_id: "overseas".into(),
                    checked_at: Timestamp::EPOCH,
                    reachable,
                    latency_ms: None,
                    pat_ok,
                    apikey_ok,
                    codex_path: path,
                    blocked_by_edge: false,
                    key_access: None,
                    checks: vec![],
                    detail: None,
                };
                assert_eq!(
                    caps.allows(AuthMode::Pat),
                    reachable && path == Some(CodexPath::BackendApiCodex) && pat_ok,
                    "{caps:?}"
                );
                assert_eq!(
                    caps.allows(AuthMode::ApiKey),
                    reachable && path.is_some() && apikey_ok,
                    "{caps:?}"
                );
                assert!(!caps.allows(AuthMode::OfficialChatGpt));
            }
        }
    }
}

#[test]
fn session_distribution_uses_distinct_session_counts_per_provider() {
    let mut report = SessionReport {
        codex_home: "/fixtures/codex".into(),
        scanned_at: Timestamp::EPOCH,
        sqlite_paths: vec!["/fixtures/codex/state_5.sqlite".into()],
        by_provider: BTreeMap::from([
            (
                "openai".into(),
                ProviderSessions {
                    threads: 7,
                    rollouts: 5,
                    archived: 20,
                    compressed: 3,
                    distinct: 7,
                },
            ),
            (
                "krill".into(),
                ProviderSessions {
                    threads: 2,
                    rollouts: 4,
                    archived: 30,
                    compressed: 1,
                    // archived-only sessions count too
                    distinct: 4,
                },
            ),
            (
                "other".into(),
                ProviderSessions {
                    threads: 3,
                    rollouts: 3,
                    archived: 10,
                    compressed: 0,
                    distinct: 3,
                },
            ),
        ]),
        inconsistent: 1,
        warnings: vec![],
    };
    assert_eq!(
        report.distribution(),
        BTreeMap::from([
            ("openai".into(), 7),
            ("krill".into(), 4),
            ("other".into(), 3)
        ])
    );
    assert_eq!(report.hidden_under("openai"), 7);
    assert_eq!(report.hidden_under("krill"), 10);
    assert_eq!(report.hidden_under("other"), 11);
    assert_eq!(report.hidden_under("unknown"), 14);
    report.by_provider.clear();
    assert!(report.distribution().is_empty());
    assert_eq!(report.hidden_under("openai"), 0);
}

fn process(
    pid: u32,
    kind: CodexProcessKind,
    authapi_env: EnvObservation,
    stale: bool,
) -> ProcessInfo {
    ProcessInfo {
        pid,
        ppid: None,
        kind,
        exe: None,
        cmdline: vec![],
        started_at: None,
        authapi_env,
        codex_home: None,
        stale,
        ide: None,
        cwd: None,
    }
}

#[test]
fn cli_counts_separate_stale_processes_from_known_missing_auth_environment() {
    let cli = CliOverview {
        binaries: vec![],
        processes: vec![
            process(1, CodexProcessKind::AppMain, EnvObservation::Absent, true),
            process(2, CodexProcessKind::CliTui, EnvObservation::Absent, true),
            process(3, CodexProcessKind::CliExec, EnvObservation::Unknown, false),
            process(
                4,
                CodexProcessKind::CliDaemon,
                EnvObservation::Present("https://api.krill-code.net".into()),
                true,
            ),
            process(
                5,
                CodexProcessKind::IdeAppServer,
                EnvObservation::Absent,
                false,
            ),
            process(
                6,
                CodexProcessKind::AppServer,
                EnvObservation::Absent,
                false,
            ),
        ],
    };
    assert_eq!(cli.stale_count(), 3);
    assert_eq!(cli.missing_env_count(), 3);
    assert_eq!(CliOverview::default().stale_count(), 0);
    assert_eq!(CliOverview::default().missing_env_count(), 0);
}

#[test]
fn remote_shared_home_detection_requires_two_different_machine_ids() {
    for (machine, marker, expected) in [
        (None, None, false),
        (Some("machine-a"), None, false),
        (None, Some("machine-b"), false),
        (Some("machine-a"), Some("machine-a"), false),
        (Some("machine-a"), Some("machine-b"), true),
    ] {
        let state = RemoteState {
            disk_free_kib: None,
            kernel: None,
            arch: None,
            codex_install: None,
            codex_resolved_path: None,
            target_id: TargetId::remote("build"),
            probed_at: Timestamp::EPOCH,
            os: RemoteOs::Linux,
            shell: ShellKind::Bash,
            codex_home: "/home/test/.codex".into(),
            codex_path: None,
            codex_version: None,
            config_sha256: None,
            auth_sha256: None,
            auth: AuthShape::Missing,
            model_provider: None,
            openai_base_url: None,
            machine_id: machine.map(str::to_owned),
            marker_machine_id: marker.map(str::to_owned),
            authapi_env: EnvObservation::Unknown,
            app_server_pids: vec![],
            owned_paths: vec![],
            config_toml: None,
            app_bash: None,
            login_fallback: None,
            gateway: None,
        };
        assert_eq!(
            state.shared_home_suspected(),
            expected,
            "machine={machine:?}, marker={marker:?}"
        );
    }
}

#[test]
fn target_ids_distinguish_local_and_remote_and_local_consumers_share_a_home() {
    let local = TargetId::local();
    assert_eq!(local.as_str(), "local");
    assert!(local.is_local());
    let remote = TargetId::remote("build");
    assert_eq!(remote.as_str(), "ssh:build");
    assert!(!remote.is_local());
    assert_ne!(remote, local);
    assert_ne!(remote, TargetId::remote("other"));
    assert!(!TargetId::remote("local").is_local());
    assert_eq!(Target::LocalApp.id(), local);
    assert_eq!(Target::LocalCli.id(), local);
    assert!(Target::LocalApp.shares_codex_home_with(&Target::LocalCli));
    let target = Target::Remote(RemoteHost {
        target_id: remote.clone(),
        label: "Build".into(),
        host_id: Some("build".into()),
        kind: superba_core::HostKind::Ssh,
        ssh: SshEndpoint {
            alias: Some("build".into()),
            ..SshEndpoint::default()
        },
        sources: vec![],
        connected: None,
        wsl: None,
    });
    assert_eq!(target.id(), remote);
    assert!(!target.is_local());
    assert!(!target.shares_codex_home_with(&Target::LocalApp));
    assert!(target.shares_codex_home_with(&target.clone()));
}

#[test]
fn auth_mode_and_shape_helpers_agree_on_token_and_chatgpt_families() {
    for (mode, family, key, authapi) in [
        (AuthMode::Pat, AuthFamily::Token, true, true),
        (AuthMode::ApiKey, AuthFamily::Token, true, false),
        (AuthMode::OfficialChatGpt, AuthFamily::ChatGpt, false, false),
    ] {
        assert_eq!(mode.family(), family);
        assert_eq!(mode.uses_key(), key);
        assert_eq!(mode.needs_authapi_env(), authapi);
        assert_eq!(mode.writes_chatgpt_base_url(), authapi);
    }
    for (shape, mode) in [
        (AuthShape::Missing, None),
        (
            AuthShape::Unknown {
                detail: "unknown format".into(),
            },
            None,
        ),
        (
            AuthShape::Pat {
                key_last4: "abcd".into(),
            },
            Some(AuthMode::Pat),
        ),
        (
            AuthShape::ApiKey {
                key_last4: "abcd".into(),
            },
            Some(AuthMode::ApiKey),
        ),
        (
            AuthShape::ChatGpt {
                email: Some("user@example.com".into()),
                account_id: None,
            },
            Some(AuthMode::OfficialChatGpt),
        ),
    ] {
        assert_eq!(shape.mode(), mode);
        assert_eq!(shape.family(), mode.map(AuthMode::family));
    }
}
