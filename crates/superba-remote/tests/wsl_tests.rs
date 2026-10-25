//! WSL distributions as managed hosts (user bug 45): the WSL transport's
//! command line, the WSL flavor of the scripts (the desktop app's `bash -l` view,
//! its stdio app-server, the default gateway), env blocks for the app and the
//! CLI, router reachability from inside, discovery of listed distributions.
//!
//! The scripts run under the local `/bin/sh` with an isolated `HOME`
//! ([`LocalShell`]) or through a fake `wsl.exe` that runs what follows `--exec`;
//! nothing reads or writes the real home directory.
#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{engine, token_profile};
use pretty_assertions::assert_eq;
use superba_core::codex::ENV_AUTHAPI_BASE_URL;
use superba_core::env::MARKER_BEGIN;
use superba_core::wsl::{host_id, target_id};
use superba_core::{
    AuthMode, CodexHome, DataDir, Error, HostKind, HostSource, RemoteHost, RemoteHosts, ShellKind,
    SshEndpoint, WslDistro, WslNetworking,
};
use superba_remote::discover::merge_wsl;
use superba_remote::envrc::{RC_FILES, RcBases, RcFile, plan_env_for, rc_path};
use superba_remote::ssh::RemoteShell;
use superba_remote::wsl::{
    FixedWsl, LxssEntry, WslInventory, WslShell, distros_from_lxss, require_distro, wsl_args,
};
use superba_remote::{LocalShell, SshRemoteHosts, env_shells};
use tempfile::TempDir;

const ORIGIN: &str = "https://api.krill-code.net";
const KEY: &str = "sk-test-0000000000000000wxyz";

fn write(path: &Path, contents: &str, mode: u32) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    if mode & 0o111 == 0 {
        std::fs::write(path, contents).unwrap();
    } else {
        // A program this process wrote fails to start with ETXTBSY ("Text file
        // busy") while another test thread's fork still holds the write
        // descriptor; `cp` writes it from its own process instead.
        let staged = path.with_file_name(format!(
            "{}.staged",
            path.file_name().unwrap().to_string_lossy()
        ));
        std::fs::write(&staged, contents).unwrap();
        let copied = std::process::Command::new("cp")
            .arg(&staged)
            .arg(path)
            .status()
            .unwrap();
        assert!(copied.success(), "cp {}", path.display());
        std::fs::remove_file(&staged).unwrap();
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn distro(name: &str) -> RemoteHost {
    RemoteHost {
        target_id: target_id(name),
        label: String::new(),
        host_id: Some(host_id(name)),
        kind: HostKind::Wsl,
        ssh: SshEndpoint::default(),
        sources: vec![HostSource::Wsl],
        connected: None,
        wsl: Some(WslDistro {
            version: Some(2),
            ..WslDistro::named(name)
        }),
    }
}

struct Env {
    _tmp: TempDir,
    home: PathBuf,
    hosts: SshRemoteHosts,
}

/// A distribution home whose user shell is `shell` and whose `~/.profile` puts
/// only programs without `codex` on `PATH` (as in the flow tests).
fn setup(shell: &str) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for src in ["/usr/local/bin", "/usr/bin", "/bin"] {
        let Ok(entries) = std::fs::read_dir(src) else {
            continue;
        };
        for e in entries.flatten() {
            let link = bin.join(e.file_name());
            if e.file_name() != "codex" && link.symlink_metadata().is_err() {
                std::os::unix::fs::symlink(e.path(), &link).unwrap();
            }
        }
    }
    write(
        &home.join(".profile"),
        "PATH=\"$HOME/../bin\"; export PATH\n\
         if [ -n \"$BASH_VERSION\" ] && [ -f \"$HOME/.bashrc\" ]; then . \"$HOME/.bashrc\"; fi\n",
        0o644,
    );
    write(
        &home.join(".bashrc"),
        "case $- in *i*) ;; *) return;; esac\nexport PATH=\"$HOME/interactive:$PATH\"\n",
        0o644,
    );
    write(
        &home.join(".codex/config.toml"),
        "model = \"gpt-5\"\n",
        0o644,
    );
    let ls = LocalShell::new(&home).with_login_shell(shell);
    let hosts = SshRemoteHosts::new(engine(), DataDir::new(tmp.path().join("data")))
        .with_shell(Arc::new(ls))
        .with_wsl_lister(Arc::new(FixedWsl::default()));
    Env {
        _tmp: tmp,
        home,
        hosts,
    }
}

fn have_bash() -> bool {
    Path::new("/usr/bin/bash").is_file()
}

