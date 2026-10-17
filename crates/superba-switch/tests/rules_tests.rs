//! Pure planner contracts: plan 7.1 restart rules, 9.8 version gates and 7.7 consumers.

use std::path::Path;

use pretty_assertions::assert_eq;
use superba_core::{
    AuthApiHealth, AuthFamily, AuthMode, BlockReason, CheckResult, CodexProcessKind, CodexVersion,
    ConsumerKind, ConsumerRestart, EnvObservation, FullRestartReason, Line, LineId, LineSource,
    LocalizedText, Origin, ProcessInfo, RestartKind, SiteKind, TargetId, Timestamp, Verification,
    catalog, codex::VersionCapability,
};
use superba_switch::rules::{
    self, AppFacts, ConsumerRole, LocalFacts, VersionGate, VersionedConsumer,
};

fn hot() -> RestartKind {
    RestartKind::AppServerHot {
        host_id: "local".into(),
        kill_remote: false,
    }
}

fn full(reason: FullRestartReason) -> RestartKind {
    RestartKind::FullAppRestart { reason }
}

fn running(
    cdp: bool,
    bridge_ready: bool,
    env: EnvObservation,
    health: Option<AuthApiHealth>,
) -> AppFacts {
    AppFacts::Running {
        cdp,
        bridge_ready,
        authapi_env: env,
        authapi_health: health,
    }
}

fn ready_facts() -> LocalFacts {
    LocalFacts {
        blocks: Vec::new(),
        migration: false,
        current_family: Some(AuthFamily::Token),
        target_family: Some(AuthFamily::Token),
        target_needs_authapi: true,
        app: running(true, true, valid_env(), Some(AuthApiHealth::Healthy)),
        hot_supported: true,
    }
}

fn valid_env() -> EnvObservation {
    EnvObservation::Present("https://relay.example".into())
}

fn environments() -> [EnvObservation; 6] {
    [
        EnvObservation::Absent,
        EnvObservation::Unknown,
        EnvObservation::Present(String::new()),
        EnvObservation::Present(" \t\n".into()),
        EnvObservation::Present("\u{2003}".into()),
        valid_env(),
    ]
}

fn health_states() -> [Option<AuthApiHealth>; 3] {
    [
        None,
        Some(AuthApiHealth::Unhealthy {
            reason: "whoami timed out".into(),
        }),
        Some(AuthApiHealth::Healthy),
    ]
}

#[test]
fn rule_1_preserves_every_block_in_order_before_migration() {
    // Plan 7.1: rule 1 wins over rule 2, even with duplicate reasons and a stopped app.
    for app in [ready_facts().app, AppFacts::NotRunning] {
        let facts = LocalFacts {
            blocks: vec![
                BlockReason::MissingKey,
                BlockReason::JournalPending,
                BlockReason::MissingKey,
            ],
            migration: true,
            app,
            ..ready_facts()
        };
        assert_eq!(
            rules::decide_local(&facts),
            RestartKind::Blocked {
                reasons: facts.blocks.clone()
            }
        );
    }
}

#[test]
fn rule_2_migration_wins_over_cross_family_and_app_stopped() {
    // Plan 7.1: migration requires all consumers to stop, including without an app (rules 2/3/4).
    for app in [ready_facts().app, AppFacts::NotRunning] {
        let facts = LocalFacts {
            migration: true,
            current_family: Some(AuthFamily::ChatGpt),
            app,
            ..ready_facts()
        };
        assert_eq!(
            rules::decide_local(&facts),
            full(FullRestartReason::SessionMigration)
        );
    }
}

#[test]
fn rule_3_requires_two_different_known_families_and_a_running_app() {
    // Plan 7.1 rule 3: both directions require a restart; an unknown family never does.
    let families = [None, Some(AuthFamily::Token), Some(AuthFamily::ChatGpt)];
    for current_family in families {
        for target_family in families {
            for is_running in [false, true] {
                let facts = LocalFacts {
                    current_family,
                    target_family,
                    app: if is_running {
                        ready_facts().app
                    } else {
                        AppFacts::NotRunning
                    },
                    ..ready_facts()
                };
                let expected = match (is_running, current_family, target_family) {
                    (false, _, _) => RestartKind::None,
                    (true, Some(from), Some(to)) if from != to => {
                        full(FullRestartReason::CrossAuthFamily)
                    }
                    _ => hot(),
                };
                assert_eq!(rules::decide_local(&facts), expected, "{facts:?}");
            }
        }
    }
}

#[test]
fn rule_3_cross_family_precedes_every_hot_prerequisite() {
    // Plan 7.1 rules 3/5: family changes beat platform, CDP and bridge failures.
    for hot_supported in [false, true] {
        let facts = LocalFacts {
            current_family: Some(AuthFamily::ChatGpt),
            hot_supported,
            app: running(false, false, EnvObservation::Absent, None),
            ..ready_facts()
        };
        assert_eq!(
            rules::decide_local(&facts),
            full(FullRestartReason::CrossAuthFamily)
        );
    }
}

