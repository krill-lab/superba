//! A read-only snapshot of every place a Codex home records session providers,
//! with the provider report and selector resolution built on top of it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use superba_core::{
    CodexHome, Error, MigrationRequest, MigrationSelector, Notice, ProviderSessions, Result,
    SessionReport, Timestamp,
};

use crate::ledger::Ledger;
use crate::rollout::{self, RolloutFile};
use crate::sqlite::{self, DbInfo, ThreadRow};
use crate::text;
use crate::util::{normalize_dir, uuid_v7_ms};

/// Snapshot of sqlite rows and rollout heads.
#[derive(Debug)]
pub(crate) struct Inventory {
    pub dbs: Vec<DbInfo>,
    /// Databases that could not be read (busy, permissions, ...): writes refuse.
    pub failed_dbs: Vec<(PathBuf, Error)>,
    /// Rollouts or folders skipped because too many files were open: writes refuse.
    pub failed_rollouts: Vec<Error>,
    pub rows: Vec<ThreadRow>,
    pub rollouts: Vec<RolloutFile>,
    pub warnings: Vec<Notice>,
}

/// Directories that may hold Codex SQLite databases: the home, `sqlite_home()`
/// (`CODEX_SQLITE_HOME`) and the `sqlite_home` key of `config.toml`.
pub(crate) fn sqlite_dirs(
    home: &CodexHome,
    home_dir: Option<&Path>,
    warnings: &mut Vec<Notice>,
) -> Vec<PathBuf> {
    let mut dirs = vec![home.root().to_path_buf(), home.sqlite_home().to_path_buf()];
    if let Some(value) = config_sqlite_home(&home.config_toml()) {
        match resolve_config_path(&value, home.root(), home_dir) {
            Some(path) => dirs.push(path),
            None => warnings.push(text::sqlite_home_unresolved(&value)),
        }
    }
    let mut seen = BTreeSet::new();
    dirs.retain(|d| seen.insert(normalize_dir(d)));
    dirs
}

/// Top-level `sqlite_home` string of `config.toml`, if any.
fn config_sqlite_home(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let table: toml::Table = text.parse().ok()?;
    let value = table.get("sqlite_home")?.as_str()?.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// Resolve `~`, `~/x` (needs the home directory) and relative paths (relative to
/// the Codex home).
fn resolve_config_path(value: &str, root: &Path, home_dir: Option<&Path>) -> Option<PathBuf> {
    if value == "~" {
        return home_dir.map(Path::to_path_buf);
    }
    if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        return home_dir.map(|h| h.join(rest));
    }
    if value.starts_with('~') {
        return None;
    }
    let path = PathBuf::from(value);
    Some(if path.is_absolute() {
        path
    } else {
        root.join(path)
    })
}

impl Inventory {
    /// Scan `home`.
    pub(crate) fn load(home: &CodexHome, home_dir: Option<&Path>) -> Inventory {
        let mut warnings = Vec::new();
        let dirs = sqlite_dirs(home, home_dir, &mut warnings);
        let (dbs, mut failed_dbs) = sqlite::discover(&dirs, &mut warnings);
        let mut rows = Vec::new();
        for (i, db) in dbs.iter().enumerate() {
            match sqlite::read_rows(db, i) {
                Ok(mut r) => rows.append(&mut r),
                Err(err) => {
                    warnings.push(text::db_unreadable(&db.path, &err.to_string()));
                    failed_dbs.push((db.path.clone(), err));
                }
            }
        }
        let mut failed_rollouts = Vec::new();
        let rollouts = rollout::discover(
            &home.sessions_dir(),
            &home.archived_sessions_dir(),
            &mut warnings,
            &mut failed_rollouts,
        );
        let headless = rollouts
            .iter()
            .filter(|r| r.head.as_ref().and_then(|h| h.thread_id.as_ref()).is_none())
            .count();
        if headless > 0 {
            warnings.push(text::no_session_meta(headless));
        }
        if session_index_has_provider(&home.root().join("session_index.jsonl")) {
            warnings.push(text::session_index_provider());
        }
        Inventory {
            dbs,
            failed_dbs,
            failed_rollouts,
            rows,
            rollouts,
            warnings,
        }
    }

