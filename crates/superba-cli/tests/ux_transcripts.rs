mod common;

use std::path::Path;
use std::sync::Arc;

use common::*;
use superba_core::*;
use superba_switch::messages;

fn line_change(request: SwitchRequest, home: &Path) -> SwitchPlan {
    let mut plan = plan(request);
    plan.to.label = messages::text("switch.destination.profile")
        .arg_text("site", catalog::krill_site().name)
        .arg_text(
            "line",
            catalog::krill_site()
                .line(&LineId::new("overseas"))
                .unwrap()
                .name
                .clone(),
        )
        .arg_text("mode", AuthMode::Pat.text());
    plan.primary = RestartKind::AppServerHot {
        host_id: "local".into(),
        kill_remote: false,
    };
    plan.env = Some(EnvPlan {
        var: "CODEX_AUTHAPI_BASE_URL".into(),
        op: EnvOp::Set {
            value: "https://127.0.0.1:41003".into(),
            take_over_foreign: false,
        },
        edits: [".bashrc", ".profile", ".config/environment.d/60-superba.conf"]
            .into_iter()
            .map(|path| EnvEdit {
                location: if path.starts_with(".config/") {
                    EnvLocation::EnvironmentD {
                        path: home.join(path),
                    }
                } else {
                    EnvLocation::ShellRc {
                        path: home.join(path),
                        shell: ShellKind::Bash,
                    }
                },
                action: EnvEditAction::WriteBlock,
                preview: format!(
                    "# >>> superba >>>\n{}CODEX_AUTHAPI_BASE_URL=\"https://127.0.0.1:41003\"\n# <<< superba <<<",
                    if path.starts_with(".config/") { "" } else { "export " }
                ),
                before: None,
                after: None,
            })
            .collect(),
        scope_notes: Vec::new(),
    });
    plan
}

fn isolated_runtime(service: Arc<FakeService>, home: &Path) -> superba_cli::Runtime {
    let mut rt = runtime(service);
    rt.env = Arc::new(
        MapEnv::new()
            .with("HOME", home.as_os_str())
            .with("SUPERBA_SECRET_BACKEND", "file")
            .with("SUPERBA_LOCALE", "en"),
    );
    rt
}

fn transcript(command: &str, code: u8, stdout: &str, stderr: &str, home: &Path) {
    let home = home.to_string_lossy();
    println!("$ {command}");
    for (stream, contents) in [("stdout", stdout), ("stderr", stderr)] {
        if !contents.is_empty() {
            println!(
                "[{stream}]\n{}",
                contents.replace(home.as_ref(), "<HOME>").trim_end()
            );
        }
    }
    println!("[exit {code}]\n");
}

// Run with --nocapture to review the same isolated fixtures before and after a
// presentation change. No production app, shell files, network or keyring is used.
#[tokio::test]
async fn isolated_cli_transcripts() {
    let directory = tempfile::tempdir().unwrap();
    let ctx = LocalContext::for_tests(directory.path());
    let home = ctx.home_dir.clone();
    std::fs::create_dir(&home).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_spb"))
        .env_clear()
        .env("HOME", &home)
        .env("SUPERBA_SECRET_BACKEND", "file")
        .env("SUPERBA_LOCALE", "en")
        .output()
        .unwrap();
    transcript(
        "spb",
        output.status.code().unwrap() as u8,
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
        &home,
    );
    assert_eq!(output.status.code(), Some(2));

    let service = FakeService::new();
    let plan_home = home.clone();
    service.script(move |request| Ok(line_change(request, &plan_home)));
    let rt = isolated_runtime(service.clone(), &home);
    let (code, stdout, stderr) =
        invoke_runtime(&rt, &["switch", "--line", "overseas", "--dry-run"]).await;
    transcript(
        "spb switch --line overseas --dry-run",
        code,
        &stdout,
        &stderr,
        &home,
    );
    assert_eq!(code, 0);
    assert!(service.calls().executions.is_empty());
    assert!(stdout.contains("Overseas (overseas)"));
    let locations = [
        ".bashrc",
        ".profile",
        ".config/environment.d/60-superba.conf",
    ]
    .map(|path| Path::new("~").join(path).display().to_string());
    assert!(stdout.contains(&format!(
        "Environment: CODEX_AUTHAPI_BASE_URL → https://127.0.0.1:41003 in {}",
        locations.join(", ")
    )));
    assert!(!stdout.contains("# >>> superba >>>"));
    let (_, verbose, _) = invoke_runtime(&rt, &["switch", "--dry-run", "--verbose"]).await;
    assert_eq!(verbose.matches("# >>> superba >>>").count(), 3);
    for path in locations {
        assert!(verbose.contains(&format!("{path}:\n# >>> superba >>>")));
    }
    let (_, ordinary, _) = invoke_runtime(&rt, &["switch", "--verbose"]).await;
    assert!(!ordinary.contains("# >>> superba >>>"));

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
    *service.outcome.lock().unwrap() = Ok(outcome(OutcomeStatus::Cancelled));
    service
        .decisions
        .lock()
        .unwrap()
        .push(DecisionRequest::WaitForTurns {
            busy: vec![thread()],
            waited_ms: 300,
        });
    service.events.lock().unwrap().extend([
        SwitchEvent::Phase {
            phase: SwitchPhase::WaitingTurn,
            at: Timestamp::EPOCH,
        },
        SwitchEvent::Turns {
            activity: Vec::new(),
            waited_ms: 300,
        },
        SwitchEvent::Phase {
            phase: SwitchPhase::Done,
            at: Timestamp::EPOCH,
        },
    ]);
    let rt = isolated_runtime(service, &home);
    let (code, stdout, stderr) = invoke_runtime(&rt, &["switch", "--countdown", "5"]).await;
    transcript(
        "spb switch --countdown 5 # one busy turn",
        code,
        &stdout,
        &stderr,
        &home,
    );
    assert_eq!(code, 5);

    let service = FakeService::new();
    {
        let mut overview = service.overview.lock().unwrap();
        overview.codex_home = home.join(".codex");
        overview.active = None;
        overview.app.installs.clear();
        overview.app.session = None;
        overview.app.hot_switch = HotSwitchStatus::NotInstalled;
        overview.app.authapi_line = None;
        overview.cli.binaries.clear();
        overview.cli.processes.clear();
        overview.remotes.clear();
        overview.notices.clear();
    }
    let rt = isolated_runtime(service, &home);
    let (code, stdout, stderr) = invoke_runtime(&rt, &["status"]).await;
    transcript("spb status # empty machine", code, &stdout, &stderr, &home);
    assert_eq!(code, 0);
    for section in ["Desktop installs", "CLI binaries", "Remote hosts"] {
        assert!(stdout.contains(&format!("{section}:\nNone")), "{stdout}");
    }
    assert!(!stdout.contains(" | "));
}
