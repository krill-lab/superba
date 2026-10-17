//! v0.3 host operations without ssh: scoped plans (connection only / settings
//! only), probe additions, Codex install / rollback and the read-only session
//! scan. The exact remote scripts run under the local `/bin/sh` with an isolated
//! temporary `HOME` ([`LocalShell`]); nothing touches the real home.
#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use common::{FakeEngine, engine, host, token_profile};
use pretty_assertions::assert_eq;
use superba_core::{
    AuthMode, BlockReason, CodexInstallKind, CodexVersion, DataDir, Error, FileChange,
    RemoteApplyScope, RemoteHost, RemoteHosts, ResolvedProfile, SettingEntry, SettingValue,
};
use superba_remote::install::{InstallSpec, install_script, parse_outcome};
use superba_remote::records::Records;
use superba_remote::scope::{family_of, writes};
use superba_remote::script::new_nonce;
use superba_remote::ssh::RemoteShell;
use superba_remote::store::HostStore;
use superba_remote::{LocalShell, SshRemoteHosts};
use tempfile::TempDir;

const ORIGIN: &str = "https://api.krill-code.net";
const OTHER_ORIGIN: &str = "https://api-slb.krill-code.net";
const KEY: &str = "sk-test-0000000000000000wxyz";
const CHATGPT_AUTH: &str = "{\n  \"tokens\": {\"id_token\": \"x\", \"access_token\": \"y\"}\n}\n";
const CONFIG: &str = "model = \"gpt-5\"\n\n[mcp_servers.docs]\ncommand = \"npx\"\n";

struct Env {
    tmp: TempDir,
    home: PathBuf,
    data: DataDir,
    _engine: Arc<FakeEngine>,
    hosts: SshRemoteHosts,
    shell: Arc<LocalShell>,
}

fn write(path: &Path, contents: &[u8], mode: u32) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn setup() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    write(&home.join(".codex/config.toml"), CONFIG.as_bytes(), 0o644);
    write(
        &home.join(".codex/auth.json"),
        CHATGPT_AUTH.as_bytes(),
        0o600,
    );
    write(&home.join(".profile"), b"", 0o644);
    let data = DataDir::new(tmp.path().join("data"));
    let engine = engine();
    let shell = Arc::new(LocalShell::new(&home).with_login_shell("/bin/sh"));
    let hosts = SshRemoteHosts::new(engine.clone(), data.clone()).with_shell(shell.clone());
    Env {
        tmp,
        home,
        data,
        _engine: engine,
        hosts,
        shell,
    }
}

fn devbox() -> RemoteHost {
    host("devbox", Some("remote-ssh-discovered:devbox"))
}

fn with_settings(mut p: ResolvedProfile, settings: &[(&str, Option<&str>)]) -> ResolvedProfile {
    for (path, value) in settings {
        let entry = match value {
            Some(v) => SettingEntry::Set(SettingValue::String((*v).to_owned())),
            None => SettingEntry::Unset,
        };
        p.settings.insert(path, entry).unwrap();
    }
    p
}

async fn owned_paths(env: &Env) -> Vec<String> {
    HostStore::new(&env.data, &devbox().target_id)
        .load_state()
        .await
        .unwrap()
        .last_written
        .into_iter()
        .map(|k| k.path)
        .collect()
}

// ---------------------------------------------------------------------------
// key families
// ---------------------------------------------------------------------------

#[test]
fn reserved_paths_are_the_connection_family_everything_else_is_a_setting() {
    for p in [
        "model_provider",
        "openai_base_url",
        "chatgpt_base_url",
        "model",
        "model_reasoning_effort",
        "service_tier",
        "model_catalog_json",
        "model_providers.x.base_url",
    ] {
        assert_eq!(family_of(p), RemoteApplyScope::Connection, "{p}");
        assert!(writes(RemoteApplyScope::Connection, p));
        assert!(!writes(RemoteApplyScope::Settings, p));
        assert!(writes(RemoteApplyScope::Both, p));
    }
    for p in ["approval_policy", "features.memories", "tui.notifications"] {
        assert_eq!(family_of(p), RemoteApplyScope::Settings, "{p}");
        assert!(!writes(RemoteApplyScope::Connection, p));
        assert!(writes(RemoteApplyScope::Settings, p));
    }
}

