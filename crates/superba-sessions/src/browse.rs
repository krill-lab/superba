//! The session browser's list ([`list`]): every `threads` row of the local
//! `state_*.sqlite` databases and every row of the desktop app's
//! `local_thread_catalog` (which also holds the threads of SSH and WSL hosts),
//! merged by host and thread id. Read-only: databases are opened
//! `SQLITE_OPEN_READ_ONLY` (a WAL reader never blocks Codex's writer), shares
//! that cannot be locked are skipped, and each source is capped at
//! [`MAX_ROWS`] newest threads.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use superba_core::{CodexHome, LOCAL_HOST_ID, SessionEntry, SessionList, Timestamp};

use crate::sqlite;
use crate::titles::clean;

/// Rows read per source (newest first).
pub const MAX_ROWS: usize = 5000;

/// List the threads of `home` (and of the hosts the app's catalog knows).
pub fn list(home: &CodexHome, home_dir: Option<&Path>) -> SessionList {
    let mut warnings = Vec::new();
    let dirs = crate::inventory::sqlite_dirs(home, home_dir, &mut warnings);
    let mut map: BTreeMap<(String, String), SessionEntry> = BTreeMap::new();
    let mut kinds: BTreeMap<String, String> = BTreeMap::new();
    let mut truncated = false;
    for path in sqlite::candidate_files(&dirs) {
        if crate::util::is_unlockable_share(&path) {
            continue;
        }
        let Ok(conn) = sqlite::open_ro(&path) else {
            continue;
        };
        let tables = tables(&conn);
        if tables.contains("threads") {
            match read_threads(&conn) {
                Ok((rows, cut)) => {
                    truncated |= cut;
                    for row in rows {
                        merge(&mut map, row, true);
                    }
                }
                Err(e) => warnings.push(crate::text::db_unreadable(&path, &e.to_string())),
            }
        }
        if tables.contains("local_thread_catalog") {
            match read_catalog(&conn) {
                Ok((rows, cut)) => {
                    truncated |= cut;
                    for row in rows {
                        merge(&mut map, row, false);
                    }
                }
                Err(e) => warnings.push(crate::text::db_unreadable(&path, &e.to_string())),
            }
        }
        if tables.contains("local_thread_catalog_hosts")
            && let Ok(mut stmt) =
                conn.prepare("SELECT host_id, host_kind FROM local_thread_catalog_hosts")
            && let Ok(rows) =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        {
            kinds.extend(rows.flatten());
        }
    }
    let mut entries: Vec<SessionEntry> = map
        .into_values()
        .map(|mut e| {
            if e.host_kind.is_none() {
                e.host_kind = kinds
                    .get(&e.host_id)
                    .cloned()
                    .or_else(|| (e.host_id == LOCAL_HOST_ID).then(|| LOCAL_HOST_ID.to_owned()));
            }
            e
        })
        .collect();
    entries.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.thread_id.cmp(&b.thread_id))
    });
    SessionList {
        entries,
        truncated,
        scanned_at: Timestamp::now(),
        warnings,
    }
}

/// Fill `map` with `row`: a state row wins for every field it has; a catalog
/// row only fills what is missing.
fn merge(map: &mut BTreeMap<(String, String), SessionEntry>, row: SessionEntry, state: bool) {
    let key = (row.host_id.clone(), row.thread_id.clone());
    let Some(have) = map.get_mut(&key) else {
        map.insert(key, row);
        return;
    };
    let (mut base, other) = if state {
        (row, std::mem::take(have))
    } else {
        (std::mem::take(have), row)
    };
    if base.title.is_empty() {
        base.title = other.title;
    }
    if base.cwd.is_empty() {
        base.cwd = other.cwd;
    }
    if base.provider.is_empty() {
        base.provider = other.provider;
    }
    base.model = base.model.or(other.model);
    base.git_branch = base.git_branch.or(other.git_branch);
    base.created_at = base.created_at.or(other.created_at);
    base.updated_at = base.updated_at.max(other.updated_at);
    base.tokens_used = base.tokens_used.or(other.tokens_used);
    base.rollout_path = base.rollout_path.or(other.rollout_path);
    base.host_kind = base.host_kind.or(other.host_kind);
    *have = base;
}

fn tables(conn: &Connection) -> BTreeSet<String> {
    let Ok(mut stmt) = conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table'") else {
        return BTreeSet::new();
    };
    let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) else {
        return BTreeSet::new();
    };
    rows.flatten().collect()
}

fn columns(conn: &Connection, table: &str) -> BTreeSet<String> {
    let Ok(mut stmt) = conn.prepare(&format!("PRAGMA table_info({table})")) else {
        return BTreeSet::new();
    };
    let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(1)) else {
        return BTreeSet::new();
    };
    rows.flatten().collect()
}

/// `name` when the table has it, else `NULL`.
fn col(have: &BTreeSet<String>, name: &str) -> String {
    if have.contains(name) {
        name.to_owned()
    } else {
        "NULL".to_owned()
    }
}

