//! Planning and the writing operations: migrate, rollback, recover.
//!
//! Order of a migration: locks and safety checks (no pending ledger, no live
//! Codex, every database readable) -> fresh inventory and selection -> computed
//! rewrites -> refusal of unsupported triggers -> Codex writer locks held ->
//! disk-space check -> backups (rollout pre-images, SQLite online backups +
//! integrity_check) -> ledger (pending) -> rollouts (hash-guarded, mtime kept) ->
//! one transaction per database (cells recorded in the ledger before commit) ->
//! verification -> ledger completed. Any failure (or panic) rolls everything back
//! through the ledger before returning; a crash leaves a pending ledger for
//! `recover`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use superba_core::{
    Clock, CodexHome, ConsumersStopped, DataDir, Error, MigrationItem, MigrationPlan,
    MigrationReceipt, MigrationRequest, Result, RollbackReport, SessionReport, Timestamp,
};

use crate::guard::{self, MigrateLock};
use crate::inventory::{Inventory, Selected, validate_provider_id};
use crate::ledger::{self, DbEntry, Ledger, LedgerState, RolloutEntry, Stored};
use crate::rollout::{self, Edit, Rewrite, RolloutFile};
use crate::sqlite::{self, CellChange, CellState, DbWork};
use crate::util::{Fingerprint, atomic_replace, db_size, normalize_dir, sha256_hex, write_new};
use crate::{FaultPlan, FaultPoint};

/// Finished migrations whose pre-images are kept.
pub(crate) const KEEP_BACKUPS: usize = 5;
/// Free space kept on top of the backup size.
const SPACE_MARGIN: u64 = 16 * 1024 * 1024;

/// Everything the operations need.
pub(crate) struct Engine<'a> {
    pub data_dir: &'a DataDir,
    pub home_dir: Option<&'a Path>,
    pub clock: &'a dyn Clock,
    pub faults: &'a FaultPlan,
}

/// `home` with absolute paths, so ledgers stay valid from any working directory.
pub(crate) fn absolute_home(home: &CodexHome) -> CodexHome {
    let abs = |p: &Path| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    CodexHome::new(abs(home.root())).with_sqlite_home(abs(home.sqlite_home()))
}

/// Computed work of one migration.
struct Work {
    rollouts: Vec<(RolloutFile, Rewrite)>,
    dbs: Vec<sqlite::DbInfo>,
    /// thread id -> (sources, destination).
    providers: BTreeMap<String, (BTreeSet<String>, String)>,
    /// thread id -> edits of the rollout Codex projects.
    offsets: BTreeMap<String, Vec<Edit>>,
}