    /// Writes need every database (and every rollout that was only skipped for
    /// lack of file handles): refuse while one could not be read, with the
    /// reason it could not be read (in use by another process, too many open
    /// files, permission, ...; the error names the database).
    pub(crate) fn require_complete(&self) -> Result<()> {
        match (self.failed_dbs.first(), self.failed_rollouts.first()) {
            (Some((_, err)), _) | (None, Some(err)) => Err(err.clone()),
            (None, None) => Ok(()),
        }
    }

    /// Group every location by thread id.
    pub(crate) fn threads(&self) -> BTreeMap<String, ThreadView> {
        let mut map: BTreeMap<String, ThreadView> = BTreeMap::new();
        for (i, row) in self.rows.iter().enumerate() {
            map.entry(row.thread_id.clone())
                .or_insert_with(|| ThreadView::new(&row.thread_id))
                .rows
                .push(i);
        }
        for (i, file) in self.rollouts.iter().enumerate() {
            if let Some(id) = file.head.as_ref().and_then(|h| h.thread_id.as_ref()) {
                map.entry(id.clone())
                    .or_insert_with(|| ThreadView::new(id))
                    .files
                    .push(i);
            }
        }
        map
    }

    /// Provider distribution.
    pub(crate) fn report(&self, home: &CodexHome, now: Timestamp) -> SessionReport {
        let mut by_provider: BTreeMap<String, ProviderSessions> = BTreeMap::new();
        for row in self.rows.iter().filter(|r| r.is_threads) {
            by_provider.entry(row.provider.clone()).or_default().threads += 1;
        }
        for file in &self.rollouts {
            let Some(head) = &file.head else { continue };
            if head.thread_id.is_none() {
                continue;
            }
            let entry = by_provider
                .entry(head.effective_provider().to_owned())
                .or_default();
            if file.archived {
                entry.archived += 1;
            } else {
                entry.rollouts += 1;
            }
            if file.compressed {
                entry.compressed += 1;
            }
        }
        let mut inconsistent = 0;
        for view in self.threads().values() {
            // Per provider: the union of thread ids recorded under it anywhere.
            let providers = view.providers(self);
            for provider in &providers {
                by_provider.entry(provider.clone()).or_default().distinct += 1;
            }
            if providers.len() > 1 {
                inconsistent += 1;
            }
        }
        SessionReport {
            codex_home: home.root().to_path_buf(),
            scanned_at: now,
            sqlite_paths: self.dbs.iter().map(|d| d.path.clone()).collect(),
            by_provider,
            inconsistent,
            warnings: self.warnings.clone(),
        }
    }

    /// Resolve a request into sessions with the provider ids to move away from.
    /// `ledger` loads a ledger for [`MigrationSelector::Ledger`]; `now` closes an
    /// open [`MigrationSelector::CreatedBetween`] window.
    pub(crate) fn select(
        &self,
        request: &MigrationRequest,
        now: Timestamp,
        ledger: &dyn Fn(&str) -> Result<Ledger>,
    ) -> Result<Vec<Selected>> {
        let threads = self.threads();
        let mut chosen: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut add = |id: &str, provider: &str| {
            chosen
                .entry(id.to_owned())
                .or_default()
                .insert(provider.to_owned());
        };
        for selector in &request.selectors {
            match selector {
                MigrationSelector::Provider { provider } => {
                    for view in threads.values() {
                        if view.providers(self).contains(provider) {
                            add(&view.id, provider);
                        }
                    }
                }
                MigrationSelector::OtherProviders { except } => {
                    for view in threads.values() {
                        for provider in view.providers(self) {
                            if provider != *except {
                                add(&view.id, &provider);
                            }
                        }
                    }
                }
                MigrationSelector::ThreadIds { ids } => {
                    for id in ids {
                        if let Some(view) = threads.get(id) {
                            for provider in view.providers(self) {
                                add(id, &provider);
                            }
                        }
                    }
                }
                MigrationSelector::CreatedBetween { provider, from, to } => {
                    let end = to.unwrap_or(now);
                    for view in threads.values() {
                        let Some(created) = view.created_ms(self) else {
                            continue;
                        };
                        if created >= from.0
                            && created < end.0
                            && view.providers(self).contains(provider)
                        {
                            add(&view.id, provider);
                        }
                    }
                }
                MigrationSelector::Ledger { migration_id } => {
                    let l = ledger(migration_id)?;
                    let ids: BTreeSet<&String> = l
                        .rollouts
                        .iter()
                        .map(|r| &r.thread_id)
                        .chain(
                            l.databases
                                .iter()
                                .flat_map(|d| d.changes.iter())
                                .filter(|c| c.is_provider())
                                .map(|c| &c.thread_id),
                        )
                        .collect();
                    for id in ids {
                        if let Some(view) = threads.get(id)
                            && view.providers(self).contains(&l.to_provider)
                        {
                            add(id, &l.to_provider);
                        }
                    }
                }
            }
        }
        let exclude: BTreeSet<&String> = request.exclude.iter().collect();
        let mut out = Vec::new();
        for (id, mut sources) in chosen {
            sources.remove(&request.to_provider);
            if sources.is_empty() || exclude.contains(&id) {
                continue;
            }
            let Some(view) = threads.get(&id) else {
                continue;
            };
            if view.archived(self) && !request.include_archived {
                continue;
            }
            out.push(Selected {
                thread_id: id,
                sources,
                title: view.title(self),
                created_ms: view.created_ms(self),
                archived: view.archived(self),
            });
        }
        out.sort_by(|a, b| {
            b.created_ms
                .cmp(&a.created_ms)
                .then_with(|| a.thread_id.cmp(&b.thread_id))
        });
        Ok(out)
    }
}

