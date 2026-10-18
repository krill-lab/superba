//! GH#2 / GH#4 contracts: editor detection, the stop questions and their
//! answers, the runtime view's helpers and texts.

use std::path::Path;

use superba_core::*;

#[test]
fn editors_are_recognised_from_their_extension_directories() {
    let cases = [
        (
            "C:\\Users\\a\\.vscode\\extensions\\openai.chatgpt-26.5-win32-x64\\bin\\windows-x86_64\\codex.exe",
            Some(IdeKind::VsCode),
        ),
        (
            "/home/a/.vscode/extensions/openai.chatgpt-26.5-linux-x64/bin/linux-x86_64/codex",
            Some(IdeKind::VsCode),
        ),
        (
            "/home/a/.vscode-server/extensions/openai.chatgpt-26.5/bin/codex",
            Some(IdeKind::VsCode),
        ),
        (
            "/Users/a/.vscode-insiders/extensions/openai.chatgpt/bin/codex",
            Some(IdeKind::VsCodeInsiders),
        ),
        (
            "C:/Users/a/.cursor/extensions/openai.chatgpt/bin/codex.exe",
            Some(IdeKind::Cursor),
        ),
        (
            "/home/a/.windsurf/extensions/openai.chatgpt/bin/codex",
            Some(IdeKind::Windsurf),
        ),
        (
            "/home/a/.vscode-oss/extensions/openai.chatgpt/bin/codex",
            Some(IdeKind::VsCodium),
        ),
        ("/usr/local/bin/codex", None),
        ("/home/a/extensions/codex", None),
    ];
    for (path, want) in cases {
        assert_eq!(
            IdeKind::from_extension_path(Path::new(path)),
            want,
            "{path}"
        );
    }
}

#[test]
fn editors_are_recognised_from_their_host_processes() {
    for (name, want) in [
        ("Code.exe", Some(IdeKind::VsCode)),
        ("code", Some(IdeKind::VsCode)),
        ("Code Helper (Plugin)", Some(IdeKind::VsCode)),
        ("/usr/share/code/code", Some(IdeKind::VsCode)),
        ("Code - Insiders.exe", Some(IdeKind::VsCodeInsiders)),
        ("Cursor.exe", Some(IdeKind::Cursor)),
        ("Cursor Helper (Plugin)", Some(IdeKind::Cursor)),
        ("windsurf", Some(IdeKind::Windsurf)),
        ("codium", Some(IdeKind::VsCodium)),
        ("ChatGPT.exe", None),
        ("codex", None),
        ("bash", None),
    ] {
        assert_eq!(IdeKind::from_host_name(name), want, "{name}");
    }
}

#[test]
fn the_close_question_offers_stop_for_me_and_never_defaults_to_it() {
    let close = DecisionRequest::CloseConsumers {
        processes: vec![],
        busy: vec![],
    };
    assert_eq!(
        close.allowed(),
        &[
            Decision::StopProcesses,
            Decision::Continue,
            Decision::Cancel
        ]
    );
    assert_eq!(close.safe_default(), Decision::Cancel);
    let force = DecisionRequest::ForceStop {
        processes: vec![],
        busy: vec![],
    };
    assert_eq!(
        force.allowed(),
        &[Decision::Force, Decision::Continue, Decision::Cancel]
    );
    // A caller that cannot ask (no flags) ends nothing by force.
    assert_eq!(force.safe_default(), Decision::Cancel);
    assert!(!force.allowed().contains(&Decision::StopProcesses));
}

#[test]
fn stop_questions_and_answers_round_trip_in_json() {
    let request = DecisionRequest::ForceStop {
        processes: vec![],
        busy: vec![],
    };
    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["kind"], "force_stop");
    let back: DecisionRequest = serde_json::from_value(json).unwrap();
    assert_eq!(back, request);
    // Older questions without `busy` still read.
    let old: DecisionRequest =
        serde_json::from_str(r#"{"kind":"close_consumers","processes":[]}"#).unwrap();
    assert_eq!(
        old,
        DecisionRequest::CloseConsumers {
            processes: vec![],
            busy: vec![]
        }
    );
    assert_eq!(
        serde_json::to_value(Decision::StopProcesses).unwrap(),
        "stop_processes"
    );
}