#[test]
fn rule_4_stopped_app_ignores_platform_and_authapi_requirements() {
    // Plan 7.1 rules 4/5: with no migration or blocks, a stopped app needs no restart.
    let facts = LocalFacts {
        app: AppFacts::NotRunning,
        hot_supported: false,
        ..ready_facts()
    };
    assert!(!facts.app.is_running());
    assert!(ready_facts().app.is_running());
    assert_eq!(rules::decide_local(&facts), RestartKind::None);
}

#[test]
fn rule_5_checks_platform_then_cdp_then_bridge_before_authapi() {
    // Plan 7.1 rules 5/6: each prerequisite beats the next, including missing PAT env.
    for (hot_supported, cdp, bridge, reason) in [
        (false, false, false, FullRestartReason::PlatformNoHot),
        (true, false, false, FullRestartReason::NoCdp),
        (true, true, false, FullRestartReason::NoBridge),
        (true, true, true, FullRestartReason::AuthApiMissing),
    ] {
        let facts = LocalFacts {
            hot_supported,
            app: running(cdp, bridge, EnvObservation::Absent, None),
            ..ready_facts()
        };
        assert_eq!(rules::decide_local(&facts), full(reason), "{facts:?}");
    }
}

#[test]
fn rule_6_rejects_absent_unknown_empty_and_whitespace_env_before_health() {
    // Plan 7.1 rules 6/7: a healthy probe cannot compensate for an unusable app env.
    for env in environments().into_iter().take(5) {
        for health in health_states() {
            let facts = LocalFacts {
                app: running(true, true, env.clone(), health),
                ..ready_facts()
            };
            assert_eq!(
                rules::decide_local(&facts),
                full(FullRestartReason::AuthApiMissing),
                "{facts:?}"
            );
        }
    }
}

#[test]
fn rule_7_requires_an_explicitly_healthy_authapi() {
    // Plan 7.1 rules 7/8: unchecked and unhealthy are full restarts; healthy permits hot.
    for (health, expected) in [
        (None, full(FullRestartReason::AuthApiUnhealthy)),
        (
            Some(AuthApiHealth::Unhealthy {
                reason: "timeout".into(),
            }),
            full(FullRestartReason::AuthApiUnhealthy),
        ),
        (Some(AuthApiHealth::Healthy), hot()),
    ] {
        let facts = LocalFacts {
            app: running(true, true, valid_env(), health),
            ..ready_facts()
        };
        assert_eq!(rules::decide_local(&facts), expected);
    }
}

#[test]
fn rule_8_accepts_nonblank_env_and_sets_exact_local_ipc_flags() {
    // Plan 7.1 rule 8: whitespace around a nonempty value does not make the env missing.
    let facts = LocalFacts {
        app: running(
            true,
            true,
            EnvObservation::Present(" https://relay.example \t".into()),
            Some(AuthApiHealth::Healthy),
        ),
        ..ready_facts()
    };
    assert_eq!(rules::decide_local(&facts), hot());
}

#[test]
fn rules_6_and_7_do_not_apply_to_destinations_without_authapi() {
    // Plan 7.1: API-key destinations bypass both PAT checks, for every observation.
    for env in environments() {
        for health in health_states() {
            let facts = LocalFacts {
                target_needs_authapi: false,
                app: running(true, true, env.clone(), health),
                ..ready_facts()
            };
            assert_eq!(rules::decide_local(&facts), hot(), "{facts:?}");
        }
    }
}

fn documented_decision(facts: &LocalFacts) -> RestartKind {
    // Independent declarative oracle: rank applicable rules, without calling planner helpers.
    let (is_running, cdp, bridge, env_present, healthy) = match &facts.app {
        AppFacts::NotRunning => (false, false, false, false, false),
        AppFacts::Running {
            cdp,
            bridge_ready,
            authapi_env,
            authapi_health,
        } => (
            true,
            *cdp,
            *bridge_ready,
            matches!(authapi_env, EnvObservation::Present(value) if value.chars().any(|c| !c.is_whitespace())),
            matches!(authapi_health, Some(AuthApiHealth::Healthy)),
        ),
    };
    let cross_family = matches!(
        (facts.current_family, facts.target_family),
        (Some(AuthFamily::Token), Some(AuthFamily::ChatGpt))
            | (Some(AuthFamily::ChatGpt), Some(AuthFamily::Token))
    );
    [
        (
            10,
            !facts.blocks.is_empty(),
            RestartKind::Blocked {
                reasons: facts.blocks.clone(),
            },
        ),
        (
            20,
            facts.migration,
            full(FullRestartReason::SessionMigration),
        ),
        (
            30,
            is_running && cross_family,
            full(FullRestartReason::CrossAuthFamily),
        ),
        (40, !is_running, RestartKind::None),
        (
            50,
            !facts.hot_supported,
            full(FullRestartReason::PlatformNoHot),
        ),
        (51, !cdp, full(FullRestartReason::NoCdp)),
        (52, !bridge, full(FullRestartReason::NoBridge)),
        (
            60,
            facts.target_needs_authapi && !env_present,
            full(FullRestartReason::AuthApiMissing),
        ),
        (
            70,
            facts.target_needs_authapi && !healthy,
            full(FullRestartReason::AuthApiUnhealthy),
        ),
        (80, true, hot()),
    ]
    .into_iter()
    .filter(|(_, applies, _)| *applies)
    .min_by_key(|(rank, _, _)| *rank)
    .expect("rule 8 always applies")
    .2
}

