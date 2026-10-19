//! Writers for the other coding-agent harnesses: dsh, opencode, pi, ZCode
//! and WorkBuddy (`superba_core::harness`).
//!
//! Each harness ([`HarnessSpec`], one module per harness) names its files
//! and the entries Superba owns there for a [`HarnessTarget`] (one provider
//! with id `superba`, its models, the default model, ...). [`HarnessWriter`]
//! does the rest the same way for all of them, mirroring Claude Code's
//! `settings.json` writer (`superba_config::claude` on the `cc1` branch):
//!
//! 1. reads every file it may write and refuses when one is not valid in its
//!    format (or changed since the plan: sha256 compare-and-swap);
//! 2. changes only the owned entries, in place, with editors that keep
//!    comments, formatting and every other entry ([`yaml`], [`jsonc`],
//!    [`dotenv`]; each edit is re-parsed and checked to mean exactly the old
//!    document plus the change); entries someone changed after Superba wrote
//!    them are kept unless taken over ([`state`]);
//! 3. seals each file it replaces into the file editor's backups
//!    ([`crate::codex_files::EditorStore`], harness section) — the first one
//!    as "Original, before Superba first wrote it", never pruned;
//! 4. replaces the files atomically (staged next to them, hash checked
//!    again right before the rename; 0600 for files that hold a key), and
//!    puts back the ones already replaced when a later one fails;
//! 5. records what it wrote and what each owned entry held before
//!    (`<data>/harness/<id>/state.enc`, encrypted).
//!
//! "Restore" puts the values from before Superba back, takes out the
//! containers and files Superba created once they are empty, and forgets the
//! state. [`HarnessConfigStore`] keeps Superba's own settings
//! (`<data>/harnesses.json`).
//!
//! Merge note (Claude Code, `cc1`): `ClaudeWriter` is this engine with one
//! file and dotted paths; both can implement one `HarnessSpec`-like trait in
//! core after the merge (plan kind, change and report types are already
//! shaped alike), with Claude Code's `CodexFileKind::ClaudeSettings` backups
//! moving to the harness backup section or vice versa.

pub mod claude;
pub mod doc;
pub mod dotenv;
pub mod dsh;
pub mod gemini;
pub mod jsonc;
pub mod opencode;
pub mod pi;
pub mod schema;
pub mod slot;
pub mod state;
pub mod workbuddy;
pub mod yaml;
pub mod zcode;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use superba_core::harness::{
    HARNESS_CONFIG_SCHEMA, HarnessApplied, HarnessApplyReport, HarnessBackup, HarnessConfig,
    HarnessFileView, HarnessId, HarnessOptions, HarnessPlan, HarnessPlanFile, HarnessPlanKind,
    HarnessTarget, HarnessWrittenFile,
};
use superba_core::{
    BackupSource, Error, LocalContext, LocalizedText, Notice, Result, Secret, Timestamp,
};

use crate::codex_files::EditorStore;
use crate::fsx::{self, Mode};
use crate::vault::Vault;

pub use doc::{Doc, Format, HarnessFile, ReadFile};
pub use slot::{Seg, SlotPath};
pub use state::{DesiredFile, FileState, HarnessState, OwnedSlot, PlannedChange};

/// A `config.harness.*` text (the full key, so the catalog test finds it).
pub(crate) fn text(key: &str, fallback: &str) -> LocalizedText {
    LocalizedText::new(key, fallback)
}

/// One harness: its files and the entries Superba owns there.
pub trait HarnessSpec: Send + Sync {
    /// Which harness.
    fn id(&self) -> HarnessId;

    /// The files an apply writes for `options` now (paths resolved from the
    /// context's home and environment).
    fn files(&self, ctx: &LocalContext, options: &HarnessOptions) -> Result<Vec<HarnessFile>>;

    /// The entries each file should hold for `target` (`files` = the files of
    /// [`Self::files`] as read now, in that order).
    fn desired(&self, target: &HarnessTarget, files: &[ReadFile]) -> Result<Vec<DesiredFile>>;

