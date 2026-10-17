//! The macOS path of the router service, simulated on any platform: a fake
//! `launchctl` that behaves like launchd (asynchronous bootout, EIO / EALREADY
//! right after it, Login Items turned off, nested `launchctl print` blocks, a
//! crash-looping job), a temp HOME with spaces ("Jane Doe", "Application
//! Support"), and the doctor's quarantine / code signature / port / log checks.

mod common;

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{dead_base, key_of, line, member, test_config};
use superba_core::{DataDir, Error, Secret};
use superba_router::MemberKind;
use superba_router::doctor::{self, Finding, PortState};
use superba_router::manager::{
    RouterManager, pick_router_exe, router_exe_candidates_for, router_exe_name, service_spec,
    transient_location,
};
use superba_router::service::*;

const TARGET: &str = "gui/501/com.krill.superba.router";
const APP_EXE: &str = "Superba.app/Contents/MacOS/superba-router";

/// What the simulated job does once launchd has it loaded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Job {
    /// Runs (pid 4242).
    #[default]
    Running,
    /// Exits with 78 and is restarted by KeepAlive (runs = 3).
    ExitsWithConfig,
}

#[derive(Debug, Default)]
struct Sim {
    calls: Vec<String>,
    loaded: bool,
    job: Job,
    /// `print` calls left before a booted-out job is gone.
    teardown: Option<u32>,
    /// Polls a bootout takes (0 = synchronous).
    teardown_polls: u32,
    /// The loaded job never goes away.
    stuck: bool,
    /// `bootout` answers 36 "Operation now in progress".
    bootout_in_progress: bool,
    /// Exit codes of the next `bootstrap` calls (then success).
    bootstrap_failures: VecDeque<i32>,
    /// A failing bootstrap loads the job anyway (another installer won).
    loaded_by_someone_else: bool,
    /// macOS 13+: superba-router turned off under Login Items.
    login_item_off: bool,
    sleeps: u32,
    quarantined: Vec<PathBuf>,
    /// `--version` is killed (Gatekeeper).
    blocked: bool,
}

#[derive(Debug, Default)]
struct Launchd(Mutex<Sim>);

fn out(code: i32, stdout: &str, stderr: &str) -> io::Result<CommandOutput> {
    Ok(CommandOutput {
        code: Some(code),
        stdout: stdout.into(),
        stderr: stderr.into(),
    })
}

fn print_text(job: Job) -> String {
    let (state, tail) = match job {
        Job::Running => (
            "running",
            "\truns = 1\n\tpid = 4242\n\tlast exit code = (never exited)\n",
        ),
        Job::ExitsWithConfig => (
            "not running",
            "\truns = 3\n\tlast exit code = 78: EX_CONFIG\n",
        ),
    };
    format!(
        "{TARGET} = {{\n\tactive count = 1\n\tpath = /Users/Jane Doe/Library/LaunchAgents/com.krill.superba.router.plist\n\
         \ttype = LaunchAgent\n\tstate = {state}\n\n\tprogram = /Applications/{APP_EXE}\n\targuments = {{\n\
         \t\t/Applications/{APP_EXE}\n\t\trun\n\t\t--data-dir\n\t\t/Users/Jane Doe/Library/Application Support/com.krill-code.Superba\n\t}}\n\n\
         \tenvironment = {{\n\t\tRUST_LOG => info\n\t\tXPC_SERVICE_NAME => com.krill.superba.router\n\t}}\n\n\
         \tdomain = gui/501 [100005]\n{tail}\tspawn type = interactive (4)\n\
         \tresource coalition = {{\n\t\tID = 2425\n\t\ttype = resource\n\t\tstate = active\n\t\tactive count = 1\n\t}}\n\n\
         \tjetsam coalition = {{\n\t\tID = 2426\n\t\ttype = jetsam\n\t\tstate = active\n\t}}\n\n\
         \tjob state = {}\n\tproperties = keepalive | runatload | inferred program\n}}\n",
        if job == Job::Running {
            "running"
        } else {
            "exited"
        }
    )
}

