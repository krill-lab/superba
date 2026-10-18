//! The file editor on remote hosts and WSL distributions (GitHub issue 9):
//! read `config.toml` / `auth.json` with the probe (so the host's `CODEX_HOME`
//! and Codex version come along), and replace one file with an edit through
//! the same write script as a switch: under the per-home lock, only when the
//! file still holds the bytes the editor opened (compare and swap), with a
//! backup next to it in `<CODEX_HOME>/.superba/backups/edit-<id>/`. No
//! first-write capture and no marker: the edit is the user's, not Superba's.

use superba_core::{CodexFileKind, Error, RemoteCodexFile, RemoteFileWrite, RemoteHost, Result};

use crate::SshRemoteHosts;
use crate::records::FileRead;
use crate::script::{self, Expect, FileOp, Mode, REMOTE_DIR, WriteSpec, new_nonce};
use crate::util::{new_apply_id, rjoin};

impl SshRemoteHosts {
    /// [`superba_core::RemoteHosts::read_codex_file`].
    pub(crate) async fn read_codex_file_impl(
        &self,
        host: &RemoteHost,
        kind: CodexFileKind,
    ) -> Result<RemoteCodexFile> {
        let snap = self.snapshot(host, true).await?;
        let (read, path) = match kind {
            CodexFileKind::ConfigToml => (&snap.config_read, snap.config_path()),
            CodexFileKind::AuthJson => (&snap.auth_read, snap.auth_path()),
        };
        let (contents, unreadable) = match read {
            FileRead::Absent => (None, None),
            FileRead::Bytes(b) => (Some(b.clone()), None),
            FileRead::Unreadable => (None, Some(format!("{path} cannot be read"))),
            FileRead::TooLarge => (
                None,
                Some(format!(
                    "{path} is larger than {} bytes",
                    script::MAX_READ_BYTES
                )),
            ),
        };
        let owned = self.store(host).load_state().await?.last_written;
        Ok(RemoteCodexFile {
            path,
            contents,
            unreadable,
            os: snap.state.os.clone(),
            codex_version: snap.state.codex_version.clone(),
            owned,
        })
    }

    /// [`superba_core::RemoteHosts::write_codex_file`].
    pub(crate) async fn write_codex_file_impl(
        &self,
        host: &RemoteHost,
        write: &RemoteFileWrite,
    ) -> Result<()> {
        let snap = self.snapshot(host, true).await?;
        let expected_path = match write.kind {
            CodexFileKind::ConfigToml => snap.config_path(),
            CodexFileKind::AuthJson => snap.auth_path(),
        };
        if expected_path != write.path {
            return Err(Error::Conflict {
                message: format!(
                    "the Codex home of {} moved ({} is now {expected_path}); open the file again",
                    host.label, write.path
                ),
            });
        }
        let id = new_apply_id(self.clock.now().as_millis());
        let spec = WriteSpec {
            codex_home: snap.state.codex_home.clone(),
            backup_dir: rjoin(
                &snap.state.codex_home,
                &format!("{REMOTE_DIR}/backups/edit-{id}"),
            ),
            first_write: false,
            marker: None,
            ops: vec![FileOp {
                path: write.path.clone(),
                expect: write
                    .expect
                    .as_ref()
                    .map_or(Expect::Absent, |b| Expect::Bytes(b.expose().to_vec())),
                new: Some(write.contents.expose().to_vec()),
                mode: match write.kind {
                    CodexFileKind::AuthJson => Mode::Private,
                    CodexFileKind::ConfigToml => Mode::Preserve,
                },
                backup: write.expect.is_some(),
            }],
        };
        let nonce = new_nonce();
        let body = script::write_script(&nonce, &spec);
        tracing::info!(host = %host.label, file = write.kind.file_name(), "saving an edited Codex file on a remote host");
        let recs = self.run(host, &body, &nonce, self.timeouts.write).await?;
        match recs.get("result") {
            Some("ok") => Ok(()),
            Some("conflict") => Err(Error::Conflict {
                message: format!(
                    "{} on {} changed since it was opened; nothing was written",
                    write.kind.file_name(),
                    host.label
                ),
            }),
            Some("locked") => Err(Error::Locked {
                what: format!(
                    "{} ({}/{REMOTE_DIR}/lock)",
                    host.label, snap.state.codex_home
                ),
            }),
            Some("partial") => Err(Error::Ssh {
                host: host.label.clone(),
                message: format!(
                    "the write failed and the old file could not be put back: {}",
                    recs.all("undo_failed").join(", ")
                ),
            }),
            _ => Err(Error::Ssh {
                host: host.label.clone(),
                message: format!(
                    "the write failed, nothing changed: {}",
                    recs.all("error").join("; ")
                ),
            }),
        }
    }
}