impl Engine<'_> {
    fn now(&self) -> Timestamp {
        self.clock.now()
    }

    fn fault(&self, point: FaultPoint) -> Result<()> {
        if self.faults.fail_at == Some(point) {
            return Err(Error::other(format!("injected failure at {point:?}")));
        }
        Ok(())
    }

    /// [`superba_core::SessionMigrator::scan`].
    pub(crate) fn scan(&self, home: &CodexHome) -> SessionReport {
        let home = absolute_home(home);
        Inventory::load(&home, self.home_dir).report(&home, self.now())
    }

    /// [`superba_core::SessionMigrator::plan`].
    pub(crate) fn plan(
        &self,
        home: &CodexHome,
        request: &MigrationRequest,
    ) -> Result<MigrationPlan> {
        let home = absolute_home(home);
        validate_provider_id(&request.to_provider)?;
        let now = self.now();
        let inv = Inventory::load(&home, self.home_dir);
        let selected = inv.select(request, now, &|id| self.ledger_for(&home, id))?;
        let sources: BTreeMap<&str, &BTreeSet<String>> = selected
            .iter()
            .map(|s| (s.thread_id.as_str(), &s.sources))
            .collect();
        let rows: Vec<_> = inv
            .rows
            .iter()
            .filter(|r| {
                sources
                    .get(r.thread_id.as_str())
                    .is_some_and(|s| s.contains(&r.provider))
            })
            .collect();
        let files: Vec<_> = inv
            .rollouts
            .iter()
            .filter(|f| request.include_archived || !f.archived)
            .filter(|f| {
                f.head.as_ref().is_some_and(|h| {
                    h.thread_id
                        .as_deref()
                        .and_then(|id| sources.get(id))
                        .is_some_and(|s| s.contains(h.effective_provider()))
                })
            })
            .collect();
        let touched_dbs: BTreeSet<usize> = rows.iter().map(|r| r.db).collect();
        let mut backup_bytes: u64 = files.iter().map(|f| f.len).sum();
        for (i, db) in inv.dbs.iter().enumerate() {
            if touched_dbs.contains(&i) || (!files.is_empty() && !db.offset_tables.is_empty()) {
                backup_bytes += db_size(&db.path);
            }
        }
        let tag = uuid::Uuid::new_v4().simple().to_string();
        let id = format!("{}-{}", now.0, &tag[..8]);
        Ok(MigrationPlan {
            backup_dir: self.data_dir.migrate_backup_dir(&id),
            id,
            codex_home: home.root().to_path_buf(),
            request: request.clone(),
            thread_count: rows.len() as u32,
            rollout_count: files.len() as u32,
            by_source_provider: count_sources(selected.iter().map(|s| &s.sources)),
            backup_bytes,
            items: selected
                .into_iter()
                .map(|s| MigrationItem {
                    source_provider: s.primary_source(),
                    thread_id: s.thread_id,
                    title: s.title,
                    created_at: s.created_ms.map(Timestamp),
                    archived: s.archived,
                })
                .collect(),
        })
    }

    fn ledger_for(&self, home: &CodexHome, id: &str) -> Result<Ledger> {
        let stored = ledger::load(self.data_dir, id)?;
        if !stored.ledger.is_for(home.root()) {
            return Err(Error::invalid(format!(
                "session migration {id} belongs to {}",
                stored.ledger.codex_home.display()
            )));
        }
        Ok(stored.ledger)
    }

    /// Locks and checks shared by every writing operation. The returned lock must
    /// be held until the operation ends.
    fn prepare_write(
        &self,
        home: &CodexHome,
        token: Option<&ConsumersStopped>,
    ) -> Result<MigrateLock> {
        let lock = MigrateLock::acquire(home)?;
        let mut notices = Vec::new();
        guard::check_foreign_locks(home, &mut notices)?;
        let dirs = crate::inventory::sqlite_dirs(home, self.home_dir, &mut notices);
        let dbs = sqlite::candidate_files(&dirs);
        guard::check_processes(home, &dbs, token, self.now())?;
        guard::check_writer_locks(home)?;
        Ok(lock)
    }

    /// Refuse while a migration of `home` other than `except` awaits recovery.
    fn check_no_pending(&self, home: &CodexHome, except: Option<&str>) -> Result<()> {
        if let Some(err) = ledger::unusable_for(self.data_dir, home.root()) {
            return Err(err);
        }
        let pending = ledger::list(self.data_dir).into_iter().find(|s| {
            s.ledger.is_for(home.root())
                && s.ledger.state.needs_recovery()
                && Some(s.ledger.id.as_str()) != except
        });
        match pending {
            None => Ok(()),
            Some(s) => Err(Error::Conflict {
                message: format!(
                    "session migration {} was interrupted; recover it first",
                    s.ledger.id
                ),
            }),
        }
    }

    /// [`superba_core::SessionMigrator::migrate`].
    pub(crate) fn migrate(
        &self,
        home: &CodexHome,
        plan: &MigrationPlan,
        token: Option<&ConsumersStopped>,
    ) -> Result<MigrationReceipt> {
        let home = &absolute_home(home);
        validate_provider_id(&plan.request.to_provider)?;
        if normalize_dir(&plan.codex_home) != normalize_dir(home.root()) {
            return Err(Error::invalid(format!(
                "the migration plan is for {}, not {}",
                plan.codex_home.display(),
                home.root().display()
            )));
        }
        ledger::validate_id(&plan.id)?;
        let _lock = self.prepare_write(home, token)?;
        self.check_no_pending(home, None)?;
        let dir = self.data_dir.migrate_backup_dir(&plan.id);
        if dir.exists() {
            return Err(Error::Conflict {
                message: format!("migration {} was already executed", plan.id),
            });
        }
        let to = plan.request.to_provider.clone();
        let inv = Inventory::load(home, self.home_dir);
        inv.require_complete()?;
        // Re-resolve against the current state: only the plan's sessions, each
        // with every source provider the request selects for it.
        let planned: BTreeSet<&str> = plan.items.iter().map(|i| i.thread_id.as_str()).collect();
        let selected: Vec<Selected> = inv
            .select(&plan.request, self.now(), &|id| self.ledger_for(home, id))?
            .into_iter()
            .filter(|s| planned.contains(s.thread_id.as_str()))
            .collect();
        let work = self.compute_work(&inv, &selected, &to, plan.request.include_archived)?;
        for db in &work.dbs {
            let db_work = DbWork {
                providers: &work.providers,
                offsets: &work.offsets,
            };
            let triggers = sqlite::blocking_triggers(db, &db_work);
            if !triggers.is_empty() {
                return Err(Error::Unsupported {
                    what: format!(
                        "{} has triggers on the columns a migration changes ({}); \
                         their effects could not be rolled back",
                        db.path.display(),
                        triggers.join(", ")
                    ),
                });
            }
        }
        let threads: BTreeSet<&str> = work.providers.keys().map(String::as_str).collect();
        let _writers = guard::hold_writer_locks(home, threads.iter().copied())?;
        self.check_space(&work)?;
        let mut stored = self.write_backups(home, plan, &dir, &work)?;

        let applied = catch_unwind(AssertUnwindSafe(|| self.apply(&work, &mut stored)));
        let result = match applied {
            Ok(result) => result,
            Err(panic) => Err(Error::other(format!(
                "session migration panicked: {}",
                panic_text(&panic)
            ))),
        };
        if let Err(err) = result {
            return Err(self.fail(home, &mut stored, err));
        }
        ledger::prune(self.data_dir, home.root(), KEEP_BACKUPS);
        Ok(stored.ledger.receipt(&stored.dir))
    }

    /// The writing phase of `migrate` (after the pending ledger exists).
    fn apply(&self, work: &Work, stored: &mut Stored) -> Result<()> {
        self.fault(FaultPoint::AfterLedger)?;
        for (i, (file, rw)) in work.rollouts.iter().enumerate() {
            let entry = &stored.ledger.rollouts[i];
            rollout::replace_guarded(
                &file.path,
                &rw.raw_after,
                &entry.raw_before_sha256,
                &rw.fingerprint,
                &rw.meta,
                entry.mtime,
            )?;
            self.fault(FaultPoint::AfterRollout(i))?;
        }
        for (j, db) in work.dbs.iter().enumerate() {
            let mut conn = sqlite::open_rw(&db.path)?;
            let db_work = DbWork {
                providers: &work.providers,
                offsets: &work.offsets,
            };
            sqlite::apply(&mut conn, db, &db_work, |changes| {
                stored.ledger.databases[j].changes = changes.to_vec();
                stored.save()
            })?;
            sqlite::integrity_check(&conn, &db.path)?;
            self.fault(FaultPoint::AfterDatabase(j))?;
        }
        self.verify(&stored.ledger)?;
        self.fault(FaultPoint::BeforeComplete)?;
        stored.ledger.by_source_provider = actual_sources(&stored.ledger);
        stored.ledger.state = LedgerState::Completed;
        stored.ledger.completed_at = Some(self.now());
        stored.save()
    }

    fn compute_work(
        &self,
        inv: &Inventory,
        selected: &[Selected],
        to: &str,
        include_archived: bool,
    ) -> Result<Work> {
        let threads = inv.threads();
        let mut rollouts = Vec::new();
        let mut offsets = BTreeMap::new();
        for s in selected {
            let Some(view) = threads.get(&s.thread_id) else {
                continue;
            };
            let files: Vec<&RolloutFile> = view.files.iter().map(|i| &inv.rollouts[*i]).collect();
            let projected = projected_rollout(&files, view.rollout_path(inv));
            for file in files {
                if file.archived && !include_archived {
                    continue;
                }
                let Some(rw) = rollout::plan_rewrite(&file.path, &s.thread_id, &s.sources, to)?
                else {
                    continue;
                };
                // Codex's history projection follows one rollout; shift its
                // offsets only by that file's edits.
                if projected == Some(file.path.as_path())
                    && rw.edits.iter().any(|e| e.old.len() != e.new.len())
                {
                    offsets.insert(s.thread_id.clone(), rw.edits.clone());
                }
                rollouts.push((file.clone(), rw));
            }
        }
        let providers: BTreeMap<String, (BTreeSet<String>, String)> = selected
            .iter()
            .map(|s| (s.thread_id.clone(), (s.sources.clone(), to.to_owned())))
            .collect();
        let touched: BTreeSet<usize> = inv
            .rows
            .iter()
            .filter(|r| {
                providers
                    .get(&r.thread_id)
                    .is_some_and(|(src, _)| src.contains(&r.provider))
            })
            .map(|r| r.db)
            .collect();
        let dbs = inv
            .dbs
            .iter()
            .enumerate()
            .filter(|(i, db)| {
                touched.contains(i) || (!offsets.is_empty() && !db.offset_tables.is_empty())
            })
            .map(|(_, db)| db.clone())
            .collect();
        Ok(Work {
            rollouts,
            dbs,
            providers,
            offsets,
        })
    }

    fn check_space(&self, work: &Work) -> Result<()> {
        let needed: u64 = work
            .rollouts
            .iter()
            .map(|(_, rw)| rw.raw_before.len() as u64)
            .sum::<u64>()
            + work.dbs.iter().map(|d| db_size(&d.path)).sum::<u64>();
        let root = self.data_dir.backups_dir();
        fs::create_dir_all(&root).map_err(|e| Error::io(&root, &e))?;
        if let Ok(available) = fs4::available_space(&root)
            && available < needed.saturating_add(SPACE_MARGIN)
        {
            return Err(Error::Io {
                path: root,
                kind: "StorageFull".into(),
                message: format!(
                    "the session backup needs {needed} bytes but only {available} are free"
                ),
            });
        }
        Ok(())
    }

    fn write_backups(
        &self,
        home: &CodexHome,
        plan: &MigrationPlan,
        dir: &Path,
        work: &Work,
    ) -> Result<Stored> {
        let result = (|| -> Result<Stored> {
            fs::create_dir(dir).map_err(|e| Error::io(dir, &e))?;
            let dir_norm = normalize_dir(dir);
            let sources: Vec<PathBuf> = work
                .rollouts
                .iter()
                .map(|(f, _)| f.path.clone())
                .chain(work.dbs.iter().map(|d| d.path.clone()))
                .collect();
            for src in &sources {
                if normalize_dir(src).starts_with(&dir_norm) {
                    return Err(Error::invalid(format!(
                        "backup directory {} overlaps the source {}",
                        dir.display(),
                        src.display()
                    )));
                }
            }
            let mut rollouts = Vec::new();
            for (i, (file, rw)) in work.rollouts.iter().enumerate() {
                let name = file
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let rel = format!("rollouts/{i:06}-{name}");
                write_new(&dir.join(&rel), &rw.raw_before)?;
                rollouts.push(RolloutEntry {
                    path: file.path.clone(),
                    thread_id: file
                        .head
                        .as_ref()
                        .and_then(|h| h.thread_id.clone())
                        .unwrap_or_default(),
                    archived: file.archived,
                    compressed: file.compressed,
                    sources: rollout::replaced_providers(&rw.edits).into_iter().collect(),
                    to: plan.request.to_provider.clone(),
                    edits: rw.edits.clone(),
                    raw_before_sha256: sha256_hex(&rw.raw_before),
                    raw_after_sha256: sha256_hex(&rw.raw_after),
                    plain_before_sha256: rw.plain_before_sha.clone(),
                    plain_after_sha256: rw.plain_after_sha.clone(),
                    len_before: rw.raw_before.len() as u64,
                    len_after: rw.raw_after.len() as u64,
                    mtime: rw.fingerprint.mtime,
                    backup: Some(rel),
                });
            }
            let mut databases = Vec::new();
            for (j, db) in work.dbs.iter().enumerate() {
                let name = db
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let rel = format!("sqlite/{j:03}-{name}");
                let conn = sqlite::open_ro(&db.path)?;
                sqlite::backup(&conn, &db.path, &dir.join(&rel))?;
                databases.push(DbEntry {
                    path: db.path.clone(),
                    backup: Some(rel),
                    changes: Vec::new(),
                });
            }
            let stored = Stored {
                dir: dir.to_path_buf(),
                ledger: Ledger {
                    version: ledger::LEDGER_VERSION,
                    id: plan.id.clone(),
                    codex_home: home.root().to_path_buf(),
                    state: LedgerState::Pending,
                    created_at: self.now(),
                    completed_at: None,
                    rolled_back_at: None,
                    request: plan.request.clone(),
                    to_provider: plan.request.to_provider.clone(),
                    by_source_provider: BTreeMap::new(),
                    rollouts,
                    databases,
                    rollback: None,
                    pruned: false,
                },
            };
            stored.save()?;
            Ok(stored)
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(dir);
        }
        result
    }

    /// Every recorded change is in place.
    fn verify(&self, l: &Ledger) -> Result<()> {
        for r in &l.rollouts {
            let raw = fs::read(&r.path).map_err(|e| Error::io(&r.path, &e))?;
            if sha256_hex(&raw) != r.raw_after_sha256 {
                return Err(Error::Conflict {
                    message: format!("{} changed after it was migrated", r.path.display()),
                });
            }
        }
        for d in &l.databases {
            if d.changes.is_empty() {
                continue;
            }
            let conn = sqlite::open_ro(&d.path)?;
            for c in &d.changes {
                if sqlite::state(&conn, &d.path, c)? != CellState::After {
                    return Err(Error::Conflict {
                        message: format!(
                            "{}: {}.{} of thread {} is not migrated",
                            d.path.display(),
                            c.table,
                            c.column,
                            c.thread_id
                        ),
                    });
                }
            }
        }
        Ok(())
    }

    /// Roll back after a failed migration and turn `err` into the returned error.
    fn fail(&self, home: &CodexHome, stored: &mut Stored, err: Error) -> Error {
        if self.faults.crash {
            return err;
        }
        stored.ledger.state = LedgerState::RollingBack;
        let _ = stored.save();
        let undone = catch_unwind(AssertUnwindSafe(|| self.undo(home, &stored.ledger)))
            .unwrap_or_else(|p| Err(Error::other(format!("panicked: {}", panic_text(&p)))));
        match undone {
            Ok(report) => {
                stored.ledger.state = LedgerState::RolledBack;
                stored.ledger.rolled_back_at = Some(self.now());
                stored.ledger.rollback = Some(report);
                match stored.save() {
                    Ok(()) => err,
                    Err(save_err) => Error::other(format!(
                        "{err}; the changes were rolled back but the ledger {} could not be updated: {save_err}",
                        stored.path().display()
                    )),
                }
            }
            Err(undo_err) => Error::other(format!(
                "{err}; rolling back failed: {undo_err}; Superba will retry from the ledger {} on the next start",
                stored.path().display()
            )),
        }
    }

    /// [`superba_core::SessionMigrator::rollback`].
    pub(crate) fn rollback(
        &self,
        home: &CodexHome,
        migration_id: &str,
        token: Option<&ConsumersStopped>,
    ) -> Result<RollbackReport> {
        let home = &absolute_home(home);
        let mut stored = ledger::load(self.data_dir, migration_id)?;
        if !stored.ledger.is_for(home.root()) {
            return Err(Error::invalid(format!(
                "session migration {migration_id} belongs to {}",
                stored.ledger.codex_home.display()
            )));
        }
        if stored.ledger.state == LedgerState::RolledBack {
            return Ok(RollbackReport::default());
        }
        let _lock = self.prepare_write(home, token)?;
        self.check_no_pending(home, Some(migration_id))?;
        self.rollback_locked(home, &mut stored)
    }

    fn rollback_locked(&self, home: &CodexHome, stored: &mut Stored) -> Result<RollbackReport> {
        stored.ledger.state = LedgerState::RollingBack;
        stored.save()?;
        let report = catch_unwind(AssertUnwindSafe(|| self.undo(home, &stored.ledger)))
            .unwrap_or_else(|p| Err(Error::other(format!("panicked: {}", panic_text(&p)))))?;
        stored.ledger.state = LedgerState::RolledBack;
        stored.ledger.rolled_back_at = Some(self.now());
        stored.ledger.rollback = Some(report.clone());
        stored.save()?;
        Ok(report)
    }

    /// [`superba_core::SessionMigrator::recover`].
    pub(crate) fn recover(
        &self,
        home: &CodexHome,
        token: Option<&ConsumersStopped>,
    ) -> Result<RollbackReport> {
        let home = &absolute_home(home);
        if let Some(err) = ledger::unusable_for(self.data_dir, home.root()) {
            return Err(err);
        }
        let pending: Vec<Stored> = ledger::list(self.data_dir)
            .into_iter()
            .filter(|s| s.ledger.is_for(home.root()) && s.ledger.state.needs_recovery())
            .collect();
        if pending.is_empty() {
            return Ok(RollbackReport::default());
        }
        let _lock = self.prepare_write(home, token)?;
        let mut total = RollbackReport::default();
        for mut stored in pending {
            let r = self.rollback_locked(home, &mut stored)?;
            total.restored_threads += r.restored_threads;
            total.restored_rollouts += r.restored_rollouts;
            total.skipped_changed += r.skipped_changed;
        }
        Ok(total)
    }

    /// [`superba_core::SessionMigrator::history`].
    pub(crate) fn history(&self, home: &CodexHome) -> Vec<MigrationReceipt> {
        let home = absolute_home(home);
        ledger::list(self.data_dir)
            .into_iter()
            .filter(|s| {
                // Failed attempts (rolled back before completing) are not history.
                s.ledger.is_for(home.root())
                    && s.ledger.completed_at.is_some()
                    && matches!(
                        s.ledger.state,
                        LedgerState::Completed | LedgerState::RolledBack
                    )
            })
            .map(|s| s.ledger.receipt(&s.dir))
            .collect()
    }

    /// Restore every thread whose recorded items are all still in their migrated
    /// (or original) state; threads changed since are skipped as a whole.
    pub(crate) fn undo(&self, home: &CodexHome, l: &Ledger) -> Result<RollbackReport> {
        let backup_dir = self.data_dir.migrate_backup_dir(&l.id);
        let mut state: BTreeMap<&str, bool> = BTreeMap::new(); // thread -> restorable
        let mut rollout_after: Vec<(usize, Fingerprint)> = Vec::new();
        for (i, r) in l.rollouts.iter().enumerate() {
            let status = rollout_status(r)?;
            let ok = state.entry(r.thread_id.as_str()).or_insert(true);
            match status {
                Status::After(fp) => rollout_after.push((i, fp)),
                Status::Before => {}
                Status::Changed => *ok = false,
            }
        }
        let mut cells_after: Vec<Vec<usize>> = Vec::new();
        for d in &l.databases {
            let mut after = Vec::new();
            if !d.changes.is_empty() {
                let conn = if d.path.exists() {
                    Some(sqlite::open_ro(&d.path)?)
                } else {
                    None
                };
                for (k, c) in d.changes.iter().enumerate() {
                    let current = match &conn {
                        Some(conn) => sqlite::state(conn, &d.path, c)?,
                        None => CellState::Changed,
                    };
                    let ok = state.entry(c.thread_id.as_str()).or_insert(true);
                    match current {
                        CellState::After => after.push(k),
                        CellState::Before => {}
                        CellState::Changed => *ok = false,
                    }
                }
            }
            cells_after.push(after);
        }
        let restorable = |thread: &str| state.get(thread).copied().unwrap_or(false);
        let _writers =
            guard::hold_writer_locks(home, state.iter().filter(|(_, ok)| **ok).map(|(id, _)| *id))?;
        let mut report = RollbackReport {
            skipped_changed: state.values().filter(|ok| !**ok).count() as u32,
            ..RollbackReport::default()
        };
        for (d, after) in l.databases.iter().zip(&cells_after) {
            let todo: Vec<&CellChange> = after
                .iter()
                .map(|k| &d.changes[*k])
                .filter(|c| restorable(&c.thread_id))
                .collect();
            if todo.is_empty() {
                continue;
            }
            let mut conn = sqlite::open_rw(&d.path)?;
            sqlite::restore(&mut conn, &d.path, &todo)?;
            report.restored_threads += todo.iter().filter(|c| c.is_provider()).count() as u32;
        }
        for (i, fp) in rollout_after {
            let r = &l.rollouts[i];
            if !restorable(&r.thread_id) {
                continue;
            }
            restore_rollout(r, &backup_dir, &fp)?;
            report.restored_rollouts += 1;
        }
        Ok(report)
    }
}