#[test]
fn all_local_fact_combinations_follow_the_documented_first_match_order() {
    // Plan 7.1 rules 1-8: 20,736 combinations also exercise non-adjacent precedence.
    let families = [None, Some(AuthFamily::Token), Some(AuthFamily::ChatGpt)];
    let mut checked = 0;
    for mask in 0..128 {
        let flag = |bit| mask & (1 << bit) != 0;
        for current_family in families {
            for target_family in families {
                for env in environments() {
                    for health in health_states() {
                        let facts = LocalFacts {
                            blocks: if flag(0) {
                                vec![BlockReason::MissingKey]
                            } else {
                                vec![]
                            },
                            migration: flag(1),
                            app: if flag(2) {
                                running(flag(4), flag(5), env.clone(), health)
                            } else {
                                AppFacts::NotRunning
                            },
                            hot_supported: flag(3),
                            target_needs_authapi: flag(6),
                            current_family,
                            target_family,
                        };
                        assert_eq!(
                            rules::decide_local(&facts),
                            documented_decision(&facts),
                            "{facts:?}"
                        );
                        checked += 1;
                    }
                }
            }
        }
    }
    assert_eq!(checked, 20_736);
}

#[test]
fn remote_blocks_win_regardless_of_host_id() {
    // Plan 7.1 remote rule: a known host never bypasses a blocked remote plan.
    let reasons = vec![BlockReason::JournalPending, BlockReason::MissingKey];
    for host in [None, Some(""), Some(" \t"), Some("devbox")] {
        assert_eq!(
            rules::decide_remote(host, &reasons),
            RestartKind::Blocked {
                reasons: reasons.clone()
            }
        );
    }
}

#[test]
fn a_host_without_a_running_app_server_is_written_for_its_next_start() {
    // e2e s22: a settings sync to a host the app was not connected to waited
    // 15 s for a new app-server that could never appear, then failed.
    assert_eq!(
        rules::decide_remote_probed(Some("devbox"), &[], &[]),
        RestartKind::NextLaunch
    );
    assert_eq!(
        rules::decide_remote_probed(Some("devbox"), &[], &[4242]),
        RestartKind::AppServerHot {
            host_id: "devbox".into(),
            kill_remote: true
        }
    );
    let reasons = vec![BlockReason::MissingKey];
    assert_eq!(
        rules::decide_remote_probed(Some("devbox"), &reasons, &[]),
        RestartKind::Blocked { reasons }
    );
}

#[test]
fn remote_hot_requires_a_nonblank_host_and_preserves_its_id() {
    // Plan 7.1 remote rule: reconnect without an id; kill the remote core for a known id.
    for host in [None, Some(""), Some(" \t\n"), Some("\u{2003}")] {
        assert_eq!(rules::decide_remote(host, &[]), RestartKind::NextLaunch);
    }
    for host in ["devbox", "ssh:devbox", " padded-host "] {
        assert_eq!(
            rules::decide_remote(Some(host), &[]),
            RestartKind::AppServerHot {
                host_id: host.into(),
                kill_remote: true
            }
        );
    }
}

#[test]
fn version_table_and_lookup_pin_all_three_capability_thresholds() {
    // Plan 9.8: base URL starts at 0.149; PAT and accounts-check start at 0.160.
    let expected = [
        (
            VersionCapability::TopLevelOpenaiBaseUrl,
            CodexVersion::new(0, 149, 0),
        ),
        (
            VersionCapability::PersonalAccessToken,
            CodexVersion::new(0, 160, 0),
        ),
        (
            VersionCapability::WhamAccountsCheck,
            CodexVersion::new(0, 160, 0),
        ),
    ];
    assert_eq!(rules::VERSION_TABLE, expected.as_slice());
    for (capability, version) in expected {
        assert_eq!(rules::min_version(capability), version);
    }
}

#[test]
fn required_capabilities_cover_every_site_and_auth_combination() {
    // Plan 9.8: official/non-key/unknown auth needs nothing; token restores still need gates.
    for site in [
        None,
        Some(SiteKind::Official),
        Some(SiteKind::Krill),
        Some(SiteKind::Compatible),
    ] {
        for auth in [
            None,
            Some(AuthMode::OfficialChatGpt),
            Some(AuthMode::ApiKey),
            Some(AuthMode::Pat),
        ] {
            let expected = match (site, auth) {
                (Some(SiteKind::Official), _) | (_, None | Some(AuthMode::OfficialChatGpt)) => {
                    vec![]
                }
                (_, Some(AuthMode::ApiKey)) => vec![VersionCapability::TopLevelOpenaiBaseUrl],
                (_, Some(AuthMode::Pat)) => vec![
                    VersionCapability::TopLevelOpenaiBaseUrl,
                    VersionCapability::PersonalAccessToken,
                    VersionCapability::WhamAccountsCheck,
                ],
                _ => unreachable!("all current variants are listed"),
            };
            assert_eq!(
                rules::required_capabilities(site, auth),
                expected,
                "site={site:?}, auth={auth:?}"
            );
        }
    }
}

fn versioned(label: &str, role: ConsumerRole, version: Option<CodexVersion>) -> VersionedConsumer {
    VersionedConsumer {
        label: label.into(),
        role,
        version,
    }
}