// ---------------------------------------------------------------------------
// scoped plans
// ---------------------------------------------------------------------------

#[tokio::test]
async fn connection_scope_changes_routing_and_keeps_the_hosts_settings() {
    let env = setup();
    let h = devbox();
    // A 0.2-style switch wrote settings and the connection.
    let first = with_settings(
        token_profile(ORIGIN, AuthMode::ApiKey, KEY),
        &[
            ("approval_policy", Some("on-request")),
            ("web_search", Some("live")),
        ],
    );
    let plan = env.hosts.plan_apply(&h, &first).await.unwrap();
    env.hosts.apply(&h, &plan).await.unwrap();
    let cfg = read(&env.home.join(".codex/config.toml"));
    assert!(cfg.contains("approval_policy = \"on-request\""), "{cfg}");
    assert!(
        owned_paths(&env)
            .await
            .contains(&"approval_policy".to_owned())
    );

    // A following switch: another line, other settings, a model.
    let mut next = with_settings(
        token_profile(OTHER_ORIGIN, AuthMode::ApiKey, KEY),
        &[("approval_policy", Some("never"))],
    );
    next.model.model = Some("gpt-6-luna".into());
    let plan = env
        .hosts
        .plan_apply_scoped(&h, &next, RemoteApplyScope::Connection)
        .await
        .unwrap();
    assert!(plan.blocked.is_empty(), "{:?}", plan.blocked);
    let changed: Vec<&str> = plan.key_changes.iter().map(|k| k.path.as_str()).collect();
    assert!(changed.contains(&"openai_base_url"), "{changed:?}");
    assert!(changed.contains(&"model"), "{changed:?}");
    assert!(
        changed
            .iter()
            .all(|p| writes(RemoteApplyScope::Connection, p)),
        "{changed:?}"
    );
    assert!(
        plan.warnings
            .iter()
            .any(|w| w.text.key == "host_scope.connection")
    );
    // The scope survives into apply (which re-renders from the plan).
    env.hosts.apply(&h, &plan).await.unwrap();
    let cfg = read(&env.home.join(".codex/config.toml"));
    assert!(cfg.contains("api-slb.krill-code.net"), "{cfg}");
    assert!(cfg.contains("model = \"gpt-6-luna\""), "{cfg}");
    assert!(cfg.contains("approval_policy = \"on-request\""), "{cfg}");
    assert!(cfg.contains("web_search = \"live\""), "{cfg}");
    // Settings Superba wrote there stay owned (a later sync can remove them).
    let owned = owned_paths(&env).await;
    assert!(owned.contains(&"approval_policy".to_owned()), "{owned:?}");
    assert!(owned.contains(&"web_search".to_owned()), "{owned:?}");
}

