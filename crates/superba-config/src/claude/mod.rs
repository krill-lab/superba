//! Claude Code's user `settings.json` (user bug 79).
//!
//! [`ClaudeWriter`] owns only the keys of [`superba_core::claude::managed_paths`]
//! and keeps every other key with its value and place (the file is printed
//! again as JSON with two-space indentation; comments are not part of
//! JSON). Each write:
//!
//! 1. reads the file and refuses when it changed since the plan (sha256),
//!    is not a JSON object, or its `env` is not an object;
//! 2. seals the file it replaces into the editor's backups
//!    ([`crate::codex_files::EditorStore`], kind `ClaudeSettings`) — the
//!    first one as "Original, before Superba first wrote it", never pruned;
//! 3. replaces it atomically (staged next to it, hash checked again right
//!    before the rename, mode 0600: it holds a key);
//! 4. records what it wrote and, for each newly owned key, what was there
//!    before (`<data>/claude/state.enc`, encrypted).
//!
//! "Restore" puts the values from before Superba back (removing keys that
//! did not exist, the `env` block and the file itself when Superba created
//! them and they are empty) and forgets the state. Keys someone changed after
//! Superba wrote them are never written without an explicit take-over.
//!
//! [`ClaudeConfigStore`] keeps Superba's own Claude Code configuration
//! (`<data>/claude-code.json`).

pub mod json;
pub mod plan;

use std::path::{Path, PathBuf};

use superba_core::claude::ClaudeDesired;
use superba_core::{
    BackupSource, ClaudeApplied, ClaudeApplyReport, ClaudeConfig, ClaudeFileStatus, ClaudeHome,
    ClaudePlan, ClaudePlanKind, CodexFileKind, Error, LocalContext, LocalizedText, Notice,
    ProfileId, Result, TargetId, Timestamp,
};

use crate::codex_files::{EditorStore, write_file};
use crate::fsx;
use crate::vault::Vault;

pub use plan::{ClaudeState, PlannedChange};

/// Label of this computer in backups.
const LOCAL_LABEL: &str = "This computer";

/// `settings.json` as read now.
#[derive(Clone, Debug)]
pub struct SettingsFile {
    /// Its path.
    pub path: PathBuf,
    /// The bytes (`None` = absent).
    pub bytes: Option<Vec<u8>>,
    /// sha256 of the bytes.
    pub sha256: Option<String>,
    /// The parsed object (`None` when it is not one).
    pub value: Option<serde_json::Value>,
    /// Its state.
    pub status: ClaudeFileStatus,
}

/// Read `path`.
pub fn read_settings(path: &Path) -> Result<SettingsFile> {
    let bytes = fsx::read_optional(path)?;
    let (value, status) = json::parse(bytes.as_deref());
    Ok(SettingsFile {
        path: path.to_path_buf(),
        sha256: bytes.as_deref().map(fsx::sha256_hex),
        bytes,
        value,
        status,
    })
}

fn text(key: &'static str, fallback: &str) -> LocalizedText {
    LocalizedText::new(key, fallback)
}

/// Reads, plans and writes the keys Superba owns in Claude Code's
/// `settings.json` (see the module docs).
#[derive(Clone, Debug)]
pub struct ClaudeWriter {
    ctx: LocalContext,
    vault: Vault,
}

impl ClaudeWriter {
    /// A writer for `ctx` (its home, environment and data directory).
    pub fn new(ctx: LocalContext) -> Self {
        Self {
            vault: Vault::new(ctx.clone()),
            ctx,
        }
    }

    /// Claude Code's configuration directory.
    pub fn home(&self) -> ClaudeHome {
        ClaudeHome::resolve(self.ctx.env.as_ref(), &self.ctx.home_dir)
    }

    fn state_path(&self) -> PathBuf {
        self.ctx.data_dir.claude_dir().join("state.enc")
    }

    /// What Superba applied (`None` = nothing it owns now).
    pub fn state(&self) -> Result<Option<ClaudeState>> {
        match self.vault.read_sealed(&self.state_path())? {
            None => Ok(None),
            Some(b) => serde_json::from_slice(b.expose())
                .map(Some)
                .map_err(|e| Error::parse("claude/state.enc", e.to_string())),
        }
    }

    fn save_state(&self, state: Option<&ClaudeState>) -> Result<()> {
        match state {
            Some(s) => {
                let bytes = serde_json::to_vec(s)
                    .map_err(|e| Error::other(format!("serialize the Claude Code state: {e}")))?;
                self.vault.write_sealed(&self.state_path(), &bytes)
            }
            None => fsx::remove_if_exists(&self.state_path()),
        }
    }

    /// `settings.json` now.
    pub fn read(&self) -> Result<SettingsFile> {
        read_settings(&self.home().settings_json())
    }

