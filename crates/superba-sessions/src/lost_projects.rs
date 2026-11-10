//! Folders the desktop app dropped from its project list (user bugs 135/136
//! follow-up): where they can still be found, read-only.
//!
//! - The app-server's project list in `state_*.sqlite`: `projects` (id, name,
//!   times), `project_roots` (its folders) and `project_idempotency_keys` (the
//!   app's own id for the project, used as the import key). The app only
//!   changes this list through `project/import|create|update|delete`; wiping
//!   its own `.codex-global-state.json` leaves it alone (seen with app
//!   26.1103.60933), so a project the app forgot is still here.
//! - The `threads` table: the folder each chat ran in (`cwd`), which also
//!   indexes the rollouts' `session_meta.cwd`.
//!
//! A folder is offered only when it still exists, is not a project the app
//! knows (`known`), was not answered before, and is not the home directory, a
//! filesystem root, a temporary directory, inside the Codex home or inside the
//! app's own folder for chats without a project (`~/Documents/Codex`). Folders
//! seen only in chats are offered only next to at least one folder from the
//! project list: chats alone are no sign that the app lost anything.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::Connection;
use superba_core::{CodexHome, LostProject, LostProjectSource, Timestamp};

use crate::sqlite;

/// How long one database may keep this read-only probe waiting for a
/// writer's lock, in all, before it is skipped (logged): the overview polls,
/// it must not stall. Only the first read of a database waits: every read
/// runs in one read transaction, which then holds the shared lock (or the
/// WAL snapshot), so the later statements never wait again.
pub const PROBE_BUSY_TIMEOUT: Duration = Duration::from_millis(500);

/// At most this many folders seen only in chats are offered.
pub const MAX_CHAT_FOLDERS: usize = 20;

/// The app's folder for chats without a project, relative to the home
/// directory (`<home>/Documents/Codex/<date>/<slug>`).
pub const APP_CHATS_DIR: [&str; 2] = ["Documents", "Codex"];

/// Candidates for `home`, most recently used first; empty when the app's
/// project list has nothing the app forgot. `known` are the folders of the
/// projects the app knows (any account), `answered` the folders offered
/// before.
pub fn discover(
    home: &CodexHome,
    home_dir: &Path,
    known: &[PathBuf],
    answered: &[PathBuf],
) -> Vec<LostProject> {
    let dirs = database_dirs(home, home_dir);
    let mut projects = Vec::new();
    let mut chats: BTreeMap<String, (PathBuf, u32, Option<i64>)> = BTreeMap::new();
    for path in sqlite::candidate_files(&dirs) {
        if crate::util::is_unlockable_share(&path) {
            continue;
        }
        match probe(&path) {
            Ok((rows, folders)) => {
                projects.extend(rows);
                for (cwd, count, last) in folders {
                    let entry = chats
                        .entry(key(&cwd))
                        .or_insert_with(|| (cwd.clone(), 0, None));
                    entry.1 = entry.1.saturating_add(count);
                    entry.2 = entry.2.max(last);
                }
            }
            Err(error) => tracing::warn!(
                path = %path.display(),
                %error,
                "skipped a database while looking for dropped projects (busy or unreadable); \
                 its projects are not offered now"
            ),
        }
    }
    let filter = Filter::new(home, home_dir, known, answered);
    candidates(projects, chats.into_values().collect(), &filter)
}

/// The directories whose databases are read: the Codex home, its sqlite
/// home (`CODEX_SQLITE_HOME`) and the `sqlite_home` of `config.toml`.
pub fn database_dirs(home: &CodexHome, home_dir: &Path) -> Vec<PathBuf> {
    let mut warnings = Vec::new();
    crate::inventory::sqlite_dirs(home, Some(home_dir), &mut warnings)
}

type ChatFolder = (PathBuf, u32, Option<i64>);

/// Read one database: its project list and chat folders, in one read
/// transaction waiting at most [`PROBE_BUSY_TIMEOUT`] in all.
fn probe(path: &Path) -> superba_core::Result<(Vec<ProjectRow>, Vec<ChatFolder>)> {
    let conn = sqlite::open_ro_waiting(path, PROBE_BUSY_TIMEOUT)?;
    let read = |conn: &Connection| -> rusqlite::Result<(Vec<ProjectRow>, Vec<ChatFolder>)> {
        conn.execute_batch("BEGIN DEFERRED")?;
        let have = tables(conn)?;
        let projects = read_projects(conn, &have)?;
        let chats = read_chat_folders(conn, &have)?;
        conn.execute_batch("COMMIT")?;
        Ok((projects, chats))
    };
    read(&conn).map_err(|e| {
        let _ = conn.execute_batch("ROLLBACK");
        superba_core::Error::other(format!("{}: {e}", path.display()))
    })
}