    /// Check the files as they would be written (file, meaning); refuse
    /// before anything is written.
    fn check(&self, after: &[(HarnessFile, Value)]) -> Result<()> {
        let _ = after;
        Ok(())
    }

    /// Refuse keys the harness would read as something else.
    fn check_key(&self, key: &Secret) -> Result<()> {
        superba_core::harness::check_key_text(key).map_err(|k| {
            Error::invalid(match k {
                "harness.key.empty" => "the key is empty",
                _ => "the key contains spaces, quotes, backslashes or control characters; harness files cannot hold it",
            })
        })
    }

    /// The entry itself is a key (masked when shown).
    fn secret_slot(&self, path: &SlotPath) -> bool {
        let _ = path;
        false
    }

    /// Remarks for an apply or restore (when the harness picks it up, where
    /// the key is kept).
    fn notes(&self, kind: HarnessPlanKind, target: Option<&HarnessTarget>) -> Vec<Notice>;

    /// Remarks about the files and environment (overrides that hide
    /// Superba's entries, credentials that win over its key, ...).
    fn diagnostics(&self, ctx: &LocalContext, files: &[ReadFile]) -> Vec<Notice> {
        let _ = (ctx, files);
        Vec::new()
    }
}

/// The writer of `harness`.
pub fn spec(harness: HarnessId) -> &'static dyn HarnessSpec {
    match harness {
        HarnessId::ClaudeCode => &claude::ClaudeCode,
        HarnessId::Gemini => &gemini::Gemini,
        HarnessId::Dsh => &dsh::Dsh,
        HarnessId::OpenCode => &opencode::OpenCode,
        HarnessId::Pi => &pi::Pi,
        HarnessId::ZCode => &zcode::ZCode,
        HarnessId::WorkBuddy => &workbuddy::WorkBuddy,
    }
}

/// Everything a plan or an apply works from.
struct Prepared {
    files: Vec<ReadFile>,
    desired: Vec<DesiredFile>,
    changes: Vec<PlannedChange>,
    state: Option<HarnessState>,
}

fn invalid(file: &ReadFile) -> Error {
    use superba_core::harness::HarnessFileStatus as S;
    let p = file.file.path.display();
    Error::invalid(match &file.status {
        S::Invalid {
            line,
            column,
            message,
        } if *line > 0 => {
            format!("{p} is not valid (line {line}, column {column}: {message}); fix it first")
        }
        S::Invalid { message, .. } => format!("{p} cannot be edited safely: {message}"),
        S::Unreadable { message } => format!("{p} cannot be read: {message}"),
        _ => format!("{p} cannot be read"),
    })
}

/// Write `contents` to `path` only when it still has `expected` (sha256;
/// `None` = must not exist).
fn write_cas(path: &Path, contents: &[u8], expected: Option<&str>, private: bool) -> Result<()> {
    let target = fsx::resolve_write_target(path);
    let changed = || Error::Conflict {
        message: format!(
            "{} changed since the plan; nothing was written (plan again)",
            path.display()
        ),
    };
    if fsx::file_sha256(&target)?.as_deref() != expected {
        return Err(changed());
    }
    let mode = if private {
        Mode::Private
    } else {
        Mode::PreserveOrPrivate
    };
    let temp = fsx::stage(&target, contents, mode)?;
    if fsx::file_sha256(&target)?.as_deref() != expected {
        let _ = std::fs::remove_file(&temp);
        return Err(changed());
    }
    fsx::publish(&temp, &target)
}

/// The directories missing above `path`, deepest first (created by writing
/// it).
fn missing_dirs(path: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut dir = path.parent();
    while let Some(d) = dir {
        if d.as_os_str().is_empty() || d.exists() {
            break;
        }
        out.push(d.to_path_buf());
        dir = d.parent();
    }
    out
}

/// What happens to one file.
enum Action {
    Write(Vec<u8>),
    Remove,
}

/// Reads, plans and writes the entries Superba owns in one harness's files
/// (see the module docs).
#[derive(Clone)]
pub struct HarnessWriter {
    ctx: LocalContext,
    vault: Vault,
    spec: &'static dyn HarnessSpec,
}

