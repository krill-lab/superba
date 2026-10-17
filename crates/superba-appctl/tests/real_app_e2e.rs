//! Real desktop app, end to end (Linux only, ignored by default).
//!
//! Launches the extracted app with an isolated `HOME` under Xvfb, then checks the
//! CDP bridge, process catalog, renderer state, a hot restart (new app-server
//! child), turn activity and a graceful quit. Run with:
//!
//! ```text
//! SUPERBA_APP_DIR=/path/to/extracted/app \
//!   cargo test -p superba-appctl --test real_app_e2e -- --ignored --nocapture
//! ```
//!
//! Optional: `SUPERBA_E2E_DISPLAY` (default `:171`; an Xvfb is started on it when
//! no X server owns it and stopped afterwards, by pid). Nothing outside the
//! temporary home is touched; only processes this test started are stopped.

#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::FutureExt;
use superba_appctl::DesktopAppController;
use superba_core::{
    AppController, BridgeStatus, CdpMode, CodexHome, CodexProcessKind, DataDir, EnvObservation,
    HotRestartRequest, LaunchOptions, LocalContext, MapEnv, QuitOptions, QuitOutcome,
};

struct Xvfb {
    child: Option<std::process::Child>,
    num: String,
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
            // A killed Xvfb leaves its socket and lock behind; the next test would
            // take the stale socket for a running display. We started it, so we
            // remove them.
            let _ = std::fs::remove_file(format!("/tmp/.X11-unix/X{}", self.num));
            let _ = std::fs::remove_file(format!("/tmp/.X{}-lock", self.num));
        }
    }
}

