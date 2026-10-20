//! [`TomlConfigEngine`]: the local [`ConfigEngine`] (plan 9.2 - 9.5).
//!
//! Apply = (first takeover: baseline + login stash) -> re-read live files ->
//! render -> lazy baseline capture, first-write backups, login stash of a live
//! login about to be replaced -> journal Pending (encrypted pre-images) -> stage
//! temp files -> Publishing -> per file (auth.json first): re-check the hash and
//! rename -> commit `state.json` -> drop the journal. A file that changed between
//! reading and renaming restarts the whole cycle (at most 3 attempts, plan 9.2).

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use superba_core::{
    ActiveCredentials, ActiveState, ApplyPlan, ApplyReport, AuthShape, Baseline, BaselineExtras,
    BlockReason, Clock, CodexFileKind, CodexFiles, CodexHome, ConfigEngine, ConfigState, Error,
    FileChange, FileWrite, GenerationRecord, JournalAction, LocalContext, LoginStashInfo,
    PlannedFile, RecoveredJournal, RecoveryReport, RenderInput, RenderedFiles, ResolvedMode,
    ResolvedProfile, Result, SecretBytes, SiteKind, StashScope, SystemClock, Timestamp,
};
use url::Url;

use crate::authjson;
use crate::fsx::{self, Mode};
use crate::journal::{self, Commit, Entry, JournalFile, Phase};
use crate::render::{self, GenerationSnapshot, Inputs, Rendered};
use crate::stash::Stash;
use crate::state::{self, ArchivedBaseline, FirstWrite, GenerationEntry, HomeState};
use crate::toml_keys::{ConfigDoc, reprs_equal};
use crate::vault::Vault;

/// Number of most recent generations whose backups (pre-images, auth snapshots)
/// are kept in `backups/switch/`.
pub const KEEP_SWITCH_BACKUPS: u64 = 20;
/// A restored ChatGPT login whose `last_refresh` is at least this many days old
/// gets a "you may need to sign in again" warning (plan 9.4).
pub const STALE_LOGIN_DAYS: u64 = 14;
/// Maximum attempts when files change concurrently between read and rename.
const MAX_ATTEMPTS: u32 = 3;
/// Generation history entries kept in `state.json`.
const MAX_HISTORY: usize = 500;
const MAX_ARCHIVED_BASELINES: usize = 5;

/// [`ConfigEngine`] backed by `toml_edit` and an on-disk journal in the data dir.
#[derive(Debug, Clone)]
pub struct TomlConfigEngine {
    ctx: LocalContext,
    clock: Arc<dyn Clock>,
    vault: Vault,
    stash: Stash,
}

impl TomlConfigEngine {
    /// Create an engine whose journal, baseline and backups live in `ctx.data_dir`.
    pub fn new(ctx: LocalContext) -> Self {
        Self::with_clock(ctx, Arc::new(SystemClock::new()))
    }

    /// Like [`TomlConfigEngine::new`] with an injected clock (tests).
    pub fn with_clock(ctx: LocalContext, clock: Arc<dyn Clock>) -> Self {
        Self {
            vault: Vault::new(ctx.clone()),
            stash: Stash::new(ctx.clone()),
            ctx,
            clock,
        }
    }

    /// The context this engine was created with.
    pub fn context(&self) -> &LocalContext {
        &self.ctx
    }

    async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Self) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || f(&this))
            .await
            .map_err(|e| Error::other(format!("config engine task failed: {e}")))?
    }
}

/// Outcome of one publish attempt.
enum TxError {
    /// A file changed before anything was published (retry from scratch).
    Changed,
    Fatal(Error),
}

impl From<Error> for TxError {
    fn from(e: Error) -> Self {
        TxError::Fatal(e)
    }
}

/// Everything loaded around a render.
struct Prepared {
    stash: Option<SecretBytes>,
    original: Option<CodexFiles>,
    generation: Option<GenerationSnapshot>,
    /// Identity of a login the user signed out of since Superba last saw it.
    logged_out: Option<String>,
    /// Notes about the prepared inputs (shown with the plan).
    notes: Vec<superba_core::Notice>,
    /// The credential Superba wrote was replaced (GH#5).
    auth_drift: bool,
    /// Superba owns nothing: paths (and `auth.json`) changed since its last
    /// write, which restores keep unless taken over (GH#5).
    released_changed: Vec<String>,
}

/// A generation restore never names a model catalog file that is gone: Codex
/// refuses to load a `config.toml` whose `model_catalog_json` cannot be read, and
/// Superba keeps only the current and the previous catalog file. The key is
/// removed from the snapshot (and the byte-exact copy, which names it, is not
/// used). Returns the missing file.
fn drop_missing_catalog(root: &Path, g: &mut GenerationSnapshot) -> Option<String> {
    let key = superba_core::codex::config_keys::MODEL_CATALOG_JSON;
    let snap = g.keys.iter_mut().find(|s| s.path == key)?;
    let value = snap
        .value
        .as_deref()
        .and_then(crate::toml_keys::parse_repr)?;
    let file = value.as_str()?.to_owned();
    if file.trim().is_empty() || root.join(&file).is_file() {
        return None;
    }
    snap.value = None;
    g.config_toml = None;
    Some(file)
}

fn read_files(home: &Path) -> Result<CodexFiles> {
    Ok(CodexFiles {
        config_toml: fsx::read_optional(&home.join("config.toml"))?.map(SecretBytes::new),
        auth_json: fsx::read_optional(&home.join("auth.json"))?.map(SecretBytes::new),
    })
}

fn bytes_of(files: &CodexFiles, kind: CodexFileKind) -> Option<&[u8]> {
    match kind {
        CodexFileKind::ConfigToml => files.config_toml.as_ref().map(|b| b.expose()),
        CodexFileKind::AuthJson => files.auth_json.as_ref().map(|b| b.expose()),
        CodexFileKind::ClaudeSettings => None,
    }
}

fn file_name(kind: CodexFileKind) -> &'static str {
    match kind {
        CodexFileKind::ConfigToml => "config.toml",
        CodexFileKind::AuthJson => "auth.json",
        CodexFileKind::ClaudeSettings => "settings.json",
    }
}

fn mode_for(kind: CodexFileKind) -> Mode {
    match kind {
        CodexFileKind::AuthJson | CodexFileKind::ClaudeSettings => Mode::Private,
        CodexFileKind::ConfigToml => Mode::PreserveOrPrivate,
    }
}

/// Bytes after applying `change` to `current`.
fn after_bytes(current: Option<&[u8]>, change: &FileChange) -> Option<Vec<u8>> {
    match change {
        FileChange::Unchanged => current.map(<[u8]>::to_vec),
        FileChange::Write { contents, .. } => Some(contents.expose().to_vec()),
        FileChange::Remove => None,
    }
}

fn site_kind_of(mode: &ResolvedMode) -> Option<SiteKind> {
    match mode {
        ResolvedMode::Token { site_kind, .. } => Some(*site_kind),
        ResolvedMode::Official => Some(SiteKind::Official),
        _ => None,
    }
}

fn blocked_pending() -> Error {
    Error::Blocked {
        reasons: vec![BlockReason::JournalPending],
    }
}

impl TomlConfigEngine {
    fn data(&self) -> &superba_core::DataDir {
        &self.ctx.data_dir
    }

    fn pending(&self, root: &Path) -> Result<bool> {
        Ok(!journal::list(self.data(), Some(root))?.is_empty())
    }

