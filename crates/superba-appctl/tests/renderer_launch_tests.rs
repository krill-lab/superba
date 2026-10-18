//! Renderer probe interpretation, launch argument building, launch records,
//! daemon help parsing, launcher entries and CDP helpers.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use serde_json::json;
use superba_appctl::cdp::{
    CdpTarget, evaluate_response, parse_devtools_active_port, pick_main_target, validate_ws_url,
};
use superba_appctl::launch::{
    CdpPlan, build_spec, cdp_args, cdp_plan, home_override_env, macos_launched_pid,
    macos_open_argv, sanitize_args, split_app_args,
};
use superba_appctl::renderer::{
    DomHooks, busy_threads, fill_titles, navigate_js, parse_probe, probe_js, remote_host_ids,
    renderer_state, valid_thread_id,
};
use superba_appctl::{cdp_port_from_argv, daemon, launcher, store};
use superba_core::{CdpMode, LaunchOptions, ThreadRef, Timestamp};

#[test]
fn renderer_login_and_home() {
    let p = parse_probe(&json!({"url": "app://-/index.html", "login": true, "composer": false}))
        .unwrap();
    let s = renderer_state(&p);
    assert!(s.on_login_page);
    assert!(!s.on_home);
    assert!(s.current_thread.is_none());

    let p =
        parse_probe(&json!({"url": "app://-/index.html", "composer": true, "rows": []})).unwrap();
    let s = renderer_state(&p);
    assert!(s.on_home);
    assert!(busy_threads(&p).is_empty());
    assert!(remote_host_ids(&p).is_empty());
}