#[tokio::test]
async fn settings_scope_writes_only_settings_and_never_the_login() {
    let env = setup();
    let h = devbox();
    let auth_before = read(&env.home.join(".codex/auth.json"));
    let mut desired = with_settings(
        token_profile(ORIGIN, AuthMode::Pat, KEY),
        &[
            ("approval_policy", Some("never")),
            ("web_search", Some("live")),
        ],
    );
    desired.model.model = Some("gpt-6-luna".into());
    let plan = env
        .hosts
        .plan_apply_scoped(&h, &desired, RemoteApplyScope::Settings)
        .await
        .unwrap();
    assert!(plan.blocked.is_empty(), "{:?}", plan.blocked);
    let mut changed: Vec<&str> = plan.key_changes.iter().map(|k| k.path.as_str()).collect();
    changed.sort_unstable();
    assert_eq!(changed, vec!["approval_policy", "web_search"]);
    assert!(plan.auth_change.is_none());
    assert!(plan.env.is_none(), "a settings sync never edits rc files");
    let auth = plan
        .files
        .iter()
        .find(|f| f.path.ends_with("auth.json"))
        .unwrap();
    assert_eq!(auth.change, FileChange::Unchanged);
    assert!(
        plan.warnings
            .iter()
            .any(|w| w.text.key == "host_scope.settings")
    );
    // Drift notes about the protected connection keys are not shown.
    assert!(
        !plan
            .warnings
            .iter()
            .any(|w| w.text.key == "config.note.drift_kept")
    );

    env.hosts.apply(&h, &plan).await.unwrap();
    let cfg = read(&env.home.join(".codex/config.toml"));
    assert!(cfg.contains("approval_policy = \"never\""), "{cfg}");
    assert!(!cfg.contains("model_provider"), "{cfg}");
    assert!(!cfg.contains("openai_base_url"), "{cfg}");
    assert!(cfg.contains("model = \"gpt-5\""), "{cfg}");
    assert_eq!(read(&env.home.join(".codex/auth.json")), auth_before);

    // A sync of one row leaves the other owned settings alone (unticked rows).
    let desired = with_settings(
        token_profile(ORIGIN, AuthMode::Pat, KEY),
        &[("approval_policy", Some("on-request"))],
    );
    let plan = env
        .hosts
        .plan_apply_scoped(&h, &desired, RemoteApplyScope::Settings)
        .await
        .unwrap();
    let changed: Vec<&str> = plan.key_changes.iter().map(|k| k.path.as_str()).collect();
    assert_eq!(changed, vec!["approval_policy"]);
    env.hosts.apply(&h, &plan).await.unwrap();
    let cfg = read(&env.home.join(".codex/config.toml"));
    assert!(cfg.contains("web_search = \"live\""), "{cfg}");

    // A row the local profile removes (`Unset`): the key goes away.
    let desired = with_settings(
        token_profile(ORIGIN, AuthMode::Pat, KEY),
        &[("web_search", None)],
    );
    let plan = env
        .hosts
        .plan_apply_scoped(&h, &desired, RemoteApplyScope::Settings)
        .await
        .unwrap();
    let changed: Vec<&str> = plan.key_changes.iter().map(|k| k.path.as_str()).collect();
    assert_eq!(changed, vec!["web_search"]);
    env.hosts.apply(&h, &plan).await.unwrap();
    let cfg = read(&env.home.join(".codex/config.toml"));
    assert!(!cfg.contains("web_search"), "{cfg}");
    assert_eq!(read(&env.home.join(".codex/auth.json")), auth_before);
}

// ---------------------------------------------------------------------------
// ownership record and the host's own values (review platform-security #3)
// ---------------------------------------------------------------------------

/// The host's recorded own value of `path` (`None` = nothing recorded).
async fn user_value(env: &Env, path: &str) -> Option<Option<String>> {
    HostStore::new(&env.data, &devbox().target_id)
        .load_state()
        .await
        .unwrap()
        .user_values
        .into_iter()
        .find(|k| k.path == path)
        .map(|k| k.value)
}

async fn apply_in(env: &Env, desired: &ResolvedProfile, scope: RemoteApplyScope) {
    let h = devbox();
    let plan = env
        .hosts
        .plan_apply_scoped(&h, desired, scope)
        .await
        .unwrap();
    assert!(plan.blocked.is_empty(), "{:?}", plan.blocked);
    env.hosts.apply(&h, &plan).await.unwrap();
}