impl CommandRunner for Launchd {
    fn run(&self, program: &str, args: &[OsString]) -> io::Result<CommandOutput> {
        let mut s = self.0.lock().unwrap();
        s.calls.push(command_line(program, args));
        let args: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let arg = |i: usize| args.get(i).map(String::as_str).unwrap_or("");
        match program {
            LAUNCHCTL => match arg(0) {
                "bootout" if s.loaded => {
                    if !s.stuck {
                        s.teardown = Some(s.teardown_polls);
                    }
                    if s.bootout_in_progress {
                        out(36, "", "Boot-out failed: 36: Operation now in progress\n")
                    } else {
                        out(0, "", "")
                    }
                }
                "bootout" => out(
                    113,
                    "",
                    "Boot-out failed: 113: Could not find specified service\n",
                ),
                "print" => {
                    if let Some(left) = s.teardown {
                        if left == 0 {
                            s.loaded = false;
                            s.teardown = None;
                        } else {
                            s.teardown = Some(left - 1);
                        }
                    }
                    if s.loaded {
                        out(0, &print_text(s.job), "")
                    } else {
                        out(
                            113,
                            "",
                            "Bad request.\nCould not find service \"com.krill.superba.router\" in domain for user gui: 501\n",
                        )
                    }
                }
                "enable" => out(0, "", ""),
                "bootstrap" => {
                    if s.login_item_off {
                        return out(
                            5,
                            "",
                            "Bootstrap failed: 5: Input/output error\nTry re-running the command as root for richer errors.\n",
                        );
                    }
                    if let Some(code) = s.bootstrap_failures.pop_front() {
                        if s.loaded_by_someone_else {
                            s.loaded = true;
                        }
                        let why = if code == 37 {
                            "Operation already in progress"
                        } else {
                            "Input/output error"
                        };
                        return out(code, "", &format!("Bootstrap failed: {code}: {why}\n"));
                    }
                    if s.loaded {
                        return out(
                            37,
                            "",
                            "Bootstrap failed: 37: Operation already in progress\n",
                        );
                    }
                    s.loaded = true;
                    out(0, "", "")
                }
                "kickstart" if s.loaded => out(0, "", ""),
                "kickstart" => out(113, "", "Could not find service\n"),
                other => panic!("unexpected launchctl {other}"),
            },
            "/usr/bin/xattr" => {
                let path = PathBuf::from(arg(2));
                if s.quarantined.contains(&path) {
                    out(0, "0083;66f0a1b2;Safari;\n", "")
                } else {
                    out(
                        1,
                        "",
                        &format!(
                            "xattr: {}: No such xattr: com.apple.quarantine\n",
                            path.display()
                        ),
                    )
                }
            }
            "/usr/bin/codesign" => out(
                0,
                "",
                "Executable=/Applications/Superba.app/Contents/MacOS/superba-router\nFormat=Mach-O universal (x86_64 arm64)\n\
                 CodeDirectory v=20400 size=1234 flags=0x20002(adhoc,linker-signed) hashes=33+0 location=embedded\nSignature=adhoc\n",
            ),
            "/usr/bin/sw_vers" => out(0, "14.6.1\n", ""),
            exe if exe.ends_with("superba-router") && arg(0) == "--version" => {
                if s.blocked {
                    Ok(CommandOutput {
                        code: None,
                        stdout: String::new(),
                        stderr: String::new(),
                    })
                } else {
                    out(0, "superba-router 0.1.0\n", "")
                }
            }
            other => panic!("unexpected command {other}"),
        }
    }
    fn spawn_detached(
        &self,
        _: &Path,
        _: &[OsString],
        _: &[(String, String)],
        _: &Path,
    ) -> io::Result<u32> {
        unreachable!("launchd never spawns detached")
    }
    fn is_router_process(&self, _: u32) -> bool {
        false
    }
    fn terminate(&self, _: u32) -> io::Result<()> {
        unreachable!()
    }
    fn sleep(&self, _: Duration) {
        self.0.lock().unwrap().sleeps += 1;
    }
}

