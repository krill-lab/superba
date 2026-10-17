//! v0.3 real-app check (Linux, ignored by default): pin the desktop app's Codex
//! to a store package through `LaunchOptions` and prove it with the app-server's
//! process image and the rollout's `cli_version`; an unpinned launch from a
//! parent environment that has `CODEX_CLI_PATH` still runs the bundled Codex.
//!
//! Run inside an unprivileged user + PID namespace, with the private test relay
//! (`SUPERBA_E2E_UPSTREAM`, `SUPERBA_E2E_KEY`; never printed):
//!
//! ```text
//! SUPERBA_APP_DIR=<extracted app> SUPERBA_E2E_DISPLAY=:175 \
//! SUPERBA_TEST_PIN_PACKAGE=<codex-package-<target>.tar.gz> SUPERBA_TEST_PIN_VERSION=0.162.0 \
//! CODEX_CLI_PATH=/nonexistent/inherited/codex \
//!   unshare --user --map-current-user --pid --fork --mount-proc <test binary> --ignored --nocapture
//! ```

#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::FutureExt;
use superba_appctl::DesktopAppController;
use superba_core::codex::ENV_CODEX_CLI_PATH;
use superba_core::{
    AppController, CodexHome, DataDir, LaunchOptions, LocalContext, MapEnv, QuitOptions,
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

/// `cli_version` of the newest rollout's `session_meta`.
fn newest_rollout_cli_version(sessions: &Path) -> Option<String> {
    let mut files = Vec::new();
    let mut stack = vec![sessions.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "jsonl") {
                files.push(p);
            }
        }
    }
    files.sort();
    let text = std::fs::read_to_string(files.last()?).ok()?;
    let first: serde_json::Value = serde_json::from_str(text.lines().next()?).ok()?;
    first["payload"]["cli_version"].as_str().map(str::to_owned)
}

struct Setup {
    root: tempfile::TempDir,
    home: PathBuf,
    codex: CodexHome,
    controller: DesktopAppController,
    display: String,
}