/// One project row of the app-server's list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectRow {
    /// The app's id (import idempotency key), if recorded.
    pub app_id: Option<String>,
    /// Name.
    pub name: String,
    /// Folders in order.
    pub roots: Vec<PathBuf>,
    /// Last update (ms).
    pub updated_ms: Option<i64>,
}

fn read_projects(conn: &Connection, have: &BTreeSet<String>) -> rusqlite::Result<Vec<ProjectRow>> {
    if !have.contains("projects") || !have.contains("project_roots") {
        return Ok(Vec::new());
    }
    let keys = have.contains("project_idempotency_keys");
    let mut out: BTreeMap<String, ProjectRow> = BTreeMap::new();
    let mut stmt = conn.prepare(
        "SELECT p.id, p.name, p.updated_at_ms, r.path FROM projects p \
         JOIN project_roots r ON r.project_id = p.id ORDER BY p.id, r.position",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })?;
    for (id, name, updated, path) in rows.flatten() {
        let row = out.entry(id).or_insert_with(|| ProjectRow {
            app_id: None,
            name: name.unwrap_or_default(),
            roots: Vec::new(),
            updated_ms: updated,
        });
        if let Some(p) = path.filter(|p| !p.is_empty()) {
            row.roots.push(PathBuf::from(p));
        }
    }
    if keys {
        let mut stmt = conn.prepare(
            "SELECT project_id, key FROM project_idempotency_keys ORDER BY created_at_ms",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for (id, k) in rows.flatten() {
            if let Some(row) = out.get_mut(&id)
                && row.app_id.is_none()
                && !k.is_empty()
            {
                row.app_id = Some(k);
            }
        }
    }
    Ok(out.into_values().filter(|r| !r.roots.is_empty()).collect())
}

fn read_chat_folders(
    conn: &Connection,
    have: &BTreeSet<String>,
) -> rusqlite::Result<Vec<ChatFolder>> {
    if !have.contains("threads") {
        return Ok(Vec::new());
    }
    let mut cols = BTreeSet::new();
    let mut stmt = conn.prepare("SELECT name FROM pragma_table_info('threads')")?;
    for c in stmt.query_map([], |r| r.get::<_, String>(0))?.flatten() {
        cols.insert(c);
    }
    if !cols.contains("cwd") {
        return Ok(Vec::new());
    }
    let updated = match (cols.contains("updated_at_ms"), cols.contains("updated_at")) {
        (true, true) => "COALESCE(updated_at_ms, updated_at * 1000)",
        (true, false) => "updated_at_ms",
        (false, true) => "updated_at * 1000",
        (false, false) => "NULL",
    };
    let archived = if cols.contains("archived") {
        "WHERE archived = 0"
    } else {
        ""
    };
    let sql = format!(
        "SELECT cwd, COUNT(*), MAX(CAST({updated} AS INTEGER)) FROM threads {archived} GROUP BY cwd"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Option<i64>>(2)?,
        ))
    })?;
    Ok(rows
        .flatten()
        .filter_map(|(cwd, n, last)| {
            let cwd = cwd.filter(|c| !c.is_empty())?;
            Some((
                PathBuf::from(cwd),
                u32::try_from(n).unwrap_or(u32::MAX),
                last,
            ))
        })
        .collect())
}

/// The tables of the database; a busy or unreadable database is an error
/// (the caller skips it and says so), not an empty one.
fn tables(conn: &Connection) -> rusqlite::Result<BTreeSet<String>> {
    let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// Comparison key of a folder: separators unified, trailing ones dropped,
/// case folded on Windows.
pub fn key(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let trimmed = s.trim_end_matches('/');
    let s = if trimmed.is_empty() { "/" } else { trimmed };
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s.to_owned()
    }
}

/// Which folders may be offered.
pub struct Filter {
    known: BTreeSet<String>,
    answered: BTreeSet<String>,
    excluded: BTreeSet<String>,
    excluded_trees: Vec<String>,
}

