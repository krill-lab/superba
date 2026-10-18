//! Plan 7.3 wording for the plan 7.1 decisions and 7.2 UI/execution transitions.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use pretty_assertions::assert_eq;
use superba_core::{
    ActivitySignal, AuthMode, BlockReason, Catalog, CheckResult, CodexProcessKind, ConsumerKind,
    Decision, DecisionRequest, EnvObservation, Error, FullRestartReason, KeyChange, LineId, Locale,
    LocalizedText, OutcomeStatus, PlanChoice, ProcessInfo, RcLine, RestartKind, StepStatus,
    SwitchPhase, SwitchStep, TargetId, ThreadActivity, Verification, catalog,
    i18n::{core_catalog_source, flatten_toml},
};
use superba_switch::messages;

const EN_SOURCE: &str = include_str!("../locales/en.toml");
const ZH_SOURCE: &str = include_str!("../locales/zh-CN.toml");

struct Language {
    locale: Locale,
    raw: BTreeMap<String, String>,
    with_core: BTreeMap<String, String>,
    renderer: Catalog,
}

static LANGUAGES: LazyLock<[Language; 2]> = LazyLock::new(|| {
    [(Locale::En, EN_SOURCE), (Locale::ZhCn, ZH_SOURCE)].map(|(locale, source)| {
        let raw: BTreeMap<_, _> = flatten_toml(source)
            .expect("switch catalog must parse")
            .into_iter()
            .collect();
        let mut with_core: BTreeMap<_, _> = flatten_toml(core_catalog_source(locale))
            .expect("core catalog must parse")
            .into_iter()
            .collect();
        with_core.extend(raw.clone());
        Language {
            locale,
            raw,
            with_core,
            renderer: messages::catalog(locale),
        }
    })
});

fn placeholders(template: &str) -> BTreeSet<&str> {
    let mut names = BTreeSet::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        assert!(
            !rest[..start].contains('}'),
            "unmatched closing brace: {template}"
        );
        let tail = &rest[start + 1..];
        let end = tail
            .find('}')
            .unwrap_or_else(|| panic!("unclosed placeholder: {template}"));
        let name = tail[..end].split('|').next().unwrap();
        assert!(
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "invalid placeholder in {template}"
        );
        names.insert(name);
        rest = &tail[end + 1..];
    }
    assert!(!rest.contains('}'), "unmatched closing brace: {template}");
    names
}

fn assert_arguments_supplied(text: &LocalizedText, language: &Language) {
    // Check the actual language source, so English fallback cannot hide a missing Chinese key.
    let template = language
        .with_core
        .get(&text.key)
        .unwrap_or_else(|| panic!("missing {} key {}", language.locale, text.key));
    let supplied: BTreeSet<_> = text
        .args
        .iter()
        .map(|(name, _)| name.as_str())
        .chain(text.nested.iter().map(|(name, _)| name.as_str()))
        .collect();
    assert_eq!(
        supplied.len(),
        text.args.len() + text.nested.len(),
        "duplicate arguments for {}",
        text.key
    );
    for template in [template.as_str(), text.fallback.as_str()] {
        let required = placeholders(template);
        assert!(
            required.is_subset(&supplied),
            "{} {} missing {:?}",
            language.locale,
            text.key,
            required.difference(&supplied).collect::<Vec<_>>()
        );
    }
    for (_, nested) in &text.nested {
        assert_arguments_supplied(nested, language);
    }
}

fn assert_message(text: &LocalizedText, key: &str) -> [String; 2] {
    assert_eq!(text.key, key);
    assert_eq!(text.fallback, LANGUAGES[0].raw[key]);
    std::array::from_fn(|i| {
        let language = &LANGUAGES[i];
        assert!(
            language.raw.contains_key(key),
            "{} missing {key}",
            language.locale
        );
        assert_arguments_supplied(text, language);
        let rendered = language.renderer.text(text);
        assert!(
            !rendered.trim().is_empty(),
            "{} {key} rendered empty",
            language.locale
        );
        assert!(
            !rendered.contains(['{', '}']),
            "{} {key} has unresolved placeholders: {rendered}",
            language.locale
        );
        rendered
    })
}