fn setup(app: &Path, upstream: &str, key: &str, display: &str) -> Setup {
    let root = tempfile::Builder::new()
        .prefix("spb-pin-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = root.path().join("h");
    let codex_dir = home.join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(
        codex_dir.join("config.toml"),
        format!(
            "model = \"gpt-6-luna\"\nmodel_reasoning_effort = \"low\"\nmodel_provider = \"openai\"\nopenai_base_url = \"{}/backend-api/codex\"\n",
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
        .with(
            "SUPERBA_APP_ARGS",
            "--no-sandbox --disable-gpu --password-store=basic",
        )
        .with("HOME", home.as_os_str());
    let ctx = LocalContext::new(&home, DataDir::new(root.path().join("data")), Arc::new(env));
    Setup {
        codex: CodexHome::new(&codex_dir),
        controller: DesktopAppController::new(ctx),
        home,
        root,
        display: display.to_owned(),
    }
}

async fn quit_and_wait(s: &Setup, session: &superba_core::AppSession) {
    let _ = s
        .controller
        .quit(
            session,
            &QuitOptions {
                graceful_timeout_ms: 20_000,
                force: true,
            },
        )
        .await;
    let marker = s.root.path().to_string_lossy().into_owned();
    for _ in 0..40 {
        let left = superba_appctl::procs::snapshot_os(Default::default())
            .into_iter()
            .any(|e| e.argv.iter().any(|a| a.contains(&marker)));
        if !left {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "launches the real app; needs SUPERBA_APP_DIR, the e2e relay, SUPERBA_TEST_PIN_PACKAGE"]
async fn pinned_store_codex_runs_the_app_server_and_an_inherited_value_never_pins() {
    let (Some(app), Ok(upstream), Ok(key), Ok(package), Ok(version)) = (
        std::env::var_os("SUPERBA_APP_DIR").map(PathBuf::from),
        std::env::var("SUPERBA_E2E_UPSTREAM"),
        std::env::var("SUPERBA_E2E_KEY"),
        std::env::var("SUPERBA_TEST_PIN_PACKAGE"),
        std::env::var("SUPERBA_TEST_PIN_VERSION"),
    ) else {
        eprintln!("pin environment not set; skipping");
        return;
    };
    let display = std::env::var("SUPERBA_E2E_DISPLAY").unwrap_or_else(|_| ":175".into());
    let _x = ensure_display(&display);
    let s = setup(&app, &upstream, &key, &display);

    // A store entry (<data>/codex/<ver>-<target>/) from the package.
    let target = superba_core::local_codex_package_target().unwrap();
    let entry = s.root.path().join(format!("data/codex/{version}-{target}"));
    std::fs::create_dir_all(&entry).unwrap();
    let st = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(&package)
        .arg("-C")
        .arg(&entry)
        .status()
        .unwrap();
    assert!(st.success());
    let pinned = std::fs::canonicalize(entry.join("bin/codex")).unwrap();

    let mut env = BTreeMap::new();
    env.insert("DISPLAY".to_owned(), s.display.clone());
    env.insert(
        ENV_CODEX_CLI_PATH.to_owned(),
        pinned.to_string_lossy().into_owned(),
    );
    let c = &s.controller;
    let session = c
        .launch(&LaunchOptions {
            env,
            home_override: Some(s.home.clone()),
            ready_timeout_ms: 60_000,
            ..Default::default()
        })
        .await
        .unwrap();
    let port = session.cdp.as_ref().expect("cdp").port;
    let result = std::panic::AssertUnwindSafe(async {
        let exe = eventually("the app-server", Duration::from_secs(60), || async {
            c.app_server_exe(&session).await.ok().flatten()
        })
        .await;
        eprintln!("pinned launch: app-server exe {}", exe.display());
        assert_eq!(std::fs::canonicalize(&exe).unwrap(), pinned);
        eventually("home screen", Duration::from_secs(90), || async {
            let st = c.renderer_state(&session).await.ok()?;
            if st.on_home {
                return Some(());
            }
            eval(port, SKIP_ONBOARDING_JS).await;
            None
        })
        .await;
        assert_eq!(
            eval(port, &send_prompt_js("Reply with the single word OK.")).await,
            serde_json::Value::Bool(true)
        );
        let cli = eventually("a rollout", Duration::from_secs(60), || async {
            newest_rollout_cli_version(&s.codex.sessions_dir())
        })
        .await;
        eprintln!("pinned launch: rollout cli_version {cli}");
        assert_eq!(cli, version);
    })
    .catch_unwind()
    .await;
    quit_and_wait(&s, &session).await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }

    // Unpinned launch: CODEX_CLI_PATH may be set in this (the launcher's)
    // environment; the app must still run its bundled Codex.
    let inherited = std::env::var_os(ENV_CODEX_CLI_PATH);
    eprintln!(
        "unpinned launch: inherited CODEX_CLI_PATH set = {}",
        inherited.is_some()
    );
    let mut env = BTreeMap::new();
    env.insert("DISPLAY".to_owned(), s.display.clone());
    let session = c
        .launch(&LaunchOptions {
            env,
            home_override: Some(s.home.clone()),
            ready_timeout_ms: 60_000,
            ..Default::default()
        })
        .await
        .unwrap();
    let result = std::panic::AssertUnwindSafe(async {
        let exe = eventually("the app-server", Duration::from_secs(60), || async {
            c.app_server_exe(&session).await.ok().flatten()
        })
        .await;
        eprintln!("unpinned launch: app-server exe {}", exe.display());
        let bundled = std::fs::canonicalize(app.join("resources/codex")).unwrap();
        assert_eq!(std::fs::canonicalize(&exe).unwrap(), bundled);
        if let Some(seen) = session.app_server_exe.as_deref() {
            assert_eq!(std::fs::canonicalize(seen).unwrap(), bundled);
        }
    })
    .catch_unwind()
    .await;
    quit_and_wait(&s, &session).await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
