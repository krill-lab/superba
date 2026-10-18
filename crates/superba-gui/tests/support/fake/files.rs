//! Fake service, area "files": the Codex file editor (GitHub issue 9).
//! Files live in memory per (home, file); a save checks the base hash like
//! the real service, keeps the replaced text as a backup and records it.

use std::collections::BTreeMap;

use superba_core::*;

use super::FakeService;

/// Fake state of the area.
#[derive(Debug, Default)]
pub(super) struct State {
    /// The files (`None` = absent).
    pub files: BTreeMap<(TargetId, CodexFileKind), Option<String>>,
    /// Keys Superba manages in each home's config.toml.
    pub owned: BTreeMap<TargetId, Vec<OwnedKey>>,
    /// Backups, oldest first, with their text.
    pub backups: Vec<(CodexFileBackup, String)>,
    /// Every save, in order.
    pub saves: Vec<(CodexFileKind, SaveReason, bool)>,
    /// The schema handed out with config.toml.
    pub schema: Option<FileSchema>,
    /// How long reading a home's files takes (a slow remote host).
    pub delays: BTreeMap<TargetId, std::time::Duration>,
    /// Reading a home's files fails with this SSH error.
    pub failing: BTreeMap<TargetId, String>,
    /// Homes whose reads do not answer until released (a slow host).
    pub held: std::collections::BTreeSet<TargetId>,
}

fn sha(text: &str) -> String {
    // Not a real hash: enough to tell versions apart in tests.
    format!(
        "{:x}-{}",
        text.len(),
        text.bytes().map(u64::from).sum::<u64>()
    )
}

impl FakeService {
    /// Put a file in the fake home.
    pub fn set_codex_file(&self, target: TargetId, kind: CodexFileKind, text: Option<&str>) {
        self.areas
            .files
            .lock()
            .unwrap()
            .files
            .insert((target, kind), text.map(str::to_owned));
    }

    /// The fake file now.
    pub fn codex_file_text(&self, target: &TargetId, kind: CodexFileKind) -> Option<String> {
        self.areas
            .files
            .lock()
            .unwrap()
            .files
            .get(&(target.clone(), kind))
            .cloned()
            .flatten()
    }

    /// Reading `target`'s files takes `delay` (a slow remote host; real time,
    /// also with `fast`).
    pub fn set_codex_file_delay(&self, target: TargetId, delay: std::time::Duration) {
        self.areas
            .files
            .lock()
            .unwrap()
            .delays
            .insert(target, delay);
    }

    /// Hold the reads of `target`'s files until released (`false`).
    pub fn hold_codex_file(&self, target: TargetId, hold: bool) {
        let mut st = self.areas.files.lock().unwrap();
        if hold {
            st.held.insert(target);
        } else {
            st.held.remove(&target);
        }
    }

    /// Reading `target`'s files fails with an SSH error (`None`: reads again).
    pub fn fail_codex_file(&self, target: TargetId, message: Option<&str>) {
        let mut st = self.areas.files.lock().unwrap();
        match message {
            Some(m) => st.failing.insert(target, m.to_owned()),
            None => st.failing.remove(&target),
        };
    }

    /// Keys Superba manages in a home.
    pub fn set_owned(&self, target: TargetId, owned: Vec<OwnedKey>) {
        self.areas.files.lock().unwrap().owned.insert(target, owned);
    }

    /// The schema of config.toml.
    pub fn set_file_schema(&self, schema: FileSchema) {
        self.areas.files.lock().unwrap().schema = Some(schema);
    }

    /// Saves so far: (file, reason, forced).
    pub fn file_saves(&self) -> Vec<(CodexFileKind, SaveReason, bool)> {
        self.areas.files.lock().unwrap().saves.clone()
    }
}

