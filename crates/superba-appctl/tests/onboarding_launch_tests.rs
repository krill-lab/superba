//! User bug 129: the fake desktop observes the flag at process entry, through
//! the real controller. Uses the self-spawning fixture from launch_handles_tests
//! and the resources layout from install_tests. macOS needs a real app bundle.

#![cfg(not(target_os = "macos"))]
#![allow(missing_docs)]

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use superba_appctl::{DesktopAppController, Tunables};
use superba_core::{
    AppController, AppSession, CdpMode, CodexHome, LaunchOptions, LocalContext, MapEnv,
};

const CAPTURE: &str = "SUPERBA_ONBOARDING_TEST_CAPTURE";
const STOP: &str = "SUPERBA_ONBOARDING_TEST_STOP";

#[test]
fn fake_desktop() {
    let Some(capture) = std::env::var_os(CAPTURE) else {
        return;
    };
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap();
    let home = CodexHome::resolve(&superba_core::ProcessEnv, &PathBuf::from(home));
    let bytes = fs::read(home.global_state_json()).unwrap_or_else(|_| b"unreadable".to_vec());
    fs::write(capture, bytes).unwrap();
    let stop = PathBuf::from(std::env::var_os(STOP).unwrap());
    let deadline = Instant::now() + Duration::from_secs(30);
    while !stop.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
}

struct Fixture {
    tmp: tempfile::TempDir,
    controller: DesktopAppController,
    options: LaunchOptions,
    pid: Option<u32>,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = superba_core::paths::canonical(tmp.path()).unwrap();
        let app = root.join("app");
        fs::create_dir_all(app.join("resources")).unwrap();
        fs::write(app.join("resources/app.asar"), b"fixture").unwrap();
        fs::copy(
            std::env::current_exe().unwrap(),
            app.join(if cfg!(windows) {
                "ChatGPT.exe"
            } else {
                "ChatGPT"
            }),
        )
        .unwrap();
        let mut ctx = LocalContext::for_tests(&root);
        ctx.env = Arc::new(
            MapEnv::new()
                .with("SUPERBA_APP_PATH", app)
                .with("CODEX_HOME", root.join("inherited-codex-home")),
        );
        let mut options = LaunchOptions {
            cdp: CdpMode::Disabled,
            home_override: Some(ctx.home_dir.clone()),
            extra_args: vec![
                "--exact".into(),
                "fake_desktop".into(),
                "--nocapture".into(),
            ],
            ..Default::default()
        };
        options.env.insert(
            CAPTURE.into(),
            root.join("captured").to_str().unwrap().into(),
        );
        options
            .env
            .insert(STOP.into(), root.join("stop").to_str().unwrap().into());
        let controller = DesktopAppController::new(ctx).with_tunables(Tunables {
            app_server_wait: Duration::ZERO,
            ..Default::default()
        });
        Self {
            tmp,
            controller,
            options,
            pid: None,
        }
    }

    fn home(&self) -> CodexHome {
        CodexHome::new(self.options.home_override.as_ref().unwrap().join(".codex"))
    }

    async fn launch(&mut self) -> AppSession {
        let session = self.controller.launch(&self.options).await.unwrap();
        self.pid = Some(session.main_pid);
        session
    }

    async fn captured(&self) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(bytes) = fs::read(self.tmp.path().join("captured")) {
                return bytes;
            }
            assert!(Instant::now() < deadline, "fake desktop did not start");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::write(self.tmp.path().join("stop"), b"");
        if let Some(pid) = self.pid {
            let deadline = Instant::now() + Duration::from_secs(3);
            while superba_appctl::procs::liveness(pid, None)
                == superba_appctl::procs::Liveness::Alive
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

fn assert_completed(bytes: &[u8]) {
    let state: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    assert_eq!(
        state["electron-persisted-atom-state"]["electron:onboarding-projectless-completed"],
        true
    );
}

#[tokio::test]
async fn launch_seeds_the_isolated_home_before_the_desktop_starts() {
    let mut f = Fixture::new();
    let session = f.launch().await;
    assert_completed(&f.captured().await);
    assert_eq!(session.codex_home.as_deref(), Some(f.home().root()));
    assert!(!f.tmp.path().join("inherited-codex-home").exists());
    assert!(
        !f.home()
            .global_state_json()
            .with_extension("json.bak")
            .exists()
    );
}

#[tokio::test]
async fn explicit_codex_home_wins_over_home_override_and_removal() {
    let mut f = Fixture::new();
    let custom = CodexHome::new(f.tmp.path().join("custom"));
    f.options.env_remove.push("CODEX_HOME".into());
    f.options
        .env
        .insert("CODEX_HOME".into(), custom.root().to_str().unwrap().into());
    f.launch().await;
    assert_completed(&f.captured().await);
    assert!(custom.global_state_json().is_file());
    assert!(!f.home().global_state_json().exists());
    assert!(!f.tmp.path().join("inherited-codex-home").exists());
}

#[tokio::test]
async fn invalid_state_and_io_errors_do_not_prevent_launch() {
    for bytes in [
        Some(b"{broken".as_slice()),
        Some(b"\xef\xbb\xbf{}".as_slice()),
        None,
    ] {
        let mut f = Fixture::new();
        let home = f.home();
        fs::create_dir_all(home.root()).unwrap();
        if let Some(bytes) = bytes {
            fs::write(home.global_state_json(), bytes).unwrap();
        } else {
            fs::create_dir(home.global_state_json()).unwrap();
        }
        f.launch().await;
        assert_eq!(f.captured().await, bytes.unwrap_or(b"unreadable"));
    }
}

#[tokio::test]
async fn an_existing_session_returns_without_writing_state() {
    let mut f = Fixture::new();
    let first = f.launch().await;
    assert_completed(&f.captured().await);
    fs::remove_file(f.home().global_state_json()).unwrap();
    let second = f.launch().await;
    assert_eq!(first.main_pid, second.main_pid);
    assert!(!f.home().global_state_json().exists());
}