impl Launchd {
    fn calls(&self) -> Vec<String> {
        self.0.lock().unwrap().calls.clone()
    }
    fn set(&self, f: impl FnOnce(&mut Sim)) {
        f(&mut self.0.lock().unwrap());
    }
    fn get<T>(&self, f: impl FnOnce(&Sim) -> T) -> T {
        f(&self.0.lock().unwrap())
    }
}

/// A Mac-like home: `<tmp>/Users/Jane Doe` with the data dir in
/// `Library/Application Support/com.krill-code.Superba` and the app in
/// `<tmp>/Applications/Superba.app`.
struct Mac {
    _root: tempfile::TempDir,
    home: PathBuf,
    apps: PathBuf,
    data: DataDir,
    launchd: Arc<Launchd>,
}

impl Mac {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("Users/Jane Doe");
        let apps = root.path().join("Applications");
        let data = DataDir::new(home.join("Library/Application Support/com.krill-code.Superba"));
        std::fs::create_dir_all(&home).unwrap();
        Self {
            _root: root,
            home,
            apps,
            data,
            launchd: Arc::new(Launchd::default()),
        }
    }

    /// The router program inside the app bundle (an executable file).
    fn exe(&self) -> PathBuf {
        let exe = self.apps.join(APP_EXE);
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        exe
    }

    fn host(&self) -> ServiceHost {
        ServiceHost {
            home: self.home.clone(),
            xdg_config_home: None,
            uid: Some(501),
            runner: self.launchd.clone(),
            run_key: None,
        }
    }

    fn agent(&self) -> Box<dyn ServiceManager> {
        manager(self.host(), ServiceKind::LaunchAgent)
    }

    fn spec(&self) -> ServiceSpec {
        service_spec(&self.data, self.exe())
    }

    fn plist(&self) -> PathBuf {
        // Joined like the service does (directory, then file): the bootstrap
        // command line is compared as text, and on Windows a `/` inside one
        // join and a `\` between two do not compare equal.
        self.home
            .join("Library/LaunchAgents")
            .join("com.krill.superba.router.plist")
    }

    fn router(&self) -> RouterManager {
        RouterManager::new(&self.data, self.exe(), self.agent()).with_diagnostics(
            self.launchd.clone(),
            true,
            Some(self.home.clone()),
        )
    }
}

fn bootstrap_call(mac: &Mac) -> String {
    command_line(
        LAUNCHCTL,
        &[
            "bootstrap".into(),
            "gui/501".into(),
            mac.plist().into_os_string(),
        ],
    )
}

fn lc(args: &str) -> String {
    format!("{LAUNCHCTL} {args}")
}

