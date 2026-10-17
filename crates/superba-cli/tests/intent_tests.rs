mod common;

use common::*;
use superba_core::*;

#[tokio::test]
async fn restores_map_every_destination_and_provider() {
    for (mut args, expected) in [
        (
            vec!["restore"],
            SwitchRequest::profile(ProfileId::new("original")).intent,
        ),
        (
            vec!["restore", "original"],
            SwitchRequest::profile(ProfileId::new("original")).intent,
        ),
        (
            vec!["restore", "official"],
            SwitchIntent::Restore {
                target: RestoreTarget::Official,
            },
        ),
        (
            vec!["restore", "generation", "--generation", "42"],
            SwitchIntent::Restore {
                target: RestoreTarget::Generation { generation: 42 },
            },
        ),
        (
            vec!["restore", "--provider", "keep-openai"],
            SwitchIntent::Restore {
                target: RestoreTarget::Original {
                    provider: ProviderRestore::KeepOpenai,
                },
            },
        ),
        (
            vec!["restore", "--provider", "reverse-migrate"],
            SwitchIntent::Restore {
                target: RestoreTarget::Original {
                    provider: ProviderRestore::Baseline {
                        reverse_migrate: true,
                    },
                },
            },
        ),
        (
            vec!["restore", "--provider", "no-migrate"],
            SwitchIntent::Restore {
                target: RestoreTarget::Original {
                    provider: ProviderRestore::Baseline {
                        reverse_migrate: false,
                    },
                },
            },
        ),
    ] {
        let service = FakeService::new();
        args.extend(["--no-local", "--remote", "ssh:a", "--dry-run"]);
        assert_eq!(invoke(service.clone(), &args).await.0, 0);
        let request = &service.calls().plans[0];
        assert_eq!(request.intent, expected);
        assert_eq!(
            request.targets,
            TargetSelection {
                local: false,
                remotes: vec![TargetId::new("ssh:a")]
            }
        );
        assert!(service.calls().executions.is_empty());
    }
}

#[tokio::test]
async fn unrelated_restore_options_are_rejected_before_planning() {
    for args in [
        vec!["restore", "official", "--provider", "keep-openai"],
        vec!["restore", "--generation", "3"],
    ] {
        let service = FakeService::new();
        assert_eq!(invoke(service.clone(), &args).await.0, 2);
        assert!(service.calls().names.is_empty());
    }
}

#[tokio::test]
async fn session_migration_defaults_and_overrides_are_preserved() {
    let service = FakeService::new();
    assert_eq!(
        invoke(
            service.clone(),
            &["sessions", "migrate", "--from", "legacy", "--dry-run"]
        )
        .await
        .0,
        0
    );
    assert_eq!(
        service.calls().plans[0].intent,
        SwitchIntent::MigrateSessions {
            request: MigrationRequest {
                to_provider: "openai".into(),
                selectors: vec![MigrationSelector::Provider {
                    provider: "legacy".into()
                }],
                include_archived: true,
                exclude: vec![],
            }
        }
    );
    assert_eq!(
        invoke(
            service.clone(),
            &[
                "sessions",
                "migrate",
                "--from",
                "openai",
                "--to",
                "legacy",
                "--no-archived",
                "--exclude-thread",
                "a",
                "--exclude-thread",
                "b",
                "--dry-run"
            ]
        )
        .await
        .0,
        0
    );
    assert_eq!(
        service.calls().plans[1].intent,
        SwitchIntent::MigrateSessions {
            request: MigrationRequest {
                to_provider: "legacy".into(),
                selectors: vec![MigrationSelector::Provider {
                    provider: "openai".into()
                }],
                include_archived: false,
                exclude: vec!["a".into(), "b".into()],
            }
        }
    );
}

#[tokio::test]
async fn session_rollback_is_a_plan() {
    let service = FakeService::new();
    assert_eq!(
        invoke(
            service.clone(),
            &["sessions", "rollback", "migration-1", "--dry-run"]
        )
        .await
        .0,
        0
    );
    assert_eq!(
        service.calls().plans[0].intent,
        SwitchIntent::RollbackMigration {
            migration_id: "migration-1".into()
        }
    );
}