    fn home_dir_key(root: &Path) -> String {
        fsx::short_hash(root.to_string_lossy().as_bytes())
    }

    // ---------------------------------------------------------------- reads

    fn read_state_sync(&self, root: &Path) -> Result<ConfigState> {
        let files = read_files(root)?;
        let mut st = render::inspect(root, &files);
        let hs = state::read_home(self.data(), root)?;
        st.active = hs.active();
        st.drift = render::drift(&files, hs.last_written());
        st.model_owned = hs
            .last_written()
            .iter()
            .any(|k| k.path == superba_core::codex::config_keys::MODEL && k.value.is_some());
        st.auth_drift = self.auth_drift(&files, &hs);
        st.handed_off = hs.current().and_then(|e| {
            e.handed_off.clone().map(|to| superba_core::HandOffInfo {
                at: e.record.applied_at,
                to,
                generation: e.record.generation,
            })
        });
        (st.since_release, st.since_release_auth) = self.since_release(&files, &hs);
        st.journal_pending = self.pending(root)?;
        Ok(st)
    }

    /// GH#5: the `auth.json` credential Superba wrote is no longer there.
    /// Superba owns the file only while its current generation wrote a token
    /// credential and still owns keys (not after a restore or a hand-off).
    /// Rewrites that keep the credential (Codex reformatting the file, extra
    /// fields) are not a change; see [`authjson::credential_digest`].
    fn auth_drift(&self, files: &CodexFiles, hs: &HomeState) -> Option<superba_core::AuthDrift> {
        let e = hs.current()?;
        if e.handed_off.is_some()
            || e.last_written.is_empty()
            || !e
                .active
                .auth_mode
                .is_some_and(superba_core::AuthMode::uses_key)
        {
            return None;
        }
        let written_sha = e.auth_sha256.as_ref()?;
        let live = bytes_of(files, CodexFileKind::AuthJson);
        if live.map(fsx::sha256_hex).as_ref() == Some(written_sha) {
            return None;
        }
        let written = e
            .auth_snapshot
            .as_ref()
            .and_then(|p| self.vault.read_sealed(p).ok().flatten());
        let live_shape = authjson::drift_shape(live);
        match written {
            Some(w) => {
                let w = w.expose();
                (authjson::credential_digest(Some(w)) != authjson::credential_digest(live)).then(
                    || superba_core::AuthDrift {
                        written: authjson::drift_shape(Some(w)),
                        live: live_shape,
                    },
                )
            }
            // The snapshot was pruned: only the kind of credential can be compared.
            None => (live_shape.mode() != e.active.auth_mode).then(|| superba_core::AuthDrift {
                written: match e.active.auth_mode {
                    Some(superba_core::AuthMode::Pat) => AuthShape::Pat {
                        key_last4: "····".into(),
                    },
                    _ => AuthShape::ApiKey {
                        key_last4: "····".into(),
                    },
                },
                live: live_shape,
            }),
        }
    }

    /// GH#5: while Superba owns nothing (its current generation restored or
    /// handed off), what changed since that generation's write in the keys a
    /// restore would put back and in the credential.
    fn since_release(
        &self,
        files: &CodexFiles,
        hs: &HomeState,
    ) -> (
        Vec<superba_core::KeyChange>,
        Option<superba_core::AuthDrift>,
    ) {
        let Some(e) = hs.current().filter(|e| e.last_written.is_empty()) else {
            return (Vec::new(), None);
        };
        let live_config = bytes_of(files, CodexFileKind::ConfigToml);
        let mut keys = Vec::new();
        if live_config.map(fsx::sha256_hex) != e.config_sha256 {
            let written = match (&e.config_sha256, &e.config_snapshot) {
                (None, _) => Some(ConfigDoc::empty()),
                (Some(_), Some(p)) => self
                    .vault
                    .read_sealed(p)
                    .ok()
                    .flatten()
                    .and_then(|b| ConfigDoc::parse(b.expose()).ok()),
                (Some(_), None) => None,
            };
            let live = match live_config {
                Some(b) => ConfigDoc::parse(b).ok(),
                None => Some(ConfigDoc::empty()),
            };
            if let (Some(written), Some(live)) = (written, live) {
                let mut paths = rollback_paths(hs);
                if let Some(b) = &hs.baseline {
                    for k in &b.keys {
                        if !paths.contains(&k.path) {
                            paths.push(k.path.clone());
                        }
                    }
                }
                for path in paths {
                    let (before, after) = (written.get_repr(&path), live.get_repr(&path));
                    let same = match (before.as_deref(), after.as_deref()) {
                        (Some(a), Some(b)) => reprs_equal(a, b),
                        (None, None) => true,
                        _ => false,
                    };
                    if !same {
                        keys.push(superba_core::KeyChange {
                            path,
                            before,
                            after,
                        });
                    }
                }
            }
        }
        let live_auth = bytes_of(files, CodexFileKind::AuthJson);
        let auth = if live_auth.map(fsx::sha256_hex) == e.auth_sha256 {
            None
        } else {
            let written = match (&e.auth_sha256, &e.auth_snapshot) {
                (None, _) => Some(None),
                (Some(_), Some(p)) => self.vault.read_sealed(p).ok().flatten().map(Some),
                (Some(_), None) => None,
            };
            written.and_then(|w| {
                let w = w.as_ref().map(|b| b.expose());
                (authjson::credential_digest(w) != authjson::credential_digest(live_auth)).then(
                    || superba_core::AuthDrift {
                        written: authjson::drift_shape(w),
                        live: authjson::drift_shape(live_auth),
                    },
                )
            })
        };
        (keys, auth)
    }

    fn snapshot_baseline_sync(&self, root: &Path, extras: BaselineExtras) -> Result<Baseline> {
        let now = self.clock.now();
        state::with_home(self.data(), root, |hs| {
            if let Some(b) = &hs.baseline
                && !b.released
            {
                return Ok(b.clone());
            }
            let files = read_files(root)?;
            let mut b = render::capture_baseline(root, &files, extras, now)?;
            if matches!(b.auth, AuthShape::ChatGpt { .. })
                && let Some(auth) = &files.auth_json
            {
                self.stash
                    .put(&StashScope::Local(root.to_path_buf()), auth, now)?;
                b.login_stashed = true;
            }
            self.archive_baseline(root, hs, now);
            hs.baseline = Some(b.clone());
            Ok(b)
        })
    }

    fn archive_baseline(&self, root: &Path, hs: &mut HomeState, now: Timestamp) {
        let Some(mut baseline) = hs.baseline.take() else {
            return;
        };
        let mut first_write = std::mem::take(&mut hs.first_write);
        // A released baseline follows a committed generation; generation numbers
        // are never reused, even after rollback or archive metadata retention.
        let dir = self
            .data()
            .first_write_backups_dir()
            .join(Self::home_dir_key(root))
            .join(format!("archive-{}", hs.max_generation));
        for fw in first_write.values_mut() {
            let Some(old) = &fw.backup else { continue };
            let Some(name) = old.file_name() else {
                continue;
            };
            let archived = dir.join(name);
            let moved = fsx::ensure_private_dir(&dir)
                .and_then(|()| std::fs::rename(old, &archived).map_err(|e| Error::io(old, &e)));
            if let Err(error) = moved
                && old.exists()
            {
                tracing::warn!(path = %old.display(), %error, "archiving first-write backup failed");
            }
            for path in &mut baseline.first_write_backups {
                if path == old {
                    *path = archived.clone();
                }
            }
            // Never let archived metadata point at a later cycle's pre-image,
            // including when uninstall already deleted the old backup.
            fw.backup = Some(archived);
        }
        hs.archived_baselines.push(ArchivedBaseline {
            archived_at: now,
            baseline,
            first_write,
        });
        let excess = hs
            .archived_baselines
            .len()
            .saturating_sub(MAX_ARCHIVED_BASELINES);
        hs.archived_baselines.drain(..excess);
    }

