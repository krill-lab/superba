//! The file editor on a remote host (GitHub issue 9): the exact read and
//! write scripts run under the local `/bin/sh` with an isolated `HOME`
//! ([`LocalShell`]). A save replaces the file only when it still holds what
//! the editor opened, keeps `auth.json` private and backs the old file up on
//! the host; it records no first-write image and no marker (the edit is the
//! user's).
#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use common::{engine, host};
use pretty_assertions::assert_eq;
use superba_core::{
    CodexFileKind, DataDir, Error, RemoteFileWrite, RemoteHost, RemoteHosts, SecretBytes,
};
use superba_remote::{LocalShell, SshRemoteHosts};

const CONFIG: &str = "# mine\nmodel = \"gpt-5\"\n";
const AUTH: &str = "{\"OPENAI_API_KEY\": \"sk-remote-000000000000abcd\"}\n";

fn write(path: &Path, contents: &str, mode: u32) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn setup() -> (tempfile::TempDir, std::path::PathBuf, SshRemoteHosts) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    write(&home.join(".codex/config.toml"), CONFIG, 0o644);
    write(&home.join(".codex/auth.json"), AUTH, 0o600);
    write(&home.join(".profile"), "", 0o644);
    let data = DataDir::new(tmp.path().join("data"));
    let ls = LocalShell::new(&home).with_login_shell("/nonexistent/shell");
    let hosts = SshRemoteHosts::new(engine(), data).with_shell(Arc::new(ls));
    (tmp, home, hosts)
}

fn devbox() -> RemoteHost {
    host("devbox", Some("remote-ssh-discovered:devbox"))
}

#[tokio::test]
async fn an_edit_replaces_the_file_it_was_opened_on_and_keeps_a_backup() {
    let (_tmp, home, hosts) = setup();
    let read = hosts
        .read_codex_file(&devbox(), CodexFileKind::ConfigToml)
        .await
        .unwrap();
    assert_eq!(read.path, format!("{}/.codex/config.toml", home.display()));
    assert_eq!(read.contents.as_ref().unwrap().expose(), CONFIG.as_bytes());
    assert!(read.unreadable.is_none());
    let new = "# mine\nmodel = \"gpt-6\"\n";
    hosts
        .write_codex_file(
            &devbox(),
            &RemoteFileWrite {
                kind: CodexFileKind::ConfigToml,
                path: read.path.clone(),
                expect: read.contents.clone(),
                contents: SecretBytes::new(new.as_bytes().to_vec()),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(home.join(".codex/config.toml")).unwrap(),
        new
    );
    // Mode kept; the old file backed up on the host; no first-write capture,
    // no marker, the lock released.
    let mode = std::fs::metadata(home.join(".codex/config.toml"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o644);
    let sb = home.join(".codex/.superba");
    let backups: Vec<_> = std::fs::read_dir(sb.join("backups"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(backups.len(), 1);
    assert!(
        backups[0]
            .file_name()
            .to_string_lossy()
            .starts_with("edit-")
    );
    assert!(!sb.join("first-write").exists());
    assert!(!sb.join("remote-state.json").exists());
    assert!(!sb.join("lock").exists());

    // The editor's view is stale now: a second save with it is refused.
    let err = hosts
        .write_codex_file(
            &devbox(),
            &RemoteFileWrite {
                kind: CodexFileKind::ConfigToml,
                path: read.path.clone(),
                expect: read.contents.clone(),
                contents: SecretBytes::new(b"model = \"x\"\n".to_vec()),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Conflict { .. }), "{err}");
    assert_eq!(
        std::fs::read_to_string(home.join(".codex/config.toml")).unwrap(),
        new
    );
}

#[tokio::test]
async fn a_new_auth_json_is_private_and_a_missing_file_reads_as_absent() {
    let (_tmp, home, hosts) = setup();
    std::fs::remove_file(home.join(".codex/auth.json")).unwrap();
    let read = hosts
        .read_codex_file(&devbox(), CodexFileKind::AuthJson)
        .await
        .unwrap();
    assert!(read.contents.is_none());
    hosts
        .write_codex_file(
            &devbox(),
            &RemoteFileWrite {
                kind: CodexFileKind::AuthJson,
                path: read.path,
                expect: None,
                contents: SecretBytes::new(AUTH.as_bytes().to_vec()),
            },
        )
        .await
        .unwrap();
    let path = home.join(".codex/auth.json");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), AUTH);
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}