#[test]
fn stop_outcomes_say_whether_the_process_is_gone() {
    assert!(StopOutcome::Exited.is_gone());
    assert!(StopOutcome::Killed.is_gone());
    assert!(StopOutcome::NotRunning.is_gone());
    assert!(!StopOutcome::StillRunning.is_gone());
    assert!(!StopOutcome::ForceOnly.is_gone());
}

fn process(kind: CodexProcessKind, ide: Option<IdeKind>) -> ProcessInfo {
    ProcessInfo {
        pid: 7,
        ppid: None,
        kind,
        exe: None,
        cmdline: vec![],
        started_at: None,
        authapi_env: EnvObservation::Unknown,
        codex_home: None,
        stale: false,
        ide,
        cwd: None,
    }
}

#[test]
fn roles_and_stop_effects_name_what_the_process_is() {
    let ide = process(CodexProcessKind::IdeAppServer, Some(IdeKind::VsCode));
    assert_eq!(
        process_role(&ide),
        ConsumerRole::IdeExtension {
            ide: Some(IdeKind::VsCode)
        }
    );
    assert_eq!(
        process_role(&ide).text().render_fallback(),
        "VS Code · Codex extension"
    );
    // Stopping an extension's app-server never closes the editor.
    let effect = stop_effect(&ide).render_fallback();
    assert!(effect.contains("VS Code itself stays open"), "{effect}");
    assert!(
        effect.contains("Developer: Restart Extension Host"),
        "{effect}"
    );
    let tui = process(CodexProcessKind::CliTui, None);
    assert_eq!(process_role(&tui), ConsumerRole::Terminal);
    assert!(stop_effect(&tui).render_fallback().contains("codex resume"));
    assert_eq!(
        process_role(&process(CodexProcessKind::AppServer, None)),
        ConsumerRole::DesktopApp
    );
    assert_eq!(
        process_role(&process(CodexProcessKind::CliDaemon, None)),
        ConsumerRole::Daemon
    );
}

#[test]
fn advice_for_editors_names_the_command_and_keeps_the_editor_open() {
    let text = RestartAdvice::ReloadIde {
        ide: Some(IdeKind::Cursor),
    }
    .text()
    .render_fallback();
    assert!(text.contains("Cursor"), "{text}");
    assert!(text.contains("Developer: Restart Extension Host"), "{text}");
    assert!(text.contains("stays open"), "{text}");
    assert!(
        RestartAdvice::Reopen
            .text()
            .render_fallback()
            .contains("codex resume --last")
    );
}

#[test]
fn the_view_counts_consumers_on_an_older_configuration() {
    let row = |effect| ConsumerRuntime {
        role: ConsumerRole::Terminal,
        pid: 1,
        core_pid: None,
        started_at: None,
        codex_home: None,
        cwd: None,
        exe: None,
        cmdline: vec![],
        effect,
        busy: BusyState::default(),
        advice: RestartAdvice::None,
        can_stop: false,
    };
    let view = RuntimeView {
        checked_at: Timestamp::EPOCH,
        written: None,
        consumers: vec![
            row(RuntimeEffect::Current),
            row(RuntimeEffect::Previous {
                running: RunningGeneration {
                    generation: Some(1),
                    profile_id: None,
                    site_kind: None,
                    applied_at: None,
                },
            }),
            row(RuntimeEffect::Unknown {
                reason: UnknownReason::NoStartTime,
            }),
        ],
        busy_checked: false,
    };
    assert_eq!(view.previous_count(), 1);
    assert_eq!(BusyState::default(), BusyState::NotChecked);
}