    /// What Superba applied, with the owned keys someone changed since.
    pub fn applied(&self, file: &SettingsFile) -> Result<Option<ClaudeApplied>> {
        let Some(state) = self.state()? else {
            return Ok(None);
        };
        let drift = file
            .value
            .as_ref()
            .map(|v| state.drift(v))
            .unwrap_or_default();
        Ok(Some(ClaudeApplied {
            generation: state.generation,
            at: state.applied_at.unwrap_or_default(),
            provider: state.provider.clone(),
            owned: state.written.keys().cloned().collect(),
            drift,
        }))
    }

    /// Plan an apply of `desired` (`kind` = Apply) or a restore. Never
    /// writes.
    pub fn plan(
        &self,
        kind: ClaudePlanKind,
        desired: Option<ClaudeDesired>,
        provider: Option<ProfileId>,
    ) -> Result<ClaudePlan> {
        let file = self.read()?;
        let state = self.state()?;
        let current = writable(&file)?;
        let changes = match kind {
            ClaudePlanKind::Apply => {
                let d = desired
                    .as_ref()
                    .ok_or_else(|| Error::invalid("nothing to apply"))?;
                plan::plan_apply(current, d, state.as_ref())
            }
            ClaudePlanKind::Restore => match &state {
                Some(s) => plan::plan_restore(current, s),
                None => Vec::new(),
            },
        };
        let mut notes = Vec::new();
        if kind == ClaudePlanKind::Restore && state.is_none() {
            notes.push(Notice::info(text(
                "config.claude.nothing_owned",
                "Superba manages nothing in settings.json: there is nothing to restore.",
            )));
        }
        Ok(ClaudePlan {
            kind,
            settings_path: file.path.clone(),
            provider,
            base_sha256: file.sha256.clone(),
            changes: changes.iter().map(PlannedChange::display).collect(),
            notes,
            desired,
        })
    }

    /// Execute `plan` (see the module docs). `profile` names the backup.
    pub fn apply(
        &self,
        plan: &ClaudePlan,
        take_over: bool,
        now: Timestamp,
    ) -> Result<ClaudeApplyReport> {
        let file = self.read()?;
        if file.path != plan.settings_path {
            return Err(Error::Conflict {
                message: format!(
                    "Claude Code's settings moved to {} since the plan; plan again",
                    file.path.display()
                ),
            });
        }
        if file.sha256 != plan.base_sha256 {
            return Err(Error::Conflict {
                message: format!(
                    "{} changed since the plan; nothing was written (plan again)",
                    file.path.display()
                ),
            });
        }
        let current = writable(&file)?.clone();
        let state = self.state()?;
        let changes = match plan.kind {
            ClaudePlanKind::Apply => {
                let d = plan
                    .desired
                    .as_ref()
                    .ok_or_else(|| Error::invalid("the plan carries no values to apply"))?;
                plan::plan_apply(&current, d, state.as_ref())
            }
            ClaudePlanKind::Restore => match &state {
                Some(s) => plan::plan_restore(&current, s),
                None => Vec::new(),
            },
        };
        let mut applied = plan::apply_changes(&current, &changes, take_over)?;
        let mut notes = Vec::new();
        let mut remove_file = false;
        if plan.kind == ClaudePlanKind::Restore
            && let Some(s) = &state
        {
            if s.created_env {
                json::drop_empty_env(&mut applied.value);
            }
            remove_file = s.created_file
                && applied
                    .value
                    .as_object()
                    .is_some_and(serde_json::Map::is_empty);
        }
        let new_bytes = json::print(&applied.value);
        let unchanged = !remove_file && file.bytes.as_deref() == Some(new_bytes.as_slice());
        let next_generation = state.as_ref().map_or(0, |s| s.generation) + 1;

        // Back up what is there first.
        let editor = EditorStore::new(self.ctx.clone());
        let mut backup = None;
        if !unchanged && let Some(old) = &file.bytes {
            if state.is_none() && plan.kind == ClaudePlanKind::Apply {
                editor.backup(
                    &TargetId::local(),
                    LOCAL_LABEL,
                    CodexFileKind::ClaudeSettings,
                    old,
                    BackupSource::Original,
                    now,
                )?;
            }
            backup = Some(editor.backup(
                &TargetId::local(),
                LOCAL_LABEL,
                CodexFileKind::ClaudeSettings,
                old,
                BackupSource::BeforeSwitch {
                    generation: next_generation,
                    profile: plan.provider.clone(),
                },
                now,
            )?);
        }

        let mut sha256 = file.sha256.clone();
        if remove_file {
            if fsx::file_sha256(&file.path)? != file.sha256 {
                return Err(Error::Conflict {
                    message: format!(
                        "{} changed while it was being restored; nothing was written",
                        file.path.display()
                    ),
                });
            }
            fsx::remove_if_exists(&file.path)?;
            sha256 = None;
        } else if !unchanged {
            write_file(
                &file.path,
                CodexFileKind::ClaudeSettings,
                &new_bytes,
                file.sha256.as_deref(),
            )?;
            sha256 = Some(fsx::sha256_hex(&new_bytes));
        }

        match plan.kind {
            ClaudePlanKind::Apply => {
                let d = plan.desired.as_ref().expect("checked above");
                let mut next = plan::state_after_apply(
                    state.as_ref(),
                    &current,
                    &applied.value,
                    d,
                    &applied.kept,
                );
                if state.is_none() {
                    next.created_file = file.bytes.is_none();
                    next.created_env =
                        applied.created_env || current.get(superba_core::claude::KEY_ENV).is_none();
                }
                next.generation = next_generation;
                next.applied_at = Some(now);
                next.provider = plan.provider.clone();
                self.save_state(Some(&next))?;
                notes.push(Notice::info(text(
                    "config.claude.new_sessions",
                    "New Claude Code sessions use this; running sessions keep what they started with.",
                )));
            }
            ClaudePlanKind::Restore => {
                if applied.kept.is_empty() {
                    self.save_state(None)?;
                } else if let Some(mut s) = state.clone() {
                    // What someone changed stays theirs; Superba lets go of the rest.
                    s.written.retain(|p, _| applied.kept.contains(p));
                    s.baseline.retain(|p, _| applied.kept.contains(p));
                    self.save_state(Some(&s))?;
                }
                notes.push(Notice::info(text(
                    "config.claude.restored",
                    "Claude Code's settings are back to what they were before Superba (keys changed by someone else since were kept).",
                )));
            }
        }
        tracing::info!(
            kind = ?plan.kind,
            changed = applied.changed.len(),
            kept = applied.kept.len(),
            path = %file.path.display(),
            "wrote Claude Code's settings.json"
        );
        Ok(ClaudeApplyReport {
            kind: plan.kind,
            settings_path: file.path,
            sha256,
            backup,
            changed: applied.changed,
            kept: applied.kept,
            notes,
        })
    }
}

