//! User bugs 135/136 through the real controller: the fake desktop reads its
//! global state at process entry, so it sees what Superba left before the
//! spawn. Uses the self-spawning fixture of launch_handles_tests and the
//! resources layout of install_tests. macOS needs a real app bundle.

#![cfg(not(target_os = "macos"))]
#![allow(missing_docs)]

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use superba_appctl::account_state::ACCOUNT_STATE_KEY;
use superba_appctl::{DesktopAppController, Tunables};
use superba_core::{
    AppController, CdpMode, CodexHome, DataDir, LaunchOptions, LocalContext, MapEnv,
};

const CAPTURE: &str = "SUPERBA_ACCOUNT_STATE_TEST_CAPTURE";
const STOP: &str = "SUPERBA_ACCOUNT_STATE_TEST_STOP";
const PAT: &str = r#"{"personal_access_token":"nb_test","OPENAI_API_KEY":"nb_test"}"#;
const CHATGPT: &str = r#"{"OPENAI_API_KEY":null,"tokens":{"id_token":"a.b.c","access_token":"a.b.c","refresh_token":"r","account_id":"acc"}}"#;

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
    data: DataDir,
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
        ctx.env = Arc::new(MapEnv::new().with("SUPERBA_APP_PATH", app));
        let data = ctx.data_dir.clone();
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
            data,
            controller,
            options,
            pid: None,
        }
    }

    fn home(&self) -> CodexHome {
        CodexHome::new(self.options.home_override.as_ref().unwrap().join(".codex"))
    }

    fn write(&self, auth: &str, state: &Value) {
        let home = self.home();
        fs::create_dir_all(home.root()).unwrap();
        fs::write(home.auth_json(), auth).unwrap();
        fs::write(home.global_state_json(), serde_json::to_vec(state).unwrap()).unwrap();
    }

    async fn launch_and_capture(&mut self) -> Value {
        let session = self.controller.launch(&self.options).await.unwrap();
        self.pid = Some(session.main_pid);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(bytes) = fs::read(self.tmp.path().join("captured")) {
                return serde_json::from_slice(&bytes).unwrap();
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

fn stashed_projects() -> Value {
    json!({
        ACCOUNT_STATE_KEY: {
            "owner": null,
            "inactive": {
                "owner-hash": {
                    "atoms": {},
                    "globals": {
                        "electron-saved-workspace-roots": ["/w/proj-a"],
                        "local-projects": {"local-a": {"id": "local-a", "name": "proj-a", "rootPaths": ["/w/proj-a"]}}
                    }
                }
            }
        },
        "local-projects": {"c": {"id": "c", "name": "proj-c", "rootPaths": ["/w/proj-c"]}}
    })
}

#[tokio::test]
async fn a_pat_launch_sees_its_projects_and_no_account_state() {
    let mut f = Fixture::new();
    f.write(PAT, &stashed_projects());
    let seen = f.launch_and_capture().await;
    assert!(seen.get(ACCOUNT_STATE_KEY).is_none());
    assert_eq!(
        seen["local-projects"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["c", "local-a"]
    );
    assert_eq!(seen["electron-saved-workspace-roots"], json!(["/w/proj-a"]));
    let backups: Vec<_> = fs::read_dir(f.data.app_state_backups_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(backups.len(), 1);
    let original: Value = serde_json::from_slice(&fs::read(&backups[0]).unwrap()).unwrap();
    assert_eq!(original, stashed_projects());
}

#[tokio::test]
async fn a_chatgpt_launch_keeps_the_app_s_account_state() {
    let mut f = Fixture::new();
    f.write(CHATGPT, &stashed_projects());
    assert_eq!(f.launch_and_capture().await, stashed_projects());
    assert!(!f.data.app_state_backups_dir().exists());
}

#[tokio::test]
async fn a_launch_brings_back_the_queued_folders_before_the_desktop_starts() {
    let mut f = Fixture::new();
    f.write(CHATGPT, &json!({"local-projects": {}}));
    superba_appctl::project_restore::queue(
        &f.data.app_state_dir(),
        f.home().root(),
        vec![superba_appctl::project_restore::RestoreItem {
            path: "/w/proj-c".into(),
            extra_roots: Vec::new(),
            name: "proj-c".into(),
            app_id: Some("877476ef-uuid".into()),
        }],
        superba_core::Timestamp::EPOCH,
    )
    .unwrap();
    let seen = f.launch_and_capture().await;
    assert_eq!(
        seen["local-projects"]["877476ef-uuid"]["rootPaths"],
        json!(["/w/proj-c"])
    );
    assert_eq!(
        superba_appctl::project_restore::pending(&f.data.app_state_dir(), f.home().root()).unwrap(),
        None
    );
}