#[tokio::test]
async fn a_released_model_returns_to_the_hosts_latest_own_value() {
    let env = setup();
    let config = env.home.join(".codex/config.toml");
    let mut with_model = token_profile(ORIGIN, AuthMode::ApiKey, KEY);
    with_model.model.model = Some("gpt-6-luna".into());

    // A followed switch to a profile with a default model takes `model` over and
    // records the host's own value.
    apply_in(&env, &with_model, RemoteApplyScope::Connection).await;
    assert!(read(&config).contains("model = \"gpt-6-luna\""));
    assert_eq!(user_value(&env, "model").await, Some(Some("gpt-5".into())));
    assert!(owned_paths(&env).await.contains(&"model".to_owned()));

    // The official login (no default model) releases it: the host's own value
    // comes back instead of the key being deleted, and the key is no longer
    // owned.
    apply_in(
        &env,
        &common::official_profile(),
        RemoteApplyScope::Connection,
    )
    .await;
    let cfg = read(&config);
    assert!(cfg.contains("model = \"gpt-5\""), "{cfg}");
    assert!(!cfg.contains("openai_base_url"), "{cfg}");
    let owned = owned_paths(&env).await;
    assert!(owned.is_empty(), "{owned:?}");

    // The app picks another model on the host; the next takeover records that
    // value, so the next release restores the user's latest choice.
    std::fs::write(&config, read(&config).replace("gpt-5", "gpt-6-sol")).unwrap();
    apply_in(&env, &with_model, RemoteApplyScope::Connection).await;
    assert_eq!(
        user_value(&env, "model").await,
        Some(Some("gpt-6-sol".into()))
    );
    apply_in(
        &env,
        &common::official_profile(),
        RemoteApplyScope::Connection,
    )
    .await;
    let cfg = read(&config);
    assert!(cfg.contains("model = \"gpt-6-sol\""), "{cfg}");
    // The captured baseline itself never changes.
    let baseline = HostStore::new(&env.data, &devbox().target_id)
        .load_baseline()
        .await
        .unwrap()
        .unwrap();
    assert!(baseline.keys.iter().all(|k| k.path != "model"));
}

#[tokio::test]
async fn a_settings_release_restores_the_hosts_pre_sync_value() {
    let env = setup();
    let config = env.home.join(".codex/config.toml");
    std::fs::write(
        &config,
        format!("approval_policy = \"on-request\"\n{CONFIG}"),
    )
    .unwrap();
    // A sync writes over the host's own approval_policy and adds a key the host
    // did not have.
    let synced = with_settings(
        token_profile(ORIGIN, AuthMode::ApiKey, KEY),
        &[
            ("approval_policy", Some("never")),
            ("hide_agent_reasoning", Some("true")),
        ],
    );
    apply_in(&env, &synced, RemoteApplyScope::Settings).await;
    let cfg = read(&config);
    assert!(cfg.contains("approval_policy = \"never\""), "{cfg}");
    assert_eq!(
        user_value(&env, "approval_policy").await,
        Some(Some("on-request".into()))
    );
    assert_eq!(user_value(&env, "hide_agent_reasoning").await, Some(None));
    let mut owned = owned_paths(&env).await;
    owned.sort();
    assert_eq!(owned, vec!["approval_policy", "hide_agent_reasoning"]);

    // A following connection switch keeps them (outside its view).
    apply_in(
        &env,
        &token_profile(OTHER_ORIGIN, AuthMode::ApiKey, KEY),
        RemoteApplyScope::Connection,
    )
    .await;
    assert!(
        owned_paths(&env)
            .await
            .contains(&"approval_policy".to_owned())
    );

    // Release one of them (a "remove" row): resolved, not listed. It returns to
    // the host's own value; the other synced key stays as written.
    let mut release = token_profile(ORIGIN, AuthMode::ApiKey, KEY);
    release.key_resolutions.insert(
        "approval_policy".into(),
        superba_core::KeyResolution::Overwrite,
    );
    let plan = env
        .hosts
        .plan_apply_scoped(&devbox(), &release, RemoteApplyScope::Settings)
        .await
        .unwrap();
    let changed: Vec<&str> = plan.key_changes.iter().map(|k| k.path.as_str()).collect();
    assert_eq!(changed, vec!["approval_policy"]);
    env.hosts.apply(&devbox(), &plan).await.unwrap();
    let cfg = read(&config);
    assert!(cfg.contains("approval_policy = \"on-request\""), "{cfg}");
    assert!(cfg.contains("hide_agent_reasoning = \"true\""), "{cfg}");
    let owned = owned_paths(&env).await;
    assert!(!owned.contains(&"approval_policy".to_owned()), "{owned:?}");
    assert!(
        owned.contains(&"hide_agent_reasoning".to_owned()),
        "{owned:?}"
    );
    assert!(owned.contains(&"openai_base_url".to_owned()), "{owned:?}");

    // A key the host did not have before the sync is removed on release.
    let mut release = token_profile(ORIGIN, AuthMode::ApiKey, KEY);
    release.key_resolutions.insert(
        "hide_agent_reasoning".into(),
        superba_core::KeyResolution::Overwrite,
    );
    apply_in(&env, &release, RemoteApplyScope::Settings).await;
    let cfg = read(&config);
    assert!(!cfg.contains("hide_agent_reasoning"), "{cfg}");
    assert!(
        cfg.contains("api-slb.krill-code.net"),
        "routing stays: {cfg}"
    );
}