#[test]
fn first_install_writes_a_plist_with_spaces_and_bootstraps_it() {
    let mac = Mac::new();
    let spec = mac.spec();
    let status = mac.agent().install(&spec).unwrap();
    assert_eq!(
        (status.installed, status.running, status.pid),
        (true, Some(true), Some(4242))
    );
    assert_eq!(
        mac.launchd.calls(),
        vec![
            lc(&format!("bootout {TARGET}")),
            lc(&format!("print {TARGET}")),
            lc(&format!("enable {TARGET}")),
            bootstrap_call(&mac),
            lc(&format!("print {TARGET}")),
        ]
    );
    // The bootstrap names the plist as one argument, quoted only for display.
    assert!(bootstrap_call(&mac).contains("'"), "spaces are quoted");
    let plist = std::fs::read_to_string(mac.plist()).unwrap();
    // Every argument is its own <string>: spaces need no quoting in a plist.
    let exe = spec.exe.to_str().unwrap();
    let dir = spec.data_dir.to_str().unwrap();
    assert!(plist.contains(&format!(
        "<array>\n    <string>{exe}</string>\n    <string>run</string>\n    <string>--data-dir</string>\n    <string>{dir}</string>\n  </array>"
    )));
    assert!(dir.contains("Application Support") && exe.contains("Superba.app"));
    let log = spec.log_file.to_str().unwrap();
    assert!(plist.contains(&format!(
        "<key>StandardOutPath</key>\n  <string>{log}</string>"
    )));
    assert!(plist.contains(&format!(
        "<key>StandardErrorPath</key>\n  <string>{log}</string>"
    )));
    // launchd does not create the log directory: install does.
    assert!(spec.log_file.parent().unwrap().is_dir());
    for key in ["RunAtLoad", "KeepAlive"] {
        assert!(
            plist.contains(&format!("<key>{key}</key>\n  <true/>")),
            "{key}"
        );
    }
    assert!(plist.contains("<key>ProcessType</key>\n  <string>Interactive</string>"));
    assert!(plist.contains("<key>RUST_LOG</key>\n    <string>info</string>"));
    // launchd's default soft limit of 256 open files is too low for the
    // router (user bug 35); the hard limit asks for no more than macOS allows.
    for (key, value) in [
        ("SoftResourceLimits", superba_router::service::NOFILE_SOFT),
        ("HardResourceLimits", superba_router::service::NOFILE_HARD),
    ] {
        assert!(
            plist.contains(&format!(
                "<key>{key}</key>\n  <dict>\n    <key>NumberOfFiles</key>\n    <integer>{value}</integer>\n  </dict>"
            )),
            "{key}: {plist}"
        );
    }
    assert_eq!(
        (
            superba_router::service::NOFILE_SOFT,
            superba_router::service::NOFILE_HARD
        ),
        (8192, 524_288)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(mac.plist()).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o644,
            "launchd refuses group/world-writable plists"
        );
    }
}

#[test]
fn reinstall_waits_for_an_asynchronous_bootout() {
    let mac = Mac::new();
    mac.launchd.set(|s| {
        s.loaded = true;
        s.teardown_polls = 5;
        s.bootout_in_progress = true;
    });
    mac.agent().install(&mac.spec()).unwrap();
    let calls = mac.launchd.calls();
    let bootstrap = calls
        .iter()
        .position(|c| c == &bootstrap_call(&mac))
        .unwrap();
    assert_eq!(
        calls[..bootstrap]
            .iter()
            .filter(|c| c.contains(" print "))
            .count(),
        6,
        "polled until launchd no longer had the old router: {calls:#?}"
    );
    assert_eq!(calls.iter().filter(|c| c.contains("bootstrap")).count(), 1);
    assert_eq!(mac.launchd.get(|s| s.sleeps), 5);
}

#[test]
fn bootstrap_retries_eio_and_ealready_right_after_a_bootout() {
    let mac = Mac::new();
    mac.launchd
        .set(|s| s.bootstrap_failures = VecDeque::from([37, 5]));
    let status = mac.agent().install(&mac.spec()).unwrap();
    assert_eq!(status.running, Some(true));
    let calls = mac.launchd.calls();
    assert_eq!(
        calls.iter().filter(|c| **c == bootstrap_call(&mac)).count(),
        3
    );
    assert_eq!(mac.launchd.get(|s| s.sleeps), 2);
}

#[test]
fn a_failed_bootstrap_of_a_label_launchd_now_has_counts_as_installed() {
    let mac = Mac::new();
    mac.launchd.set(|s| {
        s.bootstrap_failures = VecDeque::from([5]);
        s.loaded_by_someone_else = true;
    });
    assert!(mac.agent().install(&mac.spec()).unwrap().installed);
    assert_eq!(mac.launchd.get(|s| s.sleeps), 0);
}

#[test]
fn login_item_turned_off_gives_the_command_exit_code_stderr_and_what_to_do() {
    let mac = Mac::new();
    mac.launchd.set(|s| s.login_item_off = true);
    let Error::Process { message } = mac.agent().install(&mac.spec()).unwrap_err() else {
        panic!("expected a process error");
    };
    for needle in [
        bootstrap_call(&mac).as_str(),
        "(exit 5)",
        "Bootstrap failed: 5: Input/output error",
        "Login Items",
        "spb router doctor",
    ] {
        assert!(message.contains(needle), "{needle:?} missing in {message}");
    }
    assert!(
        message.contains("Jane Doe"),
        "the plist path is named: {message}"
    );
    // Retried for a few seconds, then reported; the plist stays for a retry.
    let attempts = mac
        .launchd
        .calls()
        .iter()
        .filter(|c| **c == bootstrap_call(&mac))
        .count();
    assert_eq!(attempts, 12);
    assert!(mac.plist().exists());
}