    fn original_files(&self, hs: &HomeState, files: &CodexFiles) -> Result<CodexFiles> {
        let mut out = CodexFiles::default();
        for kind in [CodexFileKind::ConfigToml, CodexFileKind::AuthJson] {
            let bytes = match hs.first_write.get(&kind) {
                Some(fw) if fw.existed => {
                    let saved = fw
                        .backup
                        .as_ref()
                        .map(|p| self.vault.read_sealed(p))
                        .transpose()?
                        .flatten();
                    match saved {
                        Some(bytes) => Some(bytes),
                        None if hs.baseline.as_ref().is_some_and(|b| b.released) => {
                            bytes_of(files, kind).map(|b| SecretBytes::new(b.to_vec()))
                        }
                        None => {
                            return Err(Error::NotFound {
                                what: format!(
                                    "first-write backup of {} was deleted; restore a generation or the official configuration",
                                    file_name(kind)
                                ),
                            });
                        }
                    }
                }
                Some(_) => None,
                // Never written by Superba: the current file is the original.
                None => bytes_of(files, kind).map(|b| SecretBytes::new(b.to_vec())),
            };
            match kind {
                CodexFileKind::ConfigToml => out.config_toml = bytes,
                CodexFileKind::AuthJson => out.auth_json = bytes,
                CodexFileKind::ClaudeSettings => {}
            }
        }
        Ok(out)
    }

    fn generation_snapshot(&self, hs: &HomeState, generation: u64) -> Result<GenerationSnapshot> {
        let e = hs
            .history
            .iter()
            .find(|e| e.record.generation == generation)
            .ok_or_else(|| Error::NotFound {
                what: format!("generation {generation}"),
            })?;
        let auth_json = match (&e.auth_sha256, &e.auth_snapshot) {
            (None, _) => None,
            (Some(_), Some(p)) => {
                Some(self.vault.read_sealed(p)?.ok_or_else(|| Error::NotFound {
                    what: format!(
                        "auth.json snapshot of generation {generation} was deleted or pruned"
                    ),
                })?)
            }
            (Some(_), None) => {
                return Err(Error::NotFound {
                    what: format!("auth.json snapshot of generation {generation}"),
                });
            }
        };
        let config_toml = match (&e.config_sha256, &e.config_snapshot) {
            (None, _) => Some(None),
            (Some(_), Some(p)) => Some(Some(self.vault.read_sealed(p)?.ok_or_else(|| {
                Error::NotFound {
                    what: format!(
                        "config.toml snapshot of generation {generation} was deleted or pruned"
                    ),
                }
            })?)),
            (Some(_), None) => None,
        };
        Ok(GenerationSnapshot {
            keys: e.last_written.clone(),
            auth_json,
            config_toml,
        })
    }

    fn prepare(
        &self,
        root: &Path,
        files: &CodexFiles,
        hs: &HomeState,
        desired: &ResolvedProfile,
    ) -> Result<Prepared> {
        let scope = StashScope::Local(root.to_path_buf());
        let live = authjson::shape(bytes_of(files, CodexFileKind::AuthJson));
        // Plan 9.4: a login Superba left in place that is gone now was signed out of
        // in the app; it must not be resurrected from the stash.
        let logged_out = match (hs.observed_login(), &live) {
            (Some(id), AuthShape::Missing) => Some(id.to_owned()),
            _ => None,
        };
        let signed_out: BTreeSet<&str> = hs
            .signed_out
            .iter()
            .map(String::as_str)
            .chain(logged_out.as_deref())
            .collect();
        let is_signed_out = |b: &SecretBytes| {
            authjson::login_info(b.expose())
                .is_some_and(|i| signed_out.contains(i.identity.as_str()))
        };
        let restores_login = matches!(
            desired.mode,
            ResolvedMode::Official
                | ResolvedMode::Original { .. }
                | ResolvedMode::Release { .. }
                | ResolvedMode::Generation { .. }
                | ResolvedMode::Token {
                    auth: superba_core::AuthMode::MixedOfficial,
                    ..
                }
        );
        let stash = if restores_login {
            self.stash.load(&scope)?.filter(|b| !is_signed_out(b))
        } else {
            None
        };
        let original = match desired.mode {
            ResolvedMode::Original { .. } | ResolvedMode::Release { .. } => {
                let mut o = self.original_files(hs, files)?;
                if o.auth_json.as_ref().is_some_and(&is_signed_out) {
                    o.auth_json = None;
                }
                Some(o)
            }
            _ => None,
        };
        let mut notes = Vec::new();
        let generation = match desired.mode {
            ResolvedMode::Generation { generation } => {
                let mut g = self.generation_snapshot(hs, generation)?;
                if g.auth_json.as_ref().is_some_and(&is_signed_out) {
                    g.auth_json = None;
                }
                if let Some(file) = drop_missing_catalog(root, &mut g) {
                    notes.push(superba_core::Notice::warning(
                        superba_core::LocalizedText::new(
                            "config.note.generation_catalog_missing",
                            "The model catalog file {file} of generation {generation} no longer exists, so the restored config.toml does not name it (Codex could not start otherwise); its per-model settings are not restored.",
                        )
                        .arg("file", file)
                        .arg("generation", generation),
                    ));
                }
                Some(g)
            }
            _ => None,
        };
        let (released_keys, released_auth) = self.since_release(files, hs);
        Ok(Prepared {
            stash,
            original,
            generation,
            logged_out,
            notes,
            auth_drift: self.auth_drift(files, hs).is_some(),
            released_changed: released_keys
                .into_iter()
                .map(|k| k.path)
                .chain(released_auth.map(|_| superba_core::AUTH_JSON_PATH.to_owned()))
                .collect(),
        })
    }

    fn render_local(
        &self,
        files: &CodexFiles,
        hs: &HomeState,
        desired: &ResolvedProfile,
        prep: &Prepared,
    ) -> Result<Rendered> {
        render::render(&Inputs {
            files,
            desired,
            baseline: hs.baseline.as_ref(),
            login_stash: prep.stash.as_ref(),
            original_files: prep.original.as_ref(),
            last_written: hs.last_written(),
            generation: prep.generation.as_ref(),
            auth_drift: prep.auth_drift,
            released_changed: &prep.released_changed,
        })
    }

    // ----------------------------------------------------------------- plan