/// The rollout Codex's history projection follows: the one `threads.rollout_path`
/// names (either form), else the live plain file, else live compressed, else
/// archived.
fn projected_rollout<'f>(files: &[&'f RolloutFile], recorded: Option<PathBuf>) -> Option<&'f Path> {
    if let Some(recorded) = recorded {
        let want = normalize_dir(&rollout::logical_path(&recorded));
        let exact = files
            .iter()
            .find(|f| normalize_dir(&f.path) == normalize_dir(&recorded));
        let logical = files
            .iter()
            .find(|f| normalize_dir(&rollout::logical_path(&f.path)) == want);
        if let Some(f) = exact.or(logical) {
            return Some(f.path.as_path());
        }
    }
    files
        .iter()
        .min_by_key(|f| (f.archived, f.compressed))
        .map(|f| f.path.as_path())
}

/// Source provider -> number of sessions (a session with two sources counts for
/// both).
fn count_sources<'a>(sets: impl Iterator<Item = &'a BTreeSet<String>>) -> BTreeMap<String, u32> {
    let mut out = BTreeMap::new();
    for set in sets {
        for p in set {
            *out.entry(p.clone()).or_default() += 1;
        }
    }
    out
}

/// Sources of the sessions that were actually changed.
fn actual_sources(l: &Ledger) -> BTreeMap<String, u32> {
    let mut per_thread: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for r in &l.rollouts {
        per_thread
            .entry(r.thread_id.as_str())
            .or_default()
            .extend(r.sources.iter().cloned());
    }
    for c in l.databases.iter().flat_map(|d| d.changes.iter()) {
        if c.is_provider()
            && let sqlite::SqlValue::Text(before) = &c.before
        {
            per_thread
                .entry(c.thread_id.as_str())
                .or_default()
                .insert(before.clone());
        }
    }
    count_sources(per_thread.values())
}