/// The parsed object of a writable file.
fn writable(file: &SettingsFile) -> Result<&serde_json::Value> {
    match (&file.value, &file.status) {
        (Some(v), ClaudeFileStatus::Absent | ClaudeFileStatus::Valid) => Ok(v),
        (_, ClaudeFileStatus::Invalid { line, column, .. }) => Err(Error::invalid(format!(
            "{} is not valid JSON (line {line}, column {column}); fix it first (Claude Code settings → file editor)",
            file.path.display()
        ))),
        (_, ClaudeFileStatus::NotObject) => Err(Error::invalid(format!(
            "{} is not a JSON object; fix it first",
            file.path.display()
        ))),
        (_, ClaudeFileStatus::Unreadable { message }) => Err(Error::invalid(format!(
            "{} cannot be read: {message}",
            file.path.display()
        ))),
        _ => Err(Error::invalid(format!(
            "{} cannot be read",
            file.path.display()
        ))),
    }
}

/// The keys Superba owns that `new` changes compared with `old` (both
/// `settings.json` texts): values for people (keys masked). Empty when either
/// text does not parse.
pub fn owned_changes(
    old: &str,
    new: &str,
    owned: &[superba_core::OwnedKey],
) -> Vec<superba_core::KeyChange> {
    let (Some(a), _) = json::parse(Some(old.as_bytes())) else {
        return Vec::new();
    };
    let (Some(b), _) = json::parse(Some(new.as_bytes())) else {
        return Vec::new();
    };
    owned
        .iter()
        .filter_map(|o| {
            let before = json::value_at(&a, &o.path);
            let after = json::value_at(&b, &o.path);
            (before != after).then(|| superba_core::KeyChange {
                path: o.path.clone(),
                before: before.map(|v| superba_core::claude::display_value(&o.path, &v)),
                after: after.map(|v| superba_core::claude::display_value(&o.path, &v)),
            })
        })
        .collect()
}

/// Superba's Claude Code configuration (`<data>/claude-code.json`).
#[derive(Clone, Debug)]
pub struct ClaudeConfigStore {
    path: PathBuf,
}

impl ClaudeConfigStore {
    /// The store of `ctx`'s data directory.
    pub fn new(ctx: &LocalContext) -> Self {
        Self {
            path: ctx.data_dir.claude_config_json(),
        }
    }

    /// The configuration (default when there is none).
    pub fn load(&self) -> Result<ClaudeConfig> {
        Ok(fsx::read_json::<ClaudeConfig>(&self.path, "claude-code.json")?.unwrap_or_default())
    }

    /// Save it.
    pub fn save(&self, config: &ClaudeConfig) -> Result<()> {
        fsx::write_json(&self.path, config)
    }
}