fn too_old(
    consumer: &str,
    found: CodexVersion,
    capability: VersionCapability,
    required: CodexVersion,
) -> BlockReason {
    BlockReason::VersionTooOld {
        consumer: consumer.into(),
        found,
        required,
        capability,
    }
}

#[test]
fn no_capabilities_means_no_gates_even_for_old_or_unknown_consumers() {
    // Plan 9.8: official switches neither block nor ask for unknown-version confirmation.
    let consumers = [
        versioned("old", ConsumerRole::App, Some(CodexVersion::new(0, 1, 0))),
        versioned("unknown", ConsumerRole::Remote, None),
    ];
    assert_eq!(
        rules::gate_versions(&consumers, &[]),
        VersionGate::default()
    );
    assert_eq!(
        rules::gate_versions(&[], &[VersionCapability::PersonalAccessToken]),
        VersionGate::default()
    );
}

#[test]
fn unknown_versions_are_deduplicated_by_label_in_first_seen_order() {
    // Plan 9.8: unknown consumers need one question per label, even across roles.
    let consumers = [
        versioned("second", ConsumerRole::Remote, None),
        versioned("first", ConsumerRole::App, None),
        versioned("second", ConsumerRole::Cli, None),
        versioned("first", ConsumerRole::Remote, None),
    ];
    let caps = [
        VersionCapability::PersonalAccessToken,
        VersionCapability::WhamAccountsCheck,
    ];
    assert_eq!(
        rules::gate_versions(&consumers, &caps),
        VersionGate {
            unknown: vec!["second".into(), "first".into()],
            ..VersionGate::default()
        }
    );
}

#[test]
fn version_gates_separate_cli_from_app_and_remote_and_preserve_order() {
    // Plan 9.8: old CLI binaries are separately overridable; app/remote failures block.
    let found = CodexVersion::new(0, 148, 9);
    let consumers = [
        versioned("remote-b", ConsumerRole::Remote, Some(found.clone())),
        versioned("cli-b", ConsumerRole::Cli, Some(found.clone())),
        versioned("app", ConsumerRole::App, Some(found.clone())),
        versioned("cli-a", ConsumerRole::Cli, Some(found.clone())),
        versioned("remote-a", ConsumerRole::Remote, Some(found.clone())),
    ];
    let reason = |name| {
        too_old(
            name,
            found.clone(),
            VersionCapability::TopLevelOpenaiBaseUrl,
            CodexVersion::new(0, 149, 0),
        )
    };
    assert_eq!(
        rules::gate_versions(&consumers, &[VersionCapability::TopLevelOpenaiBaseUrl]),
        VersionGate {
            blocks: vec![reason("remote-b"), reason("app"), reason("remote-a")],
            cli_too_old: vec![reason("cli-b"), reason("cli-a")],
            unknown: vec![],
        }
    );
}

#[test]
fn each_consumer_reports_only_a_strictest_failing_capability() {
    // Plan 9.8: cap order and duplicates do not produce multiple reasons per consumer.
    let found = CodexVersion::new(0, 148, 9);
    for caps in [
        vec![
            VersionCapability::PersonalAccessToken,
            VersionCapability::TopLevelOpenaiBaseUrl,
        ],
        vec![
            VersionCapability::TopLevelOpenaiBaseUrl,
            VersionCapability::PersonalAccessToken,
        ],
        vec![
            VersionCapability::PersonalAccessToken,
            VersionCapability::PersonalAccessToken,
        ],
    ] {
        let consumers = [versioned("app", ConsumerRole::App, Some(found.clone()))];
        assert_eq!(
            rules::gate_versions(&consumers, &caps),
            VersionGate {
                blocks: vec![too_old(
                    "app",
                    found.clone(),
                    VersionCapability::PersonalAccessToken,
                    CodexVersion::new(0, 160, 0)
                )],
                ..VersionGate::default()
            }
        );
    }
    let consumers = [versioned("app", ConsumerRole::App, Some(found))];
    let gate = rules::gate_versions(
        &consumers,
        &[
            VersionCapability::TopLevelOpenaiBaseUrl,
            VersionCapability::PersonalAccessToken,
            VersionCapability::WhamAccountsCheck,
        ],
    );
    assert_eq!(gate.blocks.len(), 1);
    assert!(
        matches!(&gate.blocks[0], BlockReason::VersionTooOld { capability: VersionCapability::PersonalAccessToken | VersionCapability::WhamAccountsCheck, required, .. } if *required == CodexVersion::new(0, 160, 0))
    );
    assert!(gate.cli_too_old.is_empty());
    assert!(gate.unknown.is_empty());
}