fn certificate() -> String {
    rcgen::generate_simple_self_signed(vec!["localhost".into()])
        .unwrap()
        .cert
        .pem()
}

fn routed_profile() -> superba_core::ResolvedProfile {
    let mut desired = token_profile("https://127.0.0.1:17242", AuthMode::Pat, KEY);
    if let superba_core::ResolvedMode::Token { endpoints, .. } = &mut desired.mode {
        endpoints.ca_certificate = Some(superba_core::SecretBytes::new(certificate().into_bytes()));
    }
    desired
}

#[tokio::test]
async fn wsl_router_trust_preserves_user_roots_and_restores_or_rolls_back_atomically() {
    let env = setup("/bin/bash");
    let host = distro("Ubuntu");
    let user_ca = env.home.join("user roots 中文.pem");
    let user_pem = certificate();
    write(&user_ca, &user_pem, 0o600);
    let profile_path = env.home.join(".profile");
    let original = format!(
        "{}export CODEX_CA_CERTIFICATE=\"{}\"\n",
        read(&profile_path),
        user_ca.display()
    );
    write(&profile_path, &original, 0o644);
    let bashrc = read(&env.home.join(".bashrc"));
    let desired = routed_profile();
    let plan = env.hosts.plan_apply(&host, &desired).await.unwrap();
    assert!(plan.certificate.is_some());
    assert!(
        !env.home.join(".codex/.superba/router-ca.pem").exists(),
        "preview never writes"
    );
    let report = env.hosts.apply(&host, &plan).await.unwrap();
    let bundle_path = env.home.join(".codex/.superba/router-ca.pem");
    let bundle = read(&bundle_path);
    assert!(bundle.contains(&user_pem));
    assert_eq!(bundle.matches("-----BEGIN CERTIFICATE-----").count(), 2);
    assert_eq!(
        std::fs::metadata(&bundle_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(read(&profile_path).contains(&format!(
        "CODEX_CA_CERTIFICATE=\"{}\"",
        bundle_path.display()
    )));
    assert!(report.certificate.is_some());
    assert!(read(&user_ca) == user_pem, "user roots remain untouched");
    let repeat = env.hosts.plan_apply(&host, &desired).await.unwrap();
    assert!(
        repeat.certificate.is_none(),
        "repeated routing does not grow the bundle"
    );
    let settings = env
        .hosts
        .plan_apply_scoped(&host, &desired, superba_core::RemoteApplyScope::Settings)
        .await
        .unwrap();
    assert!(settings.certificate.is_none() && settings.env.is_none());
    env.hosts.rollback(&host, &report).await.unwrap();
    assert!(!bundle_path.exists());
    assert_eq!(read(&profile_path), original);
    assert_eq!(read(&env.home.join(".bashrc")), bashrc);

    let plan = env.hosts.plan_apply(&host, &desired).await.unwrap();
    env.hosts.apply(&host, &plan).await.unwrap();
    let mut restore = desired.clone();
    restore.mode = superba_core::ResolvedMode::Original {
        provider: superba_core::ProviderRestore::Baseline {
            reverse_migrate: false,
        },
    };
    let plan = env.hosts.plan_apply(&host, &restore).await.unwrap();
    let report = env.hosts.apply(&host, &plan).await.unwrap();
    assert!(!bundle_path.exists());
    assert_eq!(read(&profile_path), original);
    assert_eq!(read(&env.home.join(".bashrc")), bashrc);
    env.hosts.rollback(&host, &report).await.unwrap();
    assert_eq!(read(&bundle_path), bundle);
}

#[tokio::test]
async fn leaving_wsl_routing_for_direct_pat_restores_the_user_ca() {
    let env = setup("/bin/bash");
    let host = distro("Ubuntu");
    let user_ca = env.home.join("root.pem");
    write(&user_ca, &certificate(), 0o600);
    let profile_path = env.home.join(".profile");
    let original = format!(
        "{}export CODEX_CA_CERTIFICATE=\"{}\"\n",
        read(&profile_path),
        user_ca.display()
    );
    write(&profile_path, &original, 0o644);
    let plan = env
        .hosts
        .plan_apply(&host, &routed_profile())
        .await
        .unwrap();
    env.hosts.apply(&host, &plan).await.unwrap();
    let direct = token_profile(ORIGIN, AuthMode::Pat, KEY);
    let plan = env.hosts.plan_apply(&host, &direct).await.unwrap();
    env.hosts.apply(&host, &plan).await.unwrap();
    assert!(!env.home.join(".codex/.superba/router-ca.pem").exists());
    assert!(read(&profile_path).contains(&format!(
        "\nexport CODEX_CA_CERTIFICATE=\"{}\"",
        user_ca.display()
    )));
    assert!(!read(&profile_path).contains(": # [superba] export CODEX_CA_CERTIFICATE"));
    assert_eq!(
        env.hosts.probe(&host).await.unwrap().authapi_env.value(),
        Some(ORIGIN)
    );
}

#[tokio::test]
async fn wsl_router_refuses_foreign_bundles_and_invalid_or_missing_user_roots() {
    for case in ["foreign", "missing", "invalid", "private"] {
        let env = setup("/bin/bash");
        let host = distro("Ubuntu");
        let profile_path = env.home.join(".profile");
        if case == "foreign" {
            write(
                &env.home.join(".codex/.superba/router-ca.pem"),
                &certificate(),
                0o600,
            );
        } else {
            let source = env.home.join("user.pem");
            if case != "missing" {
                let text = if case == "private" {
                    format!("{}\n-----BEGIN {}-----\n", certificate(), "PRIVATE KEY")
                } else {
                    "invalid".into()
                };
                write(&source, &text, 0o600);
            }
            write(
                &profile_path,
                &format!(
                    "{}export CODEX_CA_CERTIFICATE=\"{}\"\n",
                    read(&profile_path),
                    source.display()
                ),
                0o644,
            );
        }
        let original = read(&profile_path);
        assert!(
            env.hosts
                .plan_apply(&host, &routed_profile())
                .await
                .is_err(),
            "{case}"
        );
        assert_eq!(read(&profile_path), original);
        assert!(!env.home.join(".codex/auth.json").exists());
    }
}

#[tokio::test]
async fn routed_wsl_prepares_the_legacy_apps_authapi_even_with_api_key_auth() {
    if !have_bash() {
        return;
    }
    let env = setup("/bin/bash");
    let host = distro("Ubuntu");
    let old = token_profile(ORIGIN, AuthMode::Pat, KEY);
    let plan = env.hosts.plan_apply(&host, &old).await.unwrap();
    env.hosts.apply(&host, &plan).await.unwrap();
    let mut routed = token_profile("http://127.0.0.1:17242", AuthMode::ApiKey, KEY);
    let ordinary = env.hosts.plan_apply(&host, &routed).await.unwrap();
    assert!(
        ordinary.env.is_none(),
        "an independent WSL API-key home keeps its env"
    );
    if let superba_core::ResolvedMode::Token { endpoints, .. } = &mut routed.mode {
        // The local legacy app reads the Windows home with PAT credentials,
        // but its login bash reads these same rc files first.
        endpoints.authapi_origin = Some(endpoints.origin.clone());
    }
    let settings = env
        .hosts
        .plan_apply_scoped(&host, &routed, superba_core::RemoteApplyScope::Settings)
        .await
        .unwrap();
    assert!(
        settings.env.is_none(),
        "settings sync still leaves authentication alone"
    );
    let plan = env.hosts.plan_apply(&host, &routed).await.unwrap();
    let report = env.hosts.apply(&host, &plan).await.unwrap();
    let seen = env.hosts.probe(&host).await.unwrap();
    assert_eq!(seen.authapi_env.value(), Some("http://127.0.0.1:17242"));
    assert!(report.env.is_some(), "the old PAT export must be replaced");
    env.hosts.rollback(&host, &report).await.unwrap();
    assert_eq!(
        env.hosts.probe(&host).await.unwrap().authapi_env.value(),
        Some(ORIGIN)
    );
    let direct = token_profile("https://example.com", AuthMode::Pat, KEY);
    let plan = env.hosts.plan_apply(&host, &direct).await.unwrap();
    env.hosts.apply(&host, &plan).await.unwrap();
    assert_eq!(
        env.hosts.probe(&host).await.unwrap().authapi_env.value(),
        Some("https://example.com")
    );
}

#[tokio::test]
async fn the_probe_sees_what_the_apps_login_bash_sees() {
    if !have_bash() {
        return;
    }
    let env = setup("/bin/sh");
    // A Codex only on the PATH of an interactive bash (an nvm-style install in
    // ~/.bashrc): the app finds it the same way.
    write(
        &env.home.join("interactive/codex"),
        "#!/bin/sh\necho 'codex-cli 0.162.0'\n",
        0o755,
    );
    // The value the app's non-interactive login bash exports (~/.profile).
    let profile = read(&env.home.join(".profile"));
    write(
        &env.home.join(".profile"),
        &format!("{profile}export {ENV_AUTHAPI_BASE_URL}=https://example.org\n"),
        0o644,
    );
    let st = env.hosts.probe(&distro("Ubuntu")).await.unwrap();
    assert_eq!(st.app_bash, Some(true));
    assert_eq!(
        st.codex_version.map(|v| v.to_string()).as_deref(),
        Some("0.162.0")
    );
    assert!(st.codex_path.unwrap().ends_with("/interactive/codex"));
    assert_eq!(
        st.authapi_env,
        superba_core::EnvObservation::Present("https://example.org".into())
    );
    assert_eq!(st.codex_home, format!("{}/.codex", env.home.display()));
    // This machine's /proc/net/route names a default gateway (a container or VM
    // may have none).
    if std::fs::read_to_string("/proc/net/route")
        .is_ok_and(|r| superba_core::wsl::default_gateway(&r).is_some())
    {
        assert!(st.gateway.is_some());
    }
}

#[tokio::test]
async fn the_apps_wsl_app_server_is_found_by_its_stdio_command_line() {
    if !have_bash() {
        return;
    }
    let env = setup("/bin/sh");
    // What `exec codex -c features.code_mode_host=true app-server …` looks like
    // (bash's `exec -a` sets the command line).
    let mut child = std::process::Command::new("/usr/bin/bash")
        .args([
            "-c",
            "exec -a 'codex -c features.code_mode_host=true app-server --analytics-default-enabled' sleep 30",
        ])
        .spawn()
        .unwrap();
    let pid = child.id();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let wsl = env.hosts.probe(&distro("Ubuntu")).await.unwrap();
    let ssh_host = common::host("devbox", Some("remote-ssh-discovered:devbox"));
    let ssh = env.hosts.probe(&ssh_host).await.unwrap();
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        wsl.app_server_pids.contains(&pid),
        "{:?}",
        wsl.app_server_pids
    );
    // An SSH host's app-server listens (`--listen`); this one is not it.
    assert!(!ssh.app_server_pids.contains(&pid));
}

#[tokio::test]
async fn pat_on_a_distribution_writes_the_block_for_the_app_and_the_cli() {
    if !have_bash() {
        // A distribution without /usr/bin/bash (this machine has none, e.g. a
        // macOS runner): the app cannot start its Codex there, so only the
        // user's login shell gets the block, and the plan says why.
        let env = setup("/bin/sh");
        let plan = env
            .hosts
            .plan_apply(
                &distro("Ubuntu"),
                &token_profile(ORIGIN, AuthMode::Pat, KEY),
            )
            .await
            .unwrap();
        assert!(plan.blocked.is_empty(), "{:?}", plan.blocked);
        assert_eq!(plan.state.app_bash, Some(false));
        assert!(
            plan.warnings
                .iter()
                .any(|n| n.text.key == "remote.warn.wsl_login_bash"),
            "{:?}",
            plan.warnings
        );
        let report = env.hosts.apply(&distro("Ubuntu"), &plan).await.unwrap();
        assert!(report.env.is_some());
        assert!(read(&env.home.join(".profile")).contains(MARKER_BEGIN));
        let bashrc = read(&env.home.join(".bashrc"));
        assert!(!bashrc.contains(MARKER_BEGIN), "{bashrc}");
        return;
    }
    if !Path::new("/bin/zsh").is_file() && !Path::new("/usr/bin/zsh").is_file() {
        // Without zsh the union case is covered by the pure test below.
        let env = setup("/bin/sh");
        let plan = env
            .hosts
            .plan_apply(
                &distro("Ubuntu"),
                &token_profile(ORIGIN, AuthMode::Pat, KEY),
            )
            .await
            .unwrap();
        assert!(plan.blocked.is_empty(), "{:?}", plan.blocked);
        let report = env.hosts.apply(&distro("Ubuntu"), &plan).await.unwrap();
        assert!(report.env.is_some());
        let bashrc = read(&env.home.join(".bashrc"));
        assert!(bashrc.starts_with(MARKER_BEGIN), "{bashrc}");
        assert!(read(&env.home.join(".profile")).contains(MARKER_BEGIN));
        return;
    }
    let zsh = if Path::new("/bin/zsh").is_file() {
        "/bin/zsh"
    } else {
        "/usr/bin/zsh"
    };
    let env = setup(zsh);
    let plan = env
        .hosts
        .plan_apply(
            &distro("Ubuntu"),
            &token_profile(ORIGIN, AuthMode::Pat, KEY),
        )
        .await
        .unwrap();
    assert!(plan.blocked.is_empty(), "{:?}", plan.blocked);
    env.hosts.apply(&distro("Ubuntu"), &plan).await.unwrap();
    // The app's `/usr/bin/bash -l` reads ~/.profile; the user's zsh ~/.zshenv.
    assert!(read(&env.home.join(".profile")).contains(MARKER_BEGIN));
    assert!(read(&env.home.join(".bashrc")).starts_with(MARKER_BEGIN));
    assert!(read(&env.home.join(".zshenv")).contains(MARKER_BEGIN));
}

#[test]
fn env_blocks_cover_every_shell_a_distribution_starts_codex_with() {
    let bases = RcBases {
        home: "/home/u".into(),
        zdotdir: None,
        xdg_config_home: None,
    };
    let files: Vec<RcFile> = RC_FILES
        .iter()
        .map(|spec| RcFile {
            spec: *spec,
            path: rc_path(&bases, spec),
            content: (spec.name == "profile").then(|| b"PATH=x\n".to_vec()),
            unreadable: false,
        })
        .collect();
    let op = superba_core::EnvOp::Set {
        value: ORIGIN.into(),
        take_over_foreign: false,
    };
    let names = |shells: &[ShellKind]| -> Vec<&'static str> {
        let mut v: Vec<&str> = plan_env_for(shells, &files, &op)
            .unwrap()
            .writes
            .iter()
            .map(|w| w.name)
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        names(&[ShellKind::Bash, ShellKind::Zsh]),
        ["bashrc", "profile", "zshenv"]
    );
    assert_eq!(
        names(&[ShellKind::Bash, ShellKind::Fish]),
        ["bashrc", "fish_superba", "profile"]
    );
    // The CLI's csh cannot get a block; the app's bash still does.
    assert_eq!(
        names(&[ShellKind::Bash, ShellKind::Other("tcsh".into())]),
        ["bashrc", "profile"]
    );
    // The shells of a host.
    let mut st = superba_core::RemoteState {
        target_id: target_id("U"),
        probed_at: superba_core::Timestamp::from_millis(0),
        os: superba_core::RemoteOs::Linux,
        shell: ShellKind::Zsh,
        codex_home: "/home/u/.codex".into(),
        codex_path: None,
        codex_version: None,
        config_sha256: None,
        auth_sha256: None,
        auth: superba_core::AuthShape::Missing,
        model_provider: None,
        openai_base_url: None,
        machine_id: None,
        marker_machine_id: None,
        authapi_env: superba_core::EnvObservation::Unknown,
        app_server_pids: vec![],
        owned_paths: vec![],
        config_toml: None,
        arch: None,
        kernel: None,
        disk_free_kib: None,
        codex_install: None,
        codex_resolved_path: None,
        gateway: None,
        app_bash: Some(true),
        login_fallback: None,
    };
    assert_eq!(
        env_shells(&distro("U"), &st),
        [ShellKind::Bash, ShellKind::Zsh]
    );
    st.app_bash = Some(false);
    assert_eq!(env_shells(&distro("U"), &st), [ShellKind::Zsh]);
    let ssh = common::host("devbox", None);
    assert_eq!(env_shells(&ssh, &st), [ShellKind::Zsh]);
}

