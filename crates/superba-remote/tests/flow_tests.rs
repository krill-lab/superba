//! End-to-end flows of `SshRemoteHosts` without ssh: the exact remote scripts run
//! under the local `/bin/sh` with an isolated temporary `HOME` ([`LocalShell`]).
//! Nothing here reads or writes the real home directory.
//!
//! Unix only: the scripts are POSIX shell and run under the local `/bin/sh`.
#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use common::{FakeEngine, engine, host, original_profile, token_profile};
use pretty_assertions::assert_eq;
use superba_core::codex::ENV_AUTHAPI_BASE_URL;
use superba_core::env::{LEGACY_COMMENT_PREFIX, MARKER_BEGIN};
use superba_core::{
    AuthMode, AuthShape, BlockReason, CodexHome, ConfigEngine, DataDir, EnvObservation, Error,
    FileChange, HostSource, RemoteHost, RemoteHosts, StashScope,
};
use superba_remote::script::{Expect, FileOp, Mode, WriteSpec, new_nonce, write_script};
use superba_remote::ssh::RemoteShell;
use superba_remote::store::HostStore;
use superba_remote::{LocalShell, SshRemoteHosts};
use tempfile::TempDir;

const ORIGIN: &str = "https://api.krill-code.net";
const KEY: &str = "sk-test-0000000000000000wxyz";
const CHATGPT_AUTH: &str = "{\n  \"tokens\": {\"id_token\": \"x\", \"access_token\": \"y\"},\n  \"last_refresh\": \"2026-10-01T00:00:00Z\"\n}\n";
const CONFIG: &str = "model = \"gpt-5\"\n\n[mcp_servers.docs]\ncommand = \"npx\"\n";
/// The host's `~/.profile`. Its first line puts this machine's programs without
/// any `codex` on `PATH` ([`programs_without_codex`], `$HOME/../bin`): a login
/// shell reads the system profile first, which sets `PATH` itself (Debian's
/// `/etc/profile`, macOS's `path_helper`) and may list a Codex installed on the
/// machine running the tests. A Codex is then found only where a test puts one
/// (`~/.local/bin`, which the scripts put first).
const PROFILE: &str = "PATH=\"$HOME/../bin\"; export PATH\n\
    if [ -n \"$BASH_VERSION\" ] && [ -f \"$HOME/.bashrc\" ]; then . \"$HOME/.bashrc\"; fi\n";

fn legacy_line() -> String {
    format!("export {ENV_AUTHAPI_BASE_URL}=\"https://api-slb.krill-code.net\"")
}

fn bashrc() -> String {
    format!(
        "case $- in *i*) ;; *) return;; esac\nalias ll='ls -l'\n{}\n",
        legacy_line()
    )
}

struct Env {
    _tmp: TempDir,
    home: PathBuf,
    data: DataDir,
    engine: Arc<FakeEngine>,
    hosts: SshRemoteHosts,
}

fn write(path: &Path, contents: &str, mode: u32) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// `dir` with a link to every program of the scripts' `PATH` (`/usr/local/bin`,
/// `/usr/bin`, `/bin`; the first of a name wins) except `codex`.
fn programs_without_codex(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    for src in ["/usr/local/bin", "/usr/bin", "/bin"] {
        let Ok(entries) = std::fs::read_dir(src) else {
            continue;
        };
        for e in entries.flatten() {
            let link = dir.join(e.file_name());
            if e.file_name() != "codex" && link.symlink_metadata().is_err() {
                std::os::unix::fs::symlink(e.path(), &link).unwrap();
            }
        }
    }
}

