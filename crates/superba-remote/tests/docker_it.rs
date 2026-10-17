//! Integration test against a throwaway Docker container running sshd.
//!
//! Ignored by default. Run with:
//!
//! ```text
//! SUPERBA_REMOTE_IT=1 cargo test -p superba-remote --test docker_it -- --ignored --nocapture
//! ```
//!
//! The container (Debian, OpenSSH server, users `alice` with bash and `bob` with zsh,
//! a fake `codex` in `~/.local/bin`) listens on a random loopback port. ssh runs with
//! an isolated `-F` config, a throwaway key and its own known_hosts file, so the
//! real `~/.ssh` is never read or written. The container is removed on drop.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use common::{FakeEngine, engine, original_profile, token_profile};
use superba_core::{
    AuthMode, AuthShape, DataDir, EnvObservation, Error, RemoteHost, RemoteHosts, RemoteOs,
    RemoteVerifyRequest, ShellKind, SshEndpoint, TargetId,
};
use superba_remote::SshRemoteHosts;
use superba_remote::records::Records;
use superba_remote::script::{new_nonce, sh_quote};
use superba_remote::ssh::{RemoteShell, SystemSsh};
use tempfile::TempDir;

const IMAGE: &str = "superba-remote-it:v1";
const ORIGIN: &str = "https://api.krill-code.net";
const KEY: &str = "sk-it-000000000000000000000abcd";
const MACHINE_ID: &str = "5b0c0ffee5b0c0ffee5b0c0ffee5b0c0";
const CHATGPT_AUTH: &str = "{\"tokens\":{\"id_token\":\"x\",\"access_token\":\"y\"},\"last_refresh\":\"2026-10-01T00:00:00Z\"}\n";
const CONFIG: &str = "model = \"gpt-5\"\n\n[mcp_servers.docs]\ncommand = \"npx\"\n";

