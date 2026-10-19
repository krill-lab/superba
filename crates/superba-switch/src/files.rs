//! The file editor through the service (GitHub issue 9): open, save, list and
//! read back `config.toml` / `auth.json` of this computer, a remote host or a
//! WSL distribution.
//!
//! - **Open** reads the file with what the check needs: the Codex versions
//!   reading the home (the app's bundled Codex and the CLI locally, the
//!   host's Codex remotely) with the settings catalog of that operating
//!   system, and the keys Superba owns there.
//! - **Save** runs under `state.lock` (never in the middle of a switch),
//!   refuses while a switch journal is pending, when the file changed since it
//!   was opened (compare and swap, locally and on the host), when it does not
//!   parse, and, without `force`, when the catalog check finds errors. The
//!   replaced file is sealed into the editor's backups first. The save is
//!   recorded: the background check names the editor as the source of what
//!   it changed in Superba's keys (GH#5, never reverted by itself; the next
//!   switch asks), and "in effect" knows which running Codex started before it
//!   (GH#4).
//! - **Backups** are the editor's own plus, for this computer, the pre-images
//!   of Superba's recent switches and the originals; a restore is a save of
//!   the backup's bytes ([`SaveReason::Restore`]) with a backup of its own.

use superba_config::codex_files::{self, CheckContext, EditRecord, EditorStore};
use superba_core::{
    ActiveState, BackupSource, BlockReason, CodexFileBackup, CodexFileBackupContents,
    CodexFileBackupQuery, CodexFileDoc, CodexFileKind, CodexFileSave, CodexFileSaved, ConsumerKind,
    Error, FileSchema, LocalizedText, ManualEdit, Notice, OwnedKey, RemoteFileWrite, RemoteHost,
    Result, SaveReason, SecretBytes, SettingsConsumer, TargetId,
    codex_files::{BOM, text_of, texts},
};

use crate::Switcher;
use crate::journal::StateLock;
use crate::messages::text;

/// Label of this computer in backups and documents (the UIs show their own).
const LOCAL_LABEL: &str = "This computer";

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// Where an opened file lives.
enum Home {
    Local,
    Remote(Box<RemoteHost>),
}

impl Switcher {
    fn editor_store(&self) -> EditorStore {
        EditorStore::new(self.deps.ctx.clone())
    }

    /// The editor's last save in the local home when it came after what
    /// Superba wrote last ("in effect", GH#4).
    pub(crate) fn local_edit_after(&self, active: Option<&ActiveState>) -> Option<ManualEdit> {
        let home = self.deps.ctx.codex_home();
        let edits = self.editor_store().edits(&TargetId::local()).ok()?;
        let latest = edits
            .into_iter()
            .filter(|e| {
                let path = match e.kind {
                    CodexFileKind::ConfigToml => home.config_toml(),
                    CodexFileKind::AuthJson => home.auth_json(),
                    CodexFileKind::ClaudeSettings => return false,
                };
                e.path == path.to_string_lossy()
            })
            .max_by_key(|e| e.at)?;
        if active.is_some_and(|a| a.applied_at >= latest.at) {
            return None;
        }
        Some(ManualEdit {
            at: latest.at,
            kind: latest.kind,
        })
    }

    /// The editor saved the local files as they are now: the time of the
    /// newest save when every changed part (`config.toml` keys, the
    /// credential) is in a file the editor wrote last (GH#5 attribution).
    pub(crate) fn editor_source(
        &self,
        state: &superba_core::ConfigState,
        keys: bool,
        auth: bool,
    ) -> Option<superba_core::Timestamp> {
        if !keys && !auth {
            return None;
        }
        let edits = self.editor_store().edits(&TargetId::local()).ok()?;
        let saved = |kind: CodexFileKind, live: Option<&String>| {
            edits
                .iter()
                .find(|e| e.kind == kind && Some(&e.after_sha256) == live)
                .map(|e| e.at)
        };
        let mut at = None;
        if keys {
            at = Some(saved(
                CodexFileKind::ConfigToml,
                state.config_sha256.as_ref(),
            )?);
        }
        if auth {
            let t = saved(CodexFileKind::AuthJson, state.auth_sha256.as_ref())?;
            at = Some(at.map_or(t, |a: superba_core::Timestamp| a.max(t)));
        }
        at
    }