/// A host home with a ChatGPT login, a config with an MCP server and a bash setup
/// carrying a legacy tutorial line, and no Codex on its login shell's `PATH`.
/// `shell = None` = no login shell.
fn setup(shell: Option<&str>) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    programs_without_codex(&tmp.path().join("bin"));
    write(&home.join(".codex/config.toml"), CONFIG, 0o644);
    write(&home.join(".codex/auth.json"), CHATGPT_AUTH, 0o600);
    write(&home.join(".bashrc"), &bashrc(), 0o644);
    write(&home.join(".profile"), PROFILE, 0o644);
    let data = DataDir::new(tmp.path().join("data"));
    let engine = engine();
    // `None`: a SHELL that is not executable, so the login shell cannot run (with
    // SHELL unset the scripts would fall back to this machine's passwd entry).
    let ls = LocalShell::new(&home).with_login_shell(shell.unwrap_or("/nonexistent/shell"));
    let hosts = SshRemoteHosts::new(engine.clone(), data.clone()).with_shell(Arc::new(ls));
    Env {
        _tmp: tmp,
        home,
        data,
        engine,
        hosts,
    }
}

fn devbox() -> RemoteHost {
    host("devbox", Some("remote-ssh-discovered:devbox"))
}

#[tokio::test]
async fn probe_without_a_login_shell_falls_back_to_the_plain_shell() {
    let env = setup(None);
    let st = env.hosts.probe(&devbox()).await.unwrap();
    // The "remote" shell runs on this machine, so it reports the host's OS.
    let expected_os = if cfg!(target_os = "macos") {
        superba_core::RemoteOs::Darwin
    } else {
        superba_core::RemoteOs::Linux
    };
    assert_eq!(st.os, expected_os);
    assert_eq!(st.codex_home, format!("{}/.codex", env.home.display()));
    assert_eq!(
        st.auth,
        AuthShape::ChatGpt {
            email: None,
            account_id: None
        }
    );
    assert!(st.config_sha256.is_some());
    assert_eq!(st.authapi_env, EnvObservation::Absent);
    assert_eq!(st.codex_version, None);
    // The host's paths are remembered for turn checks.
    let saved = HostStore::new(&env.data, &devbox().target_id)
        .load_state()
        .await
        .unwrap();
    assert_eq!(saved.codex_home.as_deref(), Some(st.codex_home.as_str()));
}

#[tokio::test]
async fn login_shell_view_sets_codex_home_env_and_codex_version() {
    let env = setup(Some("/bin/bash"));
    // A fake codex on ~/.local/bin (the app prepends it to PATH).
    write(
        &env.home.join(".local/bin/codex"),
        "#!/bin/sh\necho 'codex-cli 0.162.0'\n",
        0o755,
    );
    // CODEX_HOME exported only for interactive login shells (like many users do).
    std::fs::rename(env.home.join(".codex"), env.home.join("codexhome")).unwrap();
    let rc = format!(
        "{}export CODEX_HOME=\"$HOME/codexhome\"\n",
        bashrc().replace(
            &legacy_line(),
            "export CODEX_AUTHAPI_BASE_URL=https://example.org"
        )
    );
    write(&env.home.join(".bashrc"), &rc, 0o644);
    let st = env.hosts.probe(&devbox()).await.unwrap();
    assert_eq!(st.shell, superba_core::ShellKind::Bash);
    assert_eq!(st.codex_home, format!("{}/codexhome", env.home.display()));
    assert_eq!(
        st.authapi_env,
        EnvObservation::Present("https://example.org".into())
    );
    assert_eq!(
        st.codex_version.map(|v| v.to_string()).as_deref(),
        Some("0.162.0")
    );
    assert!(st.codex_path.unwrap().ends_with("/.local/bin/codex"));
    assert_eq!(
        st.auth,
        AuthShape::ChatGpt {
            email: None,
            account_id: None
        }
    );
}