impl std::fmt::Debug for HarnessWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HarnessWriter")
            .field("harness", &self.spec.id())
            .finish()
    }
}

impl HarnessWriter {
    /// A writer for `harness` in `ctx` (its home, environment and data dir).
    pub fn new(ctx: LocalContext, harness: HarnessId) -> Self {
        Self {
            vault: Vault::new(ctx.clone()),
            ctx,
            spec: spec(harness),
        }
    }

    /// The harness.
    pub fn harness(&self) -> HarnessId {
        self.spec.id()
    }

    /// The harness's writer.
    pub fn spec(&self) -> &'static dyn HarnessSpec {
        self.spec
    }

    fn state_path(&self) -> PathBuf {
        self.ctx
            .data_dir
            .harness_dir(self.spec.id().as_str())
            .join("state.enc")
    }

    /// What Superba applied (`None` = nothing it owns now).
    pub fn state(&self) -> Result<Option<HarnessState>> {
        match self.vault.read_sealed(&self.state_path())? {
            None => Ok(None),
            Some(b) => serde_json::from_slice(b.expose())
                .map(Some)
                .map_err(|e| Error::parse("harness state", e.to_string())),
        }
    }

    fn save_state(&self, state: Option<&HarnessState>) -> Result<()> {
        match state {
            Some(s) if !s.files.is_empty() => {
                let bytes = serde_json::to_vec(s)
                    .map_err(|e| Error::other(format!("serialize the harness state: {e}")))?;
                self.vault.write_sealed(&self.state_path(), &bytes)
            }
            _ => fsx::remove_if_exists(&self.state_path()),
        }
    }

    /// The files an apply with `options` writes plus those Superba wrote
    /// before, as read now (in that order, each once).
    pub fn read_files(&self, options: &HarnessOptions) -> Result<Vec<ReadFile>> {
        let state = self.state()?;
        self.read_with(options, state.as_ref(), true)
    }

    fn read_with(
        &self,
        options: &HarnessOptions,
        state: Option<&HarnessState>,
        targets: bool,
    ) -> Result<Vec<ReadFile>> {
        let mut files: Vec<HarnessFile> = if targets {
            self.spec.files(&self.ctx, options)?
        } else {
            Vec::new()
        };
        if let Some(s) = state {
            for (key, fs) in &s.files {
                if !files.iter().any(|f| f.key == *key) {
                    files.push(fs.file(key));
                }
            }
        }
        files.iter().map(ReadFile::read).collect()
    }

    /// The files for people (targets and previously written ones).
    pub fn file_views(&self, options: &HarnessOptions) -> Result<Vec<HarnessFileView>> {
        let backups = self.backups()?;
        Ok(self
            .read_files(options)?
            .iter()
            .map(|f| {
                let mut view = f.view();
                view.backups = backups.iter().filter(|b| b.file == view.key).count();
                view
            })
            .collect())
    }

    /// What Superba applied, with the owned entries someone changed since.
    pub fn applied(&self) -> Result<Option<HarnessApplied>> {
        let Some(state) = self.state()? else {
            return Ok(None);
        };
        let files = self.read_with(&HarnessOptions::default(), Some(&state), false)?;
        let current: BTreeMap<String, Value> = files
            .iter()
            .map(|f| (f.file.key.clone(), f.value()))
            .collect();
        Ok(Some(HarnessApplied {
            generation: state.generation,
            at: state.applied_at.unwrap_or_default(),
            provider: state.provider.clone(),
            protocol: state.protocol,
            owned: state.owned(),
            drift: state.drift(&current),
        }))
    }

    /// Remarks about the files and environment now.
    pub fn diagnostics(&self, options: &HarnessOptions) -> Result<Vec<Notice>> {
        let files = self.read_files(options)?;
        Ok(self.spec.diagnostics(&self.ctx, &files))
    }

    fn prepare(&self, kind: HarnessPlanKind, target: Option<&HarnessTarget>) -> Result<Prepared> {
        let state = self.state()?;
        let options = target.map(|t| t.options.clone()).unwrap_or_default();
        let apply = kind == HarnessPlanKind::Apply;
        if apply {
            let t = target.ok_or_else(|| Error::invalid("nothing to apply"))?;
            if !t.harness.speaks(t.endpoint.protocol) {
                return Err(Error::invalid("the tool does not support this protocol"));
            }
            self.spec.check_key(&t.key)?;
            if t.models.is_empty() && t.harness != HarnessId::ClaudeCode {
                return Err(Error::invalid(
                    "no models to write: the provider lists none (pick models with --model)",
                ));
            }
        }
        let files = self.read_with(&options, state.as_ref(), apply)?;
        let targets: Vec<ReadFile> = if apply {
            let keys: Vec<String> = self
                .spec
                .files(&self.ctx, &options)?
                .into_iter()
                .map(|f| f.key)
                .collect();
            files
                .iter()
                .filter(|f| keys.contains(&f.file.key))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        let desired = match (apply, target) {
            (true, Some(t)) => self.spec.desired(t, &targets)?,
            _ => Vec::new(),
        };
        // Every file with something to plan must be readable.
        for f in &files {
            let touched = desired.iter().any(|d| d.file.key == f.file.key)
                || state
                    .as_ref()
                    .is_some_and(|s| s.files.contains_key(&f.file.key));
            if touched && f.doc.is_none() {
                return Err(invalid(f));
            }
        }
        let current: BTreeMap<String, Value> = files
            .iter()
            .map(|f| (f.file.key.clone(), f.value()))
            .collect();
        for d in &desired {
            let doc = current.get(&d.file.key).cloned().unwrap_or(Value::Null);
            for (p, _) in &d.slots {
                slot::check_unique(&doc, p)
                    .map_err(|m| Error::invalid(format!("{}: {m}", d.file.path.display())))?;
            }
        }
        let changes = if apply {
            state::plan_apply(&current, &desired, state.as_ref())
        } else {
            match &state {
                Some(s) => state::plan_restore(&current, s),
                None => Vec::new(),
            }
        };
        Ok(Prepared {
            files,
            desired,
            changes,
            state,
        })
    }

    fn secret_path(&self, path: &SlotPath) -> bool {
        self.spec.secret_slot(path)
    }

    /// Plan an apply of `target` (`kind` = Apply) or a restore. Never writes.
    pub fn plan(
        &self,
        kind: HarnessPlanKind,
        target: Option<HarnessTarget>,
    ) -> Result<HarnessPlan> {
        let p = self.prepare(kind, target.as_ref())?;
        let mut notes = Vec::new();
        if kind == HarnessPlanKind::Restore && p.state.is_none() {
            notes.push(Notice::info(text(
                "config.harness.nothing_owned",
                "Superba manages nothing in this harness's files: there is nothing to restore.",
            )));
        }
        // A trial run of the edits: refuses now what the apply would refuse.
        self.edited(&p, true)?;
        notes.extend(self.spec.diagnostics(&self.ctx, &p.files));
        Ok(HarnessPlan {
            kind,
            harness: self.spec.id(),
            provider: target.as_ref().and_then(|t| t.provider.clone()),
            protocol: target.as_ref().map(|t| t.endpoint.protocol),
            files: p
                .files
                .iter()
                .map(|f| HarnessPlanFile {
                    key: f.file.key.clone(),
                    path: f.file.path.clone(),
                    base_sha256: f.sha256.clone(),
                })
                .collect(),
            changes: p
                .changes
                .iter()
                .map(|c| c.display(self.secret_path(&c.path)))
                .collect(),
            notes,
            desired: target,
        })
    }

    /// The owned entries whose value differs from what `target` wants (`<file>
    /// <slot>`).
    pub fn pending(&self, target: &HarnessTarget) -> Result<Vec<String>> {
        Ok(self
            .prepare(HarnessPlanKind::Apply, Some(target))?
            .changes
            .iter()
            .map(PlannedChange::id)
            .collect())
    }

    /// Apply the planned changes to the documents (drift only with
    /// `take_over`), take out empty containers Superba created, and decide
    /// what happens to each file.
    #[allow(clippy::type_complexity)]
    fn edited(
        &self,
        p: &Prepared,
        take_over: bool,
    ) -> Result<(
        BTreeMap<String, Action>,
        BTreeMap<String, Value>,
        Vec<String>,
        Vec<String>,
    )> {
        let mut docs: BTreeMap<String, Doc> = p
            .files
            .iter()
            .filter_map(|f| f.doc.clone().map(|d| (f.file.key.clone(), d)))
            .collect();
        let mut changed = Vec::new();
        let mut kept = Vec::new();
        for c in &p.changes {
            if c.drift && !take_over {
                kept.push(c.id());
                continue;
            }
            let path = p
                .files
                .iter()
                .find(|f| f.file.key == c.file)
                .map(|f| f.file.path.display().to_string())
                .unwrap_or_default();
            let doc = docs
                .get_mut(&c.file)
                .ok_or_else(|| Error::other(format!("no document for {}", c.file)))?;
            doc.set(&c.path, c.after.as_ref())
                .map_err(|m| Error::invalid(format!("{path}: {m}")))?;
            changed.push(c.id());
        }
        // Containers Superba created that hold nothing now go again.
        if let Some(s) = &p.state {
            for (key, fs) in &s.files {
                let Some(doc) = docs.get_mut(key) else {
                    continue;
                };
                for c in state::created_deepest_first(fs) {
                    let empty =
                        slot::get(doc.value(), &c).is_some_and(|v| slot::is_skeleton(v, c.last()));
                    if empty {
                        doc.set(&c, None)
                            .map_err(|m| Error::invalid(format!("{}: {m}", fs.path.display())))?;
                    }
                }
            }
        }
        let after: BTreeMap<String, Value> = docs
            .iter()
            .map(|(k, d)| (k.clone(), d.value().clone()))
            .collect();
        let check: Vec<(HarnessFile, Value)> = p
            .files
            .iter()
            .filter(|f| p.desired.iter().any(|d| d.file.key == f.file.key))
            .map(|f| {
                (
                    f.file.clone(),
                    after.get(&f.file.key).cloned().unwrap_or(Value::Null),
                )
            })
            .collect();
        self.spec.check(&check)?;
        let mut actions = BTreeMap::new();
        for f in &p.files {
            let Some(doc) = docs.get(&f.file.key) else {
                continue;
            };
            let fs = p.state.as_ref().and_then(|s| s.files.get(&f.file.key));
            let empty = slot::is_empty_doc(doc.value());
            let new_bytes = doc.text().as_bytes().to_vec();
            let action = if empty && fs.is_some_and(|s| s.created_file) {
                f.bytes.is_some().then_some(Action::Remove)
            } else if let (true, Some(orig)) = (empty, fs.and_then(|s| s.empty_original.clone())) {
                (f.bytes.as_deref() != Some(orig.as_bytes()))
                    .then(|| Action::Write(orig.into_bytes()))
            } else if f.bytes.as_deref() != Some(new_bytes.as_slice())
                && !(f.bytes.is_none() && empty)
            {
                Some(Action::Write(new_bytes))
            } else {
                None
            };
            if let Some(a) = action {
                actions.insert(f.file.key.clone(), a);
            }
        }
        Ok((actions, after, changed, kept))
    }

    /// Execute `plan` (see the module docs).
    pub fn apply(
        &self,
        plan: &HarnessPlan,
        take_over: bool,
        now: Timestamp,
    ) -> Result<HarnessApplyReport> {
        if plan.harness != self.spec.id() {
            return Err(Error::invalid("the plan is for another harness"));
        }
        let p = self.prepare(plan.kind, plan.desired.as_ref())?;
        // The files must be the ones planned, unchanged.
        let planned: BTreeMap<&str, (&Path, Option<&str>)> = plan
            .files
            .iter()
            .map(|f| (f.key.as_str(), (f.path.as_path(), f.base_sha256.as_deref())))
            .collect();
        for f in &p.files {
            match planned.get(f.file.key.as_str()) {
                Some((path, sha)) if *path == f.file.path && *sha == f.sha256.as_deref() => {}
                _ => {
                    return Err(Error::Conflict {
                        message: format!(
                            "{} changed since the plan; nothing was written (plan again)",
                            f.file.path.display()
                        ),
                    });
                }
            }
        }
        let (actions, after, changed, kept) = self.edited(&p, take_over)?;
        let next_generation = p.state.as_ref().map_or(0, |s| s.generation) + 1;
        let editor = EditorStore::new(self.ctx.clone());
        let harness = self.spec.id();
        let created_dirs: BTreeMap<String, Vec<PathBuf>> = p
            .files
            .iter()
            .filter(|f| f.bytes.is_none())
            .map(|f| (f.file.key.clone(), missing_dirs(&f.file.path)))
            .collect();

        // Back up what is replaced, then write everything (putting back what
        // was written when a later file fails).
        let mut written: Vec<HarnessWrittenFile> = Vec::new();
        let mut done: Vec<(&ReadFile, Option<String>)> = Vec::new();
        let result = (|| -> Result<()> {
            for f in &p.files {
                let Some(action) = actions.get(&f.file.key) else {
                    continue;
                };
                let mut backup = None;
                if let Some(old) = &f.bytes {
                    let first = p
                        .state
                        .as_ref()
                        .is_none_or(|s| !s.files.contains_key(&f.file.key));
                    if first && plan.kind == HarnessPlanKind::Apply {
                        editor.harness_backup(
                            harness,
                            &f.file.key,
                            &f.file.path,
                            old,
                            BackupSource::Original,
                            now,
                        )?;
                    }
                    backup = Some(editor.harness_backup(
                        harness,
                        &f.file.key,
                        &f.file.path,
                        old,
                        BackupSource::BeforeApply {
                            generation: next_generation,
                            profile: plan.provider.clone(),
                        },
                        now,
                    )?);
                }
                let sha = match action {
                    Action::Write(bytes) => {
                        let private = f.file.holds_key
                            || p.state
                                .as_ref()
                                .and_then(|s| s.files.get(&f.file.key))
                                .is_some_and(|s| s.holds_key);
                        write_cas(&f.file.path, bytes, f.sha256.as_deref(), private)?;
                        Some(fsx::sha256_hex(bytes))
                    }
                    Action::Remove => {
                        if fsx::file_sha256(&f.file.path)? != f.sha256 {
                            return Err(Error::Conflict {
                                message: format!(
                                    "{} changed while it was being restored; nothing more was written",
                                    f.file.path.display()
                                ),
                            });
                        }
                        fsx::remove_if_exists(&f.file.path)?;
                        // The directories Superba created for it go too, when empty.
                        let dirs = p
                            .state
                            .as_ref()
                            .and_then(|s| s.files.get(&f.file.key))
                            .map(|s| s.created_dirs.clone())
                            .unwrap_or_default();
                        for d in dirs {
                            if std::fs::remove_dir(&d).is_err() {
                                break;
                            }
                        }
                        None
                    }
                };
                done.push((f, sha.clone()));
                written.push(HarnessWrittenFile {
                    key: f.file.key.clone(),
                    path: f.file.path.clone(),
                    sha256: sha,
                    backup,
                });
            }
            Ok(())
        })();
        if let Err(e) = result {
            for (f, sha) in done.iter().rev() {
                let back = match &f.bytes {
                    Some(old) => write_cas(&f.file.path, old, sha.as_deref(), f.file.holds_key),
                    None => fsx::remove_if_exists(&f.file.path),
                };
                if let Err(b) = back {
                    tracing::error!(path = %f.file.path.display(), error = %b, "could not put a harness file back");
                }
            }
            return Err(e);
        }

        // The new state.
        let before: BTreeMap<String, Value> = p
            .files
            .iter()
            .map(|f| (f.file.key.clone(), f.value()))
            .collect();
        let before_text: BTreeMap<String, Option<String>> = p
            .files
            .iter()
            .map(|f| {
                (
                    f.file.key.clone(),
                    f.bytes
                        .as_ref()
                        .map(|b| String::from_utf8_lossy(b).into_owned()),
                )
            })
            .collect();
        let mut notes = Vec::new();
        match plan.kind {
            HarnessPlanKind::Apply => {
                let mut next = state::state_after_apply(
                    p.state.as_ref(),
                    &p.desired,
                    &before,
                    &after,
                    &before_text,
                    &created_dirs,
                    &kept,
                );
                next.generation = next_generation;
                next.applied_at = Some(now);
                next.provider = plan.provider.clone();
                next.protocol = plan.protocol;
                next.route = plan.desired.as_ref().map(|t| t.route).unwrap_or_default();
                self.save_state(Some(&next))?;
            }
            HarnessPlanKind::Restore => {
                if kept.is_empty() {
                    self.save_state(None)?;
                } else if let Some(mut s) = p.state.clone() {
                    // What someone changed stays theirs; Superba lets go of the rest.
                    for (key, fs) in s.files.iter_mut() {
                        fs.slots
                            .retain(|name, _| kept.iter().any(|k| *k == format!("{key} {name}")));
                    }
                    s.files.retain(|_, fs| !fs.slots.is_empty());
                    self.save_state(Some(&s))?;
                }
            }
        }
        notes.extend(self.spec.notes(plan.kind, plan.desired.as_ref()));
        tracing::info!(
            harness = %harness,
            kind = ?plan.kind,
            changed = changed.len(),
            kept = kept.len(),
            files = written.len(),
            "wrote harness configuration"
        );
        Ok(HarnessApplyReport {
            kind: plan.kind,
            harness,
            files: written,
            changed,
            kept,
            notes,
        })
    }

    /// Open a recognized configuration file in the editor.
    pub fn file_doc(
        &self,
        options: &HarnessOptions,
        key: &str,
    ) -> Result<superba_core::harness::HarnessFileDoc> {
        let f = self
            .read_files(options)?
            .into_iter()
            .find(|f| f.file.key == key)
            .ok_or_else(|| Error::NotFound {
                what: format!("harness file {key}"),
            })?;
        Ok(superba_core::harness::HarnessFileDoc {
            harness: self.harness(),
            file: f.view(),
            contents: f.bytes.map(superba_core::SecretBytes::new),
            sha256: f.sha256,
        })
    }

    /// Validate, back up and save a hand edit using the same file transaction.
    pub fn save_file(
        &self,
        options: &HarnessOptions,
        save: &superba_core::harness::HarnessFileSave,
        now: Timestamp,
    ) -> Result<superba_core::harness::HarnessFileDoc> {
        if save.harness != self.harness() {
            return Err(Error::invalid("file belongs to another harness"));
        }
        let f = self
            .read_files(options)?
            .into_iter()
            .find(|f| f.file.key == save.file)
            .ok_or_else(|| Error::NotFound {
                what: format!("harness file {}", save.file),
            })?;
        if matches!(
            f.status,
            superba_core::harness::HarnessFileStatus::Unreadable { .. }
        ) {
            return Err(Error::invalid(
                "The file cannot be read; reload it before saving.",
            ));
        }
        if f.sha256 != save.base_sha256 {
            return Err(Error::Conflict {
                message: "The file changed since it was opened; reload it before saving.".into(),
            });
        }
        let text = std::str::from_utf8(save.contents.expose())
            .map_err(|_| Error::invalid("not UTF-8 text"))?;
        let doc = Doc::parse(f.file.format, text)
            .map_err(|status| Error::invalid(format!("{status:?}")))?;
        self.spec.check(&[(f.file.clone(), doc.value().clone())])?;
        if let Some(bytes) = &f.bytes {
            EditorStore::new(self.ctx.clone()).harness_backup(
                self.harness(),
                &save.file,
                &f.file.path,
                bytes,
                BackupSource::BeforeEdit,
                now,
            )?;
        }
        write_cas(
            &f.file.path,
            save.contents.expose(),
            save.base_sha256.as_deref(),
            f.file.holds_key,
        )?;
        self.file_doc(options, &save.file)
    }

    /// The backups of this harness's files, newest first.
    pub fn backups(&self) -> Result<Vec<HarnessBackup>> {
        EditorStore::new(self.ctx.clone()).harness_backups(self.spec.id())
    }

    /// Put one backed-up file back as a whole (after backing up what is
    /// there); Superba then owns nothing in that file.
    pub fn restore_backup(&self, id: &str, now: Timestamp) -> Result<HarnessApplyReport> {
        self.restore_backup_checked(id, None, now)
    }

    /// Restore the reviewed backup only if the current file still has the reviewed hash.
    /// `None` skips the review check for immediate CLI restores; `Some(None)` expects an absent file.
    pub fn restore_backup_checked(
        &self,
        id: &str,
        expected: Option<Option<&str>>,
        now: Timestamp,
    ) -> Result<HarnessApplyReport> {
        let editor = EditorStore::new(self.ctx.clone());
        let harness = self.spec.id();
        let (backup, contents) = editor.harness_backup_read(id)?;
        if backup.harness != harness {
            return Err(Error::NotFound {
                what: format!("backup {id} of {harness}"),
            });
        }
        let path = backup.path.clone();
        let current = fsx::read_optional(&path)?;
        let sha = current.as_deref().map(fsx::sha256_hex);
        if expected.is_some_and(|e| e != sha.as_deref()) {
            return Err(Error::Conflict {
                message: "The file changed since the backup preview; reload it before restoring."
                    .into(),
            });
        }
        let mut before = None;
        if let Some(old) = &current {
            before = Some(editor.harness_backup(
                harness,
                &backup.file,
                &path,
                old,
                BackupSource::BeforeRestore {
                    restored: id.to_owned(),
                },
                now,
            )?);
        }
        let mut state = self.state()?;
        let private = state
            .as_ref()
            .and_then(|s| s.files.get(&backup.file))
            .is_some_and(|f| f.holds_key);
        write_cas(&path, contents.expose(), sha.as_deref(), private)?;
        if let Some(s) = state.as_mut() {
            s.files.remove(&backup.file);
            self.save_state(Some(s))?;
        }
        Ok(HarnessApplyReport {
            kind: HarnessPlanKind::Restore,
            harness,
            files: vec![HarnessWrittenFile {
                key: backup.file.clone(),
                path,
                sha256: Some(fsx::sha256_hex(contents.expose())),
                backup: before,
            }],
            changed: Vec::new(),
            kept: Vec::new(),
            notes: vec![Notice::info(text(
                "config.harness.backup_restored",
                "The file is back as it was in that backup; Superba no longer manages anything in it.",
            ))],
        })
    }
}