impl Filter {
    /// For `home` / `home_dir`, skipping `known` and `answered` folders.
    pub fn new(home: &CodexHome, home_dir: &Path, known: &[PathBuf], answered: &[PathBuf]) -> Self {
        let both = |p: &Path| {
            let mut v = vec![key(p)];
            if let Ok(c) = superba_core::paths::canonical(p) {
                v.push(key(&c));
            }
            v
        };
        let mut excluded: BTreeSet<String> = both(home_dir).into_iter().collect();
        let mut excluded_trees = Vec::new();
        let mut temp = vec![std::env::temp_dir()];
        if !cfg!(windows) {
            temp.extend(
                ["/tmp", "/var/tmp", "/private/tmp", "/private/var/folders"].map(PathBuf::from),
            );
        }
        for t in &temp {
            excluded_trees.extend(both(t));
        }
        excluded_trees.extend(both(home.root()));
        let mut chats = home_dir.to_path_buf();
        chats.extend(APP_CHATS_DIR);
        excluded_trees.extend(both(&chats));
        excluded.extend(excluded_trees.iter().cloned());
        Self {
            known: known.iter().flat_map(|p| both(p)).collect(),
            answered: answered.iter().flat_map(|p| both(p)).collect(),
            excluded,
            excluded_trees,
        }
    }

    /// Whether `path` is a folder the app knows or one offered before.
    pub fn settled(&self, path: &Path) -> bool {
        let mut keys = vec![key(path)];
        if let Ok(c) = superba_core::paths::canonical(path) {
            keys.push(key(&c));
        }
        keys.iter()
            .any(|k| self.known.contains(k) || self.answered.contains(k))
    }

    /// Whether `path` may be offered.
    pub fn allows(&self, path: &Path) -> bool {
        if !path.is_absolute() || path.parent().is_none() || !path.is_dir() {
            return false;
        }
        let mut keys = vec![key(path)];
        if let Ok(c) = superba_core::paths::canonical(path) {
            if c.parent().is_none() {
                return false;
            }
            keys.push(key(&c));
        }
        !keys.iter().any(|k| {
            self.known.contains(k)
                || self.answered.contains(k)
                || self.excluded.contains(k)
                || self
                    .excluded_trees
                    .iter()
                    .any(|t| k.starts_with(&format!("{t}/")))
        })
    }
}

/// Merge, filter and rank. Folders from the project list first gather the
/// chats that ran in them; folders seen only in chats follow the same
/// filter, at most [`MAX_CHAT_FOLDERS`] of them, and only when the project
/// list offers something.
pub fn candidates(
    projects: Vec<ProjectRow>,
    chats: Vec<(PathBuf, u32, Option<i64>)>,
    filter: &Filter,
) -> Vec<LostProject> {
    let chat_of: BTreeMap<String, (u32, Option<i64>)> = chats
        .iter()
        .map(|(p, n, last)| (key(p), (*n, *last)))
        .collect();
    let mut seen = BTreeSet::new();
    let mut out: Vec<(Option<i64>, LostProject)> = Vec::new();
    for row in projects {
        // A project whose first folder the app knows again, or that was
        // offered before, is settled as a whole.
        if filter.settled(&row.roots[0]) {
            continue;
        }
        // Every root passes the same filter; one that does not (the home
        // directory, a missing folder, ...) or that an earlier candidate
        // already has is left out of the project, which stays offered with
        // the roots it has left.
        let mut roots = Vec::new();
        for r in &row.roots {
            if filter.allows(r) && seen.insert(key(r)) {
                roots.push(r.clone());
            }
        }
        if roots.is_empty() {
            continue;
        }
        let path = roots.remove(0);
        let extra_roots = roots;
        let mut sources = vec![LostProjectSource::ProjectList];
        let mut last = row.updated_ms;
        if let Some((n, l)) = chat_of.get(&key(&path)) {
            sources.push(LostProjectSource::Chats { count: *n });
            last = last.max(*l);
        }
        let name = if row.name.trim().is_empty() {
            folder_name(&path)
        } else {
            row.name.clone()
        };
        out.push((
            last,
            LostProject {
                extra_roots,
                path,
                name,
                last_used: last.map(Timestamp::from_millis),
                sources,
                app_id: row.app_id,
            },
        ));
    }
    if out.is_empty() {
        return Vec::new();
    }
    let mut chat_only: Vec<(Option<i64>, LostProject)> = chats
        .into_iter()
        .filter(|(p, _, _)| filter.allows(p) && !seen.contains(&key(p)))
        .map(|(path, count, last)| {
            (
                last,
                LostProject {
                    name: folder_name(&path),
                    path,
                    extra_roots: Vec::new(),
                    last_used: last.map(Timestamp::from_millis),
                    sources: vec![LostProjectSource::Chats { count }],
                    app_id: None,
                },
            )
        })
        .collect();
    chat_only.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.path.cmp(&b.1.path)));
    chat_only.truncate(MAX_CHAT_FOLDERS);
    out.extend(chat_only);
    out.sort_by(|a, b| {
        b.1.recommended()
            .cmp(&a.1.recommended())
            .then_with(|| b.0.cmp(&a.0))
            .then_with(|| a.1.path.cmp(&b.1.path))
    });
    out.into_iter().map(|(_, p)| p).collect()
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}
