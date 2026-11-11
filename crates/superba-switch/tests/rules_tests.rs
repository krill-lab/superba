//! Pure planner contracts: plan 7.1 restart rules, 9.8 version gates and 7.7 consumers.

use std::path::Path;

use pretty_assertions::assert_eq;
use superba_core::{
    AuthFamily, AuthMode, BlockReason, CheckResult, CodexProcessKind, CodexVersion, ConsumerKind,
    ConsumerRestart, EnvObservation, FullRestartReason, ProcessInfo, RestartKind, SiteKind,
    TargetId, Timestamp, Verification, catalog, codex::VersionCapability,
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

fn running(cdp: bool, bridge_ready: bool) -> AppFacts {
    AppFacts::Running { cdp, bridge_ready }
}

fn ready_facts() -> LocalFacts {
    LocalFacts {
        blocks: Vec::new(),
        migration: false,
        current_family: Some(AuthFamily::Token),
        target_family: Some(AuthFamily::Token),
        app: running(true, true),
        hot_supported: true,
    }
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
            app: running(false, false),
            ..ready_facts()
        };
        assert_eq!(
            rules::decide_local(&facts),
            full(FullRestartReason::CrossAuthFamily)
        );
    }
}

#[test]
fn rule_4_stopped_app_ignores_platform_requirements() {
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
fn rule_5_checks_platform_then_cdp_then_bridge() {
    // Plan 7.1 rule 5: each prerequisite beats the next.
    for (hot_supported, cdp, bridge, expected) in [
        (false, false, false, full(FullRestartReason::PlatformNoHot)),
        (true, false, false, full(FullRestartReason::NoCdp)),
        (true, true, false, full(FullRestartReason::NoBridge)),
        (true, true, true, hot()),
    ] {
        let facts = LocalFacts {
            hot_supported,
            app: running(cdp, bridge),
            ..ready_facts()
        };
        assert_eq!(rules::decide_local(&facts), expected, "{facts:?}");
    }
}

fn documented_decision(facts: &LocalFacts) -> RestartKind {
    // Independent declarative oracle: rank applicable rules, without calling planner helpers.
    let (is_running, cdp, bridge) = match &facts.app {
        AppFacts::NotRunning => (false, false, false),
        AppFacts::Running { cdp, bridge_ready } => (true, *cdp, *bridge_ready),
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
        (60, true, hot()),
    ]
    .into_iter()
    .filter(|(_, applies, _)| *applies)
    .min_by_key(|(rank, _, _)| *rank)
    .expect("rule 6 always applies")
    .2
}

#[test]
fn all_local_fact_combinations_follow_the_documented_first_match_order() {
    // Plan 7.1 rules 1-6: every combination also exercises non-adjacent precedence.
    let families = [None, Some(AuthFamily::Token), Some(AuthFamily::ChatGpt)];
    let mut checked = 0;
    for mask in 0..64 {
        let flag = |bit| mask & (1 << bit) != 0;
        for current_family in families {
            for target_family in families {
                let facts = LocalFacts {
                    blocks: if flag(0) {
                        vec![BlockReason::MissingKey]
                    } else {
                        vec![]
                    },
                    migration: flag(1),
                    app: if flag(2) {
                        running(flag(4), flag(5))
                    } else {
                        AppFacts::NotRunning
                    },
                    hot_supported: flag(3),
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
    assert_eq!(checked, 576);
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
fn version_table_and_lookup_pin_the_capability_threshold() {
    // Plan 9.8: top-level openai_base_url starts at 0.149.
    let expected = [(
        VersionCapability::TopLevelOpenaiBaseUrl,
        CodexVersion::new(0, 149, 0),
    )];
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
        ] {
            let expected = match (site, auth) {
                (Some(SiteKind::Official), _) | (_, None | Some(AuthMode::OfficialChatGpt)) => {
                    vec![]
                }
                (_, Some(AuthMode::ApiKey)) => vec![VersionCapability::TopLevelOpenaiBaseUrl],
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
        rules::gate_versions(&[], &[VersionCapability::TopLevelOpenaiBaseUrl]),
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
    let caps = [VersionCapability::TopLevelOpenaiBaseUrl];
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
fn each_consumer_reports_one_reason_even_for_repeated_capabilities() {
    // Plan 9.8: duplicates do not produce multiple reasons per consumer.
    let found = CodexVersion::new(0, 148, 9);
    let consumers = [versioned("app", ConsumerRole::App, Some(found.clone()))];
    let gate = rules::gate_versions(
        &consumers,
        &[
            VersionCapability::TopLevelOpenaiBaseUrl,
            VersionCapability::TopLevelOpenaiBaseUrl,
        ],
    );
    assert_eq!(
        gate,
        VersionGate {
            blocks: vec![too_old(
                "app",
                found,
                VersionCapability::TopLevelOpenaiBaseUrl,
                CodexVersion::new(0, 149, 0)
            )],
            ..VersionGate::default()
        }
    );
}

#[test]
fn version_gate_boundaries_ignore_prereleases_but_not_lower_release_triples() {
    // Plan 9.8: exact minima and bundled prereleases pass; the preceding minor fails.
    for (capability, minimum) in [(VersionCapability::TopLevelOpenaiBaseUrl, 149)] {
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
        ide: None,
        cwd: None,
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
        "http://api.kbsfdxc.net",
        "https://api.kbsfdxc.net/path",
        "https://api.kbsfdxc.net?query",
        "https://api.kbsfdxc.net#fragment",
        "https://user@api.kbsfdxc.net",
    ] {
        assert_eq!(rules::line_for_origin(&site, origin), None, "{origin}");
    }
    assert_eq!(
        rules::line_for_origin(&catalog::official_site(), catalog::KRILL_ORIGIN_OVERSEAS),
        None
    );
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