#[tokio::test]
async fn settings_scope_needs_a_provider_profile() {
    let env = setup();
    let plan = env
        .hosts
        .plan_apply_scoped(
            &devbox(),
            &common::original_profile(),
            RemoteApplyScope::Settings,
        )
        .await
        .unwrap();
    assert!(matches!(
        plan.blocked.as_slice(),
        [BlockReason::UnsupportedTarget { .. }]
    ));
    assert!(matches!(
        env.hosts.apply(&devbox(), &plan).await,
        Err(Error::Blocked { .. })
    ));
}

#[tokio::test]
async fn an_unscoped_plan_ignores_a_stale_scope_marker() {
    let env = setup();
    let h = devbox();
    let desired = with_settings(
        token_profile(ORIGIN, AuthMode::ApiKey, KEY),
        &[("approval_policy", Some("never"))],
    );
    // plan_apply_scoped(Settings) leaves a marker in plan.desired; reusing that
    // desired state with plan_apply is a full (Both) plan again.
    let scoped = env
        .hosts
        .plan_apply_scoped(&h, &desired, RemoteApplyScope::Settings)
        .await
        .unwrap();
    let both = env
        .hosts
        .plan_apply(&h, scoped.desired.as_ref().unwrap())
        .await
        .unwrap();
    let changed: Vec<&str> = both.key_changes.iter().map(|k| k.path.as_str()).collect();
    assert!(changed.contains(&"openai_base_url"), "{changed:?}");
    assert!(changed.contains(&"approval_policy"), "{changed:?}");
}

// ---------------------------------------------------------------------------
// probe additions
// ---------------------------------------------------------------------------

fn fake_codex(version: &str) -> Vec<u8> {
    format!(
        "#!/bin/sh\ncase \"$1\" in --version) echo \"codex-cli {version}\" ;; *) exit 0 ;; esac\n"
    )
    .into_bytes()
}

#[tokio::test]
async fn probe_reports_architecture_free_space_and_install_kind() {
    let env = setup();
    let rel = env
        .home
        .join(".codex/packages/standalone/releases/0.162.0-x86_64-unknown-linux-musl");
    write(&rel.join("bin/codex"), &fake_codex("0.162.0"), 0o755);
    std::fs::create_dir_all(env.home.join(".local/bin")).unwrap();
    std::os::unix::fs::symlink(rel.join("bin/codex"), env.home.join(".local/bin/codex")).unwrap();
    let st = env.hosts.probe(&devbox()).await.unwrap();
    let uname_m = String::from_utf8(Command::new("uname").arg("-m").output().unwrap().stdout)
        .unwrap()
        .trim()
        .to_owned();
    assert_eq!(st.arch.as_deref(), Some(uname_m.as_str()));
    assert!(st.kernel.is_some());
    assert!(st.disk_free_kib.unwrap_or(0) > 0);
    assert_eq!(st.codex_version, Some(CodexVersion::new(0, 162, 0)));
    assert_eq!(st.codex_install, Some(CodexInstallKind::Standalone));
    let resolved = st.codex_resolved_path.unwrap();
    assert!(
        resolved.ends_with("releases/0.162.0-x86_64-unknown-linux-musl/bin/codex"),
        "{resolved}"
    );
}