#[test]
fn renderer_busy_rows() {
    let p = parse_probe(&json!({
        "url": "app://-/index.html", "composer": true, "conversation": "c1", "stop": true,
        "rows": [
            {"id": "local:c1", "host": "local", "title": "A", "selected": true, "busy": false},
            {"id": "local:c2", "host": "local", "title": "B", "selected": false, "busy": true},
            {"id": "h:c3", "host": "gpu-01", "title": "C", "selected": false, "busy": true},
            {"id": "h:c4", "host": "gpu-01", "title": "D", "selected": false, "busy": false}
        ]
    }))
    .unwrap();
    let b = busy_threads(&p);
    let got: Vec<(String, Option<String>, Option<String>)> = b
        .iter()
        .map(|t| {
            (
                t.target_id.to_string(),
                t.thread_id.clone(),
                t.title.clone(),
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            ("local".into(), Some("c2".into()), Some("B".into())),
            ("ssh:gpu-01".into(), Some("c3".into()), Some("C".into())),
            ("local".into(), Some("c1".into()), Some("A".into())),
        ]
    );
    assert_eq!(remote_host_ids(&p), vec!["gpu-01".to_owned()]);
}

#[test]
fn rollout_busy_threads_get_sidebar_titles() {
    // Found by the e2e harness: a busy turn found in the rollout was reported as
    // "untitled conversation" although the sidebar showed its title.
    use superba_core::{ActivitySignal, TargetId, ThreadActivity};
    let p = parse_probe(&json!({
        "url": "app://-/index.html", "composer": true, "conversation": "t-open", "stop": true,
        "rows": [
            {"id": "local:client-new-thread:9f1c", "host": "local", "title": "Open one", "selected": true},
            {"id": "local:t-other", "host": "local", "title": "Other one", "selected": false},
            {"id": "gpu:t-remote", "host": "gpu-01", "title": "Remote one", "selected": false}
        ]
    }))
    .unwrap();
    let rollout = |id: &str, title: Option<&str>| ThreadActivity {
        target_id: TargetId::local(),
        thread_id: Some(id.into()),
        title: title.map(str::to_owned),
        rollout_path: Some(format!("/h/.codex/sessions/rollout-{id}.jsonl")),
        last_event_at: None,
        signal: ActivitySignal::RolloutTail,
        originator: None,
        cwd: None,
    };
    let mut busy = vec![
        rollout("t-open", None),
        rollout("t-other", None),
        rollout("t-remote", None),
        rollout("t-unknown", None),
        rollout("t-other", Some("Kept")),
    ];
    fill_titles(&mut busy, &p);
    let titles: Vec<Option<&str>> = busy.iter().map(|t| t.title.as_deref()).collect();
    assert_eq!(
        titles,
        vec![
            Some("Open one"),
            Some("Other one"),
            None,
            None,
            Some("Kept")
        ]
    );
}

#[test]
fn renderer_scripts() {
    let hooks = DomHooks::default();
    let js = probe_js(&hooks);
    assert!(js.contains("data-above-composer-conversation-id"));
    assert!(js.starts_with("(() =>"));
    let t = ThreadRef {
        thread_id: "0199aaaa-0000-7000-8000-000000000001".into(),
        title: Some("quote \" and </script>".into()),
    };
    let nav = navigate_js(&hooks, &t);
    assert!(nav.contains("navigate-to-route"));
    assert!(nav.contains("\"0199aaaa-0000-7000-8000-000000000001\""));
    assert!(nav.contains("quote \\\" and"));
    assert!(valid_thread_id("01a143a6-f4e0-7ef2-93b7-ce25598382d2"));
    assert!(!valid_thread_id(""));
    assert!(!valid_thread_id("a/b"));
    assert!(!valid_thread_id("x' + 1"));
}

#[test]
fn launch_arguments() {
    assert_eq!(
        cdp_args(9222),
        vec![
            "--remote-debugging-port=9222".to_owned(),
            "--remote-allow-origins=http://127.0.0.1:9222".to_owned()
        ]
    );
    assert_eq!(cdp_args(0), vec!["--remote-debugging-port=0".to_owned()]);
    let user = vec![
        "--no-sandbox".to_owned(),
        "--remote-debugging-address=0.0.0.0".to_owned(),
        "--remote-debugging-port".to_owned(),
        "1".to_owned(),
        "--remote-allow-origins=*".to_owned(),
        "--disable-gpu".to_owned(),
    ];
    assert_eq!(
        sanitize_args(&user),
        vec!["--no-sandbox".to_owned(), "--disable-gpu".to_owned()]
    );
    assert_eq!(
        split_app_args("  --no-sandbox\t--disable-gpu \n"),
        vec!["--no-sandbox".to_owned(), "--disable-gpu".to_owned()]
    );
    assert_eq!(
        cdp_port_from_argv(&["x".into(), "--remote-debugging-port=4321".into()]),
        Some(4321)
    );
    assert_eq!(
        cdp_port_from_argv(&["x".into(), "--remote-debugging-port".into(), "0".into()]),
        Some(0)
    );
    assert_eq!(cdp_port_from_argv(&["x".into()]), None);
}

#[test]
fn launch_spec_env() {
    let mut env = BTreeMap::new();
    env.insert(
        "CODEX_AUTHAPI_BASE_URL".to_owned(),
        "https://api.example.test".to_owned(),
    );
    env.insert("CODEX_HOME".to_owned(), "/iso/.codex".to_owned());
    let opts = LaunchOptions {
        env,
        env_remove: vec!["OPENAI_API_KEY".into()],
        extra_args: vec!["--disable-gpu".into()],
        home_override: Some(PathBuf::from("/iso")),
        ..Default::default()
    };
    let spec = build_spec(
        Path::new("/opt/chatgpt/ChatGPT"),
        &opts,
        &["--no-sandbox".to_owned()],
        CdpPlan::Fixed(5555),
    );
    assert_eq!(
        spec.args,
        vec![
            "--remote-debugging-port=5555".to_owned(),
            "--remote-allow-origins=http://127.0.0.1:5555".to_owned(),
            "--no-sandbox".to_owned(),
            "--disable-gpu".to_owned()
        ]
    );
    assert_eq!(spec.env_set.get("HOME").map(String::as_str), Some("/iso"));
    assert_eq!(
        spec.env_set.get("ZDOTDIR").map(String::as_str),
        Some("/iso")
    );
    assert_eq!(
        spec.env_set.get("CODEX_HOME").map(String::as_str),
        Some("/iso/.codex"),
        "explicit env wins over the home override removal"
    );
    assert!(!spec.env_remove.contains(&"CODEX_HOME".to_owned()));
    assert!(spec.env_remove.contains(&"XDG_CONFIG_HOME".to_owned()));
    assert!(spec.env_remove.contains(&"OPENAI_API_KEY".to_owned()));

    let (set, remove) = home_override_env(Path::new("/h"));
    assert_eq!(set.get("HOME").map(String::as_str), Some("/h"));
    assert!(remove.contains(&"XDG_DATA_HOME".to_owned()));
}

#[test]
fn cdp_plans() {
    assert_eq!(
        cdp_plan(CdpMode::Disabled, None).unwrap(),
        CdpPlan::Disabled
    );
    assert_eq!(
        cdp_plan(CdpMode::Port(7000), None).unwrap(),
        CdpPlan::Fixed(7000)
    );
    assert!(cdp_plan(CdpMode::Port(0), None).is_err());
    let auto = cdp_plan(CdpMode::Auto, Some(Path::new("/u/.config/Codex"))).unwrap();
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        assert_eq!(
            auto,
            CdpPlan::ActivePortFile("/u/.config/Codex/DevToolsActivePort".into())
        );
    } else {
        assert!(matches!(auto, CdpPlan::Fixed(p) if p > 0));
    }
    assert!(matches!(
        cdp_plan(CdpMode::Auto, None).unwrap(),
        CdpPlan::Fixed(_)
    ));
}

