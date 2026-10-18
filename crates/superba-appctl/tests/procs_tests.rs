//! Process classification on fixtures modelled on real command lines.

use std::collections::BTreeMap;
use std::path::PathBuf;

use pretty_assertions::assert_eq;
use superba_appctl::procs::{
    CodexCommand, ProcEntry, Snapshot, codex_args, codex_command, codex_words,
    effective_codex_home, is_electron_child, parse_environ, ssh_destination,
};
use superba_core::{CodexProcessKind, EnvObservation, Timestamp};

fn argv(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_owned).collect()
}

fn env(pairs: &[(&str, &str)]) -> Option<BTreeMap<String, String>> {
    Some(
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
    )
}

fn entry(pid: u32, ppid: u32, cmd: &str) -> ProcEntry {
    let argv = argv(cmd);
    ProcEntry {
        pid,
        ppid: Some(ppid),
        exe: argv.first().map(PathBuf::from),
        argv,
        started_at: Some(Timestamp::from_millis(i64::from(pid) * 1000)),
        env: None,
        zombie: false,
        foreign_user: false,
        cwd: None,
    }
}

const APP: &str = "/opt/chatgpt/ChatGPT --no-sandbox --remote-debugging-port=25914 --remote-allow-origins=http://127.0.0.1:25914";
const APP_SERVER: &str = "/opt/chatgpt/resources/codex -c features.code_mode_host=true app-server --analytics-default-enabled -c plugins.x.enabled=true";
const RENDERER: &str = "/opt/chatgpt/ChatGPT --type=renderer --remote-debugging-port=25914";

#[test]
fn codex_argument_parsing() {
    assert_eq!(
        codex_args(&argv("codex exec hi"), None),
        Some(argv("exec hi"))
    );
    assert_eq!(
        codex_args(
            &argv("/usr/bin/node /home/u/.nvm/versions/node/v24/bin/codex app-server proxy"),
            None
        ),
        Some(argv("app-server proxy"))
    );
    assert_eq!(
        codex_args(
            &argv("C:\\Users\\u\\AppData\\Roaming\\npm\\codex.cmd resume"),
            None
        ),
        Some(argv("resume"))
    );
    assert_eq!(codex_args(&argv("node server.js"), None), None);
    assert_eq!(codex_args(&argv("/usr/bin/codexctl"), None), None);

    let (words, listen) = codex_words(&argv(
        "-c a=b --enable x app-server --listen unix:// --analytics-default-enabled",
    ));
    assert_eq!(words, argv("app-server"));
    assert_eq!(listen.as_deref(), Some("unix://"));
    let (_, listen) = codex_words(&argv("app-server --listen=ws://127.0.0.1:1"));
    assert_eq!(listen.as_deref(), Some("ws://127.0.0.1:1"));

    assert_eq!(codex_command(&argv("")), CodexCommand::Tui);
    assert_eq!(
        codex_command(&argv("-m gpt-5 resume --last")),
        CodexCommand::Tui
    );
    assert_eq!(
        codex_command(&argv("exec -m x -s workspace-write hello")),
        CodexCommand::Exec
    );
    assert_eq!(codex_command(&argv("e hello")), CodexCommand::Exec);
    assert_eq!(
        codex_command(&argv("-c x=1 app-server")),
        CodexCommand::AppServer { listen: None }
    );
    assert_eq!(
        codex_command(&argv("app-server proxy")),
        CodexCommand::Other
    );
    assert_eq!(
        codex_command(&argv("app-server daemon restart")),
        CodexCommand::Other
    );
    assert_eq!(codex_command(&argv("login status")), CodexCommand::Other);
}

#[test]
fn ssh_parsing() {
    let a =
        argv("ssh -T -o ConnectTimeout=8 -p 2222 -i /k devbox sh -c 'exec codex app-server proxy'");
    let (dest, cmd) = ssh_destination(&a).unwrap();
    assert_eq!(dest, "devbox");
    assert!(cmd.contains("app-server"));
    let (dest, _) = ssh_destination(&argv("/usr/bin/ssh -Tp22 me@host.example true")).unwrap();
    assert_eq!(dest, "me@host.example");
    assert!(ssh_destination(&argv("sshd: user")).is_none());
}