#[test]
fn an_old_router_that_never_unloads_is_reported_instead_of_bootstrapping_over_it() {
    let mac = Mac::new();
    mac.launchd.set(|s| {
        s.loaded = true;
        s.stuck = true;
    });
    let Error::Process { message } = mac.agent().install(&mac.spec()).unwrap_err() else {
        panic!("expected a process error");
    };
    assert!(message.contains("did not stop"), "{message}");
    assert!(message.contains(&format!("bootout {TARGET}")), "{message}");
    assert!(!mac.launchd.calls().iter().any(|c| c.contains("bootstrap")));
}

#[test]
fn restart_kickstarts_a_loaded_job_and_bootstraps_an_unloaded_one() {
    let mac = Mac::new();
    let spec = mac.spec();
    mac.launchd.set(|s| s.loaded = true);
    mac.agent().restart(&spec).unwrap();
    assert_eq!(
        mac.launchd.calls(),
        vec![
            lc(&format!("print {TARGET}")),
            lc(&format!("kickstart -k {TARGET}"))
        ]
    );
    // Turned off under Login Items and back on, or booted out by hand: the
    // plist exists but launchd does not have the job; kickstart alone would
    // fail with 113.
    let mac = Mac::new();
    let spec = mac.spec();
    mac.agent().restart(&spec).unwrap();
    let calls = mac.launchd.calls();
    assert!(calls.contains(&bootstrap_call(&mac)), "{calls:#?}");
    assert!(!calls.iter().any(|c| c.contains("kickstart")));
    assert!(mac.plist().exists());
}

#[test]
fn status_reads_the_jobs_own_state_not_its_coalitions() {
    let mac = Mac::new();
    mac.launchd.set(|s| s.loaded = true);
    let status = mac.agent().status(&mac.spec()).unwrap();
    assert_eq!(status.running, Some(true));
    assert_eq!(status.detail.as_deref(), Some("running"));
    assert_eq!(status.pid, Some(4242));
    let fields = launchctl_print_fields(&print_text(Job::Running));
    assert_eq!(
        fields
            .iter()
            .filter(|(k, _)| k == "state")
            .map(|(_, v)| v.as_str())
            .collect::<Vec<_>>(),
        ["running"],
        "the `state = active` lines of the coalitions are nested"
    );
    assert!(fields.contains(&("arguments".into(), "{…}".into())));
    assert!(!fields.iter().any(|(k, _)| k == "RUST_LOG"));
}

#[test]
fn diagnose_reports_a_crash_loop_with_the_exit_code() {
    let mac = Mac::new();
    mac.launchd.set(|s| {
        s.loaded = true;
        s.job = Job::ExitsWithConfig;
    });
    let spec = mac.spec();
    mac.agent().install(&spec).unwrap();
    let d = mac.agent().diagnose(&spec);
    assert!(d.crash_looping);
    assert_eq!(d.loaded, Some(true));
    assert_eq!(d.last_exit_code, Some(78));
    assert_eq!(d.field("state"), Some("not running"));
    assert_eq!(d.field("runs"), Some("3"));
    assert_eq!(d.program.as_deref(), Some(spec.exe.as_path()));
    assert_eq!(d.definition_path.as_deref(), Some(mac.plist().as_path()));
    assert!(
        d.query
            .unwrap()
            .command
            .ends_with(&format!("print {TARGET}"))
    );
    // Not loaded: no crash loop, and the reason is visible.
    let mac = Mac::new();
    let d = mac.agent().diagnose(&mac.spec());
    assert_eq!((d.loaded, d.crash_looping), (Some(false), false));
}