/// Seconds or milliseconds (Codex mixes both) to a timestamp.
fn time_of(v: Option<f64>) -> Option<Timestamp> {
    let v = v.filter(|v| v.is_finite() && *v > 0.0)?;
    Some(Timestamp(if v > 1e11 {
        v as i64
    } else {
        (v * 1000.0) as i64
    }))
}

fn text_of(v: Option<String>) -> String {
    v.map(|s| s.trim().to_owned()).unwrap_or_default()
}

fn opt_text(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

type Rows = (Vec<SessionEntry>, bool);

fn read_threads(conn: &Connection) -> rusqlite::Result<Rows> {
    let have = columns(conn, "threads");
    if !have.contains("id") {
        return Ok((Vec::new(), false));
    }
    let updated = if have.contains("updated_at_ms") {
        "COALESCE(updated_at_ms, updated_at)"
    } else {
        "updated_at"
    };
    let created = if have.contains("created_at_ms") {
        "COALESCE(created_at_ms, created_at)"
    } else {
        "created_at"
    };
    let sql = format!(
        "SELECT id, {name}, {title}, {first}, {cwd}, {provider}, {model}, {branch}, \
         CAST({created} AS REAL), CAST({updated} AS REAL), {tokens}, {archived}, {rollout} \
         FROM threads ORDER BY {updated} DESC LIMIT {limit}",
        name = col(&have, "name"),
        title = col(&have, "title"),
        first = col(&have, "first_user_message"),
        cwd = col(&have, "cwd"),
        provider = col(&have, "model_provider"),
        model = col(&have, "model"),
        branch = col(&have, "git_branch"),
        created = if have.contains("created_at") {
            created
        } else {
            "NULL"
        },
        updated = if have.contains("updated_at") {
            updated
        } else {
            "NULL"
        },
        tokens = col(&have, "tokens_used"),
        archived = col(&have, "archived"),
        rollout = col(&have, "rollout_path"),
        limit = MAX_ROWS + 1,
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows: Vec<SessionEntry> = stmt
        .query_map([], |r| {
            let name = opt_text(r.get(1).ok().flatten());
            let title = opt_text(r.get(2).ok().flatten());
            let first = opt_text(r.get(3).ok().flatten());
            Ok(SessionEntry {
                host_id: LOCAL_HOST_ID.to_owned(),
                host_kind: Some(LOCAL_HOST_ID.to_owned()),
                thread_id: r.get(0)?,
                title: name
                    .or(title)
                    .or(first)
                    .map(|t| clean(&t))
                    .unwrap_or_default(),
                cwd: text_of(r.get(4).ok().flatten()),
                provider: text_of(r.get(5).ok().flatten()),
                model: opt_text(r.get(6).ok().flatten()),
                git_branch: opt_text(r.get(7).ok().flatten()),
                created_at: time_of(r.get(8).ok().flatten()),
                updated_at: time_of(r.get(9).ok().flatten()),
                tokens_used: r
                    .get::<_, Option<i64>>(10)
                    .ok()
                    .flatten()
                    .and_then(|t| u64::try_from(t).ok())
                    .filter(|t| *t > 0),
                archived: r.get::<_, Option<i64>>(11).ok().flatten().unwrap_or(0) != 0,
                rollout_path: opt_text(r.get(12).ok().flatten()).map(PathBuf::from),
            })
        })?
        .flatten()
        .collect();
    let cut = rows.len() > MAX_ROWS;
    rows.truncate(MAX_ROWS);
    Ok((rows, cut))
}

fn read_catalog(conn: &Connection) -> rusqlite::Result<Rows> {
    let have = columns(conn, "local_thread_catalog");
    if !have.contains("host_id") || !have.contains("thread_id") {
        return Ok((Vec::new(), false));
    }
    let missing = if have.contains("missing_candidate") {
        "WHERE missing_candidate = 0"
    } else {
        ""
    };
    let sql = format!(
        "SELECT host_id, thread_id, {title}, {cwd}, {provider}, {branch}, \
         CAST({created} AS REAL), CAST({updated} AS REAL) FROM local_thread_catalog {missing} \
         ORDER BY {updated} DESC LIMIT {limit}",
        title = col(&have, "display_title"),
        cwd = col(&have, "cwd"),
        provider = col(&have, "model_provider"),
        branch = col(&have, "git_branch"),
        created = col(&have, "source_created_at"),
        updated = col(&have, "source_updated_at"),
        limit = MAX_ROWS + 1,
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows: Vec<SessionEntry> = stmt
        .query_map([], |r| {
            Ok(SessionEntry {
                host_id: r.get(0)?,
                thread_id: r.get(1)?,
                title: opt_text(r.get(2).ok().flatten())
                    .map(|t| clean(&t))
                    .unwrap_or_default(),
                cwd: text_of(r.get(3).ok().flatten()),
                provider: text_of(r.get(4).ok().flatten()),
                git_branch: opt_text(r.get(5).ok().flatten()),
                created_at: time_of(r.get(6).ok().flatten()),
                updated_at: time_of(r.get(7).ok().flatten()),
                ..SessionEntry::default()
            })
        })?
        .flatten()
        .collect();
    let cut = rows.len() > MAX_ROWS;
    rows.truncate(MAX_ROWS);
    Ok((rows, cut))
}