/// Superba's harness settings (`<data>/harnesses.json`).
#[derive(Clone, Debug)]
pub struct HarnessConfigStore {
    path: PathBuf,
}

impl HarnessConfigStore {
    /// The store of `ctx`'s data directory.
    pub fn new(ctx: &LocalContext) -> Self {
        Self {
            path: ctx.data_dir.harness_config_json(),
        }
    }

    /// The settings (default when there are none).
    pub fn load(&self) -> Result<HarnessConfig> {
        Ok(fsx::read_json::<HarnessConfig>(&self.path, "harnesses.json")?.unwrap_or_default())
    }

    /// Save them.
    pub fn save(&self, config: &HarnessConfig) -> Result<()> {
        let mut c = config.clone();
        c.schema = HARNESS_CONFIG_SCHEMA;
        fsx::write_json(&self.path, &c)
    }
}

/// The file with key `key` among `files`.
pub(crate) fn file_of(files: &[ReadFile], key: &str) -> Result<HarnessFile> {
    files
        .iter()
        .find(|f| f.file.key == key)
        .map(|f| f.file.clone())
        .ok_or_else(|| Error::other(format!("the harness file {key} was not read")))
}

/// The home-relative directory `name` unless `env_var` names another one
/// (empty = unset).
pub(crate) fn dir_from_env(ctx: &LocalContext, env_var: &str, default: PathBuf) -> PathBuf {
    match ctx.env.var_non_empty(env_var) {
        Some(d) => PathBuf::from(d.trim()),
        None => default,
    }
}
