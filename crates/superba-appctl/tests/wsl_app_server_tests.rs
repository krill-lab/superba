//! The desktop app's "Run Codex in WSL" mode on Windows (user bug 54 (b)): its
//! local app-server is a `wsl.exe` child of the app main that starts the app's
//! own Linux Codex (copied to `%CODEX_HOME%\bin\wsl\codex`). It is the app's
//! app-server (hot restarts wait for it, verification checks it) and sees
//! `CODEX_AUTHAPI_BASE_URL` only when `WSLENV` names it. Remote WSL connections
//! (`wsl.exe -d <distro> -- … exec codex …`) are not it. Command lines as the
//! app builds them (app 26.1013, `application-network-startup` bundle).

use std::collections::BTreeMap;
use std::path::PathBuf;

use pretty_assertions::assert_eq;
use superba_appctl::procs::{ProcEntry, Snapshot, is_wsl_app_server};
use superba_core::{CodexProcessKind, EnvObservation, Timestamp};

const APP: &str =
    r"C:\Program Files\WindowsApps\OpenAI.Codex_26.1013.4894.0_x64__2p2nqsd0c76g0\app\ChatGPT.exe";
const URL: &str = "https://api.kbsfdxc.net";

/// As Windows reports it for app 26.1013 in the test VM (the command line split
/// the way `CommandLineToArgvW` does).
fn legacy() -> Vec<String> {
    [
        r"C:\WINDOWS\system32\wsl.exe",
        "-d",
        "Ubuntu-WSL1",
        "--cd",
        "/mnt/c/Program Files/WindowsApps/OpenAI.Codex_26.1013.4894.0_x64__2p2nqsd0c76g0/app/resources",
        "--exec",
        "/usr/bin/bash",
        "-lc",
        r#"set -m; /bin/bash -c 'read -r codex_process_stat < /proc/$$/stat; codex_process_stat=${codex_process_stat##*) }; set -- $codex_process_stat; printf '"'"'codex-wsl-process-c7861eda-69e9-468c-b288-cc36161331a5:%s:%s\n'"'"' "$$" "${20}" >&2; PATH="$(wslpath -u '"'"'C:\Users\superba\.codex\bin\wsl\f19bbb73214bc62a'"'"')":'"'"'/usr/local/bin:/usr/bin:/bin'"'"' CODEX_SQLITE_HOME="$HOME/.codex/sqlite" exec "$(wslpath -u '"'"'C:\Users\superba\.codex\bin\wsl\f19bbb73214bc62a\codex'"'"')" -c features.code_mode_host=true app-server --analytics-default-enabled' <&0 & codex_process=$!; wait "$codex_process" 2>/dev/null"#,
    ]
    .map(str::to_owned)
    .to_vec()
}

fn connection() -> Vec<String> {
    [
        r"C:\WINDOWS\system32\wsl.exe",
        "-d",
        "Ubuntu",
        "--",
        "/usr/bin/bash",
        "-lc",
        r#"PATH="${CODEX_INSTALL_DIR:-$HOME/.local/bin}":/usr/bin:/bin; export PATH; if ! command -v codex >/dev/null 2>&1; then exit 127; fi; exec codex -c features.code_mode_host=true app-server --analytics-default-enabled"#,
    ]
    .map(str::to_owned)
    .to_vec()
}

fn entry(pid: u32, ppid: u32, argv: Vec<String>, env: &[(&str, &str)]) -> ProcEntry {
    ProcEntry {
        name: None,
        pid,
        ppid: Some(ppid),
        exe: argv.first().map(PathBuf::from),
        argv,
        started_at: Some(Timestamp::from_millis(i64::from(pid) * 1000)),
        env: Some(
            env.iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect::<BTreeMap<_, _>>(),
        ),
        zombie: false,
        foreign_user: false,
        cwd: None,
    }
}

fn snapshot(entries: Vec<ProcEntry>) -> Snapshot {
    Snapshot::classify_entries(entries, &|e: &ProcEntry| {
        e.argv.first().is_some_and(|a| a == APP)
    })
}

#[test]
fn the_run_codex_in_wsl_app_server_is_the_apps_own() {
    assert!(is_wsl_app_server(&legacy()));
    assert!(!is_wsl_app_server(&connection()));
    let snap = snapshot(vec![
        entry(100, 1, vec![APP.to_owned()], &[]),
        entry(110, 100, legacy(), &[("CODEX_HOME", r"C:\Users\u\.codex")]),
        entry(120, 100, connection(), &[]),
        // The same command line not started by the app is not its app-server.
        entry(130, 1, legacy(), &[]),
    ]);
    let kinds: BTreeMap<u32, CodexProcessKind> = snap.classified.iter().cloned().collect();
    assert_eq!(kinds.get(&110), Some(&CodexProcessKind::AppServer));
    assert_eq!(kinds.get(&120), None);
    assert_eq!(kinds.get(&130), None);
    assert_eq!(
        snap.app_servers_of(100)
            .iter()
            .map(|e| e.pid)
            .collect::<Vec<_>>(),
        vec![110]
    );
    // It reads this computer's Codex home (the app forwards it as CODEX_HOME/p).
    assert_eq!(
        snap.codex_home_of(110),
        Some(PathBuf::from(r"C:\Users\u\.codex"))
    );
}

#[test]
fn it_sees_the_authapi_variable_only_when_wslenv_forwards_it() {
    let with = |env: &[(&str, &str)]| {
        let snap = snapshot(vec![
            entry(100, 1, vec![APP.to_owned()], &[]),
            entry(110, 100, legacy(), env),
        ]);
        (
            snap.app_authapi_env(100),
            snap.process_info(110).unwrap().authapi_env,
        )
    };
    let absent = (EnvObservation::Absent, EnvObservation::Absent);
    // Set on Windows, not forwarded: the Codex in WSL does not see it.
    assert_eq!(
        with(&[
            ("CODEX_AUTHAPI_BASE_URL", URL),
            ("WSLENV", "PATHEXT/l:CODEX_HOME/p")
        ]),
        absent
    );
    assert_eq!(with(&[("CODEX_AUTHAPI_BASE_URL", URL)]), absent);
    let present = EnvObservation::Present(URL.into());
    assert_eq!(
        with(&[
            ("CODEX_AUTHAPI_BASE_URL", URL),
            ("WslEnv", "PATHEXT/l:CODEX_AUTHAPI_BASE_URL:CODEX_HOME/p")
        ]),
        (present.clone(), present)
    );
}
