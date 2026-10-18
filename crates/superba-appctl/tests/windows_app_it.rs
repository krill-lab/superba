//! The real ChatGPT app for Windows (the MSIX package `OpenAI.Codex`) on a
//! disposable Windows runner (user bugs 40 and 41). Ignored by default; the
//! `winapp` CI job installs the official package and runs:
//!
//! ```text
//! SUPERBA_WINAPP_IT=1 cargo test -p superba-appctl --test windows_app_it -- --ignored --nocapture
//! ```
//!
//! It checks registry discovery (no listing of WindowsApps, plain paths, the
//! family id), the AUMID from the package API, the bundled Codex version,
//! activation with `--remote-debugging-port`, CDP, a graceful quit through the
//! app's own `quit-app` message, a quit without CDP (windows close, the app
//! stays) and a forced quit of the whole process tree, and relaunches. It also
//! records what environment the activated app gets: a probe variable is
//! written to `HKCU\Environment` before each activation (with and without the
//! `WM_SETTINGCHANGE` broadcast) and read back from the app's processes. The
//! probe variable is removed at the end; only processes of the app this test
//! started are stopped. Findings are printed with a `[winapp]` prefix and
//! appended to `$SUPERBA_WINAPP_REPORT` when set.

#![cfg(windows)]

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use superba_appctl::procs::{self, Liveness, ReadOpts};
use superba_appctl::{DesktopAppController, Tunables, appx, codexbin, install, quit};
use superba_core::{
    AppController, AppSession, CdpMode, DataDir, LaunchOptions, LocalContext, ProcessEnv,
    QuitOptions, QuitOutcome,
};

const FAMILY: &str = "OpenAI.Codex_2p2nqsd0c76g0";
const PROBE: &str = "SUPERBA_IT_ENV_PROBE";

fn say(line: impl AsRef<str>) {
    let line = line.as_ref();
    println!("[winapp] {line}");
    if let Some(path) = std::env::var_os("SUPERBA_WINAPP_REPORT")
        && let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Write or delete `HKCU\Environment\<name>`, optionally broadcasting
/// `WM_SETTINGCHANGE("Environment")` like `setx` and Superba's env persister.
fn set_user_env(name: &str, value: Option<&str>, broadcast: bool) {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};
    let key = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags("Environment", KEY_READ | KEY_SET_VALUE)
        .unwrap();
    match value {
        Some(v) => key.set_value(name, &v.to_owned()).unwrap(),
        None => {
            let _ = key.delete_value(name);
        }
    }
    if broadcast {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
        };
        let text: Vec<u16> = "Environment\0".encode_utf16().collect();
        let mut result = 0usize;
        // SAFETY: NUL-terminated buffer alive for the call; valid out pointer.
        unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                text.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                5000,
                &mut result,
            );
        }
    }
}

/// `name` in the environment of `pid` and of its app-server children.
fn observed(pid: u32, name: &str) -> String {
    let entries = procs::snapshot_os(ReadOpts { environ: true });
    let of = |p: u32| {
        entries
            .iter()
            .find(|e| e.pid == p)
            .map(|e| match &e.env {
                Some(env) => env.get(name).cloned().unwrap_or_else(|| "<unset>".into()),
                None => "<unreadable>".into(),
            })
            .unwrap_or_else(|| "<no process>".into())
    };
    let children: Vec<String> = entries
        .iter()
        .filter(|e| e.ppid == Some(pid) && e.argv.iter().any(|a| a == "app-server"))
        .map(|e| format!("app-server {}={}", e.pid, of(e.pid)))
        .collect();
    format!("main {pid}={} {}", of(pid), children.join(" "))
}

fn parent_of(pid: u32) -> String {
    let entries = procs::snapshot_os(ReadOpts::default());
    let Some(ppid) = entries.iter().find(|e| e.pid == pid).and_then(|e| e.ppid) else {
        return "unknown".into();
    };
    let exe = entries
        .iter()
        .find(|e| e.pid == ppid)
        .and_then(|e| e.exe.clone().or_else(|| e.argv.first().map(PathBuf::from)))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<exited or not readable>".into());
    format!("{ppid} {exe}")
}