#[tokio::test]
async fn pat_apply_writes_files_env_marker_and_backups_then_rolls_back() {
    let env = setup(Some("/bin/bash"));
    let h = devbox();
    let desired = token_profile(ORIGIN, AuthMode::Pat, KEY);
    let plan = env.hosts.plan_apply(&h, &desired).await.unwrap();
    assert!(plan.blocked.is_empty(), "{:?}", plan.blocked);
    assert!(
        plan.files
            .iter()
            .all(|f| matches!(f.change, FileChange::Write { .. }))
    );
    assert!(plan.auth_change.as_ref().unwrap().stash_login);
    let env_plan = plan.env.as_ref().expect("PAT writes the env");
    assert!(!env_plan.edits.is_empty());
    let keys: Vec<&str> = plan.warnings.iter().map(|w| w.text.key.as_str()).collect();
    assert!(keys.contains(&"remote.warn.pkill"), "{keys:?}");
    // No Codex on the host's login PATH (see PROFILE).
    assert!(keys.contains(&"remote.warn.codex_missing"), "{keys:?}");
    // Plans serialize without contents or keys.
    let json = serde_json::to_string(&plan).unwrap();
    assert!(!json.contains(KEY) && !json.contains("access_token"));

    // Nothing written by planning.
    assert_eq!(read(&env.home.join(".codex/auth.json")), CHATGPT_AUTH);

    let report = env.hosts.apply(&h, &plan).await.unwrap();
    let codex = env.home.join(".codex");
    let auth = read(&codex.join("auth.json"));
    assert!(auth.contains(KEY) && auth.contains("personal_access_token"));
    let mode = std::fs::metadata(codex.join("auth.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let cfg = read(&codex.join("config.toml"));
    assert!(cfg.contains("openai_base_url = \"https://api.krill-code.net/backend-api/codex\""));
    assert!(cfg.contains("[mcp_servers.docs]"), "other config is kept");
    assert_eq!(
        std::fs::metadata(codex.join("config.toml"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o644,
        "mode preserved"
    );
    let rc = read(&env.home.join(".bashrc"));
    assert!(rc.starts_with(MARKER_BEGIN), "{rc}");
    assert!(rc.contains(&format!("{LEGACY_COMMENT_PREFIX}{}", legacy_line())));
    assert!(read(&env.home.join(".profile")).contains(MARKER_BEGIN));
    // Marker, first-write pre-images and backups on the host.
    let marker = read(&codex.join(".superba/remote-state.json"));
    assert!(marker.contains("\"tool\": \"superba\""));
    assert_eq!(
        read(&codex.join(".superba/first-write/auth.json")),
        CHATGPT_AUTH
    );
    for fw in &report.files {
        let b = fw.backup.as_ref().expect("backup of an existing file");
        assert!(Path::new(b).exists(), "{}", b.display());
    }
    assert_eq!(report.env.as_ref().unwrap().backups.len(), 2);
    // The ChatGPT login was stashed for this remote target.
    let stash = env
        .engine
        .load_login_stash(&StashScope::Remote(h.target_id.clone()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stash.expose(), CHATGPT_AUTH.as_bytes());

    // The app's login shell now sees the line's origin.
    let st = env.hosts.probe(&h).await.unwrap();
    assert_eq!(st.authapi_env, EnvObservation::Present(ORIGIN.into()));
    assert!(matches!(st.auth, AuthShape::Pat { .. }));
    assert_eq!(st.machine_id, st.marker_machine_id);
    assert!(!st.shared_home_suspected());
    // The settings engine sees the host's file and the keys Superba owns there.
    assert_eq!(
        st.config_toml.as_ref().map(|b| b.expose().to_vec()),
        Some(std::fs::read(codex.join("config.toml")).unwrap())
    );
    assert!(st.owned_paths.iter().any(|p| p == "openai_base_url"));
    assert!(
        !serde_json::to_string(&st).unwrap().contains("config_toml"),
        "file contents are never serialized"
    );

    // Re-planning the same profile is a no-op.
    let again = env.hosts.plan_apply(&h, &desired).await.unwrap();
    assert!(
        again
            .files
            .iter()
            .all(|f| f.change == FileChange::Unchanged)
    );
    assert!(again.env.is_none());
    let noop = env.hosts.apply(&h, &again).await.unwrap();
    assert!(noop.files.is_empty() && noop.env.is_none());

    let store = HostStore::new(&env.data, &h.target_id);
    assert_eq!(store.load_state().await.unwrap().generation, 1);
    assert!(!store.load_state().await.unwrap().last_written.is_empty());

    // Rollback restores every byte.
    env.hosts.rollback(&h, &report).await.unwrap();
    assert_eq!(read(&codex.join("auth.json")), CHATGPT_AUTH);
    assert_eq!(read(&codex.join("config.toml")), CONFIG);
    assert_eq!(read(&env.home.join(".bashrc")), bashrc());
    assert_eq!(read(&env.home.join(".profile")), PROFILE);
    let st = store.load_state().await.unwrap();
    assert_eq!(st.generation, 0);
    assert!(st.last_written.is_empty());
    // A second rollback finds everything as before and does nothing.
    env.hosts.rollback(&h, &report).await.unwrap();
}

#[tokio::test]
async fn a_codex_on_the_login_path_is_not_reported_missing() {
    let env = setup(Some("/bin/bash"));
    let h = devbox();
    let desired = token_profile(ORIGIN, AuthMode::Pat, KEY);
    let keys = |plan: &superba_core::RemotePlan| -> Vec<String> {
        plan.warnings.iter().map(|w| w.text.key.clone()).collect()
    };
    let plan = env.hosts.plan_apply(&h, &desired).await.unwrap();
    assert!(keys(&plan).contains(&"remote.warn.codex_missing".to_owned()));
    // A Codex in `~/.local/bin` (the app's PATH prefix) is found, with its version.
    write(
        &env.home.join(".local/bin/codex"),
        "#!/bin/sh\necho 'codex-cli 0.162.0'\n",
        0o755,
    );
    let plan = env.hosts.plan_apply(&h, &desired).await.unwrap();
    let keys = keys(&plan);
    assert!(
        !keys
            .iter()
            .any(|k| k == "remote.warn.codex_missing" || k == "remote.warn.version_unknown"),
        "{keys:?}"
    );
}

#[tokio::test]
async fn restore_original_uses_first_write_images_and_restores_rc_files() {
    let env = setup(Some("/bin/bash"));
    let h = devbox();
    let plan = env
        .hosts
        .plan_apply(&h, &token_profile(ORIGIN, AuthMode::Pat, KEY))
        .await
        .unwrap();
    env.hosts.apply(&h, &plan).await.unwrap();
    // Switch again (API key) so the first-write images differ from the backups.
    let plan = env
        .hosts
        .plan_apply(&h, &token_profile(ORIGIN, AuthMode::ApiKey, KEY))
        .await
        .unwrap();
    assert!(plan.env.is_none(), "API key mode leaves the env alone");
    env.hosts.apply(&h, &plan).await.unwrap();

    let plan = env.hosts.plan_apply(&h, &original_profile()).await.unwrap();
    assert!(plan.blocked.is_empty(), "{:?}", plan.blocked);
    env.hosts.apply(&h, &plan).await.unwrap();
    let codex = env.home.join(".codex");
    assert_eq!(read(&codex.join("auth.json")), CHATGPT_AUTH);
    assert_eq!(read(&codex.join("config.toml")), CONFIG);
    assert_eq!(read(&env.home.join(".bashrc")), bashrc());
    assert_eq!(read(&env.home.join(".profile")), PROFILE);
}

#[tokio::test]
async fn restore_without_a_baseline_is_blocked() {
    let env = setup(None);
    let plan = env
        .hosts
        .plan_apply(&devbox(), &original_profile())
        .await
        .unwrap();
    assert_eq!(plan.blocked, vec![BlockReason::NoBaseline]);
    assert!(matches!(
        env.hosts.apply(&devbox(), &plan).await,
        Err(Error::Blocked { .. })
    ));
}

#[tokio::test]
async fn unparseable_config_is_blocked_and_never_written() {
    let env = setup(None);
    let broken = "x = <<broken\n";
    write(&env.home.join(".codex/config.toml"), broken, 0o644);
    let plan = env
        .hosts
        .plan_apply(&devbox(), &token_profile(ORIGIN, AuthMode::ApiKey, KEY))
        .await
        .unwrap();
    assert!(matches!(
        plan.blocked.as_slice(),
        [BlockReason::Unparseable { .. }]
    ));
    assert!(env.hosts.apply(&devbox(), &plan).await.is_err());
    assert_eq!(read(&env.home.join(".codex/config.toml")), broken);
}

#[tokio::test]
async fn apply_rerenders_when_files_changed_after_planning() {
    let env = setup(None);
    let h = host("devbox", None);
    let plan = env
        .hosts
        .plan_apply(&h, &token_profile(ORIGIN, AuthMode::ApiKey, KEY))
        .await
        .unwrap();
    let keys: Vec<&str> = plan.warnings.iter().map(|w| w.text.key.as_str()).collect();
    assert!(keys.contains(&"remote.warn.no_host_id"), "{keys:?}");
    assert!(keys.contains(&"remote.warn.login_shell"), "{keys:?}");
    // The user edits config.toml between plan and apply.
    let edited = format!("{CONFIG}\n[profiles.x]\nmodel = \"o3\"\n");
    write(&env.home.join(".codex/config.toml"), &edited, 0o644);
    env.hosts.apply(&h, &plan).await.unwrap();
    let cfg = read(&env.home.join(".codex/config.toml"));
    assert!(
        cfg.contains("[profiles.x]") && cfg.contains("openai_base_url"),
        "{cfg}"
    );
    // API key mode: no rc edits.
    assert_eq!(read(&env.home.join(".bashrc")), bashrc());
}

#[tokio::test]
async fn rollback_leaves_files_changed_by_the_user_alone() {
    let env = setup(None);
    let h = devbox();
    let plan = env
        .hosts
        .plan_apply(&h, &token_profile(ORIGIN, AuthMode::ApiKey, KEY))
        .await
        .unwrap();
    let report = env.hosts.apply(&h, &plan).await.unwrap();
    let user = "model = \"mine\"\n";
    write(&env.home.join(".codex/config.toml"), user, 0o644);
    let err = env.hosts.rollback(&h, &report).await.unwrap_err();
    assert!(matches!(err, Error::Conflict { .. }), "{err:?}");
    assert_eq!(read(&env.home.join(".codex/config.toml")), user);
    assert_eq!(
        read(&env.home.join(".codex/auth.json")),
        CHATGPT_AUTH,
        "auth.json restored"
    );
}

#[tokio::test]
async fn write_script_is_compare_and_swap_and_all_or_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    write(&home.join("a"), "A0", 0o644);
    write(&home.join("b"), "B0", 0o644);
    let shell = LocalShell::new(&home);
    let p = |n: &str| home.join(n).to_string_lossy().into_owned();
    let spec = |expect_b: &str| WriteSpec {
        codex_home: p("codex"),
        backup_dir: p("bk"),
        first_write: false,
        ops: vec![
            FileOp {
                path: p("a"),
                expect: Expect::Bytes(b"A0".to_vec()),
                new: Some(b"A1".to_vec()),
                mode: Mode::Preserve,
                backup: true,
            },
            FileOp {
                path: p("b"),
                expect: Expect::Bytes(expect_b.as_bytes().to_vec()),
                new: None,
                mode: Mode::Preserve,
                backup: true,
            },
            FileOp {
                path: p("new/c"),
                expect: Expect::Absent,
                new: Some(b"C1".to_vec()),
                mode: Mode::Private,
                backup: false,
            },
        ],
        marker: None,
    };
    let run = |spec: WriteSpec| {
        let shell = shell.clone();
        async move {
            let n = new_nonce();
            let out = shell
                .run(&devbox(), &write_script(&n, &spec), Duration::from_secs(20))
                .await
                .unwrap();
            superba_remote::records::Records::parse(&out.stdout, &n).unwrap()
        }
    };
    // b changed: nothing is written.
    let r = run(spec("stale")).await;
    assert_eq!(r.get("result"), Some("conflict"));
    assert_eq!(r.all("conflict"), vec![p("b")]);
    assert_eq!(read(&home.join("a")), "A0");
    assert!(home.join("b").exists() && !home.join("new/c").exists());
    // Correct expectations: everything is written.
    let r = run(spec("B0")).await;
    assert_eq!(r.get("result"), Some("ok"), "{:?}", r.all("error"));
    assert_eq!(read(&home.join("a")), "A1");
    assert!(!home.join("b").exists());
    assert_eq!(read(&home.join("new/c")), "C1");
    assert_eq!(
        std::fs::metadata(home.join("new/c"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        read(
            &home
                .join("bk/")
                .join(superba_remote::script::backup_name(&p("a")))
        ),
        "A0"
    );
    // No temp files left behind.
    let leftovers: Vec<_> = std::fs::read_dir(&home)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("superba-tmp"))
        .collect();
    assert!(leftovers.is_empty());
}

#[tokio::test]
async fn symlinked_rc_files_are_written_through_the_link() {
    let env = setup(Some("/bin/bash"));
    let real = env.home.join("dotfiles/bashrc");
    write(&real, &bashrc(), 0o644);
    std::fs::remove_file(env.home.join(".bashrc")).unwrap();
    std::os::unix::fs::symlink(&real, env.home.join(".bashrc")).unwrap();
    let h = devbox();
    let plan = env
        .hosts
        .plan_apply(&h, &token_profile(ORIGIN, AuthMode::Pat, KEY))
        .await
        .unwrap();
    let report = env.hosts.apply(&h, &plan).await.unwrap();
    assert!(
        std::fs::symlink_metadata(env.home.join(".bashrc"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(read(&real).starts_with(MARKER_BEGIN));
    env.hosts.rollback(&h, &report).await.unwrap();
    assert!(
        std::fs::symlink_metadata(env.home.join(".bashrc"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(read(&real), bashrc());
}

#[tokio::test]
async fn shared_home_is_detected_from_a_foreign_marker() {
    let env = setup(None);
    let mid = std::fs::read_to_string("/etc/machine-id").unwrap_or_default();
    if mid.trim().is_empty() {
        return; // no machine id on this system
    }
    write(
        &env.home.join(".codex/.superba/remote-state.json"),
        "{\"schema\":1,\"tool\":\"superba\",\"machine_id\":\"another-machine\"}",
        0o600,
    );
    let plan = env
        .hosts
        .plan_apply(&devbox(), &token_profile(ORIGIN, AuthMode::ApiKey, KEY))
        .await
        .unwrap();
    assert!(plan.state.shared_home_suspected());
    assert!(
        plan.warnings
            .iter()
            .any(|w| w.text.key == "remote.warn.shared_home")
    );
}

fn set_age(path: &Path, secs: u64) {
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(secs))
        .unwrap();
}

#[tokio::test]
async fn turn_activity_reads_recent_rollouts() {
    let env = setup(None);
    let dir = env.home.join(".codex/sessions/2026/10/16");
    let id = "0199e0f2-7c1a-7b3e-9d2f-3a4b5c6d7e8f";
    let busy = dir.join(format!("rollout-2026-10-16T10-00-00-{id}.jsonl"));
    write(
        &busy,
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\"}}\n{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n{\"type\":\"response_item\"}\n",
        0o600,
    );
    set_age(&busy, 600);
    let done = dir.join("rollout-2026-10-16T09-00-00-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee.jsonl");
    write(
        &done,
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\"}}\n",
        0o600,
    );
    let quiet = dir.join("rollout-2026-10-16T08-00-00-ffffffff-bbbb-cccc-dddd-eeeeeeeeeeee.jsonl");
    write(&quiet, "{\"type\":\"session_meta\"}\n", 0o600);
    let old = dir.join("rollout-2026-10-15T08-00-00-11111111-bbbb-cccc-dddd-eeeeeeeeeeee.jsonl");
    write(&old, "{\"type\":\"session_meta\"}\n", 0o600);
    set_age(&old, 3600);

    let act = env.hosts.turn_activity(&devbox()).await.unwrap();
    assert!(act.complete);
    let got: Vec<(Option<&str>, superba_core::ActivitySignal)> = {
        let mut v: Vec<_> = act
            .busy
            .iter()
            .map(|t| (t.thread_id.as_deref(), t.signal))
            .collect();
        v.sort_by_key(|x| x.0.map(str::to_owned));
        v
    };
    assert_eq!(
        got,
        vec![
            (Some(id), superba_core::ActivitySignal::RolloutTail),
            (
                Some("ffffffff-bbbb-cccc-dddd-eeeeeeeeeeee"),
                superba_core::ActivitySignal::RolloutMtime
            ),
        ]
    );
}

#[tokio::test]
async fn turn_activity_times_out_as_incomplete() {
    #[derive(Debug)]
    struct Slow;
    #[async_trait::async_trait]
    impl RemoteShell for Slow {
        async fn run(
            &self,
            _h: &RemoteHost,
            _s: &str,
            timeout: Duration,
        ) -> superba_core::Result<superba_remote::ScriptOutput> {
            Err(Error::Timeout {
                what: format!("{timeout:?}"),
            })
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let hosts = SshRemoteHosts::new(engine(), DataDir::new(tmp.path())).with_shell(Arc::new(Slow));
    let act = hosts.turn_activity(&devbox()).await.unwrap();
    assert!(!act.complete && act.busy.is_empty());
}

#[tokio::test]
async fn discover_reads_global_state_and_ssh_config_from_injected_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    write(
        &home.join(".codex/.codex-global-state.json"),
        r#"{"codex-managed-remote-connections":[{"hostId":"remote-ssh-discovered:devbox","displayName":"devbox","source":"discovered","alias":"devbox","hostname":null,"sshPort":null,"identity":null},{"hostId":"h-2","displayName":"gpu","source":"codex-managed","alias":null,"hostname":"me@203.0.113.7","sshPort":2222,"identity":"/k/id"}]}"#,
        0o600,
    );
    write(
        &home.join(".ssh/config"),
        "Host devbox\n  HostName 203.0.113.5\nHost spare\n  User root\n",
        0o600,
    );
    let hosts = SshRemoteHosts::new(engine(), DataDir::new(tmp.path().join("data")));
    // The local home is inferred from a Codex home named `.codex`.
    let found = hosts
        .discover(&CodexHome::new(home.join(".codex")), &[], &[])
        .await
        .unwrap();
    let ids: Vec<&str> = found.iter().map(|h| h.target_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["ssh:remote-ssh-discovered:devbox", "ssh:h-2", "ssh:spare"]
    );
    assert_eq!(
        found[0].sources,
        vec![HostSource::GlobalState, HostSource::SshConfig]
    );
    assert_eq!(found[1].ssh.user.as_deref(), Some("me"));
    assert_eq!(found[1].ssh.port, Some(2222));
    assert!(superba_remote::discover::is_suggestion(&found[2]));
}

async fn run_write(shell: &LocalShell, spec: &WriteSpec) -> superba_remote::records::Records {
    let n = new_nonce();
    let out = shell
        .run(&devbox(), &write_script(&n, spec), Duration::from_secs(20))
        .await
        .unwrap();
    superba_remote::records::Records::parse(&out.stdout, &n).unwrap()
}

#[tokio::test]
async fn the_per_home_lock_blocks_writers_and_stale_locks_are_broken() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let codex = home.join(".codex");
    std::fs::create_dir_all(codex.join(".superba/lock")).unwrap();
    let shell = LocalShell::new(&home);
    let spec = WriteSpec {
        codex_home: codex.to_string_lossy().into_owned(),
        backup_dir: String::new(),
        first_write: false,
        ops: vec![FileOp {
            path: codex.join("x").to_string_lossy().into_owned(),
            expect: Expect::Absent,
            new: Some(b"1".to_vec()),
            mode: Mode::Preserve,
            backup: false,
        }],
        marker: None,
    };
    let r = run_write(&shell, &spec).await;
    assert_eq!(r.get("result"), Some("locked"));
    assert!(!codex.join("x").exists());
    // A lock older than the stale limit is broken; the lock is released afterwards.
    let lock = std::fs::File::open(codex.join(".superba/lock")).unwrap();
    lock.set_modified(SystemTime::now() - Duration::from_secs(3600))
        .unwrap();
    let r = run_write(&shell, &spec).await;
    assert_eq!(r.get("result"), Some("ok"), "{:?}", r.all("error"));
    assert_eq!(read(&codex.join("x")), "1");
    assert!(!codex.join(".superba/lock").exists(), "lock released");
}

#[tokio::test]
async fn symlinked_auth_json_ends_up_private_and_no_staging_is_left() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let codex = home.join(".codex");
    write(&home.join("creds/credentials.json"), "{}", 0o644);
    std::fs::create_dir_all(&codex).unwrap();
    std::os::unix::fs::symlink(home.join("creds/credentials.json"), codex.join("auth.json"))
        .unwrap();
    let shell = LocalShell::new(&home);
    let spec = WriteSpec {
        codex_home: codex.to_string_lossy().into_owned(),
        backup_dir: codex
            .join(".superba/backups/t")
            .to_string_lossy()
            .into_owned(),
        first_write: true,
        ops: vec![FileOp {
            path: codex.join("auth.json").to_string_lossy().into_owned(),
            expect: Expect::Bytes(b"{}".to_vec()),
            new: Some(b"{\"OPENAI_API_KEY\":\"k\"}".to_vec()),
            mode: Mode::Private,
            backup: true,
        }],
        marker: Some(b"{}".to_vec()),
    };
    let r = run_write(&shell, &spec).await;
    assert_eq!(r.get("result"), Some("ok"), "{:?}", r.all("error"));
    let link = std::fs::symlink_metadata(codex.join("auth.json")).unwrap();
    assert!(link.file_type().is_symlink());
    let referent = home.join("creds/credentials.json");
    assert_eq!(read(&referent), "{\"OPENAI_API_KEY\":\"k\"}");
    assert_eq!(
        std::fs::metadata(&referent).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(read(&codex.join(".superba/first-write/auth.json")), "{}");
    for dir in [&codex, &home.join("creds")] {
        let left: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("superba-stage") || n.contains(".remote-state."))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }
}

#[tokio::test]
async fn first_write_capture_failure_aborts_before_any_change() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let codex = home.join(".codex");
    write(&codex.join("config.toml"), "a = 1\n", 0o644);
    // An unwritable first-write directory: the originals cannot be recorded.
    std::fs::create_dir_all(codex.join(".superba/first-write")).unwrap();
    std::fs::set_permissions(
        codex.join(".superba/first-write"),
        std::fs::Permissions::from_mode(0o500),
    )
    .unwrap();
    let shell = LocalShell::new(&home);
    let spec = WriteSpec {
        codex_home: codex.to_string_lossy().into_owned(),
        backup_dir: codex
            .join(".superba/backups/t")
            .to_string_lossy()
            .into_owned(),
        first_write: true,
        ops: vec![FileOp {
            path: codex.join("config.toml").to_string_lossy().into_owned(),
            expect: Expect::Bytes(b"a = 1\n".to_vec()),
            new: Some(b"a = 2\n".to_vec()),
            mode: Mode::Preserve,
            backup: true,
        }],
        marker: None,
    };
    let r = run_write(&shell, &spec).await;
    std::fs::set_permissions(
        codex.join(".superba/first-write"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    if nix_is_root() {
        return; // root ignores directory permissions
    }
    assert_eq!(r.get("result"), Some("error"));
    assert_eq!(read(&codex.join("config.toml")), "a = 1\n");
}

fn nix_is_root() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false)
}

#[tokio::test]
async fn too_many_recent_rollouts_make_the_turn_check_incomplete() {
    let env = setup(None);
    let dir = env.home.join(".codex/sessions/2026/10/16");
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..(superba_remote::script::MAX_ROLLOUTS + 1) {
        std::fs::write(dir.join(format!("rollout-{i:04}.jsonl")), "{}\n").unwrap();
    }
    let act = env.hosts.turn_activity(&devbox()).await.unwrap();
    assert!(!act.complete);
}