#[tokio::test]
async fn the_router_is_checked_from_inside_the_distribution() {
    if !have_bash() {
        return;
    }
    let env = setup("/bin/sh");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut s, _) = listener.accept().await.unwrap();
        // Read the whole request head first, as the router (hyper) does: bash's
        // printf sends it a line per write, and answering and closing after the
        // first line resets the connection under the script's remaining writes
        // (bash dies of SIGPIPE before it reads the answer).
        let mut req = Vec::new();
        let mut buf = [0; 1024];
        while !req.ends_with(b"\r\n\r\n") {
            let n = s.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            req.extend_from_slice(&buf[..n]);
        }
        let req = String::from_utf8_lossy(&req).into_owned();
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"service\":\"superba-router\",\"instance_id\":\"abc\"}")
            .await
            .unwrap();
        req
    });
    let answer = env
        .hosts
        .http_get(&distro("Ubuntu"), "127.0.0.1", port, "/_superba/v1/health")
        .await
        .unwrap()
        .expect("an answer");
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert!(answer.contains("\"instance_id\":\"abc\""));
    let request = server.await.unwrap();
    assert!(
        request.starts_with("GET /_superba/v1/health HTTP/1.0\r\n"),
        "{request:?}"
    );
    // Nothing listens: no answer (refused at once).
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    assert_eq!(
        env.hosts
            .http_get(&distro("Ubuntu"), "127.0.0.1", port, "/_superba/v1/health")
            .await
            .unwrap(),
        None
    );
    // Only IPv4 literals and safe paths reach the script.
    assert!(
        env.hosts
            .http_get(&distro("Ubuntu"), "x;y", 1, "/")
            .await
            .is_err()
    );
    assert!(
        env.hosts
            .http_get(&distro("Ubuntu"), "127.0.0.1", 1, "/a b")
            .await
            .is_err()
    );
}