#[test]
fn version_gate_boundaries_ignore_prereleases_but_not_lower_release_triples() {
    // Plan 9.8: exact minima and bundled prereleases pass; the preceding minor fails.
    for (capability, minimum) in [
        (VersionCapability::TopLevelOpenaiBaseUrl, 149),
        (VersionCapability::PersonalAccessToken, 160),
        (VersionCapability::WhamAccountsCheck, 160),
    ] {
        for role in [ConsumerRole::App, ConsumerRole::Remote, ConsumerRole::Cli] {
            for (version, passes) in [
                (CodexVersion::new(0, minimum - 1, 999), false),
                (CodexVersion::new(0, minimum, 0), true),
                (
                    CodexVersion::parse(&format!("0.{minimum}.0-alpha.1")).unwrap(),
                    true,
                ),
                (CodexVersion::new(0, minimum, 1), true),
                (CodexVersion::new(1, 0, 0), true),
            ] {
                let gate = rules::gate_versions(
                    &[versioned("consumer", role, Some(version.clone()))],
                    &[capability],
                );
                let mut expected = VersionGate::default();
                if !passes {
                    let reason = too_old(
                        "consumer",
                        version,
                        capability,
                        CodexVersion::new(0, minimum, 0),
                    );
                    match role {
                        ConsumerRole::Cli => expected.cli_too_old.push(reason),
                        ConsumerRole::App | ConsumerRole::Remote => expected.blocks.push(reason),
                    }
                }
                assert_eq!(gate, expected, "role={role:?}, capability={capability:?}");
            }
        }
    }
}

fn process(pid: u32, kind: CodexProcessKind, home: Option<&str>) -> ProcessInfo {
    ProcessInfo {
        pid,
        kind,
        codex_home: home.map(Into::into),
        ppid: None,
        exe: None,
        cmdline: vec!["codex".into()],
        started_at: None,
        authapi_env: EnvObservation::Unknown,
        stale: false,
    }
}

fn process_kinds() -> [CodexProcessKind; 8] {
    [
        CodexProcessKind::AppMain,
        CodexProcessKind::AppServer,
        CodexProcessKind::CliTui,
        CodexProcessKind::CliExec,
        CodexProcessKind::Other,
        CodexProcessKind::CliDaemon,
        CodexProcessKind::IdeAppServer,
        CodexProcessKind::RemoteLink {
            host: "devbox".into(),
        },
    ]
}

#[test]
fn consumer_groups_cover_every_process_kind() {
    // Plan 7.1 consumer rules: app/core/SSH links have their own decisions, not CLI ones.
    let expected = [
        None,
        None,
        Some(ConsumerKind::CliProcesses),
        Some(ConsumerKind::CliProcesses),
        Some(ConsumerKind::CliProcesses),
        Some(ConsumerKind::CliDaemon),
        Some(ConsumerKind::IdeAppServers),
        None,
    ];
    for (kind, group) in process_kinds().into_iter().zip(expected) {
        assert_eq!(rules::consumer_group(&kind), group, "{kind:?}");
    }
}

#[test]
fn unknown_process_home_counts_as_ours_but_another_home_does_not() {
    // Plan 7.7: unknown homes produce a conservative warning; known foreign homes are excluded.
    for (home, expected) in [
        (None, true),
        (Some("/virtual/ours"), true),
        (Some("/virtual/other"), false),
    ] {
        assert_eq!(
            rules::uses_home(
                &process(1, CodexProcessKind::CliTui, home),
                Path::new("/virtual/ours")
            ),
            expected
        );
    }
}

#[test]
fn consumer_restarts_group_pids_and_append_remotes_in_input_order() {
    // Plan 7.1 consumer rules: app first, CLI/daemon/IDE in fixed order, then selected remotes.
    let home = Path::new("/virtual/ours");
    let processes = vec![
        process(1, CodexProcessKind::CliExec, Some("/virtual/ours")),
        process(2, CodexProcessKind::AppMain, Some("/virtual/ours")),
        process(3, CodexProcessKind::AppServer, None),
        process(4, CodexProcessKind::IdeAppServer, None),
        process(5, CodexProcessKind::CliDaemon, Some("/virtual/ours")),
        process(6, CodexProcessKind::Other, None),
        process(7, CodexProcessKind::CliTui, Some("/virtual/ours")),
        process(8, CodexProcessKind::CliExec, Some("/virtual/other")),
        process(9, CodexProcessKind::CliDaemon, Some("/virtual/other")),
        process(10, CodexProcessKind::IdeAppServer, Some("/virtual/other")),
        process(
            11,
            CodexProcessKind::RemoteLink {
                host: "devbox".into(),
            },
            None,
        ),
        process(12, CodexProcessKind::AppMain, None),
    ];
    let remote_hot = RestartKind::AppServerHot {
        host_id: "zulu".into(),
        kill_remote: true,
    };
    let remotes = [
        (TargetId::new("ssh:zulu"), remote_hot.clone()),
        (TargetId::new("ssh:alpha"), RestartKind::NextLaunch),
    ];
    assert_eq!(
        rules::consumer_restarts(&hot(), true, &processes, home, &remotes),
        vec![
            ConsumerRestart {
                consumer: ConsumerKind::DesktopApp,
                restart: hot(),
                pids: vec![2, 12]
            },
            ConsumerRestart {
                consumer: ConsumerKind::CliProcesses,
                restart: RestartKind::NextLaunch,
                pids: vec![1, 6, 7]
            },
            ConsumerRestart {
                consumer: ConsumerKind::CliDaemon,
                restart: RestartKind::DaemonRestart,
                pids: vec![5]
            },
            ConsumerRestart {
                consumer: ConsumerKind::IdeAppServers,
                restart: RestartKind::NextLaunch,
                pids: vec![4]
            },
            ConsumerRestart {
                consumer: ConsumerKind::Remote {
                    target_id: remotes[0].0.clone()
                },
                restart: remote_hot,
                pids: vec![]
            },
            ConsumerRestart {
                consumer: ConsumerKind::Remote {
                    target_id: remotes[1].0.clone()
                },
                restart: RestartKind::NextLaunch,
                pids: vec![]
            },
        ]
    );
}