#[test]
fn macos_launchservices_argv_preserves_environment_and_app_arguments() {
    let env = BTreeMap::from([
        ("ZDOTDIR".into(), "/isolated home/中文".into()),
        (
            "CODEX_AUTHAPI_BASE_URL".into(),
            "https://api.example.test".into(),
        ),
        ("APP_VALUE".into(), "spaces and=equals='quotes'".into()),
    ]);
    let bundle = Path::new("/Applications/My Codex.app");
    let args = vec![
        "--remote-debugging-port=0".into(),
        "--env".into(),
        "value with=spaces".into(),
        String::new(),
    ];
    let expected: Vec<OsString> = [
        "-n",
        "-a",
        "/Applications/My Codex.app",
        "--env",
        "APP_VALUE=spaces and=equals='quotes'",
        "--env",
        "CODEX_AUTHAPI_BASE_URL=https://api.example.test",
        "--env",
        "ZDOTDIR=/isolated home/中文",
        "--args",
        "--remote-debugging-port=0",
        "--env",
        "value with=spaces",
        "",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    assert_eq!(macos_open_argv(bundle, &env, &[], &args), expected);
    assert_eq!(
        macos_open_argv(bundle, &BTreeMap::new(), &[], &[]),
        ["-n", "-a", "/Applications/My Codex.app", "--args"].map(OsString::from)
    );
}

#[test]
fn macos_launch_pid_ignores_old_apps_helpers_and_other_users() {
    use superba_appctl::procs::ProcEntry;

    let exe = Path::new("/Applications/Codex.app/Contents/MacOS/Codex");
    let entry = |pid, millis| ProcEntry {
        pid,
        exe: Some(exe.into()),
        started_at: Some(Timestamp::from_millis(millis)),
        ..Default::default()
    };
    let mut entries = vec![
        entry(10, 10_000), // Existing app in the same second as the launch.
        entry(11, 9_000),  // Old process missing from the initial enumeration.
        ProcEntry {
            argv: vec!["Codex --type=renderer".into()],
            ..entry(12, 10_000)
        },
        ProcEntry {
            foreign_user: true,
            ..entry(13, 10_000)
        },
        ProcEntry {
            zombie: true,
            ..entry(14, 10_000)
        },
        ProcEntry {
            exe: Some("/Applications/Other.app/Contents/MacOS/Codex".into()),
            ..entry(15, 10_000)
        },
        ProcEntry {
            started_at: None,
            ..entry(16, 10_000)
        },
    ];
    let launch = Timestamp::from_millis(10_750);
    assert_eq!(macos_launched_pid(&entries, exe, &[10], launch), None);
    entries.push(entry(20, 10_000)); // Start time rounded down by the OS.
    entries.push(entry(21, 11_000));
    assert_eq!(macos_launched_pid(&entries, exe, &[10], launch), Some(20));
    assert_eq!(
        macos_launched_pid(&entries, exe, &[10, 20], launch),
        Some(21)
    );
}

#[test]
fn cdp_helpers() {
    assert!(validate_ws_url("ws://127.0.0.1:9222/devtools/page/X", 9222));
    assert!(validate_ws_url("ws://localhost:9222/devtools/page/X", 9222));
    assert!(validate_ws_url("ws://[::1]:9222/x", 9222));
    assert!(!validate_ws_url("ws://127.0.0.1:9223/x", 9222));
    assert!(!validate_ws_url("ws://192.0.2.1:9222/x", 9222));
    assert!(!validate_ws_url("wss://127.0.0.1:9222/x", 9222));
    assert!(!validate_ws_url("ws://127.0.0.1.example.com:9222/x", 9222));

    let t = |url: &str, ws: &str| CdpTarget {
        id: url.into(),
        kind: "page".into(),
        url: url.into(),
        title: String::new(),
        ws_url: Some(ws.into()),
    };
    let targets = vec![
        t(
            "app://-/index.html?initialRoute=/avatar-overlay",
            "ws://127.0.0.1:1/a",
        ),
        t(
            "app://-/index.html#/chatgpt/quick-chat-prewarm",
            "ws://127.0.0.1:1/b",
        ),
        t("app://-/index.html?x=1", "ws://127.0.0.1:1/c"),
        t("app://-/index.html", "ws://10.0.0.1:1/d"),
    ];
    assert_eq!(
        pick_main_target(&targets, 1).map(|t| t.ws_url.clone().unwrap()),
        Some("ws://127.0.0.1:1/c".into())
    );
    let mut with_exact = targets.clone();
    with_exact.push(t("app://-/index.html", "ws://127.0.0.1:1/e"));
    assert_eq!(
        pick_main_target(&with_exact, 1).map(|t| t.ws_url.clone().unwrap()),
        Some("ws://127.0.0.1:1/e".into())
    );

    assert_eq!(
        evaluate_response(
            &json!({"id": 1, "result": {"result": {"type": "boolean", "value": true}}})
        )
        .unwrap(),
        json!(true)
    );
    assert_eq!(
        evaluate_response(&json!({"id": 1, "result": {"result": {"type": "undefined"}}})).unwrap(),
        json!(null)
    );
    assert!(evaluate_response(&json!({"id": 1, "error": {"message": "nope"}})).is_err());
    assert!(
        evaluate_response(&json!({"id": 1, "result": {"exceptionDetails": {"text": "x"}}}))
            .is_err()
    );

    assert_eq!(
        parse_devtools_active_port("44749\n/devtools/browser/ab\n"),
        Some(44749)
    );
    assert_eq!(parse_devtools_active_port("0\n"), None);
    assert_eq!(parse_devtools_active_port(""), None);
}

#[test]
fn launch_records() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("data/app-sessions.json");
    let rec = |pid: u32, at: i64| store::LaunchRecord {
        install_id: Some("path:/opt/chatgpt".into()),
        executable: Some("/opt/chatgpt/ChatGPT".into()),
        main_pid: pid,
        process_started_at: Some(Timestamp::from_millis(at)),
        launched_at: Timestamp::from_millis(at),
        cdp_port: Some(9000),
        home_override: None,
        injected_env: vec!["CODEX_AUTHAPI_BASE_URL".into()],
    };
    assert!(store::load(&path).sessions.is_empty());
    store::record_launch(&path, rec(10, 1_000_000), &|_| true).unwrap();
    store::record_launch(&path, rec(11, 2_000_000), &|r| r.main_pid != 10).unwrap();
    let s = store::load(&path);
    assert_eq!(s.sessions.len(), 1, "dead record pruned");
    assert!(
        s.find(11, Some(Timestamp::from_millis(2_001_000)))
            .is_some()
    );
    assert!(
        s.find(11, Some(Timestamp::from_millis(9_000_000)))
            .is_none(),
        "pid reuse"
    );
    assert!(s.find(11, None).is_some());
    store::forget(&path, 11, &|_| true).unwrap();
    assert!(store::load(&path).sessions.is_empty());
    std::fs::write(&path, "not json").unwrap();
    assert!(store::load(&path).sessions.is_empty());
}