#[test]
fn environ_parsing() {
    assert_eq!(parse_environ(["", " ", "   "]), None);
    let m = parse_environ(["HOME=/h", "A=b=c", "junk"]).unwrap();
    assert_eq!(m.get("A").map(String::as_str), Some("b=c"));
    assert_eq!(
        effective_codex_home(&env(&[("HOME", "/h")]).unwrap()),
        Some(PathBuf::from("/h/.codex"))
    );
    assert_eq!(
        effective_codex_home(&env(&[("HOME", "/h"), ("CODEX_HOME", "/c")]).unwrap()),
        Some(PathBuf::from("/c"))
    );
    assert_eq!(
        effective_codex_home(&env(&[("HOME", "/h"), ("CODEX_HOME", "")]).unwrap()),
        Some(PathBuf::from("/h/.codex"))
    );
}

#[test]
fn snapshot_classification() {
    let mut app = entry(100, 1, APP);
    app.env = None; // clobbered on Linux
    let mut srv = entry(110, 100, APP_SERVER);
    srv.env = env(&[
        ("HOME", "/tmp/iso"),
        ("CODEX_AUTHAPI_BASE_URL", "https://api.example.test"),
    ]);
    let renderer = entry(101, 100, RENDERER);
    let ssh = entry(
        120,
        100,
        "/usr/bin/ssh -T devbox sh -c 'codex app-server proxy'",
    );
    let tui = entry(200, 50, "/usr/local/bin/codex --no-alt-screen");
    let exec_wrapper = entry(
        300,
        50,
        "node /home/u/.nvm/versions/node/v24/bin/codex exec -m gpt hi",
    );
    let exec_native = entry(
        301,
        300,
        "/home/u/.nvm/versions/node/v24/lib/node_modules/@openai/codex/vendor/codex exec -m gpt hi",
    );
    let daemon = entry(
        400,
        1,
        "/usr/local/bin/codex -c x=1 app-server --listen unix://",
    );
    let ide = entry(
        500,
        450,
        "/home/u/.vscode/extensions/openai.chatgpt/bin/codex app-server --analytics-default-enabled",
    );
    let proxy = entry(600, 599, "codex app-server proxy");
    let unrelated = entry(700, 1, "/usr/bin/vim notes.txt");

    let snap = Snapshot::classify_entries(
        vec![
            app,
            srv,
            renderer,
            ssh,
            tui,
            exec_wrapper,
            exec_native,
            daemon,
            ide,
            proxy,
            unrelated,
        ],
        &|e: &ProcEntry| e.argv.first().is_some_and(|a| a.ends_with("/ChatGPT")),
    );
    let kinds: BTreeMap<u32, CodexProcessKind> = snap.classified.iter().cloned().collect();
    assert_eq!(kinds.get(&100), Some(&CodexProcessKind::AppMain));
    assert_eq!(kinds.get(&110), Some(&CodexProcessKind::AppServer));
    assert_eq!(kinds.get(&101), None, "electron child ignored");
    assert_eq!(
        kinds.get(&120),
        Some(&CodexProcessKind::RemoteLink {
            host: "devbox".into()
        })
    );
    assert_eq!(kinds.get(&200), Some(&CodexProcessKind::CliTui));
    assert_eq!(kinds.get(&300), Some(&CodexProcessKind::CliExec));
    assert_eq!(
        kinds.get(&301),
        None,
        "native child of the npm wrapper is folded"
    );
    assert_eq!(kinds.get(&400), Some(&CodexProcessKind::CliDaemon));
    assert_eq!(kinds.get(&500), Some(&CodexProcessKind::IdeAppServer));
    assert_eq!(kinds.get(&600), Some(&CodexProcessKind::Other));
    assert_eq!(kinds.get(&700), None);

    assert_eq!(snap.app_mains(), vec![100]);
    assert_eq!(snap.app_servers_of(100).len(), 1);
    assert_eq!(
        snap.codex_home_of(100),
        Some(PathBuf::from("/tmp/iso/.codex"))
    );
    assert_eq!(
        snap.app_authapi_env(100),
        EnvObservation::Present("https://api.example.test".into())
    );
    let info = snap.process_info(100).unwrap();
    assert_eq!(info.kind, CodexProcessKind::AppMain);
    assert_eq!(
        info.authapi_env,
        EnvObservation::Present("https://api.example.test".into())
    );
    assert!(!info.stale);
    let info = snap.process_info(200).unwrap();
    assert_eq!(info.authapi_env, EnvObservation::Unknown);
    assert_eq!(info.codex_home, None);
}