pub(super) async fn codex_file(
    fake: &FakeService,
    target: TargetId,
    kind: CodexFileKind,
) -> Result<CodexFileDoc> {
    let delay = fake
        .areas
        .files
        .lock()
        .unwrap()
        .delays
        .get(&target)
        .copied();
    if let Some(d) = delay {
        tokio::time::sleep(d).await;
    }
    while fake.areas.files.lock().unwrap().held.contains(&target) {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let st = fake.areas.files.lock().unwrap();
    if let Some(message) = st.failing.get(&target) {
        return Err(Error::Ssh {
            host: target.to_string(),
            message: message.clone(),
        });
    }
    let text = st.files.get(&(target.clone(), kind)).cloned().flatten();
    let path = if kind == CodexFileKind::ClaudeSettings {
        "/home/u/.claude/settings.json".to_owned()
    } else if target.is_local() {
        format!("/home/u/.codex/{}", kind.file_name())
    } else {
        format!("/home/dev/.codex/{}", kind.file_name())
    };
    Ok(CodexFileDoc {
        target_label: if target.is_local() {
            "This computer".into()
        } else {
            target.to_string()
        },
        kind,
        path,
        sha256: text.as_deref().map(sha),
        contents: text.map(|t| SecretBytes::new(t.into_bytes())),
        schema: match kind {
            CodexFileKind::ConfigToml => st.schema.clone().unwrap_or_default(),
            CodexFileKind::AuthJson | CodexFileKind::ClaudeSettings => FileSchema::default(),
        },
        owned: match kind {
            CodexFileKind::ConfigToml => st.owned.get(&target).cloned().unwrap_or_default(),
            CodexFileKind::AuthJson | CodexFileKind::ClaudeSettings => Vec::new(),
        },
        owns_credential: kind == CodexFileKind::AuthJson && st.owned.contains_key(&target),
        profile: Some(ProfileId::new("krill")),
        read_only: None,
        notes: Vec::new(),
        read_at: Timestamp::from_millis(1_790_000_000_000),
        target,
    })
}

pub(super) async fn save_codex_file(
    fake: &FakeService,
    save: CodexFileSave,
) -> Result<CodexFileSaved> {
    let mut st = fake.areas.files.lock().unwrap();
    let key = (save.target.clone(), save.kind);
    let current = st.files.get(&key).cloned().flatten();
    if current.as_deref().map(sha) != save.base_sha256 {
        return Err(Error::Conflict {
            message: "changed since it was opened".into(),
        });
    }
    let text = String::from_utf8(save.contents.expose().to_vec()).unwrap();
    let at = Timestamp::from_millis(1_790_000_000_000 + st.backups.len() as i64 * 60_000);
    let backup = current.map(|old| {
        let b = CodexFileBackup {
            id: format!(
                "20261018-0930{:02}-{}-ab12",
                st.backups.len(),
                save.kind.short_name()
            ),
            target: save.target.clone(),
            target_label: "This computer".into(),
            kind: save.kind,
            at,
            source: match &save.reason {
                SaveReason::Restore { backup } => BackupSource::BeforeRestore {
                    restored: backup.clone(),
                },
                _ => BackupSource::BeforeEdit,
            },
            size: old.len() as u64,
            sha256: None,
        };
        st.backups.push((b.clone(), old));
        b
    });
    st.saves.push((save.kind, save.reason.clone(), save.force));
    st.files.insert(key, Some(text.clone()));
    Ok(CodexFileSaved {
        target: save.target,
        kind: save.kind,
        path: format!("/home/u/.codex/{}", save.kind.file_name()),
        sha256: sha(&text),
        backup,
        owned_changed: Vec::new(),
        credential: None,
        runtime: None,
        notes: vec![Notice::info(codex_files::texts::restart_needed(
            save.kind.file_name(),
        ))],
        saved_at: at,
    })
}

pub(super) async fn codex_file_backups(
    fake: &FakeService,
    query: CodexFileBackupQuery,
) -> Result<Vec<CodexFileBackup>> {
    let st = fake.areas.files.lock().unwrap();
    let mut out: Vec<CodexFileBackup> = st
        .backups
        .iter()
        .map(|(b, _)| b.clone())
        .filter(|b| query.target.as_ref().is_none_or(|t| t == &b.target))
        .filter(|b| query.kind.is_none_or(|k| k == b.kind))
        .collect();
    out.reverse();
    Ok(out)
}

pub(super) async fn codex_file_backup(
    fake: &FakeService,
    id: &str,
) -> Result<CodexFileBackupContents> {
    let st = fake.areas.files.lock().unwrap();
    st.backups
        .iter()
        .find(|(b, _)| b.id == id)
        .map(|(b, t)| CodexFileBackupContents {
            backup: b.clone(),
            contents: SecretBytes::new(t.clone().into_bytes()),
        })
        .ok_or_else(|| Error::NotFound {
            what: format!("backup {id}"),
        })
}