    fn plan_sync(&self, root: &Path, desired: &ResolvedProfile) -> Result<ApplyPlan> {
        if self.pending(root)? {
            return Err(blocked_pending());
        }
        let files = read_files(root)?;
        let hs = state::read_home(self.data(), root)?;
        let prep = self.prepare(root, &files, &hs, desired)?;
        let r = self.render_local(&files, &hs, desired, &prep)?;
        let final_auth = after_bytes(
            bytes_of(&files, CodexFileKind::AuthJson),
            &r.files.auth_json,
        );
        let auth_mode = desired
            .auth_mode()
            .or_else(|| authjson::shape(final_auth.as_deref()).mode());
        let mut notes = r.files.notes.clone();
        notes.extend(prep.notes.iter().cloned());
        if let Some(id) = &prep.logged_out {
            notes.push(superba_core::Notice::info(
                superba_core::LocalizedText::new(
                    "config.note.signed_out",
                    "You signed out of ChatGPT since the last switch; that saved login will not be restored.",
                )
                .arg("identity", id),
            ));
        }
        if let FileChange::Write { contents, .. } = &r.files.auth_json
            && r.files
                .auth_change
                .as_ref()
                .is_some_and(|c| matches!(c.after, AuthShape::ChatGpt { .. }))
            && let Some(info) = authjson::login_info(contents.expose())
        {
            let now = self.clock.now();
            let age_days = info
                .last_refresh
                .and_then(|t| now.duration_since(t))
                .map(|d| d.as_secs() / 86_400);
            if age_days.is_some_and(|d| d >= STALE_LOGIN_DAYS) {
                notes.push(superba_core::Notice::warning(
                    superba_core::LocalizedText::new(
                        "config.note.stale_login",
                        "This ChatGPT login was saved {days} days ago; you may need to sign in again after restoring it.",
                    )
                    .arg("days", age_days.unwrap_or_default()),
                ));
            }
        }
        let releases_ownership = releases_ownership(&hs, &r, desired);
        let planned = |kind: CodexFileKind, change: FileChange| PlannedFile {
            kind,
            path: root.join(file_name(kind)),
            before_sha256: bytes_of(&files, kind).map(fsx::sha256_hex),
            change,
        };
        Ok(ApplyPlan {
            id: uuid::Uuid::new_v4().to_string(),
            codex_home: root.to_path_buf(),
            generation: hs.max_generation + 1,
            profile_id: desired.profile_id.clone(),
            site_id: desired.site_id.clone(),
            line_id: desired.line_id.clone(),
            auth_mode,
            files: vec![
                planned(CodexFileKind::AuthJson, r.files.auth_json),
                planned(CodexFileKind::ConfigToml, r.files.config_toml),
            ],
            key_changes: r.files.key_changes,
            auth_change: r.files.auth_change,
            notes,
            desired: Some(desired.clone()),
            releases_ownership,
        })
    }

    // ---------------------------------------------------------------- apply