const DOCKERFILE: &str = r#"FROM debian:bookworm-slim
RUN apt-get update -qq \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends openssh-server bash zsh procps >/dev/null \
 && rm -rf /var/lib/apt/lists/* \
 && mkdir -p /run/sshd \
 && useradd -m -p '*' -s /bin/bash alice \
 && useradd -m -p '*' -s /usr/bin/zsh bob \
 && useradd -m -p '*' -s /bin/bash carol \
 && echo 5b0c0ffee5b0c0ffee5b0c0ffee5b0c0 > /etc/machine-id \
 && ssh-keygen -A
COPY codex /usr/local/share/fake-codex
RUN for u in alice bob; do \
      install -d -m 755 -o $u -g $u /home/$u/.local /home/$u/.local/bin \
      && install -m 755 -o $u -g $u /usr/local/share/fake-codex /home/$u/.local/bin/codex; \
    done
CMD ["/usr/sbin/sshd", "-D", "-e"]
"#;

const FAKE_CODEX: &str = r#"#!/bin/sh
case "$*" in
  *--version*) echo "codex-cli 0.162.0" ;;
  *app-server*) trap 'exit 0' TERM; while :; do sleep 1; done ;;
  *) exit 0 ;;
esac
"#;

fn enabled() -> bool {
    std::env::var("SUPERBA_REMOTE_IT").as_deref() == Ok("1")
}

fn docker(args: &[&str]) -> String {
    let out = Command::new("docker").args(args).output().expect("docker");
    assert!(
        out.status.success(),
        "docker {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A running sshd container, removed on drop.
struct Sshd {
    id: String,
    port: u16,
    dir: TempDir,
}

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.id])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

impl Sshd {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = dir.path().join("ctx");
        std::fs::create_dir_all(&ctx).unwrap();
        std::fs::write(ctx.join("Dockerfile"), DOCKERFILE).unwrap();
        std::fs::write(ctx.join("codex"), FAKE_CODEX).unwrap();
        docker(&["build", "-q", "-t", IMAGE, ctx.to_str().unwrap()]);
        let key = dir.path().join("id");
        let st = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "superba-it", "-f"])
            .arg(&key)
            .status()
            .expect("ssh-keygen");
        assert!(st.success());
        let name = format!("superba-remote-it-{}", new_nonce());
        let id = docker(&["run", "-d", "--name", &name, "-p", "127.0.0.1::22", IMAGE]);
        let mut sshd = Sshd { id, port: 0, dir };
        let mapped = docker(&["port", &sshd.id, "22/tcp"]);
        sshd.port = mapped
            .lines()
            .next()
            .and_then(|l| l.rsplit(':').next())
            .and_then(|p| p.parse().ok())
            .expect("mapped port");
        let pubkey = std::fs::read_to_string(key.with_extension("pub")).unwrap();
        for user in ["alice", "bob"] {
            let mut child = Command::new("docker")
                .args([
                    "exec",
                    "-i",
                    &sshd.id,
                    "sh",
                    "-c",
                    &format!(
                        "install -d -m 700 -o {user} -g {user} /home/{user}/.ssh && cat > /home/{user}/.ssh/authorized_keys && chown {user}:{user} /home/{user}/.ssh/authorized_keys && chmod 600 /home/{user}/.ssh/authorized_keys"
                    ),
                ])
                .stdin(Stdio::piped())
                .spawn()
                .unwrap();
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(pubkey.as_bytes())
                .unwrap();
            assert!(child.wait().unwrap().success());
        }
        sshd.write_ssh_config("ssh_config", "accept-new");
        sshd.write_ssh_config("ssh_config_strict", "yes");
        sshd
    }

    fn write_ssh_config(&self, name: &str, strict: &str) {
        let d = self.dir.path();
        let kh = if strict == "yes" {
            d.join("known_hosts_empty")
        } else {
            d.join("known_hosts")
        };
        let mut cfg = String::new();
        for user in ["alice", "bob", "carol"] {
            cfg.push_str(&format!(
                "Host superba-it-{user}\n  HostName 127.0.0.1\n  Port {}\n  User {user}\n",
                self.port
            ));
        }
        cfg.push_str(&format!(
            "Host *\n  IdentityFile {}\n  IdentitiesOnly yes\n  IdentityAgent none\n  UserKnownHostsFile {}\n  GlobalKnownHostsFile /dev/null\n  StrictHostKeyChecking {strict}\n  LogLevel ERROR\n",
            d.join("id").display(),
            kh.display()
        ));
        std::fs::write(d.join(name), cfg).unwrap();
    }

    fn ssh(&self, config: &str) -> SystemSsh {
        SystemSsh::default().with_extra_args([
            "-F".to_owned(),
            self.dir.path().join(config).to_string_lossy().into_owned(),
        ])
    }

    fn data(&self) -> DataDir {
        DataDir::new(self.dir.path().join("data"))
    }

    fn key_path(&self) -> PathBuf {
        self.dir.path().join("id")
    }
}

fn alias_host(user: &str) -> RemoteHost {
    let alias = format!("superba-it-{user}");
    RemoteHost {
        target_id: TargetId::remote(&format!("remote-ssh-discovered:{alias}")),
        label: alias.clone(),
        host_id: Some(format!("remote-ssh-discovered:{alias}")),
        kind: superba_core::HostKind::Ssh,
        ssh: SshEndpoint {
            alias: Some(alias),
            ..SshEndpoint::default()
        },
        sources: Vec::new(),
        connected: Some(true),
    }
}

/// Run a script on the host and return its records.
async fn sh(shell: &SystemSsh, host: &RemoteHost, body: &str) -> Records {
    let n = new_nonce();
    let script = format!(
        "SB_N='{n}'\nsb_put() {{ printf '@@SB:%s:K:%s:' \"$SB_N\" \"$1\"; printf '%s' \"$2\" | od -A n -v -t x1 | tr -d ' \\n'; printf '\\n'; }}\nmain() {{\n{body}\n}}\nmain </dev/null\n"
    );
    let out = shell
        .run(host, &script, Duration::from_secs(30))
        .await
        .expect("ssh run");
    Records::parse(&out.stdout, &n).expect("records")
}

async fn cat(shell: &SystemSsh, host: &RemoteHost, rel: &str) -> Option<String> {
    let r = sh(
        shell,
        host,
        &format!(
            "if [ -f \"$HOME/{rel}\" ]; then sb_put c \"$(cat \"$HOME/{rel}\"; printf x)\"; fi"
        ),
    )
    .await;
    r.get("c")
        .map(|c| c.strip_suffix('x').unwrap_or(c).to_owned())
}

async fn put(shell: &SystemSsh, host: &RemoteHost, rel: &str, contents: &str) {
    sh(
        shell,
        host,
        &format!(
            "mkdir -p \"$(dirname \"$HOME/{rel}\")\" && printf '%s' {} > \"$HOME/{rel}\"",
            sh_quote(contents)
        ),
    )
    .await;
}

/// Start an app-server the way the desktop app does (login shell, ~/.local/bin).
async fn start_app_server(shell: &SystemSsh, host: &RemoteHost) {
    sh(
        shell,
        host,
        r#"SB_P='PATH="$HOME/.local/bin:$PATH"; export PATH; nohup codex -c features.code_mode_host=true app-server --listen unix:// </dev/null >/dev/null 2>&1 &'
"$SHELL" -l -i -c "$SB_P" >/dev/null 2>&1"#,
    )
    .await;
}

fn hosts(sshd: &Sshd, engine: &Arc<FakeEngine>, config: &str) -> SshRemoteHosts {
    SshRemoteHosts::new(engine.clone(), sshd.data()).with_shell(Arc::new(sshd.ssh(config)))
}

#[tokio::test]
#[ignore = "needs docker; set SUPERBA_REMOTE_IT=1 and pass --ignored"]
async fn remote_hosts_against_an_sshd_container() {
    if !enabled() {
        eprintln!("skipped: set SUPERBA_REMOTE_IT=1 to run");
        return;
    }
    let sshd = Sshd::start();
    let engine = engine();
    let rh = hosts(&sshd, &engine, "ssh_config");
    let ssh = sshd.ssh("ssh_config");

    // Wait for sshd.
    let alice = alias_host("alice");
    let mut ready = false;
    for _ in 0..50 {
        if ssh
            .run(&alice, "echo ok\n", Duration::from_secs(5))
            .await
            .is_ok()
        {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(ready, "sshd did not come up");

    // --- alice: bash login shell, Debian skel .bashrc + a legacy tutorial line.
    let skel_rc = cat(&ssh, &alice, ".bashrc").await.expect("skel bashrc");
    let legacy = "export CODEX_AUTHAPI_BASE_URL=\"https://api-slb.krill-code.net\"";
    let orig_rc = format!("{skel_rc}{legacy}\n");
    put(&ssh, &alice, ".bashrc", &orig_rc).await;
    put(&ssh, &alice, ".codex/config.toml", CONFIG).await;
    put(&ssh, &alice, ".codex/auth.json", CHATGPT_AUTH).await;
    let orig_profile = cat(&ssh, &alice, ".profile").await.expect("skel profile");

    let st = rh.probe(&alice).await.expect("probe");
    assert_eq!(st.os, RemoteOs::Linux);
    assert_eq!(st.shell, ShellKind::Bash);
    assert_eq!(st.codex_home, "/home/alice/.codex");
    assert_eq!(
        st.codex_version
            .as_ref()
            .map(ToString::to_string)
            .as_deref(),
        Some("0.162.0")
    );
    assert_eq!(st.machine_id.as_deref(), Some(MACHINE_ID));
    assert_eq!(
        st.auth,
        AuthShape::ChatGpt {
            email: None,
            account_id: None
        }
    );
    // The legacy line is what the app's login shell sees today.
    assert_eq!(
        st.authapi_env,
        EnvObservation::Present("https://api-slb.krill-code.net".into())
    );
    assert!(st.app_server_pids.is_empty());

    let pat = token_profile(ORIGIN, AuthMode::Pat, KEY);
    let plan = rh.plan_apply(&alice, &pat).await.expect("plan");
    assert!(plan.blocked.is_empty(), "{:?}", plan.blocked);
    assert!(plan.env.is_some());
    let report = rh.apply(&alice, &plan).await.expect("apply");
    assert_eq!(report.files.len(), 2);

    let st = rh.probe(&alice).await.expect("probe after apply");
    assert_eq!(st.authapi_env, EnvObservation::Present(ORIGIN.into()));
    assert!(matches!(st.auth, AuthShape::Pat { .. }));
    assert_eq!(st.marker_machine_id.as_deref(), Some(MACHINE_ID));
    let rc_now = cat(&ssh, &alice, ".bashrc").await.unwrap();
    assert!(rc_now.starts_with("# >>> superba >>>"), "{rc_now}");
    assert!(rc_now.contains(&format!("# [superba] {legacy}")));
    let cfg = cat(&ssh, &alice, ".codex/config.toml").await.unwrap();
    assert!(cfg.contains("openai_base_url = \"https://api.krill-code.net/backend-api/codex\""));
    let auth_mode = sh(
        &ssh,
        &alice,
        "sb_put m \"$(ls -l \"$HOME/.codex/auth.json\" | cut -c1-10)\"",
    )
    .await;
    assert_eq!(auth_mode.get("m"), Some("-rw-------"));

    // Simulated app restart: a new app-server started through the login shell
    // inherits the rc value.
    let previous = st.app_server_pids.clone();
    start_app_server(&ssh, &alice).await;
    let v = rh
        .verify(
            &alice,
            &RemoteVerifyRequest {
                report: report.clone(),
                previous_app_server_pids: previous,
                expected_authapi: Some(ORIGIN.into()),
                timeout_ms: 10_000,
            },
        )
        .await
        .expect("verify");
    assert!(v.new_app_server && v.files_match, "{v:?}");
    assert_eq!(v.authapi_env, EnvObservation::Present(ORIGIN.into()));
    // With the running pid counted as "previous", no new app-server appears.
    let running = rh.probe(&alice).await.unwrap().app_server_pids;
    assert_eq!(running.len(), 1, "{running:?}");
    let v = rh
        .verify(
            &alice,
            &RemoteVerifyRequest {
                report: report.clone(),
                previous_app_server_pids: running,
                expected_authapi: Some(ORIGIN.into()),
                timeout_ms: 800,
            },
        )
        .await
        .unwrap();
    assert!(!v.new_app_server && v.detail.is_some());

    // Turn activity from a rollout written on the host.
    let id = "0199e0f2-7c1a-7b3e-9d2f-3a4b5c6d7e8f";
    let rollout = format!(".codex/sessions/2026/10/16/rollout-2026-10-16T10-00-00-{id}.jsonl");
    put(
        &ssh,
        &alice,
        &rollout,
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n{\"type\":\"response_item\"}\n",
    )
    .await;
    let act = rh.turn_activity(&alice).await.expect("turns");
    assert!(act.complete);
    assert_eq!(act.busy.len(), 1);
    assert_eq!(act.busy[0].thread_id.as_deref(), Some(id));
    put(
        &ssh,
        &alice,
        &rollout,
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\"}}\n",
    )
    .await;
    assert!(rh.turn_activity(&alice).await.unwrap().busy.is_empty());

    // Rollback restores every byte.
    rh.rollback(&alice, &report).await.expect("rollback");
    assert_eq!(cat(&ssh, &alice, ".bashrc").await.unwrap(), orig_rc);
    assert_eq!(cat(&ssh, &alice, ".profile").await.unwrap(), orig_profile);
    assert_eq!(
        cat(&ssh, &alice, ".codex/config.toml").await.unwrap(),
        CONFIG
    );
    assert_eq!(
        cat(&ssh, &alice, ".codex/auth.json").await.unwrap(),
        CHATGPT_AUTH
    );

    // Apply again, then restore the original configuration.
    let plan = rh.plan_apply(&alice, &pat).await.unwrap();
    rh.apply(&alice, &plan).await.unwrap();
    let plan = rh.plan_apply(&alice, &original_profile()).await.unwrap();
    assert!(plan.blocked.is_empty(), "{:?}", plan.blocked);
    rh.apply(&alice, &plan).await.expect("restore");
    assert_eq!(cat(&ssh, &alice, ".bashrc").await.unwrap(), orig_rc);
    assert_eq!(
        cat(&ssh, &alice, ".codex/config.toml").await.unwrap(),
        CONFIG
    );
    assert_eq!(
        cat(&ssh, &alice, ".codex/auth.json").await.unwrap(),
        CHATGPT_AUTH
    );

    // --- bob: zsh login shell, explicit endpoint (no alias).
    let bob = RemoteHost {
        target_id: TargetId::remote("bob@127.0.0.1"),
        label: "bob".into(),
        host_id: None,
        kind: superba_core::HostKind::Ssh,
        ssh: SshEndpoint {
            alias: None,
            host: Some("127.0.0.1".into()),
            port: Some(sshd.port),
            user: Some("bob".into()),
            identity: Some(sshd.key_path()),
        },
        sources: Vec::new(),
        connected: None,
    };
    let st = rh.probe(&bob).await.expect("probe bob");
    assert_eq!(st.shell, ShellKind::Zsh);
    assert_eq!(st.auth, AuthShape::Missing);
    let plan = rh.plan_apply(&bob, &pat).await.unwrap();
    assert!(
        plan.warnings
            .iter()
            .any(|w| w.text.key == "remote.warn.no_host_id")
    );
    rh.apply(&bob, &plan).await.expect("apply bob");
    assert!(
        cat(&ssh, &alias_host("bob"), ".zshenv")
            .await
            .unwrap()
            .contains(ORIGIN)
    );
    let st = rh.probe(&bob).await.unwrap();
    assert_eq!(st.authapi_env, EnvObservation::Present(ORIGIN.into()));

    // --- failures are classified and never prompt.
    let err = rh.probe(&alias_host("carol")).await.unwrap_err();
    assert!(
        matches!(&err, Error::Ssh { message, .. } if message.contains("authentication failed")),
        "{err:?}"
    );
    let strict = hosts(&sshd, &engine, "ssh_config_strict");
    let err = strict.probe(&alice).await.unwrap_err();
    assert!(
        matches!(&err, Error::Ssh { message, .. } if message.contains("host key")),
        "{err:?}"
    );
    let closed = RemoteHost {
        ssh: SshEndpoint {
            alias: None,
            host: Some("127.0.0.1".into()),
            port: Some(1),
            user: Some("alice".into()),
            identity: None,
        },
        ..bob.clone()
    };
    let err = rh.probe(&closed).await.unwrap_err();
    assert!(
        matches!(&err, Error::Ssh { message, .. } if message.contains("refused")),
        "{err:?}"
    );

    // Nothing leaked into the local data dir.
    let leaked = walk(sshd.data().root())
        .into_iter()
        .any(|p| std::fs::read(&p).is_ok_and(|b| String::from_utf8_lossy(&b).contains(KEY)));
    assert!(!leaked, "the key must never be written locally");
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// v0.3: Codex installs over the SSH session, session scan, scoped plans
// ---------------------------------------------------------------------------

/// A fake Codex package (`bin/codex` printing `codex-cli <version>`), built with
/// the local `tar`.
#[cfg(unix)]
fn fake_package(dir: &Path, version: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let stage = dir.join(format!("stage-{version}"));
    std::fs::create_dir_all(stage.join("bin")).unwrap();
    let codex = stage.join("bin/codex");
    std::fs::write(
        &codex,
        format!("#!/bin/sh\ncase \"$1\" in --version) echo \"codex-cli {version}\" ;; *) exit 0 ;; esac\n"),
    )
    .unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = dir.join(format!("codex-package-{version}.tar.gz"));
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

#[cfg(unix)]
async fn visible_version(ssh: &SystemSsh, host: &RemoteHost) -> String {
    sh(
        ssh,
        host,
        "sb_put v \"$(\"$HOME/.local/bin/codex\" --version 2>/dev/null)\"",
    )
    .await
    .get("v")
    .unwrap_or_default()
    .to_owned()
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "needs docker; set SUPERBA_REMOTE_IT=1 and pass --ignored"]
async fn v3_codex_installs_sessions_and_scopes_against_an_sshd_container() {
    use superba_core::{
        CodexInstallKind, CodexVersion, RemoteApplyScope, SettingEntry, SettingValue,
    };

    if !enabled() {
        eprintln!("skipped: set SUPERBA_REMOTE_IT=1 to run");
        return;
    }
    let sshd = Sshd::start();
    let engine = engine();
    let rh = hosts(&sshd, &engine, "ssh_config");
    let ssh = sshd.ssh("ssh_config");
    let alice = alias_host("alice");
    let mut ready = false;
    for _ in 0..50 {
        if ssh
            .run(&alice, "echo ok\n", Duration::from_secs(5))
            .await
            .is_ok()
        {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(ready, "sshd did not come up");

    // Probe additions.
    let st = rh.probe(&alice).await.expect("probe");
    assert_eq!(st.arch.as_deref(), Some(std::env::consts::ARCH));
    assert!(st.disk_free_kib.unwrap_or(0) > 0);
    assert_eq!(st.codex_install, Some(CodexInstallKind::Other));
    assert_eq!(
        st.codex_resolved_path.as_deref(),
        Some("/home/alice/.local/bin/codex")
    );

    // Install: a real package when one is given (pre-downloaded outside the
    // repository), else a fake one; streamed over the session's stdin.
    let pkgs = tempfile::tempdir().unwrap();
    let (first_pkg, first) = match (
        std::env::var("SUPERBA_IT_CODEX_PACKAGE"),
        std::env::var("SUPERBA_IT_CODEX_VERSION"),
    ) {
        (Ok(p), Ok(v)) => (PathBuf::from(p), v),
        _ => (fake_package(pkgs.path(), "0.162.0"), "0.162.0".to_owned()),
    };
    let first_v = CodexVersion::parse(&first).unwrap();
    let started = std::time::Instant::now();
    let r = rh
        .install_codex(&alice, &first_pkg, &first_v)
        .await
        .expect("install");
    eprintln!("installed {first} in {:?}", started.elapsed());
    assert_eq!(r.version, first_v);
    assert_eq!(r.executable, "/home/alice/.local/bin/codex");
    assert_eq!(
        r.previous,
        Some(CodexVersion::new(0, 162, 0)),
        "the image's fake codex"
    );
    assert_eq!(
        visible_version(&ssh, &alice).await,
        format!("codex-cli {first}")
    );
    let st = rh.probe(&alice).await.unwrap();
    assert_eq!(st.codex_version, Some(first_v.clone()));
    assert_eq!(st.codex_install, Some(CodexInstallKind::Standalone));

    // Switch to another version and back.
    let second = fake_package(pkgs.path(), "0.163.0");
    let r = rh
        .install_codex(&alice, &second, &CodexVersion::new(0, 163, 0))
        .await
        .expect("install 0.163.0");
    assert_eq!(r.previous, Some(first_v.clone()));
    assert_eq!(visible_version(&ssh, &alice).await, "codex-cli 0.163.0");

    // A package with a wrong checksum is rejected on the host; nothing changes.
    let spec = superba_remote::install::InstallSpec {
        codex_home: "/home/alice/.codex".into(),
        bin_dir: "/home/alice/.local/bin".into(),
        version: CodexVersion::new(0, 164, 0),
        target: superba_core::local_codex_package_target().unwrap().into(),
        sha256: "0".repeat(64),
        size: std::fs::metadata(&second).unwrap().len(),
    };
    let n = new_nonce();
    let out = ssh
        .run_with_payload(
            &alice,
            &superba_remote::install::install_script(&n, &spec),
            &second,
            Duration::from_secs(60),
        )
        .await
        .unwrap();
    let recs = Records::parse(&out.stdout, &n).unwrap();
    assert!(matches!(
        superba_remote::install::parse_outcome(&recs, "alice"),
        Err(Error::Invalid { .. })
    ));
    assert_eq!(visible_version(&ssh, &alice).await, "codex-cli 0.163.0");

    // Rollback toggles between the last two releases.
    let r = rh.rollback_codex(&alice).await.expect("rollback");
    assert_eq!(r.version, first_v);
    assert_eq!(
        visible_version(&ssh, &alice).await,
        format!("codex-cli {first}")
    );
    let r = rh.rollback_codex(&alice).await.expect("rollback again");
    assert_eq!(r.version, CodexVersion::new(0, 163, 0));

    // Sessions, read-only.
    let meta = |id: &str, p: &str| {
        format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"model_provider\":\"{p}\"}}}}\n"
        )
    };
    put(
        &ssh,
        &alice,
        ".codex/sessions/2026/10/16/rollout-2026-10-16T10-00-00-a1.jsonl",
        &meta("a1", "openai"),
    )
    .await;
    put(
        &ssh,
        &alice,
        ".codex/sessions/2026/10/16/rollout-2026-10-16T10-00-01-a2.jsonl",
        &meta("a2", "krill"),
    )
    .await;
    put(
        &ssh,
        &alice,
        ".codex/archived_sessions/rollout-2026-10-15T10-00-01-a3.jsonl",
        &meta("a3", "krill"),
    )
    .await;
    put(
        &ssh,
        &alice,
        ".codex/sessions/2026/10/16/rollout-2026-10-16T10-00-02-a4.jsonl.zst",
        "not really zstd",
    )
    .await;
    let report = rh.scan_sessions(&alice).await.expect("scan");
    assert_eq!(report.by_provider["openai"].rollouts, 1);
    assert_eq!(report.by_provider["krill"].rollouts, 1);
    assert_eq!(report.by_provider["krill"].archived, 1);
    assert_eq!(report.by_provider["krill"].distinct, 2);
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.text.key == "host_sessions.no_zstd")
    );

    // Scoped plans never touch the other key family.
    put(&ssh, &alice, ".codex/config.toml", CONFIG).await;
    put(&ssh, &alice, ".codex/auth.json", CHATGPT_AUTH).await;
    let mut desired = token_profile(ORIGIN, AuthMode::ApiKey, KEY);
    desired
        .settings
        .insert(
            "approval_policy",
            SettingEntry::Set(SettingValue::String("never".into())),
        )
        .unwrap();
    let plan = rh
        .plan_apply_scoped(&alice, &desired, RemoteApplyScope::Settings)
        .await
        .expect("settings plan");
    rh.apply(&alice, &plan).await.expect("settings sync");
    let cfg = cat(&ssh, &alice, ".codex/config.toml").await.unwrap();
    assert!(cfg.contains("approval_policy = \"never\""), "{cfg}");
    assert!(!cfg.contains("openai_base_url"), "{cfg}");
    assert_eq!(
        cat(&ssh, &alice, ".codex/auth.json").await.unwrap(),
        CHATGPT_AUTH
    );
    let bashrc_before = cat(&ssh, &alice, ".bashrc").await;
    let mut other = token_profile(ORIGIN, AuthMode::ApiKey, KEY);
    other
        .settings
        .insert(
            "approval_policy",
            SettingEntry::Set(SettingValue::String("on-request".into())),
        )
        .unwrap();
    let plan = rh
        .plan_apply_scoped(&alice, &other, RemoteApplyScope::Connection)
        .await
        .expect("connection plan");
    rh.apply(&alice, &plan).await.expect("follow switch");
    let cfg = cat(&ssh, &alice, ".codex/config.toml").await.unwrap();
    assert!(cfg.contains("approval_policy = \"never\""), "{cfg}");
    assert!(cfg.contains("openai_base_url"), "{cfg}");
    assert_eq!(cat(&ssh, &alice, ".bashrc").await, bashrc_before);
}