fn assert_args(text: &LocalizedText, expected: &[(&str, &str)]) {
    let expected: Vec<_> = expected
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    assert_eq!(text.args, expected, "{} arguments", text.key);
}

fn contains_cjk(text: &str) -> bool {
    text.chars().any(|c| matches!(c, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{f900}'..='\u{faff}'))
}

fn thread(target_id: &str, title: Option<&str>) -> ThreadActivity {
    ThreadActivity {
        target_id: TargetId::new(target_id),
        thread_id: Some("thread-id".into()),
        title: title.map(str::to_owned),
        rollout_path: None,
        last_event_at: None,
        signal: ActivitySignal::RolloutTail,
        originator: None,
        cwd: None,
    }
}

fn process(pid: u32, cmdline: &[&str]) -> ProcessInfo {
    ProcessInfo {
        pid,
        ppid: None,
        kind: CodexProcessKind::CliTui,
        exe: None,
        cmdline: cmdline.iter().map(|arg| (*arg).to_owned()).collect(),
        started_at: None,
        authapi_env: EnvObservation::Unknown,
        codex_home: None,
        stale: false,
        ide: None,
        cwd: None,
    }
}

fn full_restart_reasons() -> [FullRestartReason; 8] {
    [
        FullRestartReason::NoCdp,
        FullRestartReason::NoBridge,
        FullRestartReason::AuthApiMissing,
        FullRestartReason::AuthApiUnhealthy,
        FullRestartReason::CrossAuthFamily,
        FullRestartReason::SessionMigration,
        FullRestartReason::PlatformNoHot,
        FullRestartReason::Requested,
    ]
}

#[test]
fn both_catalogs_parse_and_export_exactly_the_same_keys() {
    // Plan 7.3: every plan 7.1 decision and plan 7.2 state has the same catalog surface in both locales.
    let english = flatten_toml(EN_SOURCE).unwrap();
    let chinese = flatten_toml(ZH_SOURCE).unwrap();
    assert!(!english.is_empty());
    assert_eq!(
        english.keys().collect::<BTreeSet<_>>(),
        chinese.keys().collect::<BTreeSet<_>>()
    );
    assert_eq!(messages::SWITCH_EN, EN_SOURCE);
    assert_eq!(messages::SWITCH_ZH_CN, ZH_SOURCE);
    assert_eq!(messages::catalog_source(Locale::En), EN_SOURCE);
    assert_eq!(messages::catalog_source(Locale::ZhCn), ZH_SOURCE);
}

#[test]
fn catalog_entries_are_nonempty_switch_keys_without_leaf_prefix_collisions() {
    // Plan 7.3 catalog contract: a message leaf cannot shadow a table of more messages.
    for language in LANGUAGES.iter() {
        for (key, template) in &language.raw {
            assert!(key.starts_with("switch."), "{}: {key}", language.locale);
            assert!(!template.trim().is_empty(), "{}: {key}", language.locale);
            for (dot, _) in key.match_indices('.') {
                assert!(
                    !language.raw.contains_key(&key[..dot]),
                    "{}: {} is both a leaf and a prefix of {key}",
                    language.locale,
                    &key[..dot]
                );
            }
        }
    }
}

#[test]
fn every_catalog_key_has_identical_placeholders_in_both_languages() {
    // Plan 7.3: the same LocalizedText arguments must satisfy both language templates.
    for (key, english) in &LANGUAGES[0].raw {
        let chinese = &LANGUAGES[1].raw[key];
        assert_eq!(placeholders(english), placeholders(chinese), "{key}");
    }
}

#[test]
fn text_uses_the_exact_english_template_as_fallback_for_every_key() {
    // Plan 7.3: fallback wording comes from en.toml, even when callers have no catalog.
    for (key, template) in &LANGUAGES[0].raw {
        let text = messages::text(key);
        assert_eq!(text, LocalizedText::new(key, template));
        assert!(messages::has_key(key), "{key}");
    }
}

#[test]
fn unknown_keys_fall_back_to_the_key_and_are_not_reported_present() {
    // Plan 7.3: missing switch keys are visible; core-only keys are not switch keys.
    for key in [
        "switch.missing",
        "switch.phase",
        "switch.phase.IDLE",
        "line.krill.overseas",
        "",
    ] {
        assert_eq!(messages::text(key), LocalizedText::new(key, key));
        assert!(!messages::has_key(key), "{key}");
    }
}

#[test]
fn layered_catalogs_contain_every_actual_translation_and_render_all_templates() {
    // Plan 7.3: loading the Chinese layer cannot silently fall back to English after a parse error.
    for language in LANGUAGES.iter() {
        assert_eq!(language.renderer.locale(), language.locale);
        for (key, template) in &language.raw {
            assert_eq!(
                language.renderer.get(key),
                Some(template.as_str()),
                "{}: {key}",
                language.locale
            );
            let mut text = messages::text(key);
            let mut expected = template.clone();
            for placeholder in placeholders(template) {
                let value = format!("sample-{placeholder}");
                text = text.arg(placeholder, &value);
                expected = expected.replace(&format!("{{{placeholder}}}"), &value);
            }
            let rendered = language.renderer.text(&text);
            assert_eq!(rendered, expected, "{}: {key}", language.locale);
            assert!(!rendered.contains(['{', '}']), "{key}");
        }
    }
    assert_eq!(
        LANGUAGES[0].renderer.get("line.krill.overseas"),
        Some("Overseas")
    );
    assert_eq!(
        LANGUAGES[1].renderer.get("line.krill.overseas"),
        Some("海外线路")
    );
}

#[test]
fn all_nineteen_phases_have_unique_stable_ids_and_chinese_translations() {
    // Plan 7.2: every state from Idle through recovery and Done is displayed in the active locale.
    let phases = [
        (SwitchPhase::Idle, "idle"),
        (SwitchPhase::Planning, "planning"),
        (SwitchPhase::Blocked, "blocked"),
        (SwitchPhase::Armed, "armed"),
        (SwitchPhase::ConfirmFull, "confirm_full"),
        (SwitchPhase::TurnCheck, "turn_check"),
        (SwitchPhase::WaitingTurn, "waiting_turn"),
        (SwitchPhase::Quitting, "quitting"),
        (SwitchPhase::ForceQuitConfirm, "force_quit_confirm"),
        (SwitchPhase::Applying, "applying"),
        (SwitchPhase::RollingBack, "rolling_back"),
        (SwitchPhase::Restarting, "restarting"),
        (SwitchPhase::FullRestartOffer, "full_restart_offer"),
        (SwitchPhase::Launching, "launching"),
        (SwitchPhase::Verifying, "verifying"),
        (SwitchPhase::NavRestore, "nav_restore"),
        (SwitchPhase::Recovery, "recovery"),
        (SwitchPhase::RollbackRestart, "rollback_restart"),
        (SwitchPhase::Done, "done"),
    ];
    assert_eq!(messages::ALL_PHASES, phases.map(|(phase, _)| phase));
    let mut ids = BTreeSet::new();
    for (phase, id) in phases {
        assert_eq!(messages::phase_id(phase), id);
        assert!(ids.insert(messages::phase_id(phase)), "duplicate id: {id}");
        let text = messages::phase(phase);
        assert_args(&text, &[]);
        assert!(text.nested.is_empty());
        let [english, chinese] = assert_message(&text, &format!("switch.phase.{id}"));
        assert_ne!(english, chinese, "{id}");
        assert!(contains_cjk(&chinese), "{id}: {chinese}");
    }
    assert_eq!(ids.len(), 19);
}

#[test]
fn every_step_maps_to_its_key_and_remote_steps_strip_the_ssh_prefix() {
    // Plan 7.2 Applying/Restarting/Verifying: each step has its own localized progress label.
    for (step, suffix) in [
        (SwitchStep::WriteConfig, "write_config"),
        (SwitchStep::WriteEnv, "write_env"),
        (SwitchStep::MigrateSessions, "migrate_sessions"),
        (SwitchStep::QuitApp, "quit_app"),
        (SwitchStep::RestartCore, "restart_core"),
        (SwitchStep::LaunchApp, "launch_app"),
        (SwitchStep::VerifyProcess, "verify_process"),
        (SwitchStep::VerifyFiles, "verify_files"),
        (SwitchStep::VerifyAuth, "verify_auth"),
        (SwitchStep::VerifyLine, "verify_line"),
        (SwitchStep::VerifyRenderer, "verify_renderer"),
        (
            SwitchStep::Remote {
                target_id: TargetId::new("ssh:devbox"),
            },
            "remote",
        ),
    ] {
        let text = messages::step(&step);
        assert_message(&text, &format!("switch.step.{suffix}"));
        assert_args(
            &text,
            if suffix == "remote" {
                &[("host", "devbox")]
            } else {
                &[]
            },
        );
        assert!(text.nested.is_empty());
    }
}

#[test]
fn every_step_status_renders_and_failed_status_supplies_its_message() {
    // Plan 7.2 progress: a failed step carries its reason; other statuses have no arguments.
    for (status, suffix) in [
        (StepStatus::Pending, "pending"),
        (StepStatus::Running, "running"),
        (StepStatus::Done, "done"),
        (StepStatus::Skipped, "skipped"),
        (
            StepStatus::Failed {
                message: "write was rejected".into(),
            },
            "failed",
        ),
    ] {
        let text = messages::step_status(&status);
        assert_message(&text, &format!("switch.step_status.{suffix}"));
        assert_args(
            &text,
            if suffix == "failed" {
                &[("message", "write was rejected")]
            } else {
                &[]
            },
        );
        assert!(text.nested.is_empty());
    }
}

#[test]
fn every_outcome_status_has_its_own_localized_key() {
    // Plan 7.2 Running -> Done: distinguish success, write-only, cancellation, rollback and failure.
    for (status, suffix) in [
        (OutcomeStatus::Switched, "switched"),
        (OutcomeStatus::WrittenOnly, "written_only"),
        (OutcomeStatus::Cancelled, "cancelled"),
        (OutcomeStatus::RolledBack, "rolled_back"),
        (OutcomeStatus::Partial, "partial"),
        (OutcomeStatus::Failed, "failed"),
    ] {
        let text = messages::outcome(status);
        assert_message(&text, &format!("switch.outcome.{suffix}"));
        assert_args(&text, &[]);
        assert!(text.nested.is_empty());
    }
}

#[test]
fn every_restart_kind_has_a_label_and_full_restart_reasons_share_the_kind_label() {
    // Plan 7.1 rules 1/2/4 and consumer rules: kind labels remain distinct from reason text.
    let mut cases = vec![
        (
            RestartKind::Blocked {
                reasons: vec![BlockReason::MissingKey],
            },
            "blocked",
        ),
        (RestartKind::None, "none"),
        (RestartKind::NextLaunch, "next_launch"),
        (RestartKind::DaemonRestart, "daemon_restart"),
    ];
    cases.extend(
        full_restart_reasons()
            .map(|reason| (RestartKind::FullAppRestart { reason }, "full_app_restart")),
    );
    for (kind, suffix) in cases {
        let text = messages::restart(&kind);
        assert_message(&text, &format!("switch.restart.{suffix}"));
        assert_args(&text, &[]);
        assert!(text.nested.is_empty());
    }
}

#[test]
fn local_and_remote_hot_restarts_use_different_keys_based_on_host_id() {
    // Plan 7.1 rule 8/remote rule: local hot wording omits a host; remote wording identifies it.
    for kill_remote in [false, true] {
        let local = messages::restart(&RestartKind::AppServerHot {
            host_id: "local".into(),
            kill_remote,
        });
        assert_message(&local, "switch.restart.app_server_hot");
        assert_args(&local, &[]);
        let remote = messages::restart(&RestartKind::AppServerHot {
            host_id: "remote-host-id".into(),
            kill_remote,
        });
        assert_message(&remote, "switch.restart.app_server_hot_remote");
        assert_args(&remote, &[("host", "remote-host-id")]);
        assert_ne!(local.key, remote.key);
    }
}

#[test]
fn every_consumer_group_has_a_label_including_the_remote_target_name() {
    // Plan 7.1 consumer rules: each restart row names the affected consumer.
    for (consumer, suffix) in [
        (ConsumerKind::DesktopApp, "desktop_app"),
        (ConsumerKind::CliProcesses, "cli_processes"),
        (ConsumerKind::CliDaemon, "cli_daemon"),
        (ConsumerKind::IdeAppServers, "ide_app_servers"),
        (
            ConsumerKind::Remote {
                target_id: TargetId::new("ssh:devbox"),
            },
            "remote",
        ),
    ] {
        let text = messages::consumer(&consumer);
        assert_message(&text, &format!("switch.consumer.{suffix}"));
        assert_args(
            &text,
            if suffix == "remote" {
                &[("host", "devbox")]
            } else {
                &[]
            },
        );
        assert!(text.nested.is_empty());
    }
}

#[test]
fn every_decision_maps_to_a_localized_action() {
    // Plan 7.2 WaitingTurn/ForceQuitConfirm/FullRestartOffer/Recovery: every answer has a button.
    for (decision, suffix) in [
        (Decision::Wait, "wait_turns"),
        (Decision::Force, "force"),
        (Decision::Cancel, "cancel"),
        (Decision::FullRestart, "full_restart"),
        (Decision::Undo, "undo"),
        (Decision::KeepWritten, "keep_written"),
        (Decision::Rollback, "rollback"),
        (Decision::Continue, "continue"),
    ] {
        let text = messages::decision(decision);
        assert_message(&text, &format!("switch.action.{suffix}"));
        assert_args(&text, &[]);
        assert!(text.nested.is_empty());
    }
}

#[test]
fn every_execution_question_supplies_its_specific_context() {
    // Plan 7.2 waiting, force-quit, IPC failure, recovery and migration questions render completely.
    let error = Error::other("bridge unavailable");
    let verification = Verification {
        process: CheckResult::Passed,
        files: CheckResult::Passed,
        auth: CheckResult::Failed {
            detail: "whoami timed out".into(),
        },
        line: CheckResult::Pending,
        renderer: CheckResult::Skipped {
            reason: "no renderer".into(),
        },
    };
    let cases = [
        (
            DecisionRequest::WaitForTurns {
                busy: vec![thread("local", Some("one")), thread("ssh:devbox", None)],
                waited_ms: 1234,
            },
            "switch.waiting.title",
            vec![("count", "2")],
        ),
        (
            DecisionRequest::ForceQuit { pids: vec![41, 42] },
            "switch.question.force_quit",
            vec![],
        ),
        (
            DecisionRequest::IpcFailed {
                error: error.clone(),
            },
            "switch.question.ipc_failed",
            vec![],
        ),
        (
            DecisionRequest::VerificationFailed { verification },
            "switch.question.verification_failed",
            vec![("detail", "auth: whoami timed out; line: not run")],
        ),
        (
            DecisionRequest::UnknownRemoteActivity {
                targets: vec![TargetId::new("ssh:devbox"), TargetId::new("buildbox")],
            },
            "switch.question.unknown_remote_activity",
            vec![("hosts", "devbox, buildbox")],
        ),
        (
            DecisionRequest::CloseConsumers {
                processes: vec![process(41, &["runner", "exec"]), process(42, &[])],
                busy: vec![],
            },
            "switch.question.close_consumers",
            vec![("processes", "runner (pid 41), codex (pid 42)")],
        ),
    ];
    for (request, key, args) in cases {
        let text = messages::question(&request);
        assert_message(&text, key);
        assert_args(&text, &args);
        if matches!(request, DecisionRequest::IpcFailed { .. }) {
            assert_eq!(text.nested, vec![("error".into(), error.to_text())]);
        } else {
            assert!(text.nested.is_empty());
        }
    }
}

#[test]
fn questions_with_empty_lists_still_supply_their_placeholders() {
    // Plan 7.2 live observations may become empty before a waiting/consumer dialog renders.
    for (request, key, args) in [
        (
            DecisionRequest::WaitForTurns {
                busy: vec![],
                waited_ms: 0,
            },
            "switch.waiting.title",
            vec![("count", "0")],
        ),
        (
            DecisionRequest::UnknownRemoteActivity { targets: vec![] },
            "switch.question.unknown_remote_activity",
            vec![("hosts", "")],
        ),
        (
            DecisionRequest::CloseConsumers {
                processes: vec![],
                busy: vec![],
            },
            "switch.question.close_consumers",
            vec![("processes", "")],
        ),
    ] {
        let text = messages::question(&request);
        assert_message(&text, key);
        assert_args(&text, &args);
    }
}

#[test]
fn busy_threads_distinguish_targets_trim_titles_and_localize_untitled_fallbacks() {
    // Plan 7.2 WaitingTurn: local and remote rows preserve titles or nest a translated fallback.
    for target in ["local", "ssh:devbox"] {
        for title in [
            None,
            Some(""),
            Some(" \t\n"),
            Some("\u{2003}"),
            Some("  Writing tests \t"),
            Some("中文标题"),
        ] {
            let text = messages::busy_thread(&thread(target, title));
            let key = if target == "local" {
                "switch.waiting.thread_local"
            } else {
                "switch.waiting.thread_remote"
            };
            let [_, chinese] = assert_message(&text, key);
            let mut expected = if target == "local" {
                vec![]
            } else {
                vec![("host", "devbox")]
            };
            match title.map(str::trim).filter(|value| !value.is_empty()) {
                Some(title) => {
                    expected.push(("title", title));
                    assert!(text.nested.is_empty());
                    assert!(chinese.contains(title));
                }
                None => {
                    assert_eq!(
                        text.nested,
                        vec![("title".into(), messages::text("switch.waiting.untitled"))]
                    );
                    assert!(chinese.contains("未命名对话"));
                    assert!(!chinese.contains("untitled conversation"));
                }
            }
            assert_args(&text, &expected);
        }
    }
}

#[test]
fn every_plan_choice_supplies_counts_names_and_ordered_lists() {
    // Plan 7.2 Planning -> NeedsAnswers: each choice carries enough context to answer and re-plan.
    let cases = [
        (
            PlanChoice::LegacySessions {
                providers: BTreeMap::from([("zeta".into(), 4), ("alpha".into(), 3)]),
            },
            "legacy_sessions",
            vec![("count", "7"), ("provider", "alpha, zeta")],
        ),
        (
            PlanChoice::RestoreProvider {
                baseline_provider: "legacy".into(),
                created_during: 5,
                migrated_in: 8,
            },
            "restore_provider",
            vec![("provider", "legacy"), ("created", "5"), ("migrated", "8")],
        ),
        (
            PlanChoice::ReplacementModel {
                current: "old-model".into(),
                suggestions: vec!["model-z".into(), "model-a".into()],
            },
            "replacement_model",
            vec![("model", "old-model"), ("suggestions", "model-z, model-a")],
        ),
        (
            PlanChoice::ForeignEnv {
                lines: vec![
                    RcLine {
                        path: "/virtual/.zshenv".into(),
                        line_no: 7,
                        text: "export EXAMPLE=a".into(),
                        value: "https://one.example".into(),
                    },
                    RcLine {
                        path: "/virtual/.profile".into(),
                        line_no: 12,
                        text: "export EXAMPLE=b".into(),
                        value: "https://two.example".into(),
                    },
                ],
            },
            "foreign_env",
            vec![(
                "lines",
                "/virtual/.zshenv:7 = https://one.example; /virtual/.profile:12 = https://two.example",
            )],
        ),
        (
            PlanChoice::UnknownVersion {
                consumer: "Codex app".into(),
            },
            "unknown_version",
            vec![("consumer", "Codex app")],
        ),
        (
            PlanChoice::OwnedKeyConflict {
                conflicts: vec![
                    KeyChange {
                        path: "features.fast_mode".into(),
                        before: Some("true".into()),
                        after: Some("false".into()),
                    },
                    KeyChange {
                        path: "model".into(),
                        before: None,
                        after: Some("custom".into()),
                    },
                ],
                auth: None,
                source: Default::default(),
            },
            "owned_key_conflict",
            vec![("keys", "features.fast_mode, model")],
        ),
        (
            PlanChoice::SharedHome {
                targets: vec![TargetId::new("ssh:devbox"), TargetId::new("other")],
            },
            "shared_home",
            vec![("hosts", "devbox, other")],
        ),
    ];
    for (choice, suffix, args) in cases {
        let text = messages::choice(&choice);
        assert_message(&text, &format!("switch.choice.{suffix}"));
        assert_args(&text, &args);
        if suffix == "owned_key_conflict" {
            // GH#5: who made the change is a localized text of its own.
            let names: Vec<&str> = text.nested.iter().map(|(n, _)| n.as_str()).collect();
            assert_eq!(names, ["source"]);
        } else {
            assert!(text.nested.is_empty());
        }
    }
}

#[test]
fn empty_choice_collections_render_without_missing_arguments() {
    // Plan 7.2 NeedsAnswers: empty previews still substitute counts and list placeholders.
    for (choice, suffix, args) in [
        (
            PlanChoice::LegacySessions {
                providers: BTreeMap::new(),
            },
            "legacy_sessions",
            vec![("count", "0"), ("provider", "")],
        ),
        (
            PlanChoice::ReplacementModel {
                current: "model".into(),
                suggestions: vec![],
            },
            "replacement_model",
            vec![("model", "model"), ("suggestions", "")],
        ),
        (
            PlanChoice::ForeignEnv { lines: vec![] },
            "foreign_env",
            vec![("lines", "")],
        ),
        (
            PlanChoice::OwnedKeyConflict {
                conflicts: vec![],
                auth: None,
                source: Default::default(),
            },
            "owned_key_conflict",
            vec![("keys", "")],
        ),
        (
            PlanChoice::SharedHome { targets: vec![] },
            "shared_home",
            vec![("hosts", "")],
        ),
    ] {
        let text = messages::choice(&choice);
        assert_message(&text, &format!("switch.choice.{suffix}"));
        assert_args(&text, &args);
    }
}

#[test]
fn countdown_banner_has_five_ordered_lines_with_seconds_and_a_nested_destination() {
    // Plan 7.2 Armed / 7.3: title, explanation and three interruption bullets always render.
    let destination = messages::text("switch.destination.original");
    for seconds in [0, 5, u64::MAX] {
        let lines = messages::countdown_banner(&destination, seconds);
        assert_eq!(lines.len(), 5);
        for (index, (line, suffix)) in lines
            .iter()
            .zip([
                "title",
                "body",
                "bullet_turns",
                "bullet_mcp",
                "bullet_commands",
            ])
            .enumerate()
        {
            assert_message(line, &format!("switch.countdown.{suffix}"));
            if index == 0 {
                assert_args(line, &[("seconds", &seconds.to_string())]);
                assert_eq!(
                    line.nested,
                    vec![("destination".into(), destination.clone())]
                );
            } else {
                assert_args(line, &[]);
                assert!(line.nested.is_empty());
            }
        }
    }
}

#[test]
fn nested_site_line_and_mode_use_the_banner_locale_recursively() {
    // Plan 7.2 Armed / 7.3: the Chinese countdown must not embed an English destination line.
    let site = catalog::krill_site();
    let line = site.line(&LineId::new("overseas")).unwrap();
    let destination = messages::text("switch.destination.profile")
        .arg_text("site", site.name.clone())
        .arg_text("line", line.name.clone())
        .arg_text("mode", AuthMode::Pat.text());
    let [english_destination, chinese_destination] =
        assert_message(&destination, "switch.destination.profile");
    let lines = messages::countdown_banner(&destination, 5);
    let [english, chinese] = assert_message(&lines[0], "switch.countdown.title");
    assert!(english.contains(&english_destination));
    assert!(chinese.contains(&chinese_destination));
    assert!(english.contains("Overseas"));
    assert!(chinese.contains("海外线路"));
    assert!(!chinese.contains("Overseas"));
    assert!(!chinese.contains("personal access token"));
    assert!(contains_cjk(&chinese));
    assert_ne!(english, chinese);
    for (line, suffix) in
        lines
            .iter()
            .skip(1)
            .zip(["body", "bullet_turns", "bullet_mcp", "bullet_commands"])
    {
        assert_message(line, &format!("switch.countdown.{suffix}"));
    }
}

#[test]
fn full_restart_confirmation_has_title_localized_reason_and_body_for_every_reason() {
    // Plan 7.1 rules 2/3/5/6/7 -> plan 7.2 ConfirmFull: nested reasons use core translations.
    for reason in full_restart_reasons() {
        let lines = messages::confirm_full(&RestartKind::FullAppRestart { reason });
        assert_eq!(lines.len(), 3);
        for (line, suffix) in lines.iter().zip(["title", "reason", "body"]) {
            assert_message(line, &format!("switch.confirm.{suffix}"));
            assert_args(line, &[]);
        }
        assert!(lines[0].nested.is_empty());
        assert_eq!(lines[1].nested, vec![("reason".into(), reason.text())]);
        assert!(lines[2].nested.is_empty());
        let [english, chinese] = assert_message(&lines[1], "switch.confirm.reason");
        assert_ne!(english, chinese);
        assert!(contains_cjk(&chinese));
        assert!(chinese.contains(&LANGUAGES[1].renderer.text(&reason.text())));
    }
}

#[test]
fn confirmation_for_other_restart_kinds_omits_the_reason_line() {
    // Plan 7.2 confirmation wording has only title/body when no full-app reason exists.
    for kind in [
        RestartKind::Blocked { reasons: vec![] },
        RestartKind::None,
        RestartKind::AppServerHot {
            host_id: "local".into(),
            kill_remote: false,
        },
        RestartKind::AppServerHot {
            host_id: "devbox".into(),
            kill_remote: true,
        },
        RestartKind::NextLaunch,
        RestartKind::DaemonRestart,
    ] {
        let lines = messages::confirm_full(&kind);
        assert_eq!(lines.len(), 2);
        for (line, suffix) in lines.iter().zip(["title", "body"]) {
            assert_message(line, &format!("switch.confirm.{suffix}"));
            assert_args(line, &[]);
            assert!(line.nested.is_empty());
        }
    }
}

#[test]
fn target_label_removes_only_one_leading_lowercase_ssh_prefix() {
    // Plan 7.1 remote consumers / 7.3: target labels strip transport syntax, not arbitrary ids.
    for (input, expected) in [
        ("ssh:devbox", "devbox"),
        ("devbox", "devbox"),
        ("local", "local"),
        ("ssh:ssh:devbox", "ssh:devbox"),
        ("SSH:devbox", "SSH:devbox"),
        ("custom:ssh:devbox", "custom:ssh:devbox"),
        ("host:2222", "host:2222"),
        ("ssh:", ""),
        ("", ""),
        ("ssh:开发机", "开发机"),
    ] {
        assert_eq!(messages::target_label(input), expected);
    }
}

#[test]
fn elapsed_seconds_truncate_to_one_decimal_without_rounding_or_overflow() {
    // Plan 7.2 Done / 7.3 toasts: milliseconds display as seconds rounded down to one decimal.
    for (millis, expected) in [
        (0, "0.0"),
        (1, "0.0"),
        (99, "0.0"),
        (100, "0.1"),
        (999, "0.9"),
        (1000, "1.0"),
        (1234, "1.2"),
        (1299, "1.2"),
        (1999, "1.9"),
        (u64::MAX, "18446744073709551.6"),
    ] {
        assert_eq!(messages::seconds_1dp(millis), expected);
    }
}

#[test]
fn the_shared_catalog_layers_the_config_sessions_and_settings_catalog_texts() {
    // The planner returns LocalizedText from superba-config (config.note.*,
    // env.scope.*, config.settings.*), superba-sessions (sessions.*) and
    // superba-catalog (catalog.*); the GUI and spb build on this catalog, so
    // without these layers the Chinese UI showed their English fallbacks (e2e:
    // "No settings catalog is available; ..." in the zh-CN switch overlay).
    for locale in [Locale::En, Locale::ZhCn] {
        let rendered = messages::catalog(locale);
        let switch_keys: BTreeSet<String> = flatten_toml(messages::catalog_source(locale))
            .unwrap()
            .into_keys()
            .collect();
        let config = superba_config::catalog_sources()
            .into_iter()
            .find(|(l, _)| *l == locale)
            .unwrap()
            .1;
        for (crate_name, source) in [
            ("superba-config", config),
            ("superba-sessions", superba_sessions::catalog_source(locale)),
            ("superba-catalog", superba_catalog::i18n_source(locale)),
        ] {
            let entries = flatten_toml(source).unwrap();
            assert!(!entries.is_empty(), "{crate_name} catalog is empty");
            for (key, template) in entries {
                if switch_keys.contains(&key) {
                    continue;
                }
                assert_eq!(
                    rendered.get(&key),
                    Some(template.as_str()),
                    "{crate_name} key {key} missing from the {locale:?} catalog"
                );
            }
        }
    }
}