    async fn open_home(&self, target: &TargetId) -> Result<Home> {
        if target.is_local() {
            return Ok(Home::Local);
        }
        let (host, _) = self.app_host(target).await?;
        Ok(Home::Remote(Box::new(host)))
    }

    /// [`superba_core::SwitchService::codex_file`].
    pub(crate) async fn codex_file_impl(
        &self,
        target: TargetId,
        kind: CodexFileKind,
    ) -> Result<CodexFileDoc> {
        let d = &self.deps;
        let now = d.clock.now();
        if kind == CodexFileKind::ClaudeSettings {
            if !target.is_local() {
                return Err(Error::Unsupported {
                    what: "Claude Code's settings.json on a remote host".into(),
                });
            }
            let file = self.claude_settings_file()?;
            let (owned, profile) = self.claude_owned();
            return Ok(self.doc(
                target,
                LOCAL_LABEL.to_owned(),
                kind,
                file.path.to_string_lossy().into_owned(),
                file.bytes.map(SecretBytes::new),
                FileSchema::default(),
                owned,
                false,
                profile,
                None,
                now,
            ));
        }
        match self.open_home(&target).await? {
            Home::Local => {
                let home = d.ctx.codex_home();
                let path = match kind {
                    CodexFileKind::ConfigToml => home.config_toml(),
                    CodexFileKind::AuthJson | CodexFileKind::ClaudeSettings => home.auth_json(),
                };
                let contents = match tokio::fs::read(&path).await {
                    Ok(b) => Some(SecretBytes::new(b)),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                    Err(e) => return Err(Error::io(&path, &e)),
                };
                let state = d.config.read_state(&home).await.ok();
                let owned_all = d.config.owned_keys(&home).await.unwrap_or_default();
                let owns_credential = state.as_ref().is_some_and(|s| {
                    s.handed_off.is_none()
                        && !owned_all.is_empty()
                        && s.active
                            .as_ref()
                            .and_then(|a| a.auth_mode)
                            .is_some_and(superba_core::AuthMode::uses_key)
                });
                let schema = match kind {
                    CodexFileKind::ConfigToml => {
                        let (consumers, _) = self.local_settings_inputs().await;
                        FileSchema {
                            consumers: consumers.into_iter().map(short_label).collect(),
                            catalog: Some(d.catalog.clone()),
                        }
                    }
                    CodexFileKind::AuthJson | CodexFileKind::ClaudeSettings => {
                        FileSchema::default()
                    }
                };
                let mut read_only = None;
                if state.as_ref().is_some_and(|s| s.journal_pending) {
                    read_only = Some(text("switch.codex_files.journal_pending"));
                }
                Ok(self.doc(
                    target,
                    LOCAL_LABEL.to_owned(),
                    kind,
                    path.to_string_lossy().into_owned(),
                    contents,
                    schema,
                    owned(kind, owned_all),
                    owns_credential && kind == CodexFileKind::AuthJson,
                    state.and_then(|s| s.active).and_then(|a| a.profile_id),
                    read_only,
                    now,
                ))
            }
            Home::Remote(host) => {
                let file = d.remote.read_codex_file(&host, kind).await?;
                let schema = match kind {
                    CodexFileKind::ConfigToml => FileSchema {
                        consumers: vec![SettingsConsumer::new(
                            ConsumerKind::Remote {
                                target_id: host.target_id.clone(),
                            },
                            host.label.clone(),
                            file.codex_version.clone(),
                        )],
                        catalog: Some((d.remote_catalog)(&file.os)),
                    },
                    CodexFileKind::AuthJson | CodexFileKind::ClaudeSettings => {
                        FileSchema::default()
                    }
                };
                let read_only = file
                    .unreadable
                    .as_ref()
                    .map(|why| text("switch.codex_files.unreadable").arg("detail", why));
                Ok(self.doc(
                    target,
                    host.label.clone(),
                    kind,
                    file.path,
                    file.contents,
                    schema,
                    owned(kind, file.owned),
                    false,
                    None,
                    read_only,
                    now,
                ))
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn doc(
        &self,
        target: TargetId,
        target_label: String,
        kind: CodexFileKind,
        path: String,
        contents: Option<SecretBytes>,
        schema: FileSchema,
        owned: Vec<OwnedKey>,
        owns_credential: bool,
        profile: Option<superba_core::ProfileId>,
        mut read_only: Option<LocalizedText>,
        read_at: superba_core::Timestamp,
    ) -> CodexFileDoc {
        if read_only.is_none()
            && contents
                .as_ref()
                .is_some_and(|b| text_of(b.expose()).is_none())
        {
            read_only = Some(text("switch.codex_files.not_utf8"));
        }
        CodexFileDoc {
            sha256: contents.as_ref().map(|b| sha256_hex(b.expose())),
            target,
            target_label,
            kind,
            path,
            contents,
            schema,
            owned,
            owns_credential,
            profile,
            read_only,
            notes: Vec::new(),
            read_at,
        }
    }

    /// [`superba_core::SwitchService::save_codex_file`].
    pub(crate) async fn save_codex_file_impl(&self, save: CodexFileSave) -> Result<CodexFileSaved> {
        self.refuse_elevated()?;
        let d = &self.deps;
        let _lock = StateLock::acquire(&d.ctx.data_dir)?;
        let doc = self.codex_file_impl(save.target.clone(), save.kind).await?;
        if doc.read_only.is_some() {
            if save.target.is_local()
                && d.config
                    .read_state(&d.ctx.codex_home())
                    .await
                    .is_ok_and(|s| s.journal_pending)
            {
                return Err(Error::Blocked {
                    reasons: vec![BlockReason::JournalPending],
                });
            }
            return Err(Error::Conflict {
                message: doc
                    .read_only
                    .as_ref()
                    .map(LocalizedText::render_fallback)
                    .unwrap_or_default(),
            });
        }
        if doc.sha256 != save.base_sha256 {
            return Err(Error::Conflict {
                message: format!(
                    "{} changed since it was opened; open it again (your edit was not saved)",
                    doc.path
                ),
            });
        }
        let mut new = save.contents.expose().to_vec();
        if doc.has_bom() && !new.starts_with(BOM) {
            new.splice(0..0, BOM.iter().copied());
        }
        let new_text = text_of(&new).ok_or_else(|| {
            Error::invalid(format!(
                "the new {} is not UTF-8 text",
                save.kind.file_name()
            ))
        })?;
        let old_text = doc.text().unwrap_or_default();
        let owner = doc.profile.as_ref().map(ToString::to_string);
        let problems = codex_files::validate(
            save.kind,
            &new_text,
            &CheckContext {
                schema: Some(&doc.schema),
                owned: &doc.owned,
                owner: owner.as_deref(),
                masker: None,
            },
        );
        if let Some(p) = problems.iter().find(|p| p.blocks_save()) {
            return Err(Error::invalid(format!(
                "{} line {}, column {}: {}",
                save.kind.file_name(),
                p.line,
                p.column,
                p.message.render_fallback()
            )));
        }
        if !save.force
            && let Some(p) = problems.iter().find(|p| p.needs_force())
        {
            return Err(Error::invalid(format!(
                "{} line {}, column {}: {} (save anyway to keep it)",
                save.kind.file_name(),
                p.line,
                p.column,
                p.message.render_fallback()
            )));
        }
        let now = d.clock.now();
        let new_sha = sha256_hex(&new);
        if doc.sha256.as_deref() == Some(new_sha.as_str())
            && !(doc.kind == CodexFileKind::ClaudeSettings
                && matches!(save.reason, SaveReason::Restore { .. }))
        {
            return Ok(CodexFileSaved {
                target: doc.target,
                kind: doc.kind,
                path: doc.path,
                sha256: new_sha,
                backup: None,
                owned_changed: Vec::new(),
                credential: None,
                runtime: None,
                notes: vec![Notice::info(text("switch.codex_files.unchanged"))],
                saved_at: now,
            });
        }

        if doc.kind == CodexFileKind::ClaudeSettings
            && let SaveReason::Restore { backup } = &save.reason
        {
            let store = self.editor_store();
            let (entry, contents) = store.harness_backup_read(backup)?;
            if entry.harness != superba_core::harness::HarnessId::ClaudeCode
                || contents.expose() != new
            {
                return Err(Error::invalid(
                    "The restore must use the selected Claude Code backup's exact contents.",
                ));
            }
            let writer = superba_config::harness::HarnessWriter::new(d.ctx.clone(), entry.harness);
            let id = backup.clone();
            let expected = doc.sha256.clone();
            let report = tokio::task::spawn_blocking(move || {
                writer.restore_backup_checked(&id, Some(expected.as_deref()), now)
            })
            .await
            .map_err(|e| Error::other(format!("restore task: {e}")))??;
            let backup = report
                .files
                .into_iter()
                .find_map(|f| f.backup)
                .map(|b| CodexFileBackup {
                    id: b.id,
                    target: TargetId::local(),
                    target_label: doc.target_label.clone(),
                    kind: CodexFileKind::ClaudeSettings,
                    at: b.at,
                    source: b.source,
                    size: b.size,
                    sha256: b.sha256,
                });
            return Ok(CodexFileSaved {
                target: doc.target,
                kind: doc.kind,
                path: doc.path,
                sha256: new_sha,
                backup,
                owned_changed: Vec::new(),
                credential: None,
                runtime: None,
                notes: report.notes,
                saved_at: now,
            });
        }

        // Back up what is there, then write.
        let store = self.editor_store();
        let source = match &save.reason {
            SaveReason::Restore { backup } => BackupSource::BeforeRestore {
                restored: backup.clone(),
            },
            _ => BackupSource::BeforeEdit,
        };
        let backup = match &doc.contents {
            Some(old) => {
                let (store, target, label, kind, old) = (
                    store.clone(),
                    doc.target.clone(),
                    doc.target_label.clone(),
                    doc.kind,
                    old.clone(),
                );
                Some(
                    tokio::task::spawn_blocking(move || {
                        store.backup(&target, &label, kind, old.expose(), source, now)
                    })
                    .await
                    .map_err(|e| Error::other(format!("backup task: {e}")))??,
                )
            }
            None => None,
        };
        match self.open_home(&save.target).await? {
            Home::Local => {
                let (path, kind, bytes, expected) = (
                    std::path::PathBuf::from(&doc.path),
                    doc.kind,
                    new.clone(),
                    doc.sha256.clone(),
                );
                tokio::task::spawn_blocking(move || {
                    codex_files::write_file(&path, kind, &bytes, expected.as_deref())
                })
                .await
                .map_err(|e| Error::other(format!("write task: {e}")))??;
            }
            Home::Remote(host) => {
                d.remote
                    .write_codex_file(
                        &host,
                        &RemoteFileWrite {
                            kind: doc.kind,
                            path: doc.path.clone(),
                            expect: doc.contents.clone(),
                            contents: SecretBytes::new(new.clone()),
                        },
                    )
                    .await?;
            }
        }
        tracing::info!(
            target = %doc.target,
            file = doc.kind.file_name(),
            reason = ?save.reason,
            backup = backup.as_ref().map(|b| b.id.as_str()),
            "saved an edited Codex file"
        );
        let record = EditRecord {
            target: doc.target.clone(),
            kind: doc.kind,
            path: doc.path.clone(),
            at: now,
            before_sha256: doc.sha256.clone(),
            after_sha256: new_sha.clone(),
        };
        if let Err(e) = store.record_edit(record) {
            tracing::warn!(error = %e, "recording the edit");
        }

        // What it means for Superba's keys (GH#5) and for running Codex (GH#4).
        let owned_changed = match doc.kind {
            CodexFileKind::ConfigToml => {
                codex_files::owned_changes(&old_text, &new_text, &doc.owned)
            }
            CodexFileKind::ClaudeSettings => {
                superba_config::claude::owned_changes(&old_text, &new_text, &doc.owned)
            }
            CodexFileKind::AuthJson => Vec::new(),
        };
        let credential = (doc.kind == CodexFileKind::AuthJson && doc.owns_credential)
            .then(|| {
                codex_files::credential_change(
                    doc.contents.as_ref().map(SecretBytes::expose),
                    Some(&new),
                )
            })
            .flatten();
        let mut notes = Vec::new();
        if !owned_changed.is_empty() || credential.is_some() {
            notes.push(Notice::info(texts::owned_promise()));
        }
        let runtime = if doc.kind == CodexFileKind::ClaudeSettings {
            notes.push(Notice::info(text("switch.claude.restart_needed")));
            None
        } else if doc.target.is_local() {
            notes.push(Notice::info(texts::restart_needed(doc.kind.file_name())));
            self.runtime_impl().await.ok()
        } else {
            notes.push(Notice::info(
                text("switch.codex_files.remote_restart")
                    .arg("file", doc.kind.file_name())
                    .arg("host", &doc.target_label),
            ));
            None
        };
        Ok(CodexFileSaved {
            target: doc.target,
            kind: doc.kind,
            path: doc.path,
            sha256: new_sha,
            backup,
            owned_changed,
            credential,
            runtime,
            notes,
            saved_at: now,
        })
    }

    /// [`superba_core::SwitchService::codex_file_backups`].
    pub(crate) async fn codex_file_backups_impl(
        &self,
        query: CodexFileBackupQuery,
    ) -> Result<Vec<CodexFileBackup>> {
        let store = self.editor_store();
        let home = self.deps.ctx.codex_home().root().to_path_buf();
        tokio::task::spawn_blocking(move || store.list(&query, &home, LOCAL_LABEL))
            .await
            .map_err(|e| Error::other(format!("backups task: {e}")))?
    }

    /// [`superba_core::SwitchService::codex_file_backup`].
    pub(crate) async fn codex_file_backup_impl(&self, id: &str) -> Result<CodexFileBackupContents> {
        let store = self.editor_store();
        let home = self.deps.ctx.codex_home().root().to_path_buf();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || store.read(&id, &home, LOCAL_LABEL))
            .await
            .map_err(|e| Error::other(format!("backup task: {e}")))?
    }
}

/// A short name for a consumer in the check's messages ("Codex CLI 0.162.0"
/// rather than the binary's path).
fn short_label(mut c: SettingsConsumer) -> SettingsConsumer {
    match c.consumer {
        ConsumerKind::DesktopApp => c.label = "ChatGPT app".into(),
        ConsumerKind::CliProcesses => c.label = "Codex CLI".into(),
        _ => {}
    }
    c
}

/// The owned keys of a `config.toml` (none for `auth.json`).
fn owned(kind: CodexFileKind, keys: Vec<superba_core::KeySnapshot>) -> Vec<OwnedKey> {
    match kind {
        CodexFileKind::ConfigToml => keys
            .into_iter()
            .map(|k| OwnedKey {
                path: k.path,
                written: k.value,
            })
            .collect(),
        CodexFileKind::AuthJson | CodexFileKind::ClaudeSettings => Vec::new(),
    }
}