/// A fake `wsl.exe`: checks the `-d <distro> --cd ~ --exec` prefix and runs
/// the rest, or fails like wsl.exe with a UTF-16LE message for distro `Missing`.
fn fake_wsl(dir: &Path) -> PathBuf {
    let p = dir.join("wsl.exe");
    write(
        &p,
        "#!/bin/sh\n\
         [ \"$1\" = -d ] && [ \"$3\" = --cd ] && [ \"$4\" = '~' ] && [ \"$5\" = --exec ] || { echo \"bad args: $*\" >&2; exit 2; }\n\
         [ \"$WSL_UTF8\" = 1 ] || { echo 'WSL_UTF8 not set' >&2; exit 2; }\n\
         if [ \"$2\" = Missing ]; then printf 'T\\000h\\000e\\000r\\000e\\000 \\000i\\000s\\000 \\000n\\000o\\000 \\000d\\000i\\000s\\000t\\000r\\000o\\000\\n\\000'; exit 255; fi\n\
         shift 5\n\
         # Resolve Linux executable paths on the Unix test host.\n\
         case \"$1\" in /bin/true|/bin/sh)\n\
           program=$(command -v \"${1##*/}\") || exit 127\n\
           shift; set -- \"$program\" \"$@\";;\n\
         esac\n\
         exec \"$@\"\n",
        0o755,
    );
    p
}