const DAEMON_HELP: &str = "Manage the local app-server daemon

Usage: codex app-server daemon [OPTIONS] <COMMAND>

Commands:
  bootstrap               Install durable local app-server management for SSH-driven use
  start                   Start the local app server daemon if it is not already running
  restart                 Restart the local app server daemon
  update                  Update the daemon package (may interrupt running work)
  enable-remote-control   Enable remote control for future starts and a currently running managed
                          daemon
  stop                    Stop the local app server daemon
  version                 Print local CLI and running app-server versions as JSON
  help                    Print this message or the help of the given subcommand(s)

Options:
  -c, --config <key=value>
          Override a configuration value
";

#[test]
fn daemon_help() {
    let cmds = daemon::parse_daemon_commands(DAEMON_HELP);
    assert_eq!(
        cmds,
        vec![
            "bootstrap",
            "start",
            "restart",
            "update",
            "enable-remote-control",
            "stop",
            "version",
            "help"
        ]
    );
    assert!(daemon::parse_daemon_commands("error: unrecognized subcommand 'daemon'").is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_command_runs_with_codex_home() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("calls.log");
    let script = tmp.path().join("codex");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$* CODEX_HOME=$CODEX_HOME\" >> '{}'\nif [ \"$3\" = \"--help\" ]; then cat <<'EOF'\n{DAEMON_HELP}EOF\nfi\nexit 0\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let home = superba_core::CodexHome::new(tmp.path().join("ch"));
    daemon::daemon_command(&script, "restart", &home)
        .await
        .unwrap();
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("app-server daemon --help"));
    assert!(calls.contains(&format!(
        "app-server daemon restart CODEX_HOME={}",
        home.root().display()
    )));
    let err = daemon::daemon_command(&script, "frobnicate", &home)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "unsupported");
}