#[test]
fn consumer_restarts_omit_empty_groups_but_include_an_explicit_app() {
    // Plan 7.1: include_app controls the app row independently of whether an AppMain exists.
    let home = Path::new("/virtual/ours");
    let primary = full(FullRestartReason::NoCdp);
    assert_eq!(
        rules::consumer_restarts(&primary, true, &[], home, &[]),
        vec![ConsumerRestart {
            consumer: ConsumerKind::DesktopApp,
            restart: primary.clone(),
            pids: vec![],
        }]
    );
    let processes = [
        process(1, CodexProcessKind::AppMain, None),
        process(2, CodexProcessKind::CliTui, Some("/virtual/other")),
    ];
    assert_eq!(
        rules::consumer_restarts(&primary, false, &processes, home, &[]),
        vec![]
    );
    let remotes = [(
        TargetId::new("ssh:devbox"),
        RestartKind::Blocked {
            reasons: vec![BlockReason::MissingKey],
        },
    )];
    assert_eq!(
        rules::consumer_restarts(&primary, false, &[], home, &remotes),
        vec![ConsumerRestart {
            consumer: ConsumerKind::Remote {
                target_id: remotes[0].0.clone()
            },
            restart: remotes[0].1.clone(),
            pids: vec![],
        }]
    );
}

#[test]
fn consumer_restarts_excludes_foreign_home_app_main_pids() {
    // Plan 7.1 consumer scope: known foreign-home processes must be ignored for the app too.
    let processes = [
        process(1, CodexProcessKind::AppMain, Some("/virtual/ours")),
        process(2, CodexProcessKind::AppMain, Some("/virtual/other")),
        process(3, CodexProcessKind::AppMain, None),
    ];
    assert_eq!(
        rules::consumer_restarts(&hot(), true, &processes, Path::new("/virtual/ours"), &[]),
        vec![ConsumerRestart {
            consumer: ConsumerKind::DesktopApp,
            restart: hot(),
            pids: vec![1, 3],
        }]
    );
}

#[test]
fn migration_blockers_are_only_our_tui_exec_other_and_ide_processes() {
    // Plan 7.1 rule 2 / 10.2: users close TUI/exec/IDE; app and daemon are handled separately.
    let mut processes = Vec::new();
    let mut expected = Vec::new();
    for home in [Some("/virtual/ours"), None, Some("/virtual/other")] {
        for kind in process_kinds() {
            let pid = processes.len() as u32 + 1;
            if home != Some("/virtual/other")
                && matches!(
                    kind,
                    CodexProcessKind::CliTui
                        | CodexProcessKind::CliExec
                        | CodexProcessKind::Other
                        | CodexProcessKind::IdeAppServer
                )
            {
                expected.push(pid);
            }
            processes.push(process(pid, kind, home));
        }
    }
    let actual: Vec<_> = rules::migration_blockers(&processes, Path::new("/virtual/ours"))
        .into_iter()
        .map(|p| p.pid)
        .collect();
    assert_eq!(actual, expected);
    assert!(rules::migration_blockers(&[], Path::new("/virtual/ours")).is_empty());
}

#[test]
fn stale_flags_require_strictly_older_config_readers_and_are_recomputed() {
    // Plan 7.7: timestamps must prove staleness; app-main/SSH links never read this config.
    for kind in process_kinds() {
        // Start times within PROCESS_START_SLACK_MS of the switch prove nothing
        // (OS start times are coarse and drift from the wall clock).
        let applied = 100_000;
        let slack = rules::PROCESS_START_SLACK_MS;
        let old = Timestamp(applied - slack - 1);
        for applied_at in [None, Some(Timestamp(applied))] {
            for started_at in [
                None,
                Some(old),
                Some(Timestamp(applied - slack)),
                Some(Timestamp(applied - 1)),
                Some(Timestamp(applied)),
                Some(Timestamp(applied + 1)),
            ] {
                for initial_stale in [false, true] {
                    let mut p = process(1, kind.clone(), None);
                    p.started_at = started_at;
                    p.stale = initial_stale;
                    let expected_stale = applied_at == Some(Timestamp(applied))
                        && started_at == Some(old)
                        && !matches!(
                            kind,
                            CodexProcessKind::AppMain | CodexProcessKind::RemoteLink { .. }
                        );
                    let mut expected = p.clone();
                    expected.stale = expected_stale;
                    rules::mark_stale(std::slice::from_mut(&mut p), applied_at);
                    assert_eq!(
                        p, expected,
                        "kind={kind:?}, applied={applied_at:?}, started={started_at:?}"
                    );
                }
            }
        }
    }
    rules::mark_stale(&mut [], None);
}