fn configure(mac: &Mac, router: &RouterManager) -> u16 {
    let cfg = test_config(vec![
        member(
            "krill",
            MemberKind::Krill,
            true,
            vec![line("a", dead_base())],
        ),
        member(
            "site",
            MemberKind::Compatible,
            false,
            vec![line("b", dead_base())],
        ),
    ]);
    let _ = mac;
    let keys: BTreeMap<String, Secret> = [
        ("krill".to_owned(), Secret::new(key_of("krill"))),
        ("site".to_owned(), Secret::new(key_of("site"))),
    ]
    .into();
    router.write_config(&cfg, keys).unwrap().port
}

#[tokio::test]
async fn a_router_that_never_answers_fails_with_launchds_view_and_the_log() {
    let mac = Mac::new();
    let router = mac.router();
    let cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        true,
        vec![line("a", dead_base())],
    )]);
    let keys: BTreeMap<String, Secret> =
        [("krill".to_owned(), Secret::new(key_of("krill")))].into();
    // The service starts (launchd says running) but the router never answers;
    // its log says why.
    let log = router.paths().log_file();
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    writeln!(
        std::fs::File::create(&log).unwrap(),
        "2026-10-17T03:12:44.123Z ERROR superba_router: cannot start error=network error: http://127.0.0.1:1: cannot listen: Address already in use (os error 48)"
    )
    .unwrap();
    let err = router
        .enable(&cfg, keys, Duration::from_millis(300))
        .await
        .unwrap_err();
    let Error::Timeout { what } = &err else {
        panic!("expected a timeout, got {err:?}");
    };
    for needle in [
        "did not start within",
        "not running (no runtime.json)",
        "service (LaunchAgent): state = running",
        "Address already in use",
        "spb router doctor",
    ] {
        assert!(what.contains(needle), "{needle:?} missing in {what}");
    }
}

#[tokio::test]
async fn a_crash_looping_router_fails_before_the_deadline_with_the_reason() {
    let mac = Mac::new();
    mac.launchd.set(|s| s.job = Job::ExitsWithConfig);
    let router = mac.router();
    let cfg = test_config(vec![member(
        "krill",
        MemberKind::Krill,
        true,
        vec![line("a", dead_base())],
    )]);
    let keys: BTreeMap<String, Secret> =
        [("krill".to_owned(), Secret::new(key_of("krill")))].into();
    let started = Instant::now();
    let err = router
        .enable(&cfg, keys, Duration::from_secs(60))
        .await
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(20), "no 60 s wait");
    let Error::Process { message } = &err else {
        panic!("expected a process error, got {err:?}");
    };
    for needle in [
        "keeps stopping",
        "last exit code = 78: EX_CONFIG",
        "runs = 3",
        "no output",
        "exit 78 without any router output",
    ] {
        assert!(message.contains(needle), "{needle:?} missing in {message}");
    }
}