#[test]
fn authapi_absent_vs_unknown() {
    let app = entry(100, 1, APP);
    let mut srv = entry(110, 100, APP_SERVER);
    srv.env = env(&[("HOME", "/h")]);
    let snap = Snapshot::classify_entries(vec![app, srv], &|e: &ProcEntry| e.pid == 100);
    assert_eq!(snap.app_authapi_env(100), EnvObservation::Absent);
    let snap = Snapshot::classify_entries(vec![entry(100, 1, APP)], &|e: &ProcEntry| e.pid == 100);
    assert_eq!(snap.app_authapi_env(100), EnvObservation::Unknown);
}

#[test]
fn redacts_command_lines() {
    let tui = entry(
        200,
        50,
        "codex --remote-auth-token-env X -c api_key=sk-secret123 resume",
    );
    let snap = Snapshot::classify_entries(vec![tui], &|_| false);
    let info = snap.process_info(200).unwrap();
    assert!(
        info.cmdline.iter().all(|a| !a.contains("sk-secret123")),
        "{:?}",
        info.cmdline
    );
}

#[test]
fn redacts_embedded_openai_and_krill_tokens_with_the_same_boundaries() {
    use superba_appctl::procs::redact_cmdline;

    for (input, expected) in [
        ("--note=sk-example,nb_example", "--note=***,***"),
        (
            "text nb_example-123_xyz /sk-example_2!nb_again",
            "text *** /***!***",
        ),
        ("xsk-example xnb_example", "xsk-example xnb_example"),
        ("中文nb_example (sk-example)", "中文*** (***)"),
        (
            "text nb_example=value sk-example=value",
            "text ***=value ***=value",
        ),
        ("unrelated text", "unrelated text"),
    ] {
        // A whole command line in one argv word is common with process-title rewriting.
        assert_eq!(redact_cmdline(&[input.into()]), vec![expected.to_owned()]);
    }
}
#[test]
fn electron_children() {
    assert!(is_electron_child(&argv("ChatGPT --type=gpu-process")));
    assert!(!is_electron_child(&argv("ChatGPT --no-sandbox")));
}

#[cfg(unix)]
#[test]
fn app_main_exe_detection() {
    use superba_appctl::procs::is_app_main_exe;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("chatgpt");
    std::fs::create_dir_all(dir.join("resources")).unwrap();
    std::fs::write(dir.join("resources/app.asar"), b"x").unwrap();
    assert!(is_app_main_exe(&dir.join("ChatGPT")));
    assert!(!is_app_main_exe(&dir.join("resources/codex")));
    let mac = tmp.path().join("ChatGPT.app/Contents");
    std::fs::create_dir_all(mac.join("MacOS")).unwrap();
    std::fs::create_dir_all(mac.join("Resources")).unwrap();
    std::fs::write(mac.join("Resources/app.asar"), b"x").unwrap();
    assert!(is_app_main_exe(&mac.join("MacOS/ChatGPT")));
    let cli = mac.join("Resources/codex-cli/CodexCLI.app/Contents/MacOS");
    std::fs::create_dir_all(&cli).unwrap();
    assert!(!is_app_main_exe(&cli.join("codex")));
}