fn options(install_id: &str) -> LaunchOptions {
    LaunchOptions {
        install_id: Some(install_id.to_owned()),
        cdp: CdpMode::Auto,
        ready_timeout_ms: 120_000,
        ..LaunchOptions::default()
    }
}

async fn launch(ctl: &DesktopAppController, label: &str) -> AppSession {
    let t = Instant::now();
    let session = ctl
        .launch(&options(FAMILY))
        .await
        .unwrap_or_else(|e| panic!("{label}: launch failed: {e}"));
    say(format!(
        "{label}: launched pid {} in {:.1}s, cdp {:?}, exe {:?}, parent {}",
        session.main_pid,
        t.elapsed().as_secs_f32(),
        session.cdp,
        session.executable,
        parent_of(session.main_pid)
    ));
    session
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "the real MSIX app on a disposable Windows runner (winapp CI job)"]
async fn packaged_app_launches_quits_and_relaunches() {
    if std::env::var("SUPERBA_WINAPP_IT").as_deref() != Ok("1") {
        return;
    }
    let data = tempfile::tempdir().unwrap();
    let home = PathBuf::from(std::env::var_os("USERPROFILE").unwrap());
    let ctx = LocalContext::new(home, DataDir::new(data.path()), Arc::new(ProcessEnv));
    let ctl = DesktopAppController::new(ctx).with_tunables(Tunables {
        ready_timeout: Duration::from_secs(120),
        app_server_wait: Duration::from_secs(30),
        ..Tunables::default()
    });

    // --- discovery ------------------------------------------------------------
    let installs = ctl.detect_installs().await.unwrap();
    for i in &installs {
        say(format!("install {i:?}"));
    }
    let install = installs
        .iter()
        .find(|i| i.id == FAMILY)
        .expect("the OpenAI.Codex package is discovered with its family id")
        .clone();
    for p in [&install.path, &install.executable] {
        assert!(!p.to_string_lossy().starts_with(r"\\?\"), "{}", p.display());
    }
    for full in appx::family_full_names(FAMILY) {
        say(format!(
            "package {full}: path {:?}, application ids {:?}",
            appx::package_path(&full),
            appx::application_ids(&full)
        ));
    }
    let aumid = appx::resolve_aumid(&install);
    say(format!("aumid {aumid:?}"));
    assert_eq!(aumid.as_deref(), Some("OpenAI.Codex_2p2nqsd0c76g0!App"));
    if let Some(binary) = &install.bundled_codex {
        say(format!(
            "bundled codex {}: --version {:?}, read from the binary {:?}, reported {:?}",
            binary.display(),
            install::codex_version(binary).await,
            codexbin::scanned_version(binary),
            install.bundled_codex_version
        ));
    }
    assert!(
        install.bundled_codex_version.is_some(),
        "the bundled Codex version is known"
    );

    // --- first activation: CDP, bridge, graceful quit through the app --------
    set_user_env(PROBE, Some("one-broadcast"), true);
    let first = launch(&ctl, "first").await;
    assert!(first.cdp.is_some(), "the activated app listens for CDP");
    say(format!(
        "first: env after HKCU write + broadcast: {}",
        observed(first.main_pid, PROBE)
    ));
    say(format!(
        "first: bridge {:?}",
        ctl.probe_bridge(&first).await
    ));
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        for copy in appx::relocated_codex_candidates(std::path::Path::new(&local)) {
            say(format!(
                "first: the app's copy {} ({} bytes): --version {:?}",
                copy.display(),
                std::fs::metadata(&copy).map(|m| m.len()).unwrap_or(0),
                install::codex_version(&copy).await
            ));
        }
    }
    let running = ctl
        .running_session()
        .await
        .unwrap()
        .expect("running session");
    say(format!(
        "first: running session pid {} install {:?} cdp {:?}",
        running.main_pid, running.install_id, running.cdp
    ));
    assert_eq!(running.main_pid, first.main_pid);
    assert_eq!(running.install_id.as_deref(), Some(FAMILY));
    let t = Instant::now();
    let outcome = ctl
        .quit(
            &first,
            &QuitOptions {
                graceful_timeout_ms: 30_000,
                force: false,
            },
        )
        .await
        .unwrap();
    say(format!(
        "first: graceful quit over CDP: {outcome:?} in {:.1}s",
        t.elapsed().as_secs_f32()
    ));
    assert_eq!(outcome, QuitOutcome::Exited);

    // --- second: new value + broadcast; quit without CDP, then forced ---------
    set_user_env(PROBE, Some("two-broadcast"), true);
    let second = launch(&ctl, "second").await;
    say(format!(
        "second: env after a new value + broadcast: {}",
        observed(second.main_pid, PROBE)
    ));
    let tree = quit::descendants(&procs::snapshot_os(ReadOpts::default()), second.main_pid);
    say(format!("second: process tree {} descendants", tree.len()));
    let mut no_cdp = second.clone();
    no_cdp.cdp = None;
    let t = Instant::now();
    let outcome = ctl
        .quit(
            &no_cdp,
            &QuitOptions {
                graceful_timeout_ms: 30_000,
                force: false,
            },
        )
        .await
        .unwrap();
    say(format!(
        "second: quit without CDP (WM_CLOSE only): {outcome:?} in {:.1}s",
        t.elapsed().as_secs_f32()
    ));
    if outcome == QuitOutcome::TimedOut {
        let report = ctl.quit_report(&no_cdp).await.unwrap();
        say(format!(
            "second: report after the graceful quit: {}",
            report.describe()
        ));
        assert!(
            report
                .left
                .iter()
                .any(|p| p.pid == no_cdp.main_pid && p.role == "main"),
            "the report names the main process"
        );
        // The main process with its image and architecture; every other
        // process at least with its image (one of another user may refuse
        // the architecture query).
        let main = report
            .left
            .iter()
            .find(|p| p.pid == no_cdp.main_pid)
            .unwrap();
        assert!(main.arch.is_some() && main.image.is_some(), "{main:?}");
        assert!(report.left.iter().all(|p| p.image.is_some()));
    }
    if outcome != QuitOutcome::Exited {
        let t = Instant::now();
        let outcome = ctl
            .quit(
                &no_cdp,
                &QuitOptions {
                    graceful_timeout_ms: 5_000,
                    force: true,
                },
            )
            .await
            .unwrap();
        say(format!(
            "second: forced quit: {outcome:?} in {:.1}s",
            t.elapsed().as_secs_f32()
        ));
        if outcome != QuitOutcome::Killed {
            let report = ctl.quit_report(&no_cdp).await.unwrap();
            say(format!("second: report: {}", report.describe()));
        }
        assert_eq!(outcome, QuitOutcome::Killed);
    }
    let left: Vec<u32> = tree
        .iter()
        .filter(|(pid, started)| procs::liveness(*pid, *started) == Liveness::Alive)
        .map(|(pid, _)| *pid)
        .collect();
    say(format!("second: descendants still alive: {left:?}"));
    assert!(left.is_empty());

    // --- third: new value without broadcast; relaunch after a forced quit ----
    set_user_env(PROBE, Some("three-no-broadcast"), false);
    let third = launch(&ctl, "third").await;
    say(format!(
        "third: env after a new value without broadcast: {}",
        observed(third.main_pid, PROBE)
    ));
    let outcome = ctl
        .quit(
            &third,
            &QuitOptions {
                graceful_timeout_ms: 30_000,
                force: true,
            },
        )
        .await
        .unwrap();
    say(format!("third: quit: {outcome:?}"));
    assert!(matches!(outcome, QuitOutcome::Exited | QuitOutcome::Killed));
    set_user_env(PROBE, None, true);
}

/// Electron process type (`--type=...`), `app-server` for Codex, else the
/// image name.
fn role(e: &procs::ProcEntry) -> String {
    if let Some(t) = e.argv.iter().find_map(|a| a.strip_prefix("--type=")) {
        return t.to_owned();
    }
    if e.argv.iter().any(|a| a == "app-server") {
        return "app-server".into();
    }
    e.exe
        .as_deref()
        .and_then(|x| x.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "?".into())
}

/// Every process of the tree with what the OS says about it.
fn dump(label: &str, main: u32, tree: &[(u32, Option<superba_core::Timestamp>)]) {
    use superba_appctl::osproc;
    let entries = procs::snapshot_os(ReadOpts::default());
    let mut pids = vec![(main, None)];
    pids.extend(tree.iter().cloned());
    for (pid, started) in pids {
        let e = entries.iter().find(|e| e.pid == pid);
        let f = osproc::facts(pid);
        say(format!(
            "{label}: pid {pid} role {} ppid {:?} listed {} sysinfo-start {:?} recorded {:?} os-created {:?} running {:?} arch {:?} image {:?} open-error {:?} liveness {:?}",
            e.map(role).unwrap_or_else(|| "<not listed>".into()),
            e.and_then(|e| e.ppid),
            e.is_some(),
            e.and_then(|e| e.started_at),
            started,
            f.created,
            f.running,
            f.arch,
            f.image,
            f.open_error,
            procs::liveness(pid, started.or(e.and_then(|e| e.started_at))),
        ));
    }
}

/// The user's flow (user bug 53): the app opened from the Start menu (no
/// debugging port), quit without CDP (windows close, the app stays), then by
/// force; at a timeout everything that is still running is described.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "the real MSIX app on a disposable Windows runner (winapp CI job)"]
async fn start_menu_app_force_quits() {
    if std::env::var("SUPERBA_WINAPP_IT").as_deref() != Ok("1") {
        return;
    }
    use superba_appctl::{osproc, package};
    say(format!(
        "start menu: this process runs as {}, the machine is {}",
        osproc::own_arch(),
        package::native_arch()
    ));
    let data = tempfile::tempdir().unwrap();
    let home = PathBuf::from(std::env::var_os("USERPROFILE").unwrap());
    let ctx = LocalContext::new(home, DataDir::new(data.path()), Arc::new(ProcessEnv));
    let ctl = DesktopAppController::new(ctx);
    let status = std::process::Command::new("explorer.exe")
        .arg(format!("shell:AppsFolder\\{FAMILY}!App"))
        .status();
    say(format!("start menu: explorer {status:?}"));
    let t = Instant::now();
    let session = loop {
        if let Some(s) = ctl.running_session().await.unwrap() {
            break s;
        }
        assert!(
            t.elapsed() < Duration::from_secs(90),
            "the app did not start"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    // Let it finish starting (renderers, app-server).
    tokio::time::sleep(Duration::from_secs(15)).await;
    let session = ctl.running_session().await.unwrap().unwrap_or(session);
    say(format!(
        "start menu: session pid {} started {:?} cdp {:?} exe {:?} parent {}",
        session.main_pid,
        session.started_at,
        session.cdp,
        session.executable,
        parent_of(session.main_pid)
    ));
    let tree = quit::descendants(&procs::snapshot_os(ReadOpts::default()), session.main_pid);
    dump("start menu: before", session.main_pid, &tree);
    let t = Instant::now();
    let outcome = ctl
        .quit(
            &session,
            &QuitOptions {
                graceful_timeout_ms: 15_000,
                force: false,
            },
        )
        .await
        .unwrap();
    say(format!(
        "start menu: graceful quit: {outcome:?} in {:.1}s",
        t.elapsed().as_secs_f32()
    ));
    let mut outcome = outcome;
    if outcome == QuitOutcome::TimedOut {
        let t = Instant::now();
        outcome = ctl
            .quit(
                &session,
                &QuitOptions {
                    graceful_timeout_ms: 15_000,
                    force: true,
                },
            )
            .await
            .unwrap();
        say(format!(
            "start menu: forced quit: {outcome:?} in {:.1}s",
            t.elapsed().as_secs_f32()
        ));
    }
    dump("start menu: after", session.main_pid, &tree);
    if outcome == QuitOutcome::TimedOut {
        let report = ctl.quit_report(&session).await.unwrap();
        say(format!("start menu: report: {}", report.describe()));
        let after = ctl.end_app_processes(&session, &report.left).await.unwrap();
        say(format!(
            "start menu: after ending them: {}",
            after.describe()
        ));
        dump("start menu: after ending them", session.main_pid, &tree);
    }
    let left: Vec<u32> = tree
        .iter()
        .filter(|(pid, started)| procs::liveness(*pid, *started) == Liveness::Alive)
        .map(|(pid, _)| *pid)
        .collect();
    for (pid, started) in &tree {
        if left.contains(pid) {
            let r = osproc::end(*pid, *started, Duration::from_secs(2));
            say(format!("start menu: ending left-over {pid}: {r:?}"));
        }
    }
    assert_eq!(outcome, QuitOutcome::Killed);
    assert!(left.is_empty(), "descendants still alive: {left:?}");
}