#[test]
fn install_kinds_from_resolved_paths() {
    let cases = [
        (
            "/home/u/.codex/packages/standalone/releases/0.162.0-x/bin/codex",
            CodexInstallKind::Standalone,
        ),
        (
            "/usr/lib/node_modules/@openai/codex/node_modules/@openai/codex-linux-x64/vendor/x86_64-unknown-linux-musl/bin/codex",
            CodexInstallKind::Npm,
        ),
        (
            "/opt/homebrew/Caskroom/codex/0.162.0/codex",
            CodexInstallKind::Homebrew,
        ),
        (
            "/home/linuxbrew/.linuxbrew/Cellar/codex/0.1/bin/codex",
            CodexInstallKind::Homebrew,
        ),
        ("/usr/local/bin/codex", CodexInstallKind::Other),
        (
            "C:\\Users\\u\\AppData\\Roaming\\npm\\node_modules\\@openai\\codex\\bin\\codex.js",
            CodexInstallKind::Npm,
        ),
    ];
    for (path, kind) in cases {
        assert_eq!(CodexInstallKind::from_resolved_path(path), kind, "{path}");
    }
}

// ---------------------------------------------------------------------------
// Codex install / rollback
// ---------------------------------------------------------------------------

/// A fake Codex package (`bin/codex` at the root, or under
/// `package/vendor/<target>/` like the npm tarball) as `.tar.gz`.
fn package(dir: &Path, version: &str, npm: bool) -> PathBuf {
    let stage = dir.join(format!("stage-{version}-{npm}"));
    let root = if npm {
        stage.join("package/vendor/x86_64-unknown-linux-musl")
    } else {
        stage.clone()
    };
    write(&root.join("bin/codex"), &fake_codex(version), 0o755);
    write(&root.join("codex-package.json"), b"{}", 0o644);
    let out = dir.join(format!("codex-{version}-{npm}.tar.gz"));
    let st = Command::new("tar")
        .arg("-czf")
        .arg(&out)
        .arg("-C")
        .arg(&stage)
        .arg(".")
        .status()
        .unwrap();
    assert!(st.success());
    out
}