#[test]
fn launcher_entries() {
    let base = "[Desktop Entry]\nName=ChatGPT\nExec=/usr/lib/chatgpt/codex-launcher %U\nTryExec=/usr/lib/chatgpt/ChatGPT\nIcon=chatgpt\nType=Application\n\n[Desktop Action new]\nName=New\nExec=/usr/lib/chatgpt/codex-launcher --new\n";
    let exec = vec!["/opt/my apps/superba".to_owned(), "open-app".to_owned()];
    let out = launcher::render_entry(Some(base), &exec);
    assert!(out.contains(launcher::MANAGED_MARKER));
    assert!(out.contains("Exec=\"/opt/my apps/superba\" open-app\n"));
    assert!(!out.contains("TryExec"));
    assert!(out.contains("Icon=chatgpt"));
    // Other groups keep their Exec.
    assert!(out.contains("Exec=/usr/lib/chatgpt/codex-launcher --new"));
    let minimal = launcher::render_entry(None, &exec);
    assert!(minimal.starts_with("[Desktop Entry]\n"));
    assert_eq!(launcher::quote_exec_arg("50%"), "50%%");
    assert_eq!(launcher::quote_exec_arg("a$b"), "\"a\\$b\"");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn launcher_entry_roundtrip() {
    use superba_core::AppController;
    let tmp = tempfile::tempdir().unwrap();
    let ctx = superba_core::LocalContext::for_tests(tmp.path());
    let apps = ctx.home_dir.join(".local/share/applications");
    let c = superba_appctl::DesktopAppController::new(ctx);
    assert!(!c.launcher_entry().await.unwrap());
    c.set_launcher_entry(true).await.unwrap();
    assert!(c.launcher_entry().await.unwrap());
    let files: Vec<_> = std::fs::read_dir(&apps).unwrap().flatten().collect();
    assert_eq!(files.len(), 1);
    // An unmanaged file is never touched.
    std::fs::write(apps.join("mine.desktop"), "[Desktop Entry]\nExec=x\n").unwrap();
    c.set_launcher_entry(false).await.unwrap();
    assert!(!c.launcher_entry().await.unwrap());
    assert!(apps.join("mine.desktop").exists());
}