#[tokio::test]
async fn the_wsl_transport_runs_scripts_inside_the_named_distribution() {
    let tmp = tempfile::tempdir().unwrap();
    let wsl = WslShell::with_program(fake_wsl(tmp.path()));
    let out = wsl
        .run(
            &distro("Ubuntu"),
            "echo hello-from-sh\n",
            Duration::from_secs(10),
        )
        .await
        .unwrap();
    assert_eq!(out.status, Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello-from-sh\n");
    // A payload after the script on the same stdin.
    let payload = tmp.path().join("payload.bin");
    std::fs::write(&payload, b"PAYLOAD").unwrap();
    let out = wsl
        .run_with_payload(
            &distro("Ubuntu"),
            "cat <&0\n",
            &payload,
            Duration::from_secs(10),
        )
        .await
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "PAYLOAD");
    // wsl.exe's own failure, decoded from UTF-16LE.
    match wsl
        .run(&distro("Missing"), "true\n", Duration::from_secs(10))
        .await
    {
        Err(Error::Wsl { distro, message }) => {
            assert_eq!(distro, "Missing");
            assert_eq!(message, "There is no distro");
        }
        other => panic!("{other:?}"),
    }
    // Only WSL hosts with a plain distribution name.
    let ssh = common::host("devbox", None);
    assert!(wsl.run(&ssh, "true", Duration::from_secs(1)).await.is_err());
    let mut odd = distro("Ubuntu");
    odd.wsl = Some(WslDistro::named("-x"));
    assert!(require_distro(&odd).is_err());
    assert_eq!(
        wsl_args("Ubuntu", &["/bin/sh", "-s"]),
        ["-d", "Ubuntu", "--cd", "~", "--exec", "/bin/sh", "-s"]
    );
}