#[test]
fn liveness_of_this_process() {
    use superba_appctl::procs::{Liveness, liveness};
    assert_eq!(liveness(std::process::id(), None), Liveness::Alive);
    assert_eq!(
        liveness(std::process::id(), Some(Timestamp::from_millis(1_000))),
        Liveness::Gone,
        "start time mismatch means pid reuse"
    );
}

/// Only ChatGPT / Codex installs owned by the current user count as the desktop
/// app; other Electron apps ship `resources/app.asar` too.
#[test]
fn app_main_detection_needs_a_codex_install_of_this_user() {
    use superba_appctl::procs::{is_app_main_exe, is_codex_app_entry};
    let tmp = tempfile::tempdir().unwrap();
    let make = |dir: &str, exe: &str| {
        let root = tmp.path().join(dir);
        std::fs::create_dir_all(root.join("resources")).unwrap();
        std::fs::write(root.join("resources").join("app.asar"), b"asar").unwrap();
        let path = root.join(exe);
        std::fs::write(&path, b"#!/bin/sh\n").unwrap();
        path
    };
    let chatgpt = make("chatgpt", "ChatGPT");
    let other = make("other-electron", "adspower_global");
    let proc_for = |exe: &std::path::Path, foreign: bool| {
        let mut e = entry(100, 1, &format!("{} --no-sandbox", exe.display()));
        e.exe = Some(exe.to_path_buf());
        e.foreign_user = foreign;
        e
    };
    // Both look like Electron apps by layout...
    assert!(is_app_main_exe(&chatgpt));
    assert!(is_app_main_exe(&other));
    // ...but only the ChatGPT install of this user is the desktop app.
    assert!(is_codex_app_entry(&proc_for(&chatgpt, false)));
    assert!(!is_codex_app_entry(&proc_for(&other, false)));
    assert!(!is_codex_app_entry(&proc_for(&chatgpt, true)));
}

/// Chromium children on Linux carry their whole command line in one argv word
/// (setproctitle); they are still Electron children, never app mains.
#[test]
fn electron_children_with_a_rewritten_cmdline_are_ignored() {
    let one_word = vec![
        "/opt/chatgpt/ChatGPT --type=renderer --crashpad-handler-pid=1 --user-data-dir=/x"
            .to_owned(),
    ];
    assert!(is_electron_child(&one_word));
    assert!(!is_electron_child(&argv(
        "/opt/chatgpt/ChatGPT --no-sandbox --remote-debugging-port=0"
    )));
    let mut child = entry(11, 10, "x");
    child.argv = one_word;
    child.exe = Some(PathBuf::from("/opt/chatgpt/ChatGPT"));
    let main = entry(10, 1, "/opt/chatgpt/ChatGPT --no-sandbox");
    let snap = Snapshot::classify_entries(vec![main, child], &|e: &ProcEntry| {
        e.exe.as_deref() == Some(std::path::Path::new("/opt/chatgpt/ChatGPT"))
    });
    assert_eq!(snap.app_mains(), vec![10]);
}

/// Superba's own probes (`codex --version`, `features list`, `debug models`)
/// print and exit; a snapshot taken while one runs must not count it as an
/// interactive Codex without CODEX_AUTHAPI_BASE_URL (e2e s10: Diagnostics showed
/// "1 codex processes lack CODEX_AUTHAPI_BASE_URL" for the GUI's own probe).
#[test]
fn informational_invocations_are_not_consumers() {
    for args in [
        "--version",
        "-V",
        "--help",
        "-h",
        "help",
        "help exec",
        "features list",
        "-c model_catalog_json=/x/models.json debug models",
        "debug models --bundled",
    ] {
        assert_eq!(codex_command(&argv(args)), CodexCommand::Info, "{args}");
    }
    assert_eq!(codex_command(&argv("-- --version")), CodexCommand::Tui);
    assert_eq!(codex_command(&argv("exec --help")), CodexCommand::Exec);
    assert_eq!(
        codex_command(&argv("features enable x")),
        CodexCommand::Other
    );
    assert_eq!(
        codex_command(&argv("debug landlock ls")),
        CodexCommand::Other
    );

    let probe = entry(200, 150, "/opt/chatgpt/resources/codex --version");
    let models = entry(
        201,
        150,
        "/opt/chatgpt/resources/codex debug models --bundled",
    );
    let mut tui = entry(202, 1, "codex");
    tui.env = env(&[("HOME", "/h")]);
    let snap =
        Snapshot::classify_entries(vec![entry(150, 1, "superba"), probe, models, tui], &|_| {
            false
        });
    assert_eq!(snap.classified, vec![(202, CodexProcessKind::CliTui)]);
}