#[tokio::test]
async fn remote_apply_uses_only_remotes_and_profile_resolution() {
    let service = FakeService::new();
    assert_eq!(
        invoke(
            service.clone(),
            &[
                "remote",
                "apply",
                "ssh:a",
                "ssh:b",
                "--line",
                "cdn",
                "--auth",
                "apikey",
                "--dry-run"
            ]
        )
        .await
        .0,
        0
    );
    let first = &service.calls().plans[0];
    assert_eq!(
        first.targets,
        TargetSelection {
            local: false,
            remotes: vec![TargetId::new("ssh:a"), TargetId::new("ssh:b")]
        }
    );
    assert_eq!(
        first.intent,
        SwitchIntent::Profile {
            profile_id: ProfileId::new("work"),
            line_id: Some(LineId::new("cdn")),
            auth_mode: Some(AuthMode::ApiKey)
        }
    );
    assert_eq!(
        invoke(
            service.clone(),
            &[
                "remote",
                "apply",
                "ssh:c",
                "--profile",
                "explicit",
                "--remote",
                "ssh:d",
                "--dry-run"
            ]
        )
        .await
        .0,
        0
    );
    let second = &service.calls().plans[1];
    assert_eq!(
        second.intent,
        SwitchRequest::profile(ProfileId::new("explicit")).intent
    );
    assert!(!second.targets.local);
    assert_eq!(
        second.targets.remotes,
        [TargetId::new("ssh:c"), TargetId::new("ssh:d")]
    );
}

#[tokio::test]
async fn remote_rollback_builds_the_target_intent() {
    let service = FakeService::new();
    assert_eq!(
        invoke(
            service.clone(),
            &["remote", "rollback", "ssh:a", "--dry-run"]
        )
        .await
        .0,
        0
    );
    assert_eq!(
        service.calls().plans[0].intent,
        SwitchIntent::RollbackRemote {
            target_id: TargetId::new("ssh:a")
        }
    );
}

#[tokio::test]
async fn launch_without_restart_calls_open_app_and_alias_matches() {
    for command in ["launch", "open-app"] {
        let service = FakeService::new();
        let (code, stdout, stderr) = invoke(service.clone(), &[command, "--json"]).await;
        assert_eq!(code, 0);
        assert_eq!(
            one_json(&stdout),
            serde_json::to_value(app_session()).unwrap()
        );
        assert!(stderr.is_empty());
        assert_eq!(service.calls().names, ["open_app"]);
    }
}

#[tokio::test]
async fn launch_restart_uses_full_confirmation_flow() {
    let service = FakeService::new();
    service.script(|request| {
        let mut p = plan(request);
        p.primary = RestartKind::FullAppRestart {
            reason: FullRestartReason::Requested,
        };
        Ok(p)
    });
    assert_eq!(invoke(service.clone(), &["launch", "--restart"]).await.0, 4);
    assert!(service.calls().executions.is_empty());
    assert_eq!(
        invoke(service.clone(), &["open-app", "--restart", "--yes"])
            .await
            .0,
        0
    );
    assert!(service.calls().executions[0].1.allow_full_restart);
    for request in &service.calls().plans {
        assert_eq!(
            request.intent,
            SwitchIntent::RestartApp { install_id: None }
        );
    }
    assert!(!service.calls().names.contains(&"open_app"));
}

#[tokio::test]
async fn launch_dry_run_without_restart_cannot_open_the_app() {
    let service = FakeService::new();
    assert_eq!(invoke(service.clone(), &["launch", "--dry-run"]).await.0, 2);
    assert!(service.calls().names.is_empty());
}

#[tokio::test]
async fn uninstall_preserves_provider_and_backup_choices() {
    let service = FakeService::new();
    assert_eq!(
        invoke(service.clone(), &["uninstall", "--dry-run"]).await.0,
        0
    );
    assert_eq!(
        service.calls().plans[0].intent,
        SwitchIntent::Uninstall {
            delete_backups: false,
            provider: None
        }
    );
    for (index, (flag, provider)) in [
        ("keep-openai", ProviderRestore::KeepOpenai),
        (
            "reverse-migrate",
            ProviderRestore::Baseline {
                reverse_migrate: true,
            },
        ),
        (
            "no-migrate",
            ProviderRestore::Baseline {
                reverse_migrate: false,
            },
        ),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            invoke(
                service.clone(),
                &[
                    "uninstall",
                    "--delete-backups",
                    "--provider",
                    flag,
                    "--dry-run"
                ]
            )
            .await
            .0,
            0
        );
        assert_eq!(
            service.calls().plans[index + 1].intent,
            SwitchIntent::Uninstall {
                delete_backups: true,
                provider: Some(provider)
            }
        );
    }
}