#[test]
fn line_lookup_accepts_each_origin_with_or_without_a_trailing_slash() {
    // Plan 7.1 rules 6/7: match the app's auth origin to its line, allowing canonical slashes.
    let site = catalog::krill_site();
    for line in &site.lines {
        for origin in [
            line.origin.to_string(),
            format!("{}/", line.origin),
            format!("  {}  ", line.origin),
        ] {
            assert_eq!(rules::line_for_origin(&site, &origin), Some(line));
        }
    }
}

#[test]
fn line_lookup_rejects_unknown_origins_and_invalid_urls() {
    // Plan 7.1 auth origin lookup must not guess a line for malformed or unrelated addresses.
    let site = catalog::krill_site();
    for origin in [
        "https://unknown.example",
        "",
        "not a URL",
        "http://api.krill-code.net",
        "https://api.krill-code.net/path",
        "https://api.krill-code.net?query",
        "https://api.krill-code.net#fragment",
        "https://user@api.krill-code.net",
    ] {
        assert_eq!(rules::line_for_origin(&site, origin), None, "{origin}");
    }
    assert_eq!(
        rules::line_for_origin(&catalog::official_site(), catalog::KRILL_ORIGIN_OVERSEAS),
        None
    );
}

#[test]
fn launch_authapi_origin_prefers_krill_destination_then_krill_active_then_default() {
    // Plan 7.1 launch note: destination > active Krill origin > the configured Krill default.
    let site = catalog::krill_site();
    let destination = &site.lines[0];
    let active = &site.lines[2];
    let foreign = Line {
        id: LineId::new("foreign"),
        name: LocalizedText::new("test.foreign", "Foreign"),
        origin: Origin::parse("https://foreign.example").unwrap(),
        source: LineSource::Test,
    };
    for (dest, is_krill, current, expected) in [
        (Some(destination), true, Some(active), &destination.origin),
        (Some(destination), true, None, &destination.origin),
        (Some(&foreign), false, Some(active), &active.origin),
        (None, true, Some(active), &active.origin),
        (Some(&foreign), false, Some(&foreign), &site.lines[1].origin),
        (None, false, None, &site.lines[1].origin),
    ] {
        assert_eq!(
            rules::launch_authapi_origin(&site, dest, is_krill, current),
            Some(expected.clone())
        );
    }
    let alias = Line {
        id: LineId::new("alias"),
        ..active.clone()
    };
    assert_eq!(
        rules::launch_authapi_origin(&site, None, false, Some(&alias)),
        Some(active.origin.clone())
    );
}

#[test]
fn active_krill_site_follows_the_applied_krill_kind_site_including_test_sites() {
    // Found by the e2e harness: `spb launch` with a profile on an e2e test site
    // (Krill kind, loopback lines) injected the built-in Krill default line.
    let krill = catalog::krill_site();
    let mut test = catalog::krill_site();
    test.id = superba_core::SiteId::new("krill-e2e");
    test.lines = vec![Line {
        id: LineId::new("a"),
        name: LocalizedText::new("", "Line A"),
        origin: Origin::parse("https://127.0.0.1:18001").unwrap(),
        source: LineSource::Test,
    }];
    let mut compat = catalog::krill_site();
    compat.id = superba_core::SiteId::new("compat");
    compat.kind = SiteKind::Compatible;
    let sites = vec![krill.clone(), test.clone(), compat.clone()];
    let a = LineId::new("a");

    let (site, line) = rules::active_krill_site(&sites, Some(&test.id), Some(&a));
    assert_eq!(site.id, test.id);
    assert_eq!(
        rules::launch_authapi_origin(&site, None, false, line.as_ref()),
        Some(test.lines[0].origin.clone())
    );
    // The built-in Krill site's own lines keep working.
    let overseas = &krill.lines[1];
    let (site, line) = rules::active_krill_site(&sites, Some(&krill.id), Some(&overseas.id));
    assert_eq!(line.as_ref(), Some(overseas));
    assert_eq!(site.id, krill.id);
    // Non-Krill sites, unknown lines and no applied profile fall back to the
    // built-in Krill site without an active line (its default line).
    for (s, l) in [
        (Some(&compat.id), Some(&compat.lines[0].id)),
        (Some(&test.id), Some(&LineId::new("missing"))),
        (None, None),
    ] {
        let (site, line) = rules::active_krill_site(&sites, s, l);
        assert_eq!(site.id, krill.id);
        assert_eq!(line, None);
        assert_eq!(
            rules::launch_authapi_origin(&site, None, false, None),
            krill.default_line().map(|l| l.origin.clone())
        );
    }
    // Without the built-in site in the list the catalog's is used.
    let (site, _) = rules::active_krill_site(&[], None, None);
    assert_eq!(site.id, krill.id);
}

#[test]
fn launch_authapi_origin_uses_first_line_if_default_is_missing_and_none_for_empty_site() {
    // Plan 7.1 launch note: default fallback remains defined without a configured default id.
    let mut site = catalog::krill_site();
    for default_line in [None, Some(LineId::new("missing"))] {
        site.default_line = default_line;
        assert_eq!(
            rules::launch_authapi_origin(&site, None, false, None),
            Some(site.lines[0].origin.clone())
        );
    }
    site.lines.clear();
    assert_eq!(rules::launch_authapi_origin(&site, None, false, None), None);
}