#[test]
fn ide_app_servers_name_their_editor() {
    use superba_core::IdeKind;
    // Windows: VS Code's extension host is a `Code.exe --type=utility` child;
    // the extension's own codex.exe sits in the extension directory.
    let code = entry(40, 1, "C:\\Program Files\\Microsoft VS Code\\Code.exe");
    let host = entry(
        41,
        40,
        "C:\\Program Files\\Microsoft VS Code\\Code.exe --type=utility --utility-sub-type=node.mojom.NodeService",
    );
    let vscode = entry(
        42,
        41,
        "C:\\Users\\u\\.vscode\\extensions\\openai.chatgpt-26.5.1-win32-x64\\bin\\windows-x86_64\\codex.exe app-server --analytics-default-enabled",
    );
    // A codex outside any extension directory, started by Cursor's helper.
    let cursor_helper = entry(
        50,
        1,
        "/Applications/Cursor.app/Contents/Frameworks/Cursor Helper (Plugin)",
    );
    let mut cursor = entry(51, 50, "/opt/tools/codex app-server");
    cursor.exe = Some(PathBuf::from("/opt/tools/codex"));
    let mut cursor_helper = cursor_helper;
    cursor_helper.exe = Some(PathBuf::from(
        "/Applications/Cursor.app/Contents/Frameworks/Cursor Helper (Plugin).app/Contents/MacOS/Cursor Helper (Plugin)",
    ));
    // An app-server some other tool started: no editor.
    let other = entry(60, 59, "/usr/local/bin/codex app-server");
    // The terminal codex is never an extension, even inside VS Code's terminal.
    let tui = entry(70, 40, "/usr/local/bin/codex");
    let snap = Snapshot::classify_entries(
        vec![code, host, vscode, cursor_helper, cursor, other, tui],
        &|_| false,
    );
    assert_eq!(snap.kind_of(42), Some(&CodexProcessKind::IdeAppServer));
    assert_eq!(snap.ide_of(42), Some(IdeKind::VsCode));
    assert_eq!(snap.process_info(42).unwrap().ide, Some(IdeKind::VsCode));
    assert_eq!(snap.ide_of(51), Some(IdeKind::Cursor));
    assert_eq!(snap.ide_of(60), None);
    assert_eq!(snap.kind_of(70), Some(&CodexProcessKind::CliTui));
    assert_eq!(snap.ide_of(70), None, "only app-servers are extensions");
}

#[test]
fn the_working_directory_reaches_the_process_info() {
    let mut tui = entry(200, 50, "/usr/local/bin/codex");
    tui.cwd = Some(PathBuf::from("/work/project"));
    let snap = Snapshot::classify_entries(vec![tui], &|_| false);
    assert_eq!(
        snap.process_info(200).unwrap().cwd,
        Some(PathBuf::from("/work/project"))
    );
}

#[test]
fn an_editor_app_server_has_no_working_directory_to_show() {
    // VS Code starts its extension's codex in the editor's program folder.
    let mut ide = entry(
        500,
        450,
        "/home/u/.vscode/extensions/openai.chatgpt/bin/codex app-server --analytics-default-enabled",
    );
    ide.cwd = Some(PathBuf::from("/usr/share/code"));
    let snap = Snapshot::classify_entries(vec![ide], &|_| false);
    assert_eq!(snap.process_info(500).unwrap().cwd, None);
}
