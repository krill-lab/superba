//! Service definitions, lifecycle commands and supervision, using isolated fakes.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use superba_core::Error;
use superba_router::service::*;
use supervise::{EXIT_CONFIG, SuperviseEnd, SupervisedChild, SupervisorPolicy, supervise_with};
type Call = (String, Vec<OsString>);
type Spawn = (PathBuf, Vec<OsString>, Vec<(String, String)>, PathBuf);
#[derive(Debug, Default)]
struct Fake {
    calls: Mutex<Vec<Call>>,
    replies: Mutex<VecDeque<io::Result<CommandOutput>>>,
    spawns: Mutex<Vec<Spawn>>,
    alive: Mutex<BTreeSet<u32>>,
    values: Mutex<BTreeMap<String, String>>,
    next: AtomicU32,
    fail_spawn: AtomicBool,
    fail_terminate: AtomicBool,
}
impl Fake {
    fn record(&self, p: &str, a: &[OsString]) {
        self.calls.lock().unwrap().push((p.into(), a.into()));
    }
    fn reply(&self, code: i32, text: &str) {
        let out = CommandOutput {
            code: Some(code),
            stdout: text.into(),
            stderr: "failure 中".repeat(60),
        };
        self.replies.lock().unwrap().push_back(Ok(out));
    }
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
}
impl CommandRunner for Fake {
    fn run(&self, p: &str, a: &[OsString]) -> io::Result<CommandOutput> {
        self.record(p, a);
        // Without a scripted reply every command succeeds, except `launchctl
        // print`, which answers like launchd for a label it has not loaded (113),
        // so a bootout is complete at once.
        let unloaded = p == LAUNCHCTL && a.first().is_some_and(|x| x == "print");
        let default = CommandOutput {
            code: Some(if unloaded { 113 } else { 0 }),
            ..Default::default()
        };
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(default))
    }
    fn sleep(&self, _: Duration) {}
    fn spawn_detached(
        &self,
        exe: &Path,
        args: &[OsString],
        env: &[(String, String)],
        log: &Path,
    ) -> io::Result<u32> {
        let mut argv = vec![exe.as_os_str().to_owned()];
        argv.extend_from_slice(args);
        self.record("spawn", &argv);
        let spawn = (exe.into(), args.into(), env.into(), log.into());
        self.spawns.lock().unwrap().push(spawn);
        if self.fail_spawn.load(Ordering::Relaxed) {
            return Err(io::Error::other("cannot spawn"));
        }
        let pid = self.next.fetch_add(1, Ordering::Relaxed) + 900;
        self.alive.lock().unwrap().insert(pid);
        Ok(pid)
    }
    fn is_router_process(&self, pid: u32) -> bool {
        self.record("check", &[pid.to_string().into()]);
        self.alive.lock().unwrap().contains(&pid)
    }
    fn terminate(&self, pid: u32) -> io::Result<()> {
        self.record("terminate", &[pid.to_string().into()]);
        self.alive.lock().unwrap().remove(&pid);
        if self.fail_terminate.load(Ordering::Relaxed) {
            Err(io::Error::other("already exited"))
        } else {
            Ok(())
        }
    }
}
impl RunKeyStore for Fake {
    fn get(&self, name: &str) -> io::Result<Option<String>> {
        self.record("get", &[name.into()]);
        Ok(self.values.lock().unwrap().get(name).cloned())
    }
    fn set(&self, name: &str, value: &str) -> io::Result<()> {
        self.record("set", &[name.into(), value.into()]);
        self.values
            .lock()
            .unwrap()
            .insert(name.into(), value.into());
        Ok(())
    }
    fn delete(&self, name: &str) -> io::Result<()> {
        self.record("delete", &[name.into()]);
        self.values.lock().unwrap().remove(name);
        Ok(())
    }
}
struct Fixture {
    home: tempfile::TempDir,
    spec: ServiceSpec,
    runner: Arc<Fake>,
}
impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let spec = ServiceSpec {
            exe: "/opt/superba-router".into(),
            data_dir: "/data/router".into(),
            log_file: home.path().join("logs/router.log"),
            pid_file: home.path().join("router.pid"),
            env: vec![("RUST_LOG".into(), "info".into())],
        };
        Self {
            home,
            spec,
            runner: Arc::new(Fake::default()),
        }
    }
    fn host(&self) -> ServiceHost {
        ServiceHost {
            home: self.home.path().into(),
            xdg_config_home: None,
            uid: Some(501),
            runner: self.runner.clone(),
            run_key: Some(self.runner.clone()),
        }
    }
    fn manager(&self, kind: ServiceKind) -> Box<dyn ServiceManager> {
        manager(self.host(), kind)
    }
    fn path(&self, kind: ServiceKind) -> PathBuf {
        // Joined like the service does (directory, then file): the launchd job
        // is bootstrapped with this path as an argument, so on Windows a
        // `/` inside one join and a `\` between two would not compare equal.
        let (dir, file) = match kind {
            S => (".config/systemd/user", "superba-router.service"),
            L => ("Library/LaunchAgents", "com.krill.superba.router.plist"),
            _ => (".config/autostart", "superba-router.desktop"),
        };
        self.home.path().join(dir).join(file)
    }
    fn definition(&self, kind: ServiceKind) -> Option<String> {
        if kind == W {
            self.runner
                .values
                .lock()
                .unwrap()
                .get(RUN_VALUE_NAME)
                .cloned()
        } else {
            std::fs::read_to_string(self.path(kind)).ok()
        }
    }
    fn pid(&self, text: &str, alive: bool) {
        std::fs::write(&self.spec.pid_file, text).unwrap();
        if alive {
            self.runner
                .alive
                .lock()
                .unwrap()
                .insert(text.trim().parse().unwrap());
        }
    }
}
use ServiceKind::{LaunchAgent as L, SystemdUser as S, WindowsRunKey as W, XdgAutostart as X};
const ALL: [ServiceKind; 4] = [S, X, L, W];
const TARGET: &str = "gui/501/com.krill.superba.router";
fn call(p: &str, a: &[&str]) -> Call {
    (p.into(), a.iter().map(OsString::from).collect())
}
fn show() -> Call {
    call(
        "systemctl",
        &[
            "--user",
            "show",
            SYSTEMD_UNIT,
            "--property=LoadState,ActiveState,SubState,MainPID",
        ],
    )
}
fn spawn_call() -> Call {
    call(
        "spawn",
        &[
            "/opt/superba-router",
            "supervise",
            "--data-dir",
            "/data/router",
        ],
    )
}
const UNIT: &str = "# Managed by Superba: the local router service. Removed when the router is disabled.\n[Unit]\nDescription=Superba local router\nAfter=network.target\nStartLimitIntervalSec=0\n\n[Service]\nType=simple\nExecStart=@CMD@\nRestart=always\nRestartSec=2\nRestartPreventExitStatus=78\nEnvironment=@ENV@\n\n[Install]\nWantedBy=default.target\n";
const DESKTOP: &str = "[Desktop Entry]\nType=Application\nName=Superba local router\nComment=Managed by Superba: the local router service\nExec=@CMD@\nTerminal=false\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n";
const PLIST: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>com.krill.superba.router</string>\n  <key>ProgramArguments</key>\n  <array>\n    <string>@EXE@</string>\n    <string>run</string>\n    <string>--data-dir</string>\n    <string>@DIR@</string>\n  </array>\n  <key>RunAtLoad</key>\n  <true/>\n  <key>KeepAlive</key>\n  <true/>\n  <key>ProcessType</key>\n  <string>Interactive</string>\n  <key>ThrottleInterval</key>\n  <integer>5</integer>\n  <key>StandardOutPath</key>\n  <string>@LOG@</string>\n  <key>StandardErrorPath</key>\n  <string>@LOG@</string>\n  <key>EnvironmentVariables</key>\n  <dict>\n    <key>RUST_LOG</key>\n    <string>@ENV@</string>\n  </dict>\n</dict>\n</plist>\n";
fn exact(f: &Fixture, kind: ServiceKind, special: bool) {
    let mut spec = f.spec.clone();
    if special {
        spec.exe = r#"/opt/a b"\%$&<中/superba-router"#.into();
        spec.data_dir = r#"/data/a b"\%$&<中"#.into();
        spec.env[0].1 = r#"a b"\%$&<中"#.into();
    }
    let expected = match (kind, special) {
        (S, false) => UNIT.replace("@CMD@", r#""/opt/superba-router" "run" "--data-dir" "/data/router""#).replace("@ENV@", r#""RUST_LOG=info""#),
        (S, true) => UNIT.replace("@CMD@", r#""/opt/a b\"\\%%$$&<中/superba-router" "run" "--data-dir" "/data/a b\"\\%%$$&<中""#).replace("@ENV@", r#""RUST_LOG=a b\"\\%%$$&<中""#),
        (X, false) => DESKTOP.replace("@CMD@", "env RUST_LOG=info /opt/superba-router supervise --data-dir /data/router"),
        (X, true) => DESKTOP.replace("@CMD@", r#"env "RUST_LOG=a b\\"\\\\%%\\$&<中" "/opt/a b\\"\\\\%%\\$&<中/superba-router" supervise --data-dir "/data/a b\\"\\\\%%\\$&<中""#),
        (W, false) => "/opt/superba-router supervise --data-dir /data/router".into(),
        (W, true) => r#""/opt/a b\"\%$&<中/superba-router" supervise --data-dir "/data/a b\"\%$&<中""#.into(),
        (L, special) => PLIST.replace("@EXE@", if special { r"/opt/a b&quot;\%$&amp;&lt;中/superba-router" } else { "/opt/superba-router" }).replace("@DIR@", if special { r"/data/a b&quot;\%$&amp;&lt;中" } else { "/data/router" }).replace("@ENV@", if special { r"a b&quot;\%$&amp;&lt;中" } else { "info" }).replace("@LOG@", spec.log_file.to_str().unwrap()),
    };
    assert!(f.manager(kind).install(&spec).unwrap().installed);
    assert_eq!(f.definition(kind).unwrap(), expected);
    if matches!(kind, X | W) {
        assert_eq!(
            f.runner.spawns.lock().unwrap()[0],
            (
                spec.exe.clone(),
                spec.supervise_args(),
                spec.env.clone(),
                spec.log_file.clone()
            )
        );
    }
}
fn lifecycle(f: &Fixture, kind: ServiceKind, expected: Vec<Call>) {
    let m = f.manager(kind);
    assert_eq!(m.kind(), kind);
    m.install(&f.spec).unwrap();
    m.status(&f.spec).unwrap();
    m.restart(&f.spec).unwrap();
    m.uninstall(&f.spec).unwrap();
    assert_eq!(f.runner.calls(), expected);
    assert!(f.definition(kind).is_none());
    assert!(!f.spec.pid_file.exists());
}
fn mandatory(f: &Fixture, kind: ServiceKind, index: usize) {
    for _ in 0..index {
        f.runner.reply(0, "");
    }
    f.runner.reply(1, "");
    let Error::Process { message } = f.manager(kind).install(&f.spec).unwrap_err() else {
        panic!("expected Process");
    };
    assert!(message.starts_with("systemctl:"));
    assert!(message.contains("failure 中"));
    assert!(message.chars().count() <= 311);
    assert!(f.definition(kind).is_some());
    assert_eq!(f.runner.calls().len(), index + 1);
}
fn idempotent(f: &Fixture, kind: ServiceKind) {
    let m = f.manager(kind);
    m.uninstall(&f.spec).unwrap();
    m.uninstall(&f.spec).unwrap();
    assert!(!m.status(&f.spec).unwrap().installed);
    m.install(&f.spec).unwrap();
    let before = f.definition(kind);
    m.install(&f.spec).unwrap();
    assert_eq!(f.definition(kind), before);
    m.uninstall(&f.spec).unwrap();
}
fn state(
    f: &Fixture,
    kind: ServiceKind,
    code: i32,
    text: &str,
    running: Option<bool>,
    pid: Option<u32>,
    detail: Option<&str>,
) {
    f.runner.reply(code, text);
    let s = f.manager(kind).status(&f.spec).unwrap();
    assert_eq!(
        (s.installed, s.running, s.pid, s.detail.as_deref()),
        (false, running, pid, detail)
    );
}
macro_rules! cases { ($($name:ident($f:ident) $body:block)*) => { $(#[test] fn $name() { let $f = Fixture::new(); $body })* }; }
cases! {
    unit_plain(f) { exact(&f, S, false); }
    unit_escaped(f) { exact(&f, S, true); }
    desktop_plain(f) { exact(&f, X, false); }
    desktop_escaped(f) { exact(&f, X, true); }
    plist_plain(f) { exact(&f, L, false); }
    plist_escaped(f) { exact(&f, L, true); }
    run_value_plain(f) { exact(&f, W, false); }
    run_value_escaped(f) { exact(&f, W, true); }
    systemd_commands(f) { lifecycle(&f, S, vec![
        call("systemctl", &["--user", "daemon-reload"]), call("systemctl", &["--user", "enable", SYSTEMD_UNIT]),
        call("systemctl", &["--user", "restart", SYSTEMD_UNIT]), show(), show(),
        call("systemctl", &["--user", "restart", SYSTEMD_UNIT]), call("systemctl", &["--user", "disable", "--now", SYSTEMD_UNIT]),
        call("systemctl", &["--user", "daemon-reload"]), call("systemctl", &["--user", "reset-failed", SYSTEMD_UNIT]),
    ]); }
    desktop_commands(f) { lifecycle(&f, X, vec![
        spawn_call(), call("check", &["900"]), call("check", &["900"]), call("check", &["900"]),
        call("terminate", &["900"]), call("check", &["900"]), spawn_call(), call("check", &["901"]),
        call("terminate", &["901"]), call("check", &["901"]),
    ]); }
    launchd_commands(f) {
        // install (not loaded yet) / status / restart of a loaded job / uninstall.
        for code in [0, 113, 0, 0, 0, 0, 0, 0, 0] { f.runner.reply(code, ""); }
        lifecycle(&f, L, vec![
            call(LAUNCHCTL, &["bootout", TARGET]), call(LAUNCHCTL, &["print", TARGET]), call(LAUNCHCTL, &["enable", TARGET]),
            (LAUNCHCTL.into(), vec!["bootstrap".into(), "gui/501".into(), f.path(L).into_os_string()]),
            call(LAUNCHCTL, &["print", TARGET]), call(LAUNCHCTL, &["print", TARGET]),
            call(LAUNCHCTL, &["print", TARGET]), call(LAUNCHCTL, &["kickstart", "-k", TARGET]), call(LAUNCHCTL, &["bootout", TARGET]),
        ]);
    }
    runkey_commands(f) { lifecycle(&f, W, vec![
        call("set", &[RUN_VALUE_NAME, "/opt/superba-router supervise --data-dir /data/router"]), spawn_call(),
        call("get", &[RUN_VALUE_NAME]), call("check", &["900"]), call("get", &[RUN_VALUE_NAME]),
        call("check", &["900"]), call("check", &["900"]), call("terminate", &["900"]), call("check", &["900"]),
        spawn_call(), call("delete", &[RUN_VALUE_NAME]), call("check", &["901"]), call("terminate", &["901"]), call("check", &["901"]),
    ]); }
    reload_required(f) { mandatory(&f, S, 0); }
    enable_required(f) { mandatory(&f, S, 1); }
    restart_required(f) { mandatory(&f, S, 2); }
    systemd_idempotence(f) { idempotent(&f, S); }
    desktop_idempotence(f) { idempotent(&f, X); }
    launchd_idempotence(f) { idempotent(&f, L); }
    runkey_idempotence(f) { idempotent(&f, W); }
    active(f) { state(&f, S, 0, "LoadState=loaded\nActiveState=active\nSubState=running\nMainPID=42\n", Some(true), Some(42), Some("active/running")); }
    inactive(f) { state(&f, S, 0, "ActiveState=inactive\nSubState=dead\nMainPID=0", Some(false), None, Some("inactive/dead")); }
    failed(f) { state(&f, S, 0, "ActiveState=failed\nSubState=failed\nMainPID=invalid", Some(false), None, Some("failed/failed")); }
    systemd_unavailable(f) { f.runner.replies.lock().unwrap().push_back(Err(io::Error::other("unavailable"))); assert_eq!(f.manager(S).status(&f.spec).unwrap().running, None); }
    launchd_running(f) { state(&f, L, 0, "gui/501 = {\n state = running\n pid = 72\n}", Some(true), Some(72), Some("running")); }
    launchd_waiting(f) { state(&f, L, 0, "state = waiting\npid = 0", Some(false), None, Some("waiting")); }
    launchd_unloaded(f) { state(&f, L, 113, "state = running\npid = 42", Some(false), None, None); }
    launchd_unavailable(f) { f.runner.replies.lock().unwrap().push_back(Err(io::Error::other("unavailable"))); assert_eq!(f.manager(L).status(&f.spec).unwrap().running, None); }
    ignored_systemd_stop_failures(f) { for _ in 0..3 { f.runner.reply(1, ""); } f.manager(S).uninstall(&f.spec).unwrap(); assert_eq!(f.runner.calls().len(), 3); }
    ignored_launchd_install_failures(f) { f.runner.reply(1, ""); f.runner.reply(1, ""); assert!(f.manager(L).install(&f.spec).unwrap().installed); }
    ignored_launchd_unload_failure(f) { f.runner.reply(1, ""); f.manager(L).uninstall(&f.spec).unwrap(); }
    mandatory_restart_failures(f) { f.runner.reply(1, ""); assert!(matches!(f.manager(S).restart(&f.spec), Err(Error::Process { .. }))); }
    command_io_failure(f) { f.runner.replies.lock().unwrap().push_back(Err(io::Error::other("not found"))); assert!(matches!(f.manager(S).install(&f.spec), Err(Error::Process { message }) if message.contains("systemctl") && message.contains("not found"))); assert!(f.path(S).exists()); }
    missing_uid(f) { let mut host = f.host(); host.uid = None; let m = manager(host, L); for result in [m.install(&f.spec).map(|_| ()), m.uninstall(&f.spec), m.status(&f.spec).map(|_| ()), m.restart(&f.spec)] { assert!(matches!(result, Err(Error::Unsupported { .. }))); } assert!(f.runner.calls().is_empty()); }
    missing_run_key(f) { let mut host = f.host(); host.run_key = None; let m = manager(host, W); for result in [m.install(&f.spec).map(|_| ()), m.uninstall(&f.spec), m.status(&f.spec).map(|_| ()), m.restart(&f.spec)] { assert!(matches!(result, Err(Error::Unsupported { .. }))); } assert!(f.runner.calls().is_empty()); }
    invalid_paths(f) { for k in ALL { for bad in ["bad\npath", "bad\0path", "bad\rpath"] { for field in 0..4 { let mut s = f.spec.clone(); *[&mut s.exe, &mut s.data_dir, &mut s.log_file, &mut s.pid_file][field] = bad.into(); assert!(matches!(f.manager(k).install(&s), Err(Error::Invalid { .. }))); } } } assert!(f.runner.calls().is_empty()); }
    invalid_environment(f) { for k in ALL { for value in ["bad\nvalue", "bad\0value"] { let mut s = f.spec.clone(); s.env[0].1 = value.into(); assert!(matches!(f.manager(k).install(&s), Err(Error::Invalid { .. }))); } } assert!(f.runner.calls().is_empty()); }
    stale_pid_never_terminated(f) { for k in [X, W] { f.pid("123", false); let s = f.manager(k).status(&f.spec).unwrap(); assert_eq!((s.pid, s.running), (Some(123), Some(false))); f.manager(k).restart(&f.spec).unwrap(); assert_eq!(std::fs::read_to_string(&f.spec.pid_file).unwrap().trim().parse::<u32>().unwrap(), if k == X { 900 } else { 901 }); } assert!(!f.runner.calls().iter().any(|(p, _)| p == "terminate")); }
    invalid_pid_files(f) { for k in [X, W] { for value in ["", "garbage", "0", "-12", "4294967296"] { f.pid(value, false); let s = f.manager(k).status(&f.spec).unwrap(); assert_eq!((s.pid, s.running), (None, Some(false))); f.manager(k).uninstall(&f.spec).unwrap(); assert!(!f.spec.pid_file.exists()); } } }
    binary_pid_is_stale(f) { for k in [X, W] { std::fs::write(&f.spec.pid_file, [0xff, 0]).unwrap(); assert_eq!(f.manager(k).status(&f.spec).unwrap().running, Some(false)); f.manager(k).uninstall(&f.spec).unwrap(); assert!(!f.spec.pid_file.exists()); } }
    valid_pid_stopped(f) { for k in [X, W] { f.pid(" 123\n", true); f.manager(k).uninstall(&f.spec).unwrap(); assert!(!f.spec.pid_file.exists()); } assert_eq!(f.runner.calls().iter().filter(|(p, _)| p == "terminate").count(), 2); }
    concurrent_exit_during_stop(f) { f.pid("123", true); f.runner.fail_terminate.store(true, Ordering::Relaxed); f.manager(X).uninstall(&f.spec).unwrap(); assert!(!f.spec.pid_file.exists()); }
    spawn_failure_keeps_definition(f) { f.runner.fail_spawn.store(true, Ordering::Relaxed); for k in [X, W] { assert!(matches!(f.manager(k).install(&f.spec), Err(Error::Process { .. }))); assert!(f.definition(k).is_some()); assert!(!f.spec.pid_file.exists()); } }
    restart_keeps_definition(f) { for k in [X, W] { let m = f.manager(k); m.install(&f.spec).unwrap(); let definition = f.definition(k); m.restart(&f.spec).unwrap(); assert_eq!(f.definition(k), definition); m.uninstall(&f.spec).unwrap(); } }
    environment_omitted(f) { let mut s = f.spec.clone(); s.env.clear(); for k in [S, X, L] { f.manager(k).install(&s).unwrap(); let text = f.definition(k).unwrap(); assert!(!text.contains("Environment")); assert!(!text.contains("Exec=env ")); } }
    xdg_config_override(f) { let mut host = f.host(); host.xdg_config_home = Some(host.home.join("custom config")); for k in [S, X] { manager(host.clone(), k).install(&f.spec).unwrap(); assert!(!f.path(k).exists()); } assert!(host.config_home().join("systemd/user").join(SYSTEMD_UNIT).exists()); assert!(host.config_home().join("autostart").join(AUTOSTART_FILE).exists()); }
    windows_trailing_backslashes(f) { let mut s = f.spec.clone(); s.exe = r#"C:\Program Files\router\"#.into(); s.data_dir = r#"C:\a\"b\"#.into(); f.manager(W).install(&s).unwrap(); assert_eq!(f.definition(W).unwrap(), r#""C:\Program Files\router\\" supervise --data-dir "C:\a\\\"b\\""#); }
    desktop_reserved_characters(f) { let mut s = f.spec.clone(); s.env.clear(); s.data_dir = "a'<>~|&;*?#()`\t$%\\\"".into(); f.manager(X).install(&s).unwrap(); assert_eq!(f.definition(X).unwrap(), DESKTOP.replace("@CMD@", r#"/opt/superba-router supervise --data-dir "a'<>~|&;*?#()\\`\t\\$%%\\\\\\"""#)); }
    xml_apostrophe_and_greater_than(f) { let mut s = f.spec.clone(); s.env = vec![("A'&<>\"".into(), "'>&<\"".into())]; f.manager(L).install(&s).unwrap(); assert!(f.definition(L).unwrap().contains("<key>A&apos;&amp;&lt;&gt;&quot;</key>\n    <string>&apos;&gt;&amp;&lt;&quot;</string>")); }
}
cases! {
    detect_platform(f) {
        #[cfg(not(any(windows, target_os = "macos")))]
        { f.runner.reply(0, ""); assert_eq!(detect(&f.host()), S); f.runner.reply(1, ""); assert_eq!(detect(&f.host()), X);
          f.runner.replies.lock().unwrap().push_back(Err(io::Error::other("missing"))); assert_eq!(for_platform(f.host()).kind(), X);
          assert_eq!(f.runner.calls(), vec![call("systemctl", &["--user", "show-environment"]); 3]); }
        #[cfg(windows)]
        { assert_eq!(detect(&f.host()), W); assert!(f.runner.calls().is_empty()); }
        #[cfg(target_os = "macos")]
        { assert_eq!(detect(&f.host()), L); assert!(f.runner.calls().is_empty()); }
    }
}
#[cfg(unix)]
cases! {
    file_permissions_and_atomic_cleanup(f) {
        use std::os::unix::fs::PermissionsExt;
        for k in [S, X, L] { f.manager(k).install(&f.spec).unwrap();
            assert_eq!(std::fs::metadata(f.path(k)).unwrap().permissions().mode() & 0o777, 0o644);
            assert_eq!(std::fs::read_dir(f.path(k).parent().unwrap()).unwrap().count(), 1); }
        assert_eq!(std::fs::metadata(&f.spec.pid_file).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
#[derive(Clone)]
struct Plan {
    lifetime: Duration,
    code: Option<i32>,
    graceful: bool,
    wait_error: bool,
}
impl Plan {
    fn exit(seconds: u64, code: Option<i32>) -> Self {
        Self {
            lifetime: Duration::from_secs(seconds),
            code,
            graceful: false,
            wait_error: false,
        }
    }
}
struct ChildFake {
    plan: Plan,
    clock: Arc<Mutex<Instant>>,
    start: Instant,
    events: Arc<Mutex<Vec<&'static str>>>,
    terminated: bool,
}
impl SupervisedChild for ChildFake {
    fn try_wait(&mut self) -> io::Result<Option<Option<i32>>> {
        if self.plan.wait_error {
            return Err(io::Error::other("wait failed"));
        }
        let elapsed = self.clock.lock().unwrap().duration_since(self.start);
        let done = (self.terminated && self.plan.graceful) || elapsed >= self.plan.lifetime;
        Ok(done.then_some(self.plan.code))
    }
    fn terminate(&mut self) {
        self.terminated = true;
        self.events.lock().unwrap().push("terminate");
    }
    fn kill(&mut self) {
        self.events.lock().unwrap().push("kill");
    }
}
struct Simulation {
    end: SuperviseEnd,
    starts: Vec<Duration>,
    sleeps: Vec<Duration>,
    events: Vec<&'static str>,
}
fn simulate(
    plans: Vec<io::Result<Plan>>,
    stop_at: Option<Duration>,
    policy: SupervisorPolicy,
) -> Simulation {
    let start = Instant::now();
    let clock = Arc::new(Mutex::new(start));
    let events = Arc::new(Mutex::new(vec![]));
    let stop = AtomicBool::new(stop_at == Some(Duration::ZERO));
    let sleeps = Mutex::new(vec![]);
    let mut starts = vec![];
    let mut plans = VecDeque::from(plans);
    let mut spawn = || -> io::Result<Box<dyn SupervisedChild>> {
        let now = *clock.lock().unwrap();
        starts.push(now.duration_since(start));
        let plan = plans.pop_front().expect("unexpected spawn")?;
        Ok(Box::new(ChildFake {
            plan,
            clock: clock.clone(),
            start: now,
            events: events.clone(),
            terminated: false,
        }))
    };
    let now = || *clock.lock().unwrap();
    let sleep = |d| {
        sleeps.lock().unwrap().push(d);
        let mut time = clock.lock().unwrap();
        *time += d;
        if stop_at.is_some_and(|at| time.duration_since(start) >= at) {
            stop.store(true, Ordering::Relaxed);
        }
    };
    let end = supervise_with(&mut spawn, &stop, &policy, &now, &sleep);
    Simulation {
        end,
        starts,
        sleeps: sleeps.into_inner().unwrap(),
        events: events.lock().unwrap().clone(),
    }
}
fn seconds(values: &[u64]) -> Vec<Duration> {
    values.iter().copied().map(Duration::from_secs).collect()
}
macro_rules! supervisor_cases { ($($name:ident $body:block)*) => { $(#[test] fn $name() $body)* }; }
supervisor_cases! {
    supervisor_defaults { let p = SupervisorPolicy::default(); assert_eq!([p.initial_backoff, p.max_backoff, p.healthy_after, p.poll, p.stop_grace], [Duration::from_secs(1), Duration::from_secs(30), Duration::from_secs(60), Duration::from_millis(200), Duration::from_secs(5)]); }
    config_exit_stops { let s = simulate(vec![Ok(Plan::exit(0, Some(EXIT_CONFIG)))], None, SupervisorPolicy::default()); assert_eq!(s.end, SuperviseEnd::ConfigInvalid); assert!(s.sleeps.is_empty()); }
    backoff_doubles_and_caps { let mut plans = vec![Ok(Plan::exit(0, Some(1))), Ok(Plan::exit(0, None)), Ok(Plan::exit(0, Some(0))), Ok(Plan::exit(0, Some(2)))]; plans.push(Ok(Plan::exit(0, Some(EXIT_CONFIG)))); let p = SupervisorPolicy { max_backoff: Duration::from_secs(3), ..Default::default() }; let s = simulate(plans, None, p); assert_eq!(s.starts, seconds(&[0, 1, 3, 6, 9])); assert!(s.sleeps.iter().all(|d| *d <= Duration::from_millis(200))); }
    healthy_run_resets_backoff { let s = simulate(vec![Ok(Plan::exit(0, Some(1))), Ok(Plan::exit(60, Some(1))), Ok(Plan::exit(0, Some(1))), Ok(Plan::exit(0, Some(EXIT_CONFIG)))], None, SupervisorPolicy::default()); assert_eq!(s.starts, seconds(&[0, 1, 62, 64])); }
    stop_during_backoff { let s = simulate(vec![Ok(Plan::exit(0, None))], Some(Duration::from_millis(400)), SupervisorPolicy::default()); assert_eq!(s.end, SuperviseEnd::Stopped); assert_eq!(s.starts, seconds(&[0])); assert_eq!(s.sleeps.iter().sum::<Duration>(), Duration::from_millis(400)); assert!(s.events.is_empty()); }
    stop_grace_then_kill { let p = SupervisorPolicy { stop_grace: Duration::from_millis(550), ..Default::default() }; let s = simulate(vec![Ok(Plan::exit(100, None))], Some(Duration::from_millis(200)), p); assert_eq!(s.end, SuperviseEnd::Stopped); assert_eq!(s.events, ["terminate", "kill"]); assert_eq!(s.sleeps.iter().sum::<Duration>(), Duration::from_millis(750)); }
    graceful_stop_needs_no_kill { let mut plan = Plan::exit(100, None); plan.graceful = true; let s = simulate(vec![Ok(plan)], Some(Duration::from_millis(200)), SupervisorPolicy::default()); assert_eq!(s.events, ["terminate"]); assert_eq!(s.end, SuperviseEnd::Stopped); }
    repeated_spawn_errors { let plans = (0..10).map(|n| Err(io::Error::other(format!("spawn {n}")))).collect(); let s = simulate(plans, None, SupervisorPolicy::default()); assert_eq!(s.end, SuperviseEnd::SpawnFailed("spawn 9".into())); assert_eq!(s.starts, seconds(&[0, 1, 3, 7, 15, 31, 61, 91, 121, 151])); }
    spawn_failure_counter_resets { let mut plans = (0..9).map(|_| Err(io::Error::other("spawn"))).collect::<Vec<_>>(); plans.push(Ok(Plan::exit(0, Some(1)))); plans.extend((0..9).map(|_| Err(io::Error::other("spawn")))); plans.push(Ok(Plan::exit(0, Some(EXIT_CONFIG)))); let s = simulate(plans, None, SupervisorPolicy::default()); assert_eq!(s.end, SuperviseEnd::ConfigInvalid); assert_eq!(s.starts.len(), 20); }
    stop_before_spawn { let s = simulate(vec![], Some(Duration::ZERO), SupervisorPolicy::default()); assert_eq!(s.end, SuperviseEnd::Stopped); assert!(s.starts.is_empty()); }
    polling_error_stops_child { let mut plan = Plan::exit(100, None); plan.wait_error = true; let s = simulate(vec![Ok(plan), Ok(Plan::exit(0, Some(EXIT_CONFIG)))], None, SupervisorPolicy::default()); assert_eq!(s.end, SuperviseEnd::ConfigInvalid); assert_eq!(s.events, ["terminate", "kill"]); }
}