#[tokio::test]
async fn doctor_on_a_quarantined_app_with_a_crash_loop_and_a_busy_port() {
    let mac = Mac::new();
    let router = mac.router();
    let port = configure(&mac, &router);
    // One member's key is missing from the key file.
    let mut secrets = superba_router::RouterSecrets::load(router.paths()).unwrap();
    secrets.member_keys.remove("site");
    secrets.save(router.paths()).unwrap();
    let exe = router.spec().exe.clone();
    let app = exe.ancestors().nth(3).unwrap().to_path_buf();
    assert!(app.ends_with("Superba.app"));
    mac.launchd.set(|s| {
        s.quarantined = vec![app.clone(), exe.clone()];
        s.loaded = true;
        s.job = Job::ExitsWithConfig;
    });
    std::fs::create_dir_all(mac.plist().parent().unwrap()).unwrap();
    std::fs::write(mac.plist(), "<plist/>").unwrap();
    // Another program holds the port: it answers HTTP, but not as a router.
    let busy = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    let server = std::thread::spawn(move || {
        if let Ok((mut s, _)) = busy.accept() {
            let mut buf = [0u8; 1024];
            let _ = std::io::Read::read(&mut s, &mut buf);
            let _ = s.write_all(
                b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            );
        }
    });
    let facts = router.doctor().await;
    server.join().unwrap();

    assert_eq!(facts.platform.split(' ').next(), Some("macos"));
    assert_eq!(facts.os_version.as_deref(), Some("14.6.1"));
    assert!(facts.exe.exists && facts.exe.executable != Some(false));
    assert!(facts.exe.version.as_ref().unwrap().success());
    assert!(facts.exe.quarantine.is_some());
    assert_eq!(
        facts.exe.bundle.as_ref().map(|(b, _)| b.clone()),
        Some(app.clone())
    );
    assert!(
        facts
            .exe
            .codesign
            .as_ref()
            .unwrap()
            .command
            .starts_with("/usr/bin/codesign -dv")
    );
    assert_eq!(facts.config.members.len(), 2);
    assert!(facts.config.members[0].has_key && !facts.config.members[1].has_key);
    #[cfg(unix)]
    assert_eq!(facts.config.secrets_mode, Some(0o600));
    assert!(matches!(
        facts.port.as_ref().unwrap().state,
        PortState::Busy { .. }
    ));
    assert!(!facts.live.up);
    assert_eq!(
        facts.foreground,
        vec![
            exe.to_string_lossy().into_owned(),
            "run".into(),
            "--data-dir".into(),
            mac.data.root().to_string_lossy().into_owned()
        ]
    );
    let kinds: Vec<&Finding> = facts.findings.iter().collect();
    assert!(
        matches!(kinds[0], Finding::Quarantined { path } if *path == app),
        "{kinds:#?}"
    );
    for expected in [
        Finding::MembersWithoutKey {
            members: vec!["site".into()],
        },
        Finding::StaleDefinition {
            program: PathBuf::new(),
            expected: PathBuf::new(),
        },
        Finding::CrashLoop {
            exit: Some(78),
            signal: None,
        },
        Finding::NotStartedByServiceManager,
    ] {
        let found = facts
            .findings
            .iter()
            .any(|f| std::mem::discriminant(f) == std::mem::discriminant(&expected));
        // The plist above was written by hand: it names no program.
        let expected_present = !matches!(expected, Finding::StaleDefinition { .. });
        assert_eq!(
            found, expected_present,
            "{expected:?} in {:#?}",
            facts.findings
        );
    }
    assert!(
        facts
            .findings
            .iter()
            .any(|f| matches!(f, Finding::PortBusy { port: p, .. } if *p == port))
    );
    // Nothing secret in the facts.
    let text = format!("{facts:?}");
    assert!(!text.contains(&key_of("krill")));
}

#[tokio::test]
async fn doctor_flags_protected_folders_and_a_program_that_does_not_run() {
    let mac = Mac::new();
    // The whole release folder left in ~/Downloads, spb run from there.
    let exe = mac
        .home
        .join("Downloads/superba-0.1.0-macos-universal")
        .join(APP_EXE);
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, b"").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    mac.launchd.set(|s| s.blocked = true);
    let router = RouterManager::new(&mac.data, exe.clone(), mac.agent()).with_diagnostics(
        mac.launchd.clone(),
        true,
        Some(mac.home.clone()),
    );
    let facts = router.doctor().await;
    assert!(facts.exe.protected_folder);
    assert!(
        facts
            .findings
            .contains(&Finding::ProtectedFolder { path: exe.clone() })
    );
    assert!(
        facts
            .findings
            .iter()
            .any(|f| matches!(f, Finding::ExeDoesNotRun { .. }))
    );
    assert!(facts.findings.contains(&Finding::NotConfigured));
    // Nothing configured: no service or port complaints yet.
    assert!(!facts.findings.iter().any(|f| matches!(
        f,
        Finding::ServiceNotInstalled | Finding::PortBusy { .. } | Finding::RouterDown { .. }
    )));
}