/// One selected session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Selected {
    pub thread_id: String,
    /// Provider ids of this session that move (never the destination).
    pub sources: BTreeSet<String>,
    pub title: Option<String>,
    pub created_ms: Option<i64>,
    pub archived: bool,
}

impl Selected {
    /// The source shown in previews (`MigrationItem::source_provider`).
    pub(crate) fn primary_source(&self) -> String {
        self.sources.iter().next().cloned().unwrap_or_default()
    }
}

/// All locations of one thread.
#[derive(Clone, Debug)]
pub(crate) struct ThreadView {
    pub id: String,
    /// Indexes into `Inventory::rows`.
    pub rows: Vec<usize>,
    /// Indexes into `Inventory::rollouts`.
    pub files: Vec<usize>,
}

impl ThreadView {
    fn new(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            rows: Vec::new(),
            files: Vec::new(),
        }
    }

    /// Providers recorded anywhere: every provider table row and rollout head.
    pub(crate) fn providers(&self, inv: &Inventory) -> BTreeSet<String> {
        let mut set: BTreeSet<String> = self
            .rows
            .iter()
            .map(|i| inv.rows[*i].provider.clone())
            .collect();
        for i in &self.files {
            if let Some(head) = &inv.rollouts[*i].head {
                set.insert(head.effective_provider().to_owned());
            }
        }
        set
    }

    /// `threads.rollout_path` of this thread, if recorded.
    pub(crate) fn rollout_path(&self, inv: &Inventory) -> Option<PathBuf> {
        self.rows
            .iter()
            .map(|i| &inv.rows[*i])
            .filter(|r| r.is_threads)
            .find_map(|r| r.rollout_path.clone())
    }

    /// Archived in sqlite or stored under `archived_sessions/`.
    pub(crate) fn archived(&self, inv: &Inventory) -> bool {
        self.rows.iter().any(|i| inv.rows[*i].archived)
            || self.files.iter().any(|i| inv.rollouts[*i].archived)
    }

    /// Creation time: sqlite, else `session_meta.timestamp`, else the UUIDv7 id.
    pub(crate) fn created_ms(&self, inv: &Inventory) -> Option<i64> {
        self.rows
            .iter()
            .find_map(|i| inv.rows[*i].created_ms)
            .or_else(|| {
                self.files
                    .iter()
                    .find_map(|i| inv.rollouts[*i].head.as_ref().and_then(|h| h.created_ms))
            })
            .or_else(|| uuid_v7_ms(&self.id))
    }

    /// Title from sqlite.
    pub(crate) fn title(&self, inv: &Inventory) -> Option<String> {
        self.rows.iter().find_map(|i| inv.rows[*i].title.clone())
    }
}

/// `session_index.jsonl` entries are `{id, thread_name, updated_at}` in Codex
/// 0.162; report it if a future version adds provider information.
fn session_index_has_provider(path: &Path) -> bool {
    fs::read(path)
        .map(|bytes| bytes.windows(14).any(|w| w == b"model_provider"))
        .unwrap_or(false)
}

/// Provider ids must be non-empty plain text.
pub(crate) fn validate_provider_id(id: &str) -> Result<()> {
    if id.trim().is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
        return Err(Error::invalid(format!("invalid provider id {id:?}")));
    }
    Ok(())
}