fn visible_version(home: &Path) -> String {
    let out = Command::new(home.join(".local/bin/codex"))
        .arg("--version")
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn local_target_supported() -> bool {
    superba_core::local_codex_package_target().is_some_and(|t| t.contains("linux"))
}

#[tokio::test]
async fn install_switch_back_and_rollback_in_the_standalone_layout() {
    if !local_target_supported() {
        return;
    }
    let env = setup();
    let h = devbox();
    // A codex the user put there by hand: kept for the very first rollback.
    write(
        &env.home.join(".local/bin/codex"),
        &fake_codex("0.150.1"),
        0o755,
    );
    let pkgs = env.tmp.path().join("pkgs");
    std::fs::create_dir_all(&pkgs).unwrap();
    let v162 = CodexVersion::new(0, 162, 0);
    let v140 = CodexVersion::new(0, 140, 0);

    let r = env
        .hosts
        .install_codex(&h, &package(&pkgs, "0.162.0", false), &v162)
        .await
        .unwrap();
    assert_eq!(r.version, v162);
    assert_eq!(r.previous, Some(CodexVersion::new(0, 150, 1)));
    assert!(
        r.executable.ends_with("/.local/bin/codex"),
        "{}",
        r.executable
    );
    assert_eq!(visible_version(&env.home), "codex-cli 0.162.0");
    let root = env.home.join(".codex/packages/standalone");
    assert!(
        std::fs::symlink_metadata(root.join("current"))
            .unwrap()
            .is_symlink()
    );
    assert!(!root.join("auto-update-version").exists());
    let st = env.hosts.probe(&h).await.unwrap();
    assert_eq!(st.codex_version, Some(v162.clone()));
    assert_eq!(st.codex_install, Some(CodexInstallKind::Standalone));

    // Another version from an npm-layout tarball, then back.
    let r = env
        .hosts
        .install_codex(&h, &package(&pkgs, "0.140.0", true), &v140)
        .await
        .unwrap();
    assert_eq!(r.previous, Some(v162.clone()));
    assert_eq!(visible_version(&env.home), "codex-cli 0.140.0");
    let r = env.hosts.rollback_codex(&h).await.unwrap();
    assert_eq!(r.version, v162);
    assert_eq!(visible_version(&env.home), "codex-cli 0.162.0");
    // A second rollback returns to the version switched away from.
    let r = env.hosts.rollback_codex(&h).await.unwrap();
    assert_eq!(r.version, v140);

    // Superba's own old releases beyond the current and the previous go away;
    // nothing is left in the staging area.
    env.hosts
        .install_codex(
            &h,
            &package(&pkgs, "0.163.0", false),
            &CodexVersion::new(0, 163, 0),
        )
        .await
        .unwrap();
    let mut names: Vec<String> = std::fs::read_dir(root.join("releases"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "0.140.0-x86_64-unknown-linux-musl".to_owned(),
            "0.163.0-x86_64-unknown-linux-musl".to_owned()
        ]
    );
}

#[tokio::test]
async fn first_rollback_restores_the_command_that_was_there() {
    if !local_target_supported() {
        return;
    }
    let env = setup();
    let h = devbox();
    write(
        &env.home.join(".local/bin/codex"),
        &fake_codex("0.150.1"),
        0o755,
    );
    let pkgs = env.tmp.path().join("pkgs");
    std::fs::create_dir_all(&pkgs).unwrap();
    env.hosts
        .install_codex(
            &h,
            &package(&pkgs, "0.162.0", false),
            &CodexVersion::new(0, 162, 0),
        )
        .await
        .unwrap();
    let r = env.hosts.rollback_codex(&h).await.unwrap();
    assert_eq!(r.version, CodexVersion::new(0, 150, 1));
    assert!(
        !std::fs::symlink_metadata(env.home.join(".local/bin/codex"))
            .unwrap()
            .is_symlink()
    );
}

#[tokio::test]
async fn rollback_without_anything_kept_is_not_found() {
    let env = setup();
    assert!(matches!(
        env.hosts.rollback_codex(&devbox()).await,
        Err(Error::NotFound { .. })
    ));
}

async fn run_install(env: &Env, pkg: &Path, spec: InstallSpec) -> superba_core::Result<()> {
    let nonce = new_nonce();
    let script = install_script(&nonce, &spec);
    let out = env
        .shell
        .run_with_payload(&devbox(), &script, pkg, std::time::Duration::from_secs(60))
        .await
        .unwrap();
    let recs = Records::parse(&out.stdout, &nonce).unwrap();
    parse_outcome(&recs, "devbox").map(|_| ())
}

fn spec_for(env: &Env, pkg: &Path, version: &str) -> InstallSpec {
    let bytes = std::fs::read(pkg).unwrap();
    use sha2::Digest;
    InstallSpec {
        codex_home: format!("{}/.codex", env.home.display()),
        bin_dir: format!("{}/.local/bin", env.home.display()),
        version: CodexVersion::parse(version).unwrap(),
        target: "x86_64-unknown-linux-musl".into(),
        sha256: hex::encode(sha2::Sha256::digest(&bytes)),
        size: bytes.len() as u64,
    }
}

#[tokio::test]
async fn the_host_rejects_bad_packages_and_changes_nothing() {
    let env = setup();
    let pkgs = env.tmp.path().join("pkgs");
    std::fs::create_dir_all(&pkgs).unwrap();
    let pkg = package(&pkgs, "0.162.0", false);

    let mut bad_sha = spec_for(&env, &pkg, "0.162.0");
    bad_sha.sha256 = "0".repeat(64);
    let err = run_install(&env, &pkg, bad_sha).await.unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err:?}");

    let mut bad_size = spec_for(&env, &pkg, "0.162.0");
    bad_size.size += 1;
    let err = run_install(&env, &pkg, bad_size).await.unwrap_err();
    assert!(matches!(err, Error::Invalid { .. }), "{err:?}");

    // The archive is fine but holds another version.
    let wrong = spec_for(&env, &pkg, "0.163.0");
    let err = run_install(&env, &pkg, wrong).await.unwrap_err();
    assert!(err.to_string().contains("codex-cli 0.162.0"), "{err}");

    // Not a gzip archive at all.
    let junk = pkgs.join("junk.tar.gz");
    std::fs::write(&junk, b"<html>not a package</html>").unwrap();
    let err = run_install(&env, &junk, spec_for(&env, &junk, "0.162.0"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("extracted"), "{err}");

    let root = env.home.join(".codex/packages/standalone");
    assert!(!root.join("current").exists());
    assert!(!env.home.join(".local/bin/codex").exists());
    let leftovers: Vec<_> = std::fs::read_dir(root.join("releases"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

// ---------------------------------------------------------------------------
// session scan
// ---------------------------------------------------------------------------

fn meta(id: &str, provider: Option<&str>) -> String {
    let p = provider.map_or_else(String::new, |p| format!(",\"model_provider\":\"{p}\""));
    format!(
        "{{\"timestamp\":\"2026-10-16T07:35:26.660Z\",\"type\":\"session_meta\",\"payload\":{{\"session_id\":\"s-{id}\",\"id\":\"{id}\",\"cwd\":\"/x\"{p},\"base_instructions\":{{\"text\":\"say \\\"model_provider\\\":\\\"evil\\\"\"}}}}}}\n{{\"type\":\"turn_context\"}}\n"
    )
}

#[tokio::test]
async fn scan_counts_sessions_per_provider_read_only() {
    let env = setup();
    let s = env.home.join(".codex/sessions/2026/10/16");
    let a = env.home.join(".codex/archived_sessions");
    write(
        &s.join("rollout-2026-10-16T07-35-26-t1.jsonl"),
        meta("t1", Some("openai")).as_bytes(),
        0o600,
    );
    write(
        &s.join("rollout-2026-10-16T07-35-27-t2.jsonl"),
        meta("t2", None).as_bytes(),
        0o600,
    );
    write(
        &s.join("rollout-2026-10-16T07-35-28-t3.jsonl"),
        meta("t3", Some("krill")).as_bytes(),
        0o600,
    );
    write(
        &a.join("rollout-2026-10-15T07-35-28-t4.jsonl"),
        meta("t4", Some("krill")).as_bytes(),
        0o600,
    );
    // The same thread archived again: counted once in `distinct`.
    write(
        &a.join("rollout-2026-10-15T07-35-29-t3.jsonl"),
        meta("t3", Some("krill")).as_bytes(),
        0o600,
    );
    write(
        &s.join("rollout-2026-10-16T08-00-00-t9.jsonl"),
        b"garbage\n",
        0o600,
    );
    write(&s.join("notes.txt"), b"not a rollout", 0o600);
    let zst = s.join("rollout-2026-10-16T09-00-00-t5.jsonl.zst");
    let zstd_ok = Command::new("zstd")
        .args(["-q", "-o"])
        .arg(&zst)
        .arg(s.join("rollout-2026-10-16T07-35-28-t3.jsonl"))
        .status()
        .is_ok_and(|st| st.success());
    let before = dir_listing(&env.home);

    let report = env.hosts.scan_sessions(&devbox()).await.unwrap();
    let openai = &report.by_provider["openai"];
    assert_eq!(
        (openai.rollouts, openai.archived, openai.distinct),
        (2, 0, 2)
    );
    let krill = &report.by_provider["krill"];
    if zstd_ok {
        assert_eq!(
            (krill.rollouts, krill.archived, krill.compressed),
            (2, 2, 1)
        );
    } else {
        assert_eq!((krill.rollouts, krill.archived), (1, 2));
    }
    assert_eq!(krill.distinct, 2);
    assert!(!report.by_provider.contains_key("evil"));
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.text.key == "host_sessions.no_meta")
    );
    assert_eq!(report.codex_home, env.home.join(".codex"));
    // Read-only.
    assert_eq!(dir_listing(&env.home), before);
}

#[tokio::test]
async fn scan_of_a_home_without_sessions_is_empty() {
    let env = setup();
    let report = env.hosts.scan_sessions(&devbox()).await.unwrap();
    assert!(report.by_provider.is_empty());
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
}

fn dir_listing(root: &Path) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let m = e.metadata().unwrap();
            if m.is_dir() {
                stack.push(e.path());
            } else {
                out.push((e.path().display().to_string(), m.len()));
            }
        }
    }
    out.sort();
    out
}