fn panic_text(panic: &Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_owned())
}

enum Status {
    /// Still as migrated (with the fingerprint seen).
    After(Fingerprint),
    /// As before the migration (never written, or already restored).
    Before,
    /// Changed since (or missing).
    Changed,
}

fn rollout_status(r: &RolloutEntry) -> Result<Status> {
    let Ok(fp) = Fingerprint::of(&r.path) else {
        return Ok(Status::Changed);
    };
    let raw = match fs::read(&r.path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Status::Changed),
        Err(e) => return Err(Error::io(&r.path, &e)),
    };
    let sha = sha256_hex(&raw);
    if sha == r.raw_after_sha256 {
        return Ok(Status::After(fp));
    }
    if sha == r.raw_before_sha256 {
        return Ok(Status::Before);
    }
    if r.compressed
        && let Ok(plain) = zstd::stream::decode_all(raw.as_slice())
    {
        let plain_sha = sha256_hex(&plain);
        if plain_sha == r.plain_after_sha256 {
            return Ok(Status::After(fp));
        }
        if plain_sha == r.plain_before_sha256 {
            return Ok(Status::Before);
        }
    }
    Ok(Status::Changed)
}

/// Write the pre-migration bytes of `r` back: the backed-up pre-image when present,
/// otherwise the inverse edits applied to the current content (exact for the
/// uncompressed content).
fn restore_rollout(r: &RolloutEntry, backup_dir: &Path, fp: &Fingerprint) -> Result<()> {
    let from_backup = r.backup.as_ref().and_then(|rel| {
        let bytes = fs::read(backup_dir.join(rel)).ok()?;
        (sha256_hex(&bytes) == r.raw_before_sha256).then_some(bytes)
    });
    let bytes = match from_backup {
        Some(bytes) => bytes,
        None => {
            let loaded = rollout::load(&r.path)?;
            let plain = rollout::apply_edits(&loaded.plain, &rollout::inverse_edits(&r.edits))?;
            if sha256_hex(&plain) != r.plain_before_sha256 {
                return Err(Error::Conflict {
                    message: format!("cannot reconstruct the original of {}", r.path.display()),
                });
            }
            rollout::encode(&r.path, &plain)?
        }
    };
    let meta = fs::symlink_metadata(&r.path).map_err(|e| Error::io(&r.path, &e))?;
    if Fingerprint::of(&r.path)? != *fp {
        return Err(Error::Conflict {
            message: format!("{} changed during the rollback", r.path.display()),
        });
    }
    atomic_replace(&r.path, &bytes, Some(&meta), r.mtime)
}
