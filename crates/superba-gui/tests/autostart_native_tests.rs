#![cfg(target_os = "linux")]

#[path = "support/sni_host.rs"]
mod sni_host;

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use superba_core::{ProfileSet, Settings};
use superba_gui::instance::Endpoint;

struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !check() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn visible(pid: u32) -> bool {
    let out = Command::new("xdotool")
        .args(["search", "--onlyvisible", "--pid", &pid.to_string()])
        .output()
        .unwrap();
    out.status.success() && !out.stdout.is_empty()
}

/// Run under an isolated Xvfb display/user+PID namespace. The private bus and
/// fake tray host exercise the real GUI binary with a disposable home.
#[test]
#[ignore = "requires an isolated X11 display and xdotool"]
fn real_login_start_is_hidden_single_instance_and_refreshes_after_move() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let data = root.path().join("data");
    let install = root.path().join("custom install");
    for p in [&home, &data, &install] {
        std::fs::create_dir_all(p).unwrap();
    }
    let set = ProfileSet {
        settings: Settings {
            start_in_tray: true,
            auto_probe: false,
            auto_update_check: false,
            ..Default::default()
        },
        ..Default::default()
    };
    std::fs::write(
        data.join("profiles.json"),
        serde_json::to_vec(&set).unwrap(),
    )
    .unwrap();
    let program = install.join("superba");
    std::fs::copy(env!("CARGO_BIN_EXE_superba"), &program).unwrap();
    let bus = sni_host::Bus::start(root.path());
    let host = sni_host::Host::start(&bus);
    let command = |exe: &Path| {
        let mut c = Command::new(exe);
        c.env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("SUPERBA_DATA_DIR", &data)
            .env("CODEX_HOME", home.join("codex"))
            .env("SUPERBA_SECRET_BACKEND", "file")
            .env("LIBGL_ALWAYS_SOFTWARE", "1")
            .env("DISPLAY", std::env::var("DISPLAY").unwrap())
            .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
            .stdin(Stdio::null());
        c
    };
    let run = |exe: &Path, args: &[&str]| {
        let output = command(exe).args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    run(&program, &["autostart", "on"]);
    let entry = home.join(".config/autostart/superba.desktop");
    assert!(
        std::fs::read_to_string(&entry)
            .unwrap()
            .contains(&format!("Exec=\"{}\" --autostart", program.display()))
    );
    let first = Running(
        command(&program)
            .arg("--autostart")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let pid = first.0.id();
    host.wait_item(1, Duration::from_secs(30));
    wait_for("IPC ready", || {
        Endpoint::read(&data).is_some_and(|e| e.pid == pid)
    });
    assert!(!visible(pid), "login start must never show its window");
    run(&program, &["--autostart"]);
    assert_eq!(Endpoint::read(&data).unwrap().pid, pid);
    assert!(!visible(pid), "another login start stays hidden");
    run(&program, &[]);
    wait_for("manual launch shows existing window", || visible(pid));
    assert_eq!(Endpoint::read(&data).unwrap().pid, pid);
    drop(first);
    let moved = root.path().join("moved install");
    std::fs::rename(&install, &moved).unwrap();
    let program = moved.join("superba");
    let second = Running(
        command(&program)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for("moved installation refresh", || {
        std::fs::read_to_string(&entry)
            .unwrap()
            .contains("moved install")
    });
    run(&program, &["autostart", "off"]);
    assert!(!entry.exists());
    drop(second);
    let third = Running(
        command(&program)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for("disabled startup stays disabled after restart", || {
        Endpoint::read(&data).is_some_and(|e| e.pid == third.0.id())
    });
    assert!(!entry.exists());
}
