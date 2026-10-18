//! The desktop app's "Run Codex in WSL" mode (user bug 54 (b)): this computer's
//! Codex runs inside WSL and sees a Windows variable only when `WSLENV` names
//! it. While Superba persists `CODEX_AUTHAPI_BASE_URL` on Windows in that mode,
//! `WSLENV` names it too; the entry goes with the value, the user's own entries
//! stay, and a revert restores `WSLENV` exactly.

use std::fs;
use std::sync::Arc;

use pretty_assertions::assert_eq;
use superba_config::{EnvPlatform, MemoryNativeEnv, NativeEnv, ShellEnvPersister};
use superba_core::{DataDir, EnvLocation, EnvOp, EnvPersister, EnvPlan, LocalContext, MapEnv};
use tempfile::TempDir;

const VAR: &str = "CODEX_AUTHAPI_BASE_URL";
const VALUE: &str = "https://api.krill-code.net";
const WSLENV: &str = "WSLENV";

struct Env {
    _tmp: TempDir,
    ctx: LocalContext,
    native: Arc<MemoryNativeEnv>,
    persister: ShellEnvPersister,
}

fn env(platform: EnvPlatform, wslenv: Option<&str>) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = LocalContext::new(
        tmp.path().join("home"),
        DataDir::new(tmp.path().join("data")),
        Arc::new(MapEnv::new()),
    );
    fs::create_dir_all(ctx.codex_home().root()).unwrap();
    let mut native = MemoryNativeEnv::new();
    if let Some(w) = wslenv {
        native = native.with(&EnvLocation::WindowsUserEnv, WSLENV, w);
    }
    let native = Arc::new(native);
    let persister = ShellEnvPersister::with_platform(ctx.clone(), platform, native.clone());
    Env {
        _tmp: tmp,
        ctx,
        native,
        persister,
    }
}

impl Env {
    fn run_in_wsl(&self, config: bool) {
        let home = self.ctx.codex_home();
        if config {
            fs::write(
                home.config_toml(),
                "model = \"gpt-5\"\n\n[desktop]\nrunCodexInWindowsSubsystemForLinux = true\n",
            )
            .unwrap();
        } else {
            fs::write(
                home.global_state_json(),
                r#"{"runCodexInWindowsSubsystemForLinux": true}"#,
            )
            .unwrap();
        }
    }

    async fn plan(&self, op: EnvOp) -> EnvPlan {
        self.persister.plan(VAR, op).await.unwrap()
    }

    fn wslenv(&self) -> Option<String> {
        self.native
            .get(&EnvLocation::WindowsUserEnv, WSLENV)
            .unwrap()
    }

    fn value(&self) -> Option<String> {
        self.native.get(&EnvLocation::WindowsUserEnv, VAR).unwrap()
    }
}

fn set() -> EnvOp {
    EnvOp::Set {
        value: VALUE.into(),
        take_over_foreign: false,
    }
}

fn forwards(plan: &EnvPlan) -> bool {
    plan.edits
        .iter()
        .any(|e| e.location == EnvLocation::WslEnvForward)
}

#[tokio::test]
async fn without_the_app_setting_wslenv_is_left_alone() {
    let e = env(EnvPlatform::Windows, Some("USERPROFILE/p"));
    let plan = e.plan(set()).await;
    assert!(!forwards(&plan));
    e.persister.apply(&plan).await.unwrap();
    assert_eq!(e.wslenv().as_deref(), Some("USERPROFILE/p"));
    // Not on Linux / macOS either, whatever the Codex home says.
    let e = env(EnvPlatform::Linux, None);
    e.run_in_wsl(true);
    assert!(!forwards(&e.plan(set()).await));
}

#[tokio::test]
async fn run_codex_in_wsl_forwards_the_value_while_superba_sets_it() {
    for config in [true, false] {
        let e = env(EnvPlatform::Windows, Some("USERPROFILE/p"));
        e.run_in_wsl(config);
        let plan = e.plan(set()).await;
        assert!(forwards(&plan), "{config}: {:?}", plan.edits);
        assert!(
            plan.scope_notes
                .iter()
                .any(|n| n.key == "env.scope.windows_wsl")
        );
        e.persister.apply(&plan).await.unwrap();
        assert_eq!(e.value().as_deref(), Some(VALUE));
        assert_eq!(
            e.wslenv().as_deref(),
            Some("USERPROFILE/p:CODEX_AUTHAPI_BASE_URL")
        );
        // Applying again changes nothing.
        assert!(e.plan(set()).await.is_noop());
        // Clearing removes our entry, keeps the user's.
        let clear = e.plan(EnvOp::Clear).await;
        assert!(forwards(&clear));
        e.persister.apply(&clear).await.unwrap();
        assert_eq!(e.value(), None);
        assert_eq!(e.wslenv().as_deref(), Some("USERPROFILE/p"));
    }
}

#[tokio::test]
async fn a_revert_restores_wslenv_and_an_entry_of_the_user_stays() {
    // No WSLENV before: reverting deletes it again.
    let e = env(EnvPlatform::Windows, None);
    e.run_in_wsl(true);
    let applied = e.persister.apply(&e.plan(set()).await).await.unwrap();
    assert_eq!(e.wslenv().as_deref(), Some(VAR));
    e.persister.revert(&applied).await.unwrap();
    assert_eq!(e.wslenv(), None);
    assert_eq!(e.value(), None);

    // The user forwards it already: nothing to add, and a clear keeps it.
    let e = env(
        EnvPlatform::Windows,
        Some("CODEX_AUTHAPI_BASE_URL/u:PATH/l"),
    );
    e.run_in_wsl(true);
    let plan = e.plan(set()).await;
    assert!(!forwards(&plan));
    e.persister.apply(&plan).await.unwrap();
    e.persister
        .apply(&e.plan(EnvOp::Clear).await)
        .await
        .unwrap();
    assert_eq!(
        e.wslenv().as_deref(),
        Some("CODEX_AUTHAPI_BASE_URL/u:PATH/l")
    );
}

#[tokio::test]
async fn the_entry_goes_when_the_app_stops_running_codex_in_wsl() {
    let e = env(EnvPlatform::Windows, Some("USERPROFILE/p"));
    e.run_in_wsl(true);
    e.persister.apply(&e.plan(set()).await).await.unwrap();
    // The user turned the setting off in the app; the next switch keeps the
    // value but drops the forwarding Superba added.
    fs::write(e.ctx.codex_home().config_toml(), "model = \"gpt-5\"\n").unwrap();
    let plan = e.plan(set()).await;
    assert!(forwards(&plan));
    e.persister.apply(&plan).await.unwrap();
    assert_eq!(e.value().as_deref(), Some(VALUE));
    assert_eq!(e.wslenv().as_deref(), Some("USERPROFILE/p"));
}