fn ensure_display(display: &str) -> Xvfb {
    let num = display.trim_start_matches(':');
    if Path::new(&format!("/tmp/.X11-unix/X{num}")).exists() {
        return Xvfb {
            child: None,
            num: num.to_owned(),
        };
    }
    let child = std::process::Command::new("Xvfb")
        .args([display, "-screen", "0", "1280x900x24", "-nolisten", "tcp"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("Xvfb");
    for _ in 0..50 {
        if Path::new(&format!("/tmp/.X11-unix/X{num}")).exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Xvfb {
        child: Some(child),
        num: num.to_owned(),
    }
}

fn app_dir() -> Option<PathBuf> {
    std::env::var_os("SUPERBA_APP_DIR")
        .or_else(|| std::env::var_os("SUPERBA_APP_PATH"))
        .map(PathBuf::from)
}

async fn eventually<T, F, Fut>(what: &str, timeout: Duration, mut f: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "launches the real desktop app; needs SUPERBA_APP_DIR and Xvfb"]
async fn launch_hot_restart_quit() {
    let Some(app) = app_dir() else {
        eprintln!("SUPERBA_APP_DIR not set; skipping");
        return;
    };
    let display = std::env::var("SUPERBA_E2E_DISPLAY").unwrap_or_else(|_| ":171".into());
    let _x = ensure_display(&display);

    // Short paths: Chromium and Codex create sockets under the home.
    let root = tempfile::Builder::new()
        .prefix("spb-appctl-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = root.path().join("h");
    std::fs::create_dir_all(&home).unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    let env = MapEnv::new()
        .with("SUPERBA_APP_PATH", app.as_os_str())
        .with("SUPERBA_APP_ARGS", "--no-sandbox --disable-gpu")
        .with("HOME", home.as_os_str())
        .with("PATH", path);
    let ctx = LocalContext::new(&home, DataDir::new(root.path().join("data")), Arc::new(env));
    let codex_home = CodexHome::new(home.join(".codex"));
    let c = DesktopAppController::new(ctx);

    let installs = c.detect_installs().await.unwrap();
    assert!(!installs.is_empty(), "the explicit install is detected");
    eprintln!("install: {:?}", installs[0]);

    let authapi = "https://authapi.example.test";
    let mut env = BTreeMap::new();
    env.insert("DISPLAY".to_owned(), display.clone());
    env.insert("CODEX_AUTHAPI_BASE_URL".to_owned(), authapi.to_owned());
    let options = LaunchOptions {
        install_id: Some(installs[0].id.clone()),
        cdp: CdpMode::Auto,
        env,
        home_override: Some(home.clone()),
        ready_timeout_ms: 60_000,
        ..Default::default()
    };
    let session = c.launch(&options).await.unwrap();
    eprintln!("session: {session:?}");
    let pid = session.main_pid;
    let result = std::panic::AssertUnwindSafe(async {
        let cdp = session.cdp.clone().expect("CDP endpoint");
        assert!(cdp.owner_verified, "listener owned by the app main process");
        assert!(session.launched_by_us);
        assert_eq!(
            session.codex_home.as_deref(),
            Some(codex_home.root()),
            "the app uses the isolated Codex home"
        );
        assert_eq!(session.authapi_env, EnvObservation::Present(authapi.into()));

        let bridge = c.probe_bridge(&session).await.unwrap();
        assert!(matches!(bridge, BridgeStatus::Ready { .. }), "{bridge:?}");

        // Launching again reuses the running instance.
        let again = c.launch(&options).await.unwrap();
        assert_eq!(again.main_pid, pid);

        let running = c.running_session().await.unwrap().expect("running session");
        assert_eq!(running.main_pid, pid);
        assert!(running.launched_by_us);
        assert_eq!(running.cdp.as_ref().map(|c| c.port), Some(cdp.port));

        let procs = c.processes(&codex_home).await.unwrap();
        let old_servers: Vec<u32> = procs
            .iter()
            .filter(|p| p.kind == CodexProcessKind::AppServer && p.ppid == Some(pid))
            .map(|p| p.pid)
            .collect();
        assert!(
            procs
                .iter()
                .any(|p| p.pid == pid && p.kind == CodexProcessKind::AppMain)
        );
        assert!(!old_servers.is_empty(), "app-server child: {procs:?}");

        let state = eventually(
            "renderer shows the login page",
            Duration::from_secs(60),
            || {
                let c = &c;
                let s = &session;
                async move {
                    c.renderer_state(s)
                        .await
                        .ok()
                        .filter(|st| st.on_login_page || st.on_home)
                }
            },
        )
        .await;
        eprintln!("renderer: {state:?}");

        let out = c
            .hot_restart(&session, &HotRestartRequest::local())
            .await
            .unwrap();
        let procs = c.processes(&codex_home).await.unwrap();
        let new_servers: Vec<_> = procs
            .iter()
            .filter(|p| p.kind == CodexProcessKind::AppServer && p.ppid == Some(pid))
            .collect();
        assert!(
            new_servers.iter().any(|p| !old_servers.contains(&p.pid)),
            "a new app-server after the restart: {new_servers:?}"
        );
        assert!(
            new_servers
                .iter()
                .filter(|p| !old_servers.contains(&p.pid))
                .all(|p| p
                    .started_at
                    .is_none_or(|t| t.as_millis() >= out.sent_at.as_millis() - 2_000)),
            "new app-server started after the IPC"
        );
        assert_eq!(
            new_servers[0].authapi_env,
            EnvObservation::Present(authapi.into()),
            "the restarted app-server inherits the launch environment"
        );

        let activity = c.turn_activity(&codex_home, Some(&session)).await.unwrap();
        assert!(activity.complete);
        assert!(!activity.is_busy(), "{activity:?}");
    })
    .catch_unwind()
    .await;

    let quit = c
        .quit(
            &session,
            &QuitOptions {
                graceful_timeout_ms: 20_000,
                force: true,
            },
        )
        .await
        .unwrap();
    // Chromium helpers (crashpad, zygotes) exit shortly after the main process;
    // wait for them before the temporary home is removed.
    let marker = root.path().to_string_lossy().into_owned();
    for _ in 0..40 {
        let left = superba_appctl::procs::snapshot_os(Default::default())
            .into_iter()
            .any(|e| e.argv.iter().any(|a| a.contains(&marker)));
        if !left {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    assert!(
        matches!(quit, QuitOutcome::Exited | QuitOutcome::Killed),
        "{quit:?}"
    );
}

/// Click-through and composer helpers evaluated in the renderer (test only).
const SKIP_ONBOARDING_JS: &str = r#"(() => {
  const click = (t) => { const b = [...document.querySelectorAll('button')].find((b) => b.innerText.trim() === t); if (b) { b.click(); return true; } return false; };
  const el = [...document.querySelectorAll('*')].find((e) => e.children.length === 0 && e.innerText === 'Engineering');
  if (el) el.click();
  return ['Continue', 'Skip', 'Go to ChatGPT'].map(click).some(Boolean);
})()"#;

fn send_prompt_js(text: &str) -> String {
    format!(
        r#"(async () => {{
  const e = document.querySelector('[contenteditable="true"]');
  if (!e) return false;
  e.focus();
  document.execCommand('insertText', false, {});
  await new Promise((r) => setTimeout(r, 300));
  const b = document.querySelector('button[aria-label="Send"]');
  if (!b) return false;
  b.click();
  return true;
}})()"#,
        superba_core::codex::js_string(text)
    )
}

async fn eval(port: u16, js: &str) -> serde_json::Value {
    superba_appctl::cdp::evaluate_in_main(port, js, Duration::from_secs(10))
        .await
        .map(|(_, v)| v)
        .unwrap_or(serde_json::Value::Null)
}

/// Logged-in app (API key against the private test relay from `.secrets/e2e.env`,
/// passed as `SUPERBA_E2E_UPSTREAM` / `SUPERBA_E2E_KEY`): a running turn is seen by
/// the rollout tail and the renderer, a hot restart mid-turn leaves no phantom busy
/// thread, and the renderer can be navigated back to the thread.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs SUPERBA_APP_DIR, SUPERBA_E2E_UPSTREAM, SUPERBA_E2E_KEY and Xvfb"]
async fn logged_in_turn_activity_and_navigation() {
    let (Some(app), Ok(upstream), Ok(key)) = (
        app_dir(),
        std::env::var("SUPERBA_E2E_UPSTREAM"),
        std::env::var("SUPERBA_E2E_KEY"),
    ) else {
        eprintln!("e2e environment not set; skipping");
        return;
    };
    let display = std::env::var("SUPERBA_E2E_DISPLAY").unwrap_or_else(|_| ":171".into());
    let _x = ensure_display(&display);
    let root = tempfile::Builder::new()
        .prefix("spb-appctl-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = root.path().join("h");
    let codex_dir = home.join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(
        codex_dir.join("config.toml"),
        format!(
            "model_provider = \"openai\"\nopenai_base_url = \"{}/backend-api/codex\"\n",
            upstream.trim_end_matches('/')
        ),
    )
    .unwrap();
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(codex_dir.join("auth.json"))
            .unwrap();
        let body = serde_json::json!({"auth_mode": "apikey", "OPENAI_API_KEY": key});
        f.write_all(body.to_string().as_bytes()).unwrap();
    }
    let env = MapEnv::new()
        .with("SUPERBA_APP_PATH", app.as_os_str())
        .with("SUPERBA_APP_ARGS", "--no-sandbox --disable-gpu")
        .with("HOME", home.as_os_str());
    let ctx = LocalContext::new(&home, DataDir::new(root.path().join("data")), Arc::new(env));
    let codex_home = CodexHome::new(&codex_dir);
    let c = DesktopAppController::new(ctx);
    let mut env = BTreeMap::new();
    env.insert("DISPLAY".to_owned(), display.clone());
    let session = c
        .launch(&LaunchOptions {
            env,
            home_override: Some(home.clone()),
            ready_timeout_ms: 60_000,
            ..Default::default()
        })
        .await
        .unwrap();
    let port = session.cdp.as_ref().expect("cdp").port;
    let result = std::panic::AssertUnwindSafe(async {
        // Through onboarding to the home screen.
        eventually("home screen", Duration::from_secs(90), || {
            let c = &c;
            let s = &session;
            async move {
                let st = c.renderer_state(s).await.ok()?;
                if st.on_home {
                    return Some(());
                }
                eval(port, SKIP_ONBOARDING_JS).await;
                None
            }
        })
        .await;
        assert_eq!(
            eval(port, &send_prompt_js("Count slowly from 1 to 200, one number per line.")).await,
            serde_json::Value::Bool(true)
        );
        let busy = eventually("a busy thread", Duration::from_secs(60), || {
            let c = &c;
            let s = &session;
            let h = &codex_home;
            async move {
                let a = c.turn_activity(h, Some(s)).await.ok()?;
                let tail = a
                    .busy
                    .iter()
                    .find(|t| t.signal == superba_core::ActivitySignal::RolloutTail)?
                    .clone();
                Some((a, tail))
            }
        })
        .await;
        eprintln!("busy: {:?}", busy.0.busy);
        let thread_id = busy.1.thread_id.clone().expect("thread id from the rollout");
        let state = c.renderer_state(&session).await.unwrap();
        assert_eq!(
            state.current_thread.as_ref().map(|t| t.thread_id.clone()),
            Some(thread_id.clone())
        );

        c.hot_restart(&session, &HotRestartRequest::local()).await.unwrap();
        // The interrupted turn never gets an end event; it must not stay busy.
        eventually("no busy thread after the restart", Duration::from_secs(30), || {
            let c = &c;
            let s = &session;
            let h = &codex_home;
            async move {
                let a = c.turn_activity(h, Some(s)).await.ok()?;
                (!a.busy.iter().any(|t| t.signal == superba_core::ActivitySignal::RolloutTail))
                    .then_some(())
            }
        })
        .await;

        // Leave the thread, then come back to it.
        eval(
            port,
            r#"(() => { window.dispatchEvent(new MessageEvent('message', {data: {type: 'navigate-to-route', path: '/'}})); return true; })()"#,
        )
        .await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        let st = c.renderer_state(&session).await.unwrap();
        assert!(st.current_thread.is_none(), "{st:?}");
        c.return_to_thread(
            &session,
            &superba_core::ThreadRef {
                thread_id: thread_id.clone(),
                title: None,
            },
        )
        .await
        .unwrap();
        let st = c.renderer_state(&session).await.unwrap();
        assert_eq!(st.current_thread.map(|t| t.thread_id), Some(thread_id));
    })
    .catch_unwind()
    .await;
    let _ = c
        .quit(
            &session,
            &QuitOptions {
                graceful_timeout_ms: 20_000,
                force: true,
            },
        )
        .await;
    let marker = root.path().to_string_lossy().into_owned();
    for _ in 0..40 {
        let left = superba_appctl::procs::snapshot_os(Default::default())
            .into_iter()
            .any(|e| e.argv.iter().any(|a| a.contains(&marker)));
        if !left {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