    fn apply_sync(&self, plan: &ApplyPlan) -> Result<ApplyReport> {
        let desired = plan.desired.as_ref().ok_or_else(|| {
            Error::invalid("this plan carries no desired state (it was not produced by plan())")
        })?;
        let root = plan.codex_home.as_path();
        if self.pending(root)? {
            return Err(blocked_pending());
        }
        // A restore must keep a released baseline available. Re-capture only
        // when the rendered apply actually takes ownership again below.
        if state::read_home(self.data(), root)?.baseline.is_none() {
            self.snapshot_baseline_sync(root, BaselineExtras::default())?;
        }
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.apply_attempt(plan, desired) {
                Ok(report) => return Ok(report),
                Err(TxError::Changed) if attempt < MAX_ATTEMPTS => {
                    tracing::info!(
                        "Codex files changed while applying; re-reading (attempt {attempt})"
                    );
                }
                Err(TxError::Changed) => {
                    return Err(Error::Conflict {
                        message: format!(
                            "{} kept changing while Superba was writing it; nothing was changed",
                            root.display()
                        ),
                    });
                }
                Err(TxError::Fatal(e)) => return Err(e),
            }
        }
    }

    fn apply_attempt(
        &self,
        plan: &ApplyPlan,
        desired: &ResolvedProfile,
    ) -> std::result::Result<ApplyReport, TxError> {
        let root = plan.codex_home.as_path();
        let now = self.clock.now();
        let scope = StashScope::Local(root.to_path_buf());
        let files = read_files(root)?;
        let hs = state::read_home(self.data(), root)?;
        let prep = self.prepare(root, &files, &hs, desired)?;
        let r = self.render_local(&files, &hs, desired, &prep)?;

        if !r.owned.is_empty() && hs.baseline.as_ref().is_some_and(|b| b.released) {
            tracing::warn!(home = %root.display(), "takeover skipped snapshot_baseline; re-capturing with default extras");
            self.snapshot_baseline_sync(root, BaselineExtras::default())?;
            return self.apply_attempt(plan, desired);
        }

        // Side preparations; all idempotent, so a later failure leaves no damage.
        if let Some(id) = &prep.logged_out {
            self.stash.forget(&scope, id)?;
        }
        let live_identity = files
            .auth_json
            .as_ref()
            .and_then(|b| authjson::login_info(b.expose()))
            .map(|i| i.identity);
        if prep.logged_out.is_some()
            || live_identity
                .as_ref()
                .is_some_and(|id| hs.signed_out.contains(id))
        {
            state::with_home(self.data(), root, |h| {
                if let Some(id) = &prep.logged_out
                    && !h.signed_out.contains(id)
                {
                    h.signed_out.push(id.clone());
                }
                // Signed in again with a previously signed-out account.
                if let Some(id) = &live_identity {
                    h.signed_out.retain(|x| x != id);
                }
                Ok(())
            })?;
        }
        if r.files.auth_change.as_ref().is_some_and(|c| c.stash_login)
            && let Some(live) = &files.auth_json
        {
            self.stash.put(&scope, live, now)?;
        }
        self.record_first_writes(root, &files, &r.files)?;
        self.extend_baseline(root, &r)?;

        let generation = hs.max_generation + 1;
        let backup_dir = self
            .data()
            .switch_backups_dir()
            .join(Self::home_dir_key(root))
            .join(format!("g{generation:06}-{}", short_id(&plan.id)));
        let changes = [
            (CodexFileKind::AuthJson, &r.files.auth_json),
            (CodexFileKind::ConfigToml, &r.files.config_toml),
        ];
        let mut jfiles = Vec::new();
        for (kind, change) in changes {
            if matches!(change, FileChange::Unchanged) {
                continue;
            }
            let current = bytes_of(&files, kind);
            let backup = match current {
                Some(b) => {
                    let p = backup_dir.join(format!("{}.pre.enc", file_name(kind)));
                    self.vault.write_sealed(&p, b)?;
                    Some(p)
                }
                None => None,
            };
            jfiles.push(JournalFile {
                kind,
                path: fsx::resolve_write_target(&root.join(file_name(kind))),
                before_sha256: current.map(fsx::sha256_hex),
                after_sha256: match change {
                    FileChange::Write { sha256, .. } => Some(sha256.clone()),
                    _ => None,
                },
                temp: None,
                backup,
                source: None,
            });
        }

        // Post-apply facts for the generation entry.
        let post_auth = after_bytes(
            bytes_of(&files, CodexFileKind::AuthJson),
            &r.files.auth_json,
        );
        let post_config = after_bytes(
            bytes_of(&files, CodexFileKind::ConfigToml),
            &r.files.config_toml,
        );
        let auth_snapshot = match &post_auth {
            Some(b) => {
                let p = backup_dir.join("auth.json.post.enc");
                self.vault.write_sealed(&p, b)?;
                Some(p)
            }
            None => None,
        };
        let config_snapshot = match &post_config {
            Some(b) => {
                let p = backup_dir.join("config.toml.post.enc");
                self.vault.write_sealed(&p, b)?;
                Some(p)
            }
            None => None,
        };
        let login_identity = post_auth
            .as_deref()
            .and_then(authjson::login_info)
            .map(|i| i.identity);
        let auth_mode = desired
            .auth_mode()
            .or_else(|| authjson::shape(post_auth.as_deref()).mode());
        let active = ActiveState {
            profile_id: desired.profile_id.clone(),
            site_id: desired.site_id.clone(),
            line_id: desired.line_id.clone(),
            auth_mode,
            generation,
            applied_at: now,
        };
        let entry = GenerationEntry {
            record: GenerationRecord {
                generation,
                profile_id: desired.profile_id.clone(),
                site_kind: site_kind_of(&desired.mode),
                applied_at: now,
            },
            journal_id: plan.id.clone(),
            active,
            last_written: r.owned.clone(),
            config_sha256: post_config.as_deref().map(fsx::sha256_hex),
            auth_sha256: post_auth.as_deref().map(fsx::sha256_hex),
            auth_snapshot,
            config_snapshot,
            login_identity,
            handed_off: match &desired.mode {
                ResolvedMode::Release { to } => Some(to.clone()),
                _ => None,
            },
        };
        let mut jentry = Entry {
            id: plan.id.clone(),
            codex_home: root.to_path_buf(),
            created_at: now,
            phase: Phase::Pending,
            files: jfiles,
            commit: Commit::Apply {
                entry: Box::new(entry),
            },
        };
        journal::write(self.data(), &jentry)?;
        #[cfg(test)]
        if hooks::crash(hooks::Crash::Pending) {
            return Err(TxError::Fatal(Error::other("injected crash")));
        }

        // Stage temp files.
        let stage_result = (|| -> Result<()> {
            for f in &mut jentry.files {
                let change = if f.kind == CodexFileKind::AuthJson {
                    &r.files.auth_json
                } else {
                    &r.files.config_toml
                };
                if let FileChange::Write { contents, .. } = change {
                    f.temp = Some(fsx::stage(&f.path, contents.expose(), mode_for(f.kind))?);
                }
            }
            jentry.phase = Phase::Publishing;
            journal::write(self.data(), &jentry)
        })();
        if let Err(e) = stage_result {
            journal::cleanup_temps(&jentry);
            journal::remove(self.data(), &jentry.id)?;
            return Err(TxError::Fatal(e));
        }
        #[cfg(test)]
        if hooks::crash(hooks::Crash::Staged) {
            return Err(TxError::Fatal(Error::other("injected crash")));
        }

        // Publish, auth.json first.
        let mut published: Vec<&JournalFile> = Vec::new();
        for f in &jentry.files {
            #[cfg(test)]
            hooks::before_publish(&f.path);
            let step = (|| -> std::result::Result<(), TxError> {
                if fsx::file_sha256(&f.path)? != f.before_sha256 {
                    return Err(TxError::Changed);
                }
                match &f.temp {
                    Some(t) => fsx::publish(t, &f.path)?,
                    None => fsx::remove_if_exists(&f.path)?,
                }
                Ok(())
            })();
            if let Err(e) = step {
                // Undo what was already renamed, then drop the entry.
                return match self.undo(&published) {
                    Ok(()) => {
                        journal::cleanup_temps(&jentry);
                        journal::remove(self.data(), &jentry.id)?;
                        Err(e)
                    }
                    // Keep the journal: start-up recovery finishes the job.
                    Err(undo_err) => Err(TxError::Fatal(Error::Conflict {
                        message: format!(
                            "write failed ({}) and restoring failed ({undo_err}); recovery will retry",
                            match e {
                                TxError::Changed => "file changed".to_owned(),
                                TxError::Fatal(e) => e.to_string(),
                            }
                        ),
                    })),
                };
            }
            published.push(f);
            #[cfg(test)]
            if hooks::crash(hooks::Crash::FirstPublish) {
                return Err(TxError::Fatal(Error::other("injected crash")));
            }
        }

        self.commit(&jentry)?;
        journal::remove(self.data(), &jentry.id)?;
        if let Err(e) = self.prune_backups(root) {
            tracing::warn!("pruning old switch backups failed: {e}");
        }
        Ok(ApplyReport {
            journal_id: jentry.id.clone(),
            codex_home: root.to_path_buf(),
            generation,
            applied_at: now,
            files: jentry
                .files
                .iter()
                .map(|f| FileWrite {
                    kind: f.kind,
                    path: f.path.clone(),
                    before_sha256: f.before_sha256.clone(),
                    after_sha256: f.after_sha256.clone(),
                    backup: f.backup.clone(),
                })
                .collect(),
        })
    }

    /// Restore pre-images of files this process just published (only where the
    /// file still holds what was written).
    fn undo(&self, published: &[&JournalFile]) -> Result<()> {
        for f in published.iter().rev() {
            if fsx::file_sha256(&f.path)? != f.after_sha256 {
                continue;
            }
            self.restore_before(f)?;
        }
        Ok(())
    }

    fn restore_before(&self, f: &JournalFile) -> Result<()> {
        match (&f.before_sha256, &f.backup) {
            (None, _) => fsx::remove_if_exists(&f.path),
            (Some(_), Some(b)) => {
                let bytes = self.vault.read_sealed(b)?.ok_or_else(|| Error::NotFound {
                    what: format!("backup {}", b.display()),
                })?;
                fsx::write_atomic(&f.path, bytes.expose(), mode_for(f.kind))
            }
            (Some(_), None) => Err(Error::NotFound {
                what: format!("pre-image of {}", f.path.display()),
            }),
        }
    }

    fn record_first_writes(
        &self,
        root: &Path,
        files: &CodexFiles,
        r: &RenderedFiles,
    ) -> Result<()> {
        let dir = self
            .data()
            .first_write_backups_dir()
            .join(Self::home_dir_key(root));
        state::with_home(self.data(), root, |hs| {
            for (kind, change) in [
                (CodexFileKind::AuthJson, &r.auth_json),
                (CodexFileKind::ConfigToml, &r.config_toml),
            ] {
                if matches!(change, FileChange::Unchanged) || hs.first_write.contains_key(&kind) {
                    continue;
                }
                let fw = match bytes_of(files, kind) {
                    Some(b) => {
                        let p = dir.join(format!("{}.orig.enc", file_name(kind)));
                        self.vault.write_sealed(&p, b)?;
                        if let Some(bl) = &mut hs.baseline
                            && !bl.first_write_backups.contains(&p)
                        {
                            bl.first_write_backups.push(p.clone());
                        }
                        FirstWrite {
                            existed: true,
                            sha256: Some(fsx::sha256_hex(b)),
                            backup: Some(p),
                        }
                    }
                    None => FirstWrite {
                        existed: false,
                        sha256: None,
                        backup: None,
                    },
                };
                hs.first_write.insert(kind, fw);
            }
            Ok(())
        })
    }

    fn extend_baseline(&self, root: &Path, r: &Rendered) -> Result<()> {
        if r.newly_owned.is_empty() {
            return Ok(());
        }
        state::with_home(self.data(), root, |hs| {
            let Some(b) = &mut hs.baseline else {
                return Ok(());
            };
            let captured = render::baseline_paths();
            for s in &r.newly_owned {
                // Model keys and Codex settings follow the user's latest own value:
                // a key Superba released and takes over again comes back as the
                // user left it, not as it was at the first takeover. The routing
                // and recommended keys keep their takeover-time value.
                let refresh = render::is_model_key(&s.path) || !captured.contains(&s.path.as_str());
                match b.keys.iter_mut().find(|x| x.path == s.path) {
                    Some(x) if refresh => x.value = s.value.clone(),
                    Some(_) => {}
                    None => b.keys.push(s.clone()),
                }
            }
            Ok(())
        })
    }

    fn commit(&self, entry: &Entry) -> Result<()> {
        let root = entry.codex_home.as_path();
        state::with_home(self.data(), root, |hs| {
            match &entry.commit {
                Commit::Apply { entry: g } => {
                    if g.last_written.is_empty()
                        && let Some(b) = &mut hs.baseline
                    {
                        b.released = true;
                    }
                    hs.max_generation = hs.max_generation.max(g.record.generation);
                    hs.history
                        .retain(|e| e.record.generation != g.record.generation);
                    hs.history.push((**g).clone());
                    if hs.history.len() > MAX_HISTORY {
                        let excess = hs.history.len() - MAX_HISTORY;
                        hs.history.drain(..excess);
                    }
                }
                Commit::Rollback { generation } => {
                    if hs.current().map(|e| e.record.generation) == Some(*generation) {
                        hs.history.pop();
                        // Undo the release along with the generation, rather than
                        // treating the next switch as a new takeover of our own
                        // restored managed configuration.
                        let released = hs.last_written().is_empty();
                        if let Some(b) = &mut hs.baseline {
                            b.released = released;
                        }
                    }
                }
            }
            Ok(())
        })
    }

    fn prune_backups(&self, root: &Path) -> Result<()> {
        let dir = self
            .data()
            .switch_backups_dir()
            .join(Self::home_dir_key(root));
        let hs = state::read_home(self.data(), root)?;
        let floor = hs.max_generation.saturating_sub(KEEP_SWITCH_BACKUPS);
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => return Ok(()),
        };
        for item in rd.flatten() {
            let name = item.file_name().to_string_lossy().into_owned();
            let Some(g) = name
                .strip_prefix('g')
                .and_then(|s| s.split('-').next())
                .and_then(|s| s.parse::<u64>().ok())
            else {
                continue;
            };
            if g <= floor {
                let _ = std::fs::remove_dir_all(item.path());
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------- rollback

    /// Plan one restoration without writing a journal, staging, or publishing.
    fn plan_rollback_file(
        &self,
        fw: &FileWrite,
        hs: &HomeState,
        generation: u64,
    ) -> Result<(JournalFile, Option<SecretBytes>)> {
        let generation = hs
            .current()
            .filter(|g| g.record.generation == generation)
            .ok_or_else(|| Error::other("the current generation differs from the report"))?;
        let live = fsx::read_optional(&fw.path)?.map(SecretBytes::new);
        let live_sha256 = live.as_ref().map(|b| fsx::sha256_hex(b.expose()));
        let exact = live_sha256 == fw.after_sha256;
        if !exact && fw.kind == CodexFileKind::AuthJson {
            return Err(Error::other("changed since the switch, left as is"));
        }
        let before = match (&fw.before_sha256, &fw.backup) {
            (None, _) => None,
            (Some(hash), Some(path)) => {
                let bytes = self
                    .vault
                    .read_sealed(path)?
                    .ok_or_else(|| Error::NotFound {
                        what: format!("backup {}", path.display()),
                    })?;
                if fsx::sha256_hex(bytes.expose()) != *hash {
                    return Err(Error::other("backup does not match the recorded hash"));
                }
                Some(bytes)
            }
            (Some(_), None) => return Err(Error::other("no backup")),
        };
        let contents = if exact {
            before
        } else {
            let live = live
                .as_ref()
                .ok_or_else(|| Error::other("changed since the switch, left as is (missing)"))?;
            let mut doc = ConfigDoc::parse(live.expose())
                .map_err(|_| Error::other("live config.toml cannot be parsed"))?;
            let before_doc = match &before {
                Some(bytes) => ConfigDoc::parse(bytes.expose())
                    .map_err(|_| Error::other("config.toml backup cannot be parsed"))?,
                None => ConfigDoc::empty(),
            };

            // Released keys and model keys a profile did not set are absent from
            // last_written. Check their post-apply values too before restoring
            // every owned path, so a later edit cannot be silently overwritten.
            let written = generation
                .config_snapshot
                .as_ref()
                .map(|path| self.vault.read_sealed(path))
                .transpose()?
                .flatten();
            if written.as_ref().map(|b| fsx::sha256_hex(b.expose())) != fw.after_sha256 {
                return Err(Error::other(
                    "config.toml snapshot is missing or has changed",
                ));
            }
            let written_doc = match &written {
                Some(bytes) => ConfigDoc::parse(bytes.expose())
                    .map_err(|_| Error::other("config.toml snapshot cannot be parsed"))?,
                None => ConfigDoc::empty(),
            };
            for path in rollback_paths(hs).iter().map(String::as_str) {
                let current = doc.get_repr(path);
                let written = hs
                    .last_written()
                    .iter()
                    .find(|s| s.path == path)
                    .map_or_else(|| written_doc.get_repr(path), |s| s.value.clone());
                let equal = match (current.as_deref(), written.as_deref()) {
                    (Some(a), Some(b)) => reprs_equal(a, b),
                    (None, None) => !doc.has_path(path),
                    _ => false,
                };
                if !equal {
                    return Err(Error::other(format!(
                        "owned key {path} changed since the switch, left as is"
                    )));
                }
                match before_doc.get_repr(path) {
                    Some(value) => doc.set_repr(path, &value)?,
                    None => {
                        doc.remove(path);
                    }
                }
            }
            let bytes = doc.to_bytes();
            if before.is_none() && bytes.iter().all(u8::is_ascii_whitespace) {
                None
            } else {
                Some(SecretBytes::new(bytes))
            }
        };
        Ok((
            JournalFile {
                kind: fw.kind,
                path: fw.path.clone(),
                before_sha256: live_sha256,
                after_sha256: contents.as_ref().map(|b| fsx::sha256_hex(b.expose())),
                temp: None,
                backup: None,
                source: if exact && contents.is_some() {
                    fw.backup.clone()
                } else {
                    None
                },
            },
            contents,
        ))
    }

    fn rollback_sync(&self, report: &ApplyReport) -> Result<()> {
        let root = report.codex_home.as_path();
        if self.pending(root)? {
            return Err(blocked_pending());
        }
        let now = self.clock.now();
        let hs = state::read_home(self.data(), root)?;
        // A conflict in either file must leave both files and the state alone.
        // Finish all reads, hash checks, and TOML edits before creating anything.
        let mut planned = report
            .files
            .iter()
            .rev()
            .map(|fw| {
                self.plan_rollback_file(fw, &hs, report.generation)
                    .map_err(|error| Error::Conflict {
                        message: format!(
                            "{}: {error}; no files were rolled back",
                            fw.path.display()
                        ),
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        for (file, contents) in &mut planned {
            // Recovery may need to undo a partially published rollback if the
            // other file was edited meanwhile, just as it does for an apply.
            if let Some(before) = fsx::read_optional(&file.path)? {
                if Some(fsx::sha256_hex(&before)) != file.before_sha256 {
                    return Err(Error::Conflict {
                        message: format!(
                            "{} changed before rollback; no files were rolled back",
                            file.path.display()
                        ),
                    });
                }
                let backup = self
                    .data()
                    .switch_backups_dir()
                    .join(Self::home_dir_key(root))
                    .join(format!(
                        "g{:06}-{}",
                        report.generation,
                        short_id(&report.journal_id)
                    ))
                    .join(format!("{}.rollback.pre.enc", file_name(file.kind)));
                self.vault.write_sealed(&backup, &before)?;
                file.backup = Some(backup);
            } else if file.before_sha256.is_some() {
                return Err(Error::Conflict {
                    message: format!(
                        "{} disappeared before rollback; no files were rolled back",
                        file.path.display()
                    ),
                });
            }
            if file.source.is_none()
                && let Some(bytes) = contents
            {
                // Recovery must also work if a staged temp disappears. Keep the
                // merged live document encrypted alongside this switch's backups.
                let source = self
                    .data()
                    .switch_backups_dir()
                    .join(Self::home_dir_key(root))
                    .join(format!(
                        "g{:06}-{}",
                        report.generation,
                        short_id(&report.journal_id)
                    ))
                    .join(format!("{}.rollback.enc", file_name(file.kind)));
                self.vault.write_sealed(&source, bytes.expose())?;
                file.source = Some(source);
            }
        }
        let (jfiles, contents): (Vec<_>, Vec<_>) = planned.into_iter().unzip();
        let mut entry = Entry {
            id: format!("rollback-{}", report.journal_id),
            codex_home: root.to_path_buf(),
            created_at: now,
            phase: Phase::Pending,
            files: jfiles,
            commit: Commit::Rollback {
                generation: report.generation,
            },
        };
        journal::write(self.data(), &entry)?;
        #[cfg(test)]
        if hooks::crash(hooks::Crash::Pending) {
            return Err(Error::other("injected crash"));
        }
        let staged = (|| -> Result<()> {
            for (f, bytes) in entry.files.iter_mut().zip(&contents) {
                if let Some(bytes) = bytes {
                    f.temp = Some(fsx::stage(&f.path, bytes.expose(), mode_for(f.kind))?);
                }
            }
            entry.phase = Phase::Publishing;
            journal::write(self.data(), &entry)
        })();
        if let Err(e) = staged {
            journal::cleanup_temps(&entry);
            journal::remove(self.data(), &entry.id)?;
            return Err(e);
        }
        #[cfg(test)]
        if hooks::crash(hooks::Crash::Staged) {
            return Err(Error::other("injected crash"));
        }
        for (index, f) in entry.files.iter().enumerate() {
            #[cfg(test)]
            hooks::before_publish(&f.path);
            if fsx::file_sha256(&f.path)? != f.before_sha256 {
                // A writer can still race with publishing after preflight. Stop
                // here; previous renames have happened, later files stay live,
                // and the generation must not be committed as rolled back.
                journal::cleanup_temps(&entry);
                journal::remove(self.data(), &entry.id)?;
                let paths = |files: &[JournalFile]| {
                    files
                        .iter()
                        .map(|f| f.path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                let restored = if index == 0 {
                    "none".to_owned()
                } else {
                    paths(&entry.files[..index])
                };
                return Err(Error::Conflict {
                    message: format!(
                        "{} changed since the switch, left as is; already restored: {restored}; \
                         files left as is: {}; state generation unchanged",
                        f.path.display(),
                        paths(&entry.files[index..])
                    ),
                });
            }
            match &f.temp {
                Some(t) => fsx::publish(t, &f.path)?,
                None => fsx::remove_if_exists(&f.path)?,
            }
            #[cfg(test)]
            if hooks::crash(hooks::Crash::FirstPublish) {
                return Err(Error::other("injected crash"));
            }
        }
        self.commit(&entry)?;
        journal::remove(self.data(), &entry.id)?;
        Ok(())
    }

    // ------------------------------------------------------------- recovery

    fn recover_sync(&self, root: &Path) -> Result<RecoveryReport> {
        let mut report = RecoveryReport::default();
        for entry in journal::list(self.data(), Some(root))? {
            let action = self.recover_entry(&entry)?;
            journal::cleanup_temps(&entry);
            journal::remove(self.data(), &entry.id)?;
            report.entries.push(RecoveredJournal {
                journal_id: entry.id.clone(),
                action,
                files: entry.files.iter().map(|f| f.path.clone()).collect(),
            });
        }
        Ok(report)
    }

    fn recover_entry(&self, entry: &Entry) -> Result<JournalAction> {
        #[derive(PartialEq)]
        enum S {
            Done,
            NotDone,
            Foreign,
        }
        let mut states = Vec::new();
        for f in &entry.files {
            let cur = fsx::file_sha256(&f.path)?;
            states.push(if cur == f.after_sha256 {
                S::Done
            } else if cur == f.before_sha256 {
                S::NotDone
            } else {
                S::Foreign
            });
        }
        if states.contains(&S::Foreign) {
            let hs = state::read_home(self.data(), &entry.codex_home)?;
            let committed = match &entry.commit {
                Commit::Apply { entry: g } => {
                    hs.history.iter().any(|h| h.journal_id == g.journal_id)
                }
                Commit::Rollback { generation } => !hs
                    .history
                    .iter()
                    .any(|h| h.record.generation == *generation),
            };
            if committed {
                // A crash after state.json was committed only left journal
                // cleanup unfinished. Do not undo the active generation.
                return Ok(JournalAction::Drift);
            }
            // Keep the external edit, but undo our published half before dropping
            // the journal. In particular, new relay auth must never be left with
            // the old routing config. A failed undo keeps the journal for retry.
            let published: Vec<_> = entry
                .files
                .iter()
                .zip(&states)
                .filter_map(|(file, state)| (*state == S::Done).then_some(file))
                .collect();
            self.undo(&published)?;
            return Ok(JournalAction::Drift);
        }
        if states.iter().all(|s| *s == S::NotDone) {
            return Ok(JournalAction::Discarded);
        }
        // Roll forward what is missing, from the staged temp or the source backup.
        let mut forward_ok = true;
        for (f, s) in entry.files.iter().zip(&states) {
            if *s == S::Done {
                continue;
            }
            match self.forward_contents(f)? {
                Some(Some(bytes)) => fsx::write_atomic(&f.path, bytes.expose(), mode_for(f.kind))?,
                Some(None) => fsx::remove_if_exists(&f.path)?,
                None => {
                    forward_ok = false;
                    break;
                }
            }
        }
        if forward_ok {
            self.commit(entry)?;
            return Ok(JournalAction::Completed);
        }
        // Cannot finish: put the published files back.
        for (f, _) in entry.files.iter().zip(&states) {
            if fsx::file_sha256(&f.path)? == f.after_sha256 && f.after_sha256 != f.before_sha256 {
                if f.backup.is_none() && f.before_sha256.is_some() {
                    return Ok(JournalAction::Drift);
                }
                self.restore_before(f)?;
            }
        }
        Ok(JournalAction::RolledBack)
    }

    /// New contents for a not-yet-published file: `Some(None)` = remove,
    /// `None` = unavailable.
    fn forward_contents(&self, f: &JournalFile) -> Result<Option<Option<SecretBytes>>> {
        let Some(after) = &f.after_sha256 else {
            return Ok(Some(None));
        };
        if let Some(t) = &f.temp
            && let Some(b) = fsx::read_optional(t)?
            && &fsx::sha256_hex(&b) == after
        {
            return Ok(Some(Some(SecretBytes::new(b))));
        }
        if let Some(src) = &f.source
            && let Some(b) = self.vault.read_sealed(src)?
            && &fsx::sha256_hex(b.expose()) == after
        {
            return Ok(Some(Some(b)));
        }
        Ok(None)
    }

    // ---------------------------------------------------------- credentials

    fn active_credentials_sync(&self, root: &Path) -> Result<Option<ActiveCredentials>> {
        let files = read_files(root)?;
        let Some((auth_mode, key)) = files
            .auth_json
            .as_ref()
            .and_then(|b| authjson::token_credentials(b.expose()))
        else {
            return Ok(None);
        };
        let st = render::inspect(root, &files);
        let url = |s: Option<String>| s.and_then(|s| Url::parse(&s).ok());
        Ok(Some(ActiveCredentials {
            auth_mode,
            key,
            openai_base_url: url(st.openai_base_url),
            chatgpt_base_url: url(st.chatgpt_base_url),
        }))
    }
}

/// Test-only fault injection (thread-local, so call the `*_sync` methods
/// directly from the test thread).
#[cfg(test)]
pub(crate) mod hooks {
    use std::cell::RefCell;
    use std::path::Path;

    /// After which step to simulate a process crash (return without cleanup).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Crash {
        Pending,
        Staged,
        FirstPublish,
    }

    type Hook = Box<dyn FnMut(&Path)>;

    thread_local! {
        pub(crate) static CRASH: RefCell<Option<Crash>> = const { RefCell::new(None) };
        pub(crate) static BEFORE_PUBLISH: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    pub(crate) fn crash(at: Crash) -> bool {
        CRASH.with(|c| *c.borrow() == Some(at))
    }

    pub(crate) fn before_publish(path: &Path) {
        BEFORE_PUBLISH.with(|h| {
            if let Some(f) = h.borrow_mut().as_mut() {
                f(path);
            }
        });
    }
}

/// Paths a rollback of the current generation must restore: every static owned
/// path plus the Codex settings written by the current or the previous generation
/// (settings released by this generation were restored to their baseline values).
fn rollback_paths(hs: &HomeState) -> Vec<String> {
    let mut paths: Vec<String> = render::all_owned_paths()
        .into_iter()
        .map(str::to_owned)
        .collect();
    let n = hs.history.len();
    let generations = hs.history[n.saturating_sub(2)..].iter();
    for s in generations.flat_map(|g| g.last_written.iter()) {
        if !paths.contains(&s.path) {
            paths.push(s.path.clone());
        }
    }
    paths
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;

/// GH#5: an apply that leaves Superba owning nothing while it owns keys now
/// (a restore whose changes another tool already made), or any hand-off, is
/// committed even when neither file changes.
fn releases_ownership(hs: &HomeState, r: &Rendered, desired: &ResolvedProfile) -> bool {
    matches!(desired.mode, ResolvedMode::Release { .. })
        || (!hs.last_written().is_empty() && r.owned.is_empty())
}

fn short_id(id: &str) -> String {
    id.chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect()
}

#[async_trait]
impl ConfigEngine for TomlConfigEngine {
    fn render(&self, input: &RenderInput<'_>) -> Result<RenderedFiles> {
        let last_written = input.last_written.unwrap_or(&[]);
        render::render(&Inputs {
            files: input.files,
            desired: input.desired,
            baseline: input.baseline,
            login_stash: input.login_stash,
            original_files: input.original_files,
            last_written,
            generation: None,
            // Remote hosts keep their own records; the credential check is local.
            auth_drift: false,
            released_changed: &[],
        })
        .map(|r| r.files)
    }

    fn inspect_files(&self, codex_home: &Path, files: &CodexFiles) -> Result<ConfigState> {
        Ok(render::inspect(codex_home, files))
    }

    fn capture_baseline(
        &self,
        codex_home: &Path,
        files: &CodexFiles,
        extras: BaselineExtras,
        now: Timestamp,
    ) -> Result<Baseline> {
        render::capture_baseline(codex_home, files, extras, now)
    }

    async fn read_state(&self, home: &CodexHome) -> Result<ConfigState> {
        let root = home.root().to_path_buf();
        self.run(move |e| e.read_state_sync(&root)).await
    }

    async fn baseline(&self, home: &CodexHome) -> Result<Option<Baseline>> {
        let root = home.root().to_path_buf();
        self.run(move |e| Ok(state::read_home(e.data(), &root)?.baseline))
            .await
    }

    async fn snapshot_baseline(
        &self,
        home: &CodexHome,
        extras: BaselineExtras,
    ) -> Result<Baseline> {
        let root = home.root().to_path_buf();
        self.run(move |e| e.snapshot_baseline_sync(&root, extras))
            .await
    }

    async fn plan(&self, home: &CodexHome, desired: &ResolvedProfile) -> Result<ApplyPlan> {
        let root = home.root().to_path_buf();
        let desired = desired.clone();
        self.run(move |e| e.plan_sync(&root, &desired)).await
    }

    async fn apply(&self, plan: &ApplyPlan) -> Result<ApplyReport> {
        let plan = plan.clone();
        self.run(move |e| e.apply_sync(&plan)).await
    }

    async fn rollback(&self, report: &ApplyReport) -> Result<()> {
        let report = report.clone();
        self.run(move |e| e.rollback_sync(&report)).await
    }

    async fn recover(&self, home: &CodexHome) -> Result<RecoveryReport> {
        let root = home.root().to_path_buf();
        self.run(move |e| e.recover_sync(&root)).await
    }

    async fn generations(&self, home: &CodexHome) -> Result<Vec<GenerationRecord>> {
        let root = home.root().to_path_buf();
        self.run(move |e| {
            Ok(state::read_home(e.data(), &root)?
                .history
                .into_iter()
                // Generations whose snapshots were pruned or deleted (uninstall
                // --delete-backups) stay listed: reverse migration attributes
                // sessions to generations over the whole timeline (plan 10.4).
                // Restoring such a generation fails with a clear NotFound.
                .map(|g| g.record)
                .collect())
        })
        .await
    }

    async fn active_credentials(&self, home: &CodexHome) -> Result<Option<ActiveCredentials>> {
        let root = home.root().to_path_buf();
        self.run(move |e| e.active_credentials_sync(&root)).await
    }

    async fn login_stash(&self, scope: &StashScope) -> Result<Option<LoginStashInfo>> {
        let scope = scope.clone();
        self.run(move |e| e.stash.info(&scope)).await
    }

    async fn stash_login(
        &self,
        scope: &StashScope,
        auth_json: &SecretBytes,
    ) -> Result<LoginStashInfo> {
        let scope = scope.clone();
        let auth = auth_json.clone();
        self.run(move |e| e.stash.put(&scope, &auth, e.clock.now()))
            .await
    }

    async fn load_login_stash(&self, scope: &StashScope) -> Result<Option<SecretBytes>> {
        let scope = scope.clone();
        self.run(move |e| e.stash.load(&scope)).await
    }

    async fn owned_keys(&self, home: &CodexHome) -> Result<Vec<superba_core::KeySnapshot>> {
        let root = home.root().to_path_buf();
        self.run(move |e| {
            let hs = state::read_home(e.data(), &root)?;
            if hs.current().is_some_and(|g| g.handed_off.is_some()) {
                return Ok(Vec::new());
            }
            Ok(hs.last_written().to_vec())
        })
        .await
    }
}