#[test]
fn the_installed_app_wins_over_a_mounted_disk_image() {
    let name = router_exe_name();
    let dmg = Path::new("/Volumes/Superba 0.1.0");
    let candidates = router_exe_candidates_for(
        &dmg.join("spb"),
        None,
        Some(Path::new("/Users/Jane Doe")),
        true,
    );
    assert_eq!(
        candidates,
        vec![
            dmg.join(&name),
            dmg.join(APP_EXE),
            Path::new("/Applications").join(APP_EXE),
            Path::new("/Users/Jane Doe/Applications").join(APP_EXE),
        ]
    );
    let installed = Path::new("/Applications").join(APP_EXE);
    let on_dmg = dmg.join(APP_EXE);
    let both = |p: &Path| p == installed || p == on_dmg;
    assert_eq!(
        pick_router_exe(&candidates, both, PathBuf::from("x")),
        installed
    );
    let dmg_only = |p: &Path| p == on_dmg;
    assert_eq!(
        pick_router_exe(&candidates, dmg_only, PathBuf::from("x")),
        on_dmg,
        "still named, so the error says to move the app"
    );
    assert_eq!(
        pick_router_exe(&candidates, |_| false, PathBuf::from("x")),
        PathBuf::from("x")
    );
    // Off macOS no bundle locations are searched.
    assert_eq!(
        router_exe_candidates_for(&dmg.join("spb"), None, None, false),
        vec![dmg.join(&name)]
    );
}

#[test]
fn only_disk_images_and_translocated_copies_are_temporary() {
    let ro = |_: &Path| Some(true);
    let rw = |_: &Path| Some(false);
    let unknown = |_: &Path| None;
    let p = Path::new;
    for (path, read_only, expected) in [
        (
            "/Applications/Superba.app/Contents/MacOS/superba-router",
            &ro as &dyn Fn(&Path) -> Option<bool>,
            false,
        ),
        (
            "/Users/Jane Doe/Applications/Superba.app/Contents/MacOS/superba-router",
            &ro,
            false,
        ),
        (
            "/System/Volumes/Data/Applications/Superba.app/Contents/MacOS/superba-router",
            &ro,
            false,
        ),
        (
            "/Volumes/Superba 0.1.0/Superba.app/Contents/MacOS/superba-router",
            &ro,
            true,
        ),
        (
            "/Volumes/Superba 0.1.0/Superba.app/Contents/MacOS/superba-router",
            &unknown,
            true,
        ),
        // An external drive the user keeps apps on is writable: not temporary.
        (
            "/Volumes/External SSD/Applications/Superba.app/Contents/MacOS/superba-router",
            &rw,
            false,
        ),
        (
            "/private/var/folders/x/T/AppTranslocation/0B1C/d/Superba.app/Contents/MacOS/superba-router",
            &rw,
            true,
        ),
    ] {
        assert_eq!(transient_location(p(path), read_only), expected, "{path}");
    }
}

#[test]
fn command_lines_quote_what_a_shell_would_split() {
    assert_eq!(
        command_line(
            LAUNCHCTL,
            &[
                "bootstrap".into(),
                "gui/501".into(),
                "/Users/Jane Doe/Library/LaunchAgents/x.plist".into()
            ]
        ),
        "/bin/launchctl bootstrap gui/501 '/Users/Jane Doe/Library/LaunchAgents/x.plist'"
    );
    assert_eq!(command_line("a", &["it's".into()]), r"a 'it'\''s'");
}

#[test]
fn log_tail_keeps_the_last_lines_of_a_large_log() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("router.log");
    let mut f = std::fs::File::create(&path).unwrap();
    for i in 0..10_000 {
        writeln!(f, "line {i}").unwrap();
    }
    let tail = doctor::log_tail(&path, 30);
    assert!(tail.exists);
    assert_eq!(tail.tail.len(), 30);
    assert_eq!(tail.tail.last().map(String::as_str), Some("line 9999"));
    assert_eq!(tail.tail.first().map(String::as_str), Some("line 9970"));
    assert!(!doctor::log_tail(&dir.path().join("missing"), 30).exists);
}