#[tokio::test]
async fn errors_of_a_distribution_name_wsl_not_ssh() {
    let tmp = tempfile::tempdir().unwrap();
    let hosts = SshRemoteHosts::new(engine(), DataDir::new(tmp.path().join("data")))
        .with_wsl_shell(Arc::new(WslShell::with_program(fake_wsl(tmp.path()))));
    match hosts.probe(&distro("Missing")).await {
        Err(Error::Wsl { distro, .. }) => assert_eq!(distro, "Missing"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn discovery_adds_listed_distributions_and_merges_the_apps_own() {
    let tmp = tempfile::tempdir().unwrap();
    let home = CodexHome::new(tmp.path().join(".codex"));
    let inventory = WslInventory {
        distros: vec![
            WslDistro {
                version: Some(2),
                default: true,
                networking: WslNetworking::Mirrored,
                ..WslDistro::named("Ubuntu")
            },
            WslDistro {
                version: Some(1),
                ..WslDistro::named("Debian")
            },
        ],
        networking: WslNetworking::Mirrored,
        error: None,
    };
    let hosts = SshRemoteHosts::new(engine(), DataDir::new(tmp.path().join("data")))
        .with_home_dir(tmp.path())
        .with_wsl_lister(Arc::new(FixedWsl(inventory)));
    // The app reports the default distribution (its WSL connection).
    let from_app = RemoteHost {
        label: "WSL: Ubuntu".into(),
        sources: vec![HostSource::Cdp],
        connected: Some(true),
        wsl: None,
        ..distro("Ubuntu")
    };
    let found = hosts.discover(&home, &[from_app], &[]).await.unwrap();
    type Row = (String, String, Vec<HostSource>, Option<u8>, bool);
    let rows: Vec<Row> = found
        .iter()
        .map(|h| {
            (
                h.target_id.to_string(),
                h.label.clone(),
                h.sources.clone(),
                h.wsl.as_ref().and_then(|d| d.version),
                h.is_app_known() && h.is_managed(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            (
                "ssh:remote-wsl:Ubuntu".into(),
                "WSL: Ubuntu".into(),
                vec![HostSource::Cdp, HostSource::Wsl],
                Some(2),
                true
            ),
            (
                "ssh:remote-wsl:Debian".into(),
                "WSL: Debian".into(),
                vec![HostSource::Wsl],
                Some(1),
                true
            ),
        ]
    );
    assert_eq!(found[0].connected, Some(true));
    assert_eq!(
        found[0].wsl.as_ref().map(|d| d.networking),
        Some(WslNetworking::Mirrored)
    );
    // Merging twice changes nothing.
    let mut again = found.clone();
    merge_wsl(&mut again, &[WslDistro::named("Debian")]);
    assert_eq!(again.len(), 2);
}

#[test]
fn registry_entries_give_installed_distributions_with_the_default_first() {
    // As the test VM's registry has them: a WSL 1 distribution has Flags 0x7
    // (interop, Windows PATH, drive mounting), a WSL 2 one 0xF (+ VM mode).
    let e = |guid: &str, name: &str, flags: u32, state: u32| LxssEntry {
        guid: guid.into(),
        name: name.into(),
        flags: Some(flags),
        state: Some(state),
    };
    let d = distros_from_lxss(
        &[
            e("{a}", "Debian", 0x7, 1),
            e("{b}", "Ubuntu", 0xF, 1),
            e("{c}", "docker-desktop", 0xF, 1),
            e("{d}", "Installing", 0xF, 3),
        ],
        Some("{B}"),
    );
    let rows: Vec<(&str, Option<u8>, bool)> = d
        .iter()
        .map(|d| (d.name.as_str(), d.version, d.default))
        .collect();
    assert_eq!(
        rows,
        [("Ubuntu", Some(2), true), ("Debian", Some(1), false)]
    );
}

#[tokio::test]
async fn the_linux_helper_is_copied_once_and_its_lines_stream() {
    let env = setup("/bin/sh");
    let dir = tempfile::tempdir().unwrap();
    // A stand-in for spb: prints its arguments, CODEX_HOME and progress lines.
    let helper = dir.path().join("spb-linux-x86_64");
    write(
        &helper,
        "#!/bin/sh\necho \"args: $*\"\necho \"home: $CODEX_HOME\"\necho 'Migrated 1 of 2 sessions'\necho 'Migrated 2 of 2 sessions'\nexit 3\n",
        0o755,
    );
    let lines = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = lines.clone();
    let sink: superba_core::LineSink =
        Arc::new(move |l: &str| seen.lock().unwrap().push(l.to_owned()));
    let args: Vec<String> = ["sessions", "migrate", "--from", "it's krill"]
        .map(str::to_owned)
        .to_vec();
    let run = env
        .hosts
        .run_helper(&distro("Ubuntu"), &helper, &args, sink.clone())
        .await
        .unwrap();
    assert!(run.uploaded);
    assert_eq!(run.exit_code, Some(3));
    let home = format!("{}/.codex", env.home.display());
    assert_eq!(
        *lines.lock().unwrap(),
        vec![
            "args: sessions migrate --from it's krill".to_owned(),
            format!("home: {home}"),
            "Migrated 1 of 2 sessions".to_owned(),
            "Migrated 2 of 2 sessions".to_owned(),
        ]
    );
    assert_eq!(
        run.tail.last().map(String::as_str),
        Some("Migrated 2 of 2 sessions")
    );
    // The copy is checksum-addressed, private to the user and reused.
    let sha = {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(std::fs::read(&helper).unwrap()))
    };
    let copy = env
        .home
        .join(format!(".local/share/superba/wsl-helper/{sha}/spb"));
    assert_eq!(
        std::fs::metadata(&copy).unwrap().permissions().mode() & 0o777,
        0o755
    );
    let again = env
        .hosts
        .run_helper(&distro("Ubuntu"), &helper, &args, sink)
        .await
        .unwrap();
    assert!(!again.uploaded);
    // Only WSL hosts run it.
    let ssh = common::host("devbox", None);
    assert!(
        env.hosts
            .run_helper(&ssh, &helper, &args, Arc::new(|_: &str| {}))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn planning_reuses_its_probe_but_apply_and_the_next_plan_read_fresh_files() {
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use superba_core::{NoEvents, Result};
    use superba_remote::ScriptOutput;
    #[derive(Debug)]
    struct Count {
        shell: LocalShell,
        probes: AtomicUsize,
    }
    #[async_trait]
    impl RemoteShell for Count {
        async fn run(
            &self,
            h: &RemoteHost,
            script: &str,
            timeout: Duration,
        ) -> Result<ScriptOutput> {
            if script.starts_with("# superba-wsl-probe") {
                self.probes.fetch_add(1, Ordering::SeqCst);
            }
            self.shell.run(h, script, timeout).await
        }
    }
    let env = setup("/bin/sh");
    let shell = Arc::new(Count {
        shell: LocalShell::new(&env.home).with_login_shell("/bin/sh"),
        probes: AtomicUsize::new(0),
    });
    let hosts = env.hosts.with_shell(shell.clone());
    let host = distro("Ubuntu");
    let desired = token_profile(ORIGIN, AuthMode::ApiKey, KEY);
    superba_remote::operation::with_events(Arc::new(NoEvents), async {
        hosts.probe(&host).await.unwrap();
        let plan = hosts.plan_apply(&host, &desired).await.unwrap();
        assert_eq!(shell.probes.load(Ordering::SeqCst), 1);
        let path = env.home.join(".codex/config.toml");
        let original = read(&path);
        std::fs::write(&path, format!("# concurrent edit\n{original}")).unwrap();
        hosts.apply(&host, &plan).await.unwrap();
        assert_eq!(shell.probes.load(Ordering::SeqCst), 2);
        assert!(read(&path).starts_with("# concurrent edit"));
    })
    .await;
    hosts.plan_apply(&host, &desired).await.unwrap();
    assert_eq!(shell.probes.load(Ordering::SeqCst), 3);
}

/// alpha.10 re-test (W81): a cold distribution whose login shell does not
/// answer in time. With an earlier live read the probe uses it (`cache`);
/// without one it uses a plain login shell (`plain_login`), else the plain
/// environment (`none`) — the probe never fails for it.
#[tokio::test]
async fn a_slow_login_falls_back_to_the_cache_then_plain_login_then_nothing() {
    if !have_bash() {
        return;
    }
    let env = setup("/bin/sh");
    write(
        &env.home.join("interactive/codex"),
        "#!/bin/sh\necho 'codex-cli 0.162.0'\n",
        0o755,
    );
    let ls = LocalShell::new(&env.home)
        .with_login_shell("/bin/sh")
        .with_env("SB_LOGIN_SECS", "1");
    let hosts = SshRemoteHosts::new(engine(), DataDir::new(env.home.join("../data2")))
        .with_shell(Arc::new(ls))
        .with_wsl_lister(Arc::new(FixedWsl::default()));
    let host = distro("ColdCache");
    superba_remote::probe::login_cache::forget(&host.target_id);
    // A live read first: remembered.
    let live = hosts.probe(&host).await.unwrap();
    assert_eq!(live.login_fallback, None);
    assert!(
        live.codex_path
            .as_deref()
            .unwrap()
            .ends_with("/interactive/codex")
    );
    // Now ~/.bashrc hangs (an interactive shell only).
    let bashrc = read(&env.home.join(".bashrc"));
    write(
        &env.home.join(".bashrc"),
        &format!("{bashrc}sleep 30\n"),
        0o644,
    );
    let started = std::time::Instant::now();
    let cached = hosts.probe(&host).await.unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(cached.login_fallback.as_deref(), Some("cache"));
    assert_eq!(cached.codex_path, live.codex_path);
    // Never read live: the plain login shell's view.
    superba_remote::probe::login_cache::forget(&host.target_id);
    let plain = hosts.probe(&host).await.unwrap();
    assert_eq!(plain.login_fallback.as_deref(), Some("plain_login"));
    // ~/.profile hangs too: the plain environment, still no failure.
    let profile = read(&env.home.join(".profile"));
    write(
        &env.home.join(".profile"),
        &format!("sleep 30\n{profile}"),
        0o644,
    );
    superba_remote::probe::login_cache::forget(&host.target_id);
    let none = hosts.probe(&host).await.unwrap();
    assert_eq!(none.login_fallback.as_deref(), Some("none"));
    assert_eq!(none.codex_home, format!("{}/.codex", env.home.display()));
}