#[test]
fn verification_summary_lists_failed_and_pending_checks_in_verification_order() {
    // Plan 7.2 Verifying -> Recovery: diagnostics retain process/files/auth/line/renderer order.
    let verification = Verification {
        process: CheckResult::Failed {
            detail: "no core".into(),
        },
        files: CheckResult::Pending,
        auth: CheckResult::Failed {
            detail: "whoami timed out".into(),
        },
        line: CheckResult::Failed {
            detail: "models rejected".into(),
        },
        renderer: CheckResult::Failed {
            detail: "login page".into(),
        },
    };
    assert_eq!(
        rules::verification_summary(&verification),
        "process: no core; files: not run; auth: whoami timed out; line: models rejected; renderer: login page"
    );
    assert_eq!(
        rules::verification_summary(&Verification::default()),
        "process: not run; files: not run; auth: not run; line: not run; renderer: not run"
    );
}

#[test]
fn verification_summary_omits_passed_and_intentionally_skipped_checks() {
    // Plan 7.2 Verifying -> Done: deliberately skipped checks are not failures.
    let verification = Verification {
        process: CheckResult::Passed,
        files: CheckResult::Passed,
        auth: CheckResult::Skipped {
            reason: "official login".into(),
        },
        line: CheckResult::Passed,
        renderer: CheckResult::Skipped {
            reason: "no CDP".into(),
        },
    };
    assert_eq!(
        rules::verification_summary(&verification),
        "all checks passed"
    );
}

#[test]
fn retry_ipc_only_helps_a_failed_process_check() {
    // Plan 7.2 Recovery -> Restarting: only a failed core startup warrants another IPC.
    for process in [
        CheckResult::Pending,
        CheckResult::Passed,
        CheckResult::Skipped {
            reason: "not needed".into(),
        },
        CheckResult::Failed {
            detail: "no core".into(),
        },
    ] {
        let expected = matches!(process, CheckResult::Failed { .. });
        let failed = CheckResult::Failed {
            detail: "configuration error".into(),
        };
        let verification = Verification {
            process,
            files: failed.clone(),
            auth: failed.clone(),
            line: failed.clone(),
            renderer: failed,
        };
        assert_eq!(rules::retry_ipc_helps(&verification), expected);
    }
}

#[test]
#[cfg(target_os = "linux")]
fn linux_supports_hot_switching() {
    // Plan 7.1 rule 5: Linux has the verified hot restart implementation.
    assert!(rules::platform_hot_supported());
}

#[test]
fn pat_version_ok_needs_every_known_local_codex_to_support_pat() {
    let v = |minor| Some(CodexVersion::new(0, minor, 0));
    let app = |version| versioned("app", ConsumerRole::App, version);
    let cli = |version| versioned("cli", ConsumerRole::Cli, version);
    assert_eq!(rules::pat_version_ok(&[]), None);
    assert_eq!(
        rules::pat_version_ok(&[app(v(162)), cli(v(160))]),
        Some(true)
    );
    // Old enough for openai_base_url (API key) but not for PAT.
    assert_eq!(
        rules::pat_version_ok(&[app(v(150)), cli(v(162))]),
        Some(false)
    );
    assert_eq!(
        rules::pat_version_ok(&[app(v(162)), cli(v(159))]),
        Some(false)
    );
    // A known too-old version decides even when another one is unknown.
    assert_eq!(
        rules::pat_version_ok(&[app(None), cli(v(150))]),
        Some(false)
    );
    assert_eq!(rules::pat_version_ok(&[app(v(162)), cli(None)]), None);
}

#[test]
fn local_versioned_consumers_are_the_chosen_app_and_the_first_cli_on_path() {
    use std::path::PathBuf;
    use superba_core::{AppInstall, AppVariant, CodexBinary, InstallSource};
    let install = |id: &str, minor| AppInstall {
        id: id.into(),
        variant: AppVariant::ChatGpt,
        path: PathBuf::from("/opt").join(id),
        executable: PathBuf::from("/opt").join(id).join("app"),
        app_version: None,
        bundled_codex: None,
        bundled_codex_version: Some(CodexVersion::new(0, minor, 0)),
    };
    let binary = |path: &str, rank: Option<usize>, minor| CodexBinary {
        path: PathBuf::from(path),
        version: Some(CodexVersion::new(0, minor, 0)),
        source: InstallSource::Npm,
        path_rank: rank,
    };
    let installs = [install("a", 150), install("b", 162)];
    let binaries = [
        binary("/opt/other/codex", None, 140),
        binary("/usr/bin/codex", Some(0), 161),
    ];
    let c = rules::local_versioned_consumers(Some("b"), &installs, &binaries, "App");
    assert_eq!(
        c,
        vec![
            versioned("App", ConsumerRole::App, Some(CodexVersion::new(0, 162, 0))),
            versioned(
                "/usr/bin/codex",
                ConsumerRole::Cli,
                Some(CodexVersion::new(0, 161, 0))
            ),
        ]
    );
    // Unknown install id: the first install; no CLI on the PATH: none.
    let c = rules::local_versioned_consumers(Some("zz"), &installs, &binaries[..1], "App");
    assert_eq!(
        c,
        vec![versioned(
            "App",
            ConsumerRole::App,
            Some(CodexVersion::new(0, 150, 0))
        )]
    );
}
