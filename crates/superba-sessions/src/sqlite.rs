//! Codex SQLite databases (`state_5.sqlite`, `thread_history_1.sqlite`, the
//! app's `sqlite/*.db`, ... under `CODEX_HOME`, `CODEX_SQLITE_HOME` and the
//! `sqlite_home` config key).
//!
//! Tables are classified by their columns rather than by name, so new Codex
//! versions keep working:
//! - a *provider table* has `model_provider` plus a thread id column
//!   (`thread_id`, or `id`), e.g. `threads`, `local_thread_catalog`;
//! - an *offset table* has `thread_id` plus integer columns ending in
//!   `byte_offset`, e.g. `thread_turns`, `thread_history_projection_state`. Their
//!   values are byte offsets into the uncompressed rollout and must follow the
//!   rollout rewrite.
//!
//! Rows are identified by an unshadowed `rowid` alias (stable even when key
//! columns change, and unique even with `NULL` primary key parts), or by the
//! primary key of `WITHOUT ROWID` tables (then the key after the change is
//! recorded too). Tables with triggers that fire on an update of a column we
//! change are refused: their side effects could not be rolled back exactly.
//!
//! Writes happen in one `BEGIN IMMEDIATE` transaction per database with a busy
//! timeout; each changed cell is recorded (key, before, after) so it can be
//! restored exactly. Backups use the SQLite online backup API (which reads
//! through the WAL) followed by `integrity_check`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags, TransactionBehavior, params_from_iter};
use serde::{Deserialize, Serialize};
use superba_core::{Error, Notice, Result};

use crate::rollout::{Edit, shift_offset};
use crate::text;
use crate::util::{is_unlockable_share, normalize_dir};

/// Busy timeout for every connection.
pub(crate) const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
/// File extensions considered SQLite databases.
const DB_EXTENSIONS: &[&str] = &["sqlite", "db", "sqlite3"];
/// The provider column.
pub(crate) const PROVIDER_COL: &str = "model_provider";

/// How rows of a table are identified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum KeySpec {
    /// An unshadowed rowid alias (`rowid`, `_rowid_` or `oid`).
    Rowid(String),
    /// Primary key columns of a `WITHOUT ROWID` table, in key order.
    Columns(Vec<String>),
}

impl KeySpec {
    fn columns(&self) -> Vec<String> {
        match self {
            KeySpec::Rowid(alias) => vec![alias.clone()],
            KeySpec::Columns(cols) => cols.clone(),
        }
    }
}

/// A table holding a provider id per thread.
#[derive(Clone, Debug)]
pub(crate) struct ProviderTable {
    pub name: String,
    pub thread_col: String,
    pub key: KeySpec,
    /// Counted as Codex's thread list (`threads`).
    pub is_threads: bool,
    pub archived_col: Option<String>,
    pub created_ms_col: Option<String>,
    pub created_s_col: Option<String>,
    pub rollout_col: Option<String>,
    pub title_cols: Vec<String>,
    /// Triggers that fire when `model_provider` is updated.
    pub update_triggers: Vec<String>,
}

/// A table holding byte offsets into a thread's rollout.
#[derive(Clone, Debug)]
pub(crate) struct OffsetTable {
    pub name: String,
    pub key: KeySpec,
    pub offset_cols: Vec<String>,
    /// Triggers that fire when an offset column is updated.
    pub update_triggers: Vec<String>,
}

/// Classified schema of one database.
#[derive(Clone, Debug)]
pub(crate) struct DbInfo {
    pub path: PathBuf,
    pub provider_tables: Vec<ProviderTable>,
    pub offset_tables: Vec<OffsetTable>,
}

impl DbInfo {
    /// The database matters for migration.
    pub(crate) fn is_relevant(&self) -> bool {
        !self.provider_tables.is_empty() || !self.offset_tables.is_empty()
    }
}

/// A row of a provider table.
#[derive(Clone, Debug)]
pub(crate) struct ThreadRow {
    /// Index into the inventory's database list.
    pub db: usize,
    pub is_threads: bool,
    pub thread_id: String,
    pub provider: String,
    pub archived: bool,
    pub created_ms: Option<i64>,
    pub title: Option<String>,
    /// `threads.rollout_path`: the rollout Codex reads (and projects).
    pub rollout_path: Option<PathBuf>,
}

/// Serializable SQLite value (ledger).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub(crate) enum SqlValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    /// Hex.
    Blob(String),
}

impl From<Value> for SqlValue {
    fn from(v: Value) -> Self {
        match v {
            Value::Null => SqlValue::Null,
            Value::Integer(i) => SqlValue::Integer(i),
            Value::Real(r) => SqlValue::Real(r),
            Value::Text(t) => SqlValue::Text(t),
            Value::Blob(b) => SqlValue::Blob(hex::encode(b)),
        }
    }
}

impl SqlValue {
    fn to_value(&self) -> Result<Value> {
        Ok(match self {
            SqlValue::Null => Value::Null,
            SqlValue::Integer(i) => Value::Integer(*i),
            SqlValue::Real(r) => Value::Real(*r),
            SqlValue::Text(t) => Value::Text(t.clone()),
            SqlValue::Blob(h) => {
                Value::Blob(hex::decode(h).map_err(|e| Error::parse("ledger blob value", e))?)
            }
        })
    }
}

/// One changed cell.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct CellChange {
    pub table: String,
    /// Key columns and values before the change.
    pub key: Vec<(String, SqlValue)>,
    /// Key after the change, when the changed column is part of the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_after: Option<Vec<(String, SqlValue)>>,
    pub column: String,
    pub thread_id: String,
    pub before: SqlValue,
    pub after: SqlValue,
}

impl CellChange {
    /// A provider change (as opposed to an offset shift).
    pub(crate) fn is_provider(&self) -> bool {
        self.column == PROVIDER_COL
    }
}

/// State of a recorded cell now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CellState {
    /// Holds the migrated value.
    After,
    /// Holds the original value.
    Before,
    /// Changed since, or the row is gone.
    Changed,
}

/// Quote an SQL identifier.
pub(crate) fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// SQLite failure on `path`, classified so the message names the real cause: a
/// busy or locked database is a genuine lock conflict (another process is
/// writing it, after the busy timeout); a database SQLite cannot open is reported
/// with the OS error behind it when the file itself cannot be opened either (too
/// many open files, permission denied, missing); anything else stays an SQLite
/// error.
fn db_err(path: &Path, err: rusqlite::Error) -> Error {
    match err.sqlite_error_code() {
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
            Error::Locked {
                what: format!(
                    "the database {} is in use by another process ({err})",
                    path.display()
                ),
            }
        }
        Some(rusqlite::ErrorCode::CannotOpen) => match std::fs::File::open(path) {
            Err(io) => Error::io(path, &io),
            Ok(_) => Error::other(format!("SQLite error on {}: {err}", path.display())),
        },
        _ => Error::other(format!("SQLite error on {}: {err}", path.display())),
    }
}

/// Candidate database files in `dirs` (and their `sqlite/` subdirectories).
/// Symbolic links are followed (the target path is returned); duplicates are
/// removed.
pub(crate) fn candidate_files(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut seen = BTreeSet::new();
    for dir in dirs {
        for d in [dir.clone(), dir.join("sqlite")] {
            let Ok(entries) = std::fs::read_dir(&d) else {
                continue;
            };
            let mut files: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    !name.starts_with('.')
                        && p.extension()
                            .and_then(|e| e.to_str())
                            .is_some_and(|e| DB_EXTENSIONS.contains(&e))
                })
                .filter_map(|p| {
                    let meta = std::fs::symlink_metadata(&p).ok()?;
                    if meta.file_type().is_symlink() {
                        let target = std::fs::canonicalize(&p).ok()?;
                        target.is_file().then_some(target)
                    } else {
                        meta.is_file().then_some(p)
                    }
                })
                .collect();
            files.sort();
            for f in files {
                if seen.insert(normalize_dir(&f)) {
                    out.push(f);
                }
            }
        }
    }
    out
}

/// Open read-only.
pub(crate) fn open_ro(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| db_err(path, e))?;
    conn.busy_timeout(BUSY_TIMEOUT)
        .map_err(|e| db_err(path, e))?;
    Ok(conn)
}

/// Open read-write (the file must exist).
pub(crate) fn open_rw(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| db_err(path, e))?;
    conn.busy_timeout(BUSY_TIMEOUT)
        .map_err(|e| db_err(path, e))?;
    Ok(conn)
}

struct Column {
    name: String,
    decl_type: String,
    pk: i64,
}

fn columns(conn: &Connection, table: &str) -> rusqlite::Result<Vec<Column>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", ident(table)))?;
    let rows = stmt.query_map([], |r| {
        Ok(Column {
            name: r.get(1)?,
            decl_type: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            pk: r.get(5)?,
        })
    })?;
    rows.collect()
}

/// Collapse whitespace and upper-case, for matching SQL text.
fn squash(sql: &str) -> String {
    sql.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_uppercase()
}

/// Rowid tables use an unshadowed rowid alias; `WITHOUT ROWID` tables their
/// primary key. `None` when neither is usable.
fn key_spec(create_sql: &str, cols: &[Column]) -> Option<KeySpec> {
    let without_rowid = squash(create_sql).contains("WITHOUT ROWID");
    if !without_rowid {
        let alias = ["rowid", "_rowid_", "oid"]
            .into_iter()
            .find(|a| !cols.iter().any(|c| c.name.eq_ignore_ascii_case(a)))?;
        return Some(KeySpec::Rowid(alias.to_owned()));
    }
    let mut pk: Vec<&Column> = cols.iter().filter(|c| c.pk > 0).collect();
    pk.sort_by_key(|c| c.pk);
    (!pk.is_empty()).then(|| KeySpec::Columns(pk.into_iter().map(|c| c.name.clone()).collect()))
}

/// Names of the triggers on `table` that fire on an UPDATE touching any of
/// `cols` (`UPDATE OF <list>` naming one of them, or a plain `UPDATE`).
fn update_triggers(triggers: &[(String, String)], cols: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for (name, sql) in triggers {
        let text = squash(sql);
        // The event clause sits between the trigger name and " ON <table>".
        let Some(on) = text.find(" ON ") else {
            out.push(name.clone());
            continue;
        };
        let toks: Vec<&str> = text[..on].split(' ').collect();
        // CREATE [TEMP] TRIGGER [IF NOT EXISTS] name [BEFORE|AFTER|INSTEAD OF] event
        let Some(mut i) = toks.iter().position(|t| *t == "TRIGGER").map(|i| i + 1) else {
            out.push(name.clone());
            continue;
        };
        if toks[i..].starts_with(&["IF", "NOT", "EXISTS"]) {
            i += 3;
        }
        i += 1; // the trigger name
        match toks.get(i) {
            Some(&"BEFORE") | Some(&"AFTER") => i += 1,
            Some(&"INSTEAD") => i += 2,
            _ => {}
        }
        if toks.get(i) != Some(&"UPDATE") {
            continue;
        }
        let rest = toks[i + 1..].join(" ");
        let fires = match rest.strip_prefix("OF ") {
            None => true,
            Some(list) => list.split(',').any(|c| {
                let c = c.trim().trim_matches(['"', '`', '[', ']']);
                cols.iter().any(|col| col.eq_ignore_ascii_case(c))
            }),
        };
        if fires {
            out.push(name.clone());
        }
    }
    out
}

/// Classify the tables of `path` (read-only).
pub(crate) fn inspect(path: &Path) -> std::result::Result<DbInfo, InspectError> {
    let conn = open_ro(path).map_err(InspectError::Other)?;
    let mut info = DbInfo {
        path: path.to_path_buf(),
        provider_tables: Vec::new(),
        offset_tables: Vec::new(),
    };
    let sqlite = |e: rusqlite::Error| {
        if e.sqlite_error_code() == Some(rusqlite::ErrorCode::NotADatabase) {
            InspectError::NotADatabase
        } else {
            InspectError::Other(db_err(path, e))
        }
    };
    let master: Vec<(String, String, String, String)> = {
        let mut stmt = conn
            .prepare("SELECT type, name, tbl_name, COALESCE(sql, '') FROM sqlite_master")
            .map_err(sqlite)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .map_err(sqlite)?;
        rows.collect::<rusqlite::Result<_>>().map_err(sqlite)?
    };
    let mut tables: Vec<(&str, &str)> = master
        .iter()
        .filter(|(t, name, _, _)| t == "table" && !name.starts_with("sqlite_"))
        .map(|(_, name, _, sql)| (name.as_str(), sql.as_str()))
        .collect();
    tables.sort();
    for (table, create_sql) in tables {
        let cols = columns(&conn, table).map_err(sqlite)?;
        let has = |n: &str| cols.iter().any(|c| c.name.eq_ignore_ascii_case(n));
        let find = |n: &str| {
            cols.iter()
                .find(|c| c.name.eq_ignore_ascii_case(n))
                .map(|c| c.name.clone())
        };
        let triggers: Vec<(String, String)> = master
            .iter()
            .filter(|(t, _, tbl, _)| t == "trigger" && tbl.eq_ignore_ascii_case(table))
            .map(|(_, name, _, sql)| (name.clone(), sql.clone()))
            .collect();
        let thread_col = find("thread_id").or_else(|| find("id"));
        let offset_cols: Vec<String> = cols
            .iter()
            .filter(|c| {
                c.name.to_ascii_lowercase().ends_with("byte_offset")
                    && (c.decl_type.is_empty() || c.decl_type.to_ascii_uppercase().contains("INT"))
            })
            .map(|c| c.name.clone())
            .collect();
        let is_provider = has(PROVIDER_COL) && thread_col.is_some();
        let is_offsets = has("thread_id") && !offset_cols.is_empty();
        if !is_provider && !is_offsets {
            continue;
        }
        let Some(key) = key_spec(create_sql, &cols) else {
            return Err(InspectError::Other(Error::Unsupported {
                what: format!(
                    "table {table} of {} has no usable row identity",
                    path.display()
                ),
            }));
        };
        if let (true, Some(thread_col)) = (is_provider, thread_col) {
            info.provider_tables.push(ProviderTable {
                key: key.clone(),
                is_threads: table == "threads",
                archived_col: find("archived"),
                created_ms_col: find("created_at_ms"),
                created_s_col: find("created_at"),
                rollout_col: find("rollout_path"),
                title_cols: ["name", "title", "display_title"]
                    .iter()
                    .filter_map(|n| find(n))
                    .collect(),
                update_triggers: update_triggers(&triggers, &[PROVIDER_COL.to_owned()]),
                name: table.to_owned(),
                thread_col,
            });
        }
        if is_offsets {
            info.offset_tables.push(OffsetTable {
                key,
                name: table.to_owned(),
                update_triggers: update_triggers(&triggers, &offset_cols),
                offset_cols,
            });
        }
    }
    Ok(info)
}

/// Why a database could not be classified.
#[derive(Debug)]
pub(crate) enum InspectError {
    /// Not an SQLite file (an unrelated `*.db`): ignored with a warning.
    NotADatabase,
    /// Busy, unreadable, ...: writes must not proceed without it.
    Other(Error),
}

/// Inspect every candidate. Databases on unlockable shares are skipped and
/// non-SQLite files ignored (both with a warning); databases that could not be
/// read are returned in the second list (writes refuse while it is not empty).
pub(crate) fn discover(
    dirs: &[PathBuf],
    warnings: &mut Vec<Notice>,
) -> (Vec<DbInfo>, Vec<(PathBuf, Error)>) {
    let mut out = Vec::new();
    let mut failed = Vec::new();
    for path in candidate_files(dirs) {
        if is_unlockable_share(&path) {
            warnings.push(text::db_on_share(&path));
            continue;
        }
        match inspect(&path) {
            Ok(info) if info.is_relevant() => out.push(info),
            Ok(_) => {}
            Err(InspectError::NotADatabase) => {
                warnings.push(text::db_unreadable(&path, "not an SQLite database"));
            }
            Err(InspectError::Other(err)) => {
                warnings.push(text::db_unreadable(&path, &err.to_string()));
                failed.push((path, err));
            }
        }
    }
    (out, failed)
}

fn value_text(v: Value) -> Option<String> {
    match v {
        Value::Text(t) => Some(t),
        Value::Integer(i) => Some(i.to_string()),
        Value::Blob(b) => String::from_utf8(b).ok(),
        _ => None,
    }
}

fn value_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Integer(i) => Some(*i),
        Value::Real(r) if r.is_finite() && r.abs() < 9.0e15 => Some(*r as i64),
        Value::Text(t) => t.trim().parse().ok(),
        _ => None,
    }
}

/// Rows of every provider table of `info` (read-only).
pub(crate) fn read_rows(info: &DbInfo, db_index: usize) -> Result<Vec<ThreadRow>> {
    let path = &info.path;
    let conn = open_ro(path)?;
    let mut out = Vec::new();
    for t in &info.provider_tables {
        let mut select = vec![ident(&t.thread_col), ident(PROVIDER_COL)];
        let mut push = |col: &Option<String>| {
            col.as_ref().map(|c| {
                select.push(ident(c));
                select.len() - 1
            })
        };
        let archived_idx = push(&t.archived_col);
        let ms_idx = push(&t.created_ms_col);
        let s_idx = push(&t.created_s_col);
        let rollout_idx = push(&t.rollout_col);
        let title_start = select.len();
        for c in &t.title_cols {
            select.push(ident(c));
        }
        let sql = format!("SELECT {} FROM {}", select.join(", "), ident(&t.name));
        let mut stmt = conn.prepare(&sql).map_err(|e| db_err(path, e))?;
        let ncols = select.len();
        let rows = stmt
            .query_map([], |r| {
                let mut vals = Vec::with_capacity(ncols);
                for i in 0..ncols {
                    vals.push(r.get::<_, Value>(i)?);
                }
                Ok(vals)
            })
            .map_err(|e| db_err(path, e))?;
        for vals in rows {
            let mut vals = vals.map_err(|e| db_err(path, e))?;
            let created_ms = ms_idx.and_then(|i| value_i64(&vals[i])).or_else(|| {
                s_idx
                    .and_then(|i| value_i64(&vals[i]))
                    .and_then(|s| s.checked_mul(1000))
            });
            let archived = archived_idx
                .and_then(|i| value_i64(&vals[i]))
                .is_some_and(|v| v != 0);
            let title = vals[title_start..].iter().find_map(|v| match v {
                Value::Text(t) if !t.trim().is_empty() => Some(t.clone()),
                _ => None,
            });
            let rollout_path = rollout_idx
                .and_then(|i| value_text(vals[i].clone()))
                .filter(|p| !p.is_empty())
                .map(PathBuf::from);
            let provider = value_text(std::mem::replace(&mut vals[1], Value::Null));
            let thread_id = value_text(std::mem::replace(&mut vals[0], Value::Null));
            let (Some(thread_id), Some(provider)) = (thread_id, provider) else {
                continue;
            };
            out.push(ThreadRow {
                db: db_index,
                is_threads: t.is_threads,
                thread_id,
                provider,
                archived,
                created_ms,
                title,
                rollout_path,
            });
        }
    }
    Ok(out)
}

/// `PRAGMA integrity_check` must answer exactly `ok`.
pub(crate) fn integrity_check(conn: &Connection, path: &Path) -> Result<()> {
    let mut stmt = conn
        .prepare("PRAGMA integrity_check")
        .map_err(|e| db_err(path, e))?;
    let rows: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| db_err(path, e))?
        .collect::<rusqlite::Result<_>>()
        .map_err(|e| db_err(path, e))?;
    if rows.len() == 1 && rows[0] == "ok" {
        Ok(())
    } else {
        Err(Error::other(format!(
            "integrity_check failed for {}: {}",
            path.display(),
            rows.join("; ")
        )))
    }
}

/// Copy the database behind `src` to `dest` with the online backup API (reads
/// through the WAL), then run `integrity_check` on the copy.
pub(crate) fn backup(src: &Connection, src_path: &Path, dest: &Path) -> Result<()> {
    if dest.exists() {
        return Err(Error::Conflict {
            message: format!("backup target {} already exists", dest.display()),
        });
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, &e))?;
    }
    let mut out = Connection::open(dest).map_err(|e| db_err(dest, e))?;
    {
        let bk = rusqlite::backup::Backup::new(src, &mut out).map_err(|e| db_err(src_path, e))?;
        let deadline = Instant::now() + BUSY_TIMEOUT * 2;
        loop {
            match bk.step(256).map_err(|e| db_err(src_path, e))? {
                rusqlite::backup::StepResult::Done => break,
                rusqlite::backup::StepResult::More => {}
                _ => {
                    if Instant::now() > deadline {
                        return Err(Error::Locked {
                            what: format!("{} is busy", src_path.display()),
                        });
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        }
    }
    // A self-contained single file (no -wal/-shm), so a manual restore is a copy.
    out.pragma_update(None, "journal_mode", "DELETE")
        .map_err(|e| db_err(dest, e))?;
    integrity_check(&out, dest)?;
    out.close().map_err(|(_, e)| db_err(dest, e))
}

fn key_where(key: &[(String, SqlValue)]) -> String {
    key.iter()
        .map(|(c, _)| format!("{} IS ?", ident(c)))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn key_params(key: &[(String, SqlValue)]) -> Result<Vec<Value>> {
    key.iter().map(|(_, v)| v.to_value()).collect()
}

/// Work for one database.
pub(crate) struct DbWork<'a> {
    /// thread id -> (source providers, destination).
    pub providers: &'a BTreeMap<String, (BTreeSet<String>, String)>,
    /// thread id -> edits of the rollout the projection follows.
    pub offsets: &'a BTreeMap<String, Vec<Edit>>,
}

/// Tables of `info` that `work` would write but that carry update triggers.
pub(crate) fn blocking_triggers(info: &DbInfo, work: &DbWork<'_>) -> Vec<String> {
    let mut out = Vec::new();
    if !work.providers.is_empty() {
        for t in &info.provider_tables {
            out.extend(t.update_triggers.iter().map(|n| format!("{}.{n}", t.name)));
        }
    }
    if !work.offsets.is_empty() {
        for t in &info.offset_tables {
            out.extend(t.update_triggers.iter().map(|n| format!("{}.{n}", t.name)));
        }
    }
    out
}

fn select_rows(
    tx: &Connection,
    path: &Path,
    sql: &str,
    params: &[&str],
    width: usize,
) -> Result<Vec<Vec<Value>>> {
    let mut stmt = tx.prepare_cached(sql).map_err(|e| db_err(path, e))?;
    let rows = stmt
        .query_map(params_from_iter(params.iter()), |r| {
            (0..width).map(|i| r.get::<_, Value>(i)).collect()
        })
        .map_err(|e| db_err(path, e))?
        .collect::<rusqlite::Result<_>>()
        .map_err(|e| db_err(path, e))?;
    Ok(rows)
}

/// Apply provider moves and offset shifts in one immediate transaction. The
/// changes are computed inside the transaction and passed to `record` (which must
/// persist them) before the commit.
pub(crate) fn apply(
    conn: &mut Connection,
    info: &DbInfo,
    work: &DbWork<'_>,
    record: impl FnOnce(&[CellChange]) -> Result<()>,
) -> Result<Vec<CellChange>> {
    let path = info.path.clone();
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| db_err(&path, e))?;
    let mut changes = Vec::new();
    for t in &info.provider_tables {
        let keys = t.key.columns();
        let sql = format!(
            "SELECT {}, {} FROM {} WHERE {} = ?",
            keys.iter().map(|k| ident(k)).collect::<Vec<_>>().join(", "),
            ident(PROVIDER_COL),
            ident(&t.name),
            ident(&t.thread_col),
        );
        for (thread_id, (sources, to)) in work.providers {
            for row in select_rows(&tx, &path, &sql, &[thread_id], keys.len() + 1)? {
                let Some(current) = value_text(row[keys.len()].clone()) else {
                    continue;
                };
                if !sources.contains(&current) {
                    continue;
                }
                let key: Vec<(String, SqlValue)> = keys
                    .iter()
                    .cloned()
                    .zip(row[..keys.len()].iter().cloned().map(SqlValue::from))
                    .collect();
                let key_after = keys.iter().any(|k| k == PROVIDER_COL).then(|| {
                    key.iter()
                        .map(|(k, v)| {
                            if k == PROVIDER_COL {
                                (k.clone(), SqlValue::Text(to.clone()))
                            } else {
                                (k.clone(), v.clone())
                            }
                        })
                        .collect()
                });
                changes.push(CellChange {
                    table: t.name.clone(),
                    key,
                    key_after,
                    column: PROVIDER_COL.to_owned(),
                    thread_id: thread_id.clone(),
                    before: SqlValue::from(row[keys.len()].clone()),
                    after: SqlValue::Text(to.clone()),
                });
            }
        }
    }
    for t in &info.offset_tables {
        let keys = t.key.columns();
        let mut select = keys.iter().map(|k| ident(k)).collect::<Vec<_>>();
        select.extend(t.offset_cols.iter().map(|c| ident(c)));
        let sql = format!(
            "SELECT {} FROM {} WHERE \"thread_id\" = ?",
            select.join(", "),
            ident(&t.name)
        );
        for (thread_id, edits) in work.offsets {
            if edits.iter().all(|e| e.old.len() == e.new.len()) {
                continue;
            }
            for row in select_rows(&tx, &path, &sql, &[thread_id], select.len())? {
                let key: Vec<(String, SqlValue)> = keys
                    .iter()
                    .cloned()
                    .zip(row[..keys.len()].iter().cloned().map(SqlValue::from))
                    .collect();
                for (i, col) in t.offset_cols.iter().enumerate() {
                    let Value::Integer(before) = row[keys.len() + i] else {
                        continue;
                    };
                    let after = shift_offset(edits, before).ok_or_else(|| {
                        Error::invalid(format!(
                            "{}: {}.{col} of thread {thread_id} holds an impossible offset {before}",
                            path.display(),
                            t.name
                        ))
                    })?;
                    if after != before {
                        let key_after = keys.iter().any(|k| k == col).then(|| {
                            key.iter()
                                .map(|(k, v)| {
                                    if k == col {
                                        (k.clone(), SqlValue::Integer(after))
                                    } else {
                                        (k.clone(), v.clone())
                                    }
                                })
                                .collect()
                        });
                        changes.push(CellChange {
                            table: t.name.clone(),
                            key: key.clone(),
                            key_after,
                            column: col.clone(),
                            thread_id: thread_id.clone(),
                            before: SqlValue::Integer(before),
                            after: SqlValue::Integer(after),
                        });
                    }
                }
            }
        }
    }
    for c in &changes {
        let n = update_cell(&tx, &path, c, &c.key, &c.before, &c.after)?;
        if n != 1 {
            return Err(Error::Conflict {
                message: format!(
                    "{}: expected to update one row of {}, updated {n}",
                    path.display(),
                    c.table
                ),
            });
        }
    }
    record(&changes)?;
    tx.commit().map_err(|e| db_err(&path, e))?;
    Ok(changes)
}

/// `UPDATE table SET column = to WHERE key AND column IS from`.
fn update_cell(
    tx: &Connection,
    path: &Path,
    c: &CellChange,
    key: &[(String, SqlValue)],
    from: &SqlValue,
    to: &SqlValue,
) -> Result<usize> {
    let sql = format!(
        "UPDATE {} SET {} = ? WHERE {} AND {} IS ?",
        ident(&c.table),
        ident(&c.column),
        key_where(key),
        ident(&c.column),
    );
    let mut params = vec![to.to_value()?];
    params.extend(key_params(key)?);
    params.push(from.to_value()?);
    tx.execute(&sql, params_from_iter(params))
        .map_err(|e| db_err(path, e))
}

fn cell_value(
    conn: &Connection,
    path: &Path,
    c: &CellChange,
    key: &[(String, SqlValue)],
) -> Result<Option<SqlValue>> {
    let sql = format!(
        "SELECT {} FROM {} WHERE {}",
        ident(&c.column),
        ident(&c.table),
        key_where(key)
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| db_err(path, e))?;
    let mut rows = stmt
        .query(params_from_iter(key_params(key)?))
        .map_err(|e| db_err(path, e))?;
    match rows.next().map_err(|e| db_err(path, e))? {
        Some(row) => Ok(Some(SqlValue::from(
            row.get::<_, Value>(0).map_err(|e| db_err(path, e))?,
        ))),
        None => Ok(None),
    }
}

/// Where a recorded cell stands now.
pub(crate) fn state(conn: &Connection, path: &Path, c: &CellChange) -> Result<CellState> {
    let after_key = c.key_after.as_deref().unwrap_or(&c.key);
    if cell_value(conn, path, c, after_key)?.as_ref() == Some(&c.after) {
        return Ok(CellState::After);
    }
    if cell_value(conn, path, c, &c.key)?.as_ref() == Some(&c.before) {
        return Ok(CellState::Before);
    }
    Ok(CellState::Changed)
}

/// Restore `changes` (cells still equal to `after`) in one immediate transaction.
/// Returns how many cells were restored.
pub(crate) fn restore(
    conn: &mut Connection,
    path: &Path,
    changes: &[&CellChange],
) -> Result<usize> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| db_err(path, e))?;
    let mut restored = 0;
    for c in changes {
        let key = c.key_after.as_deref().unwrap_or(&c.key);
        restored += update_cell(&tx, path, c, key, &c.after, &c.before)?;
    }
    tx.commit().map_err(|e| db_err(path, e))?;
    Ok(restored)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggers_on_written_columns_are_found() {
        let triggers = vec![
            (
                "a".to_owned(),
                "CREATE TRIGGER a AFTER UPDATE OF updated_at ON threads BEGIN SELECT 1; END"
                    .to_owned(),
            ),
            (
                "b".to_owned(),
                "CREATE TRIGGER b AFTER UPDATE OF \"model_provider\", x ON threads BEGIN SELECT 1; END"
                    .to_owned(),
            ),
            (
                "c".to_owned(),
                "create trigger c before\n update on threads begin select 1; end".to_owned(),
            ),
            (
                "d".to_owned(),
                "CREATE TRIGGER d AFTER DELETE ON threads BEGIN SELECT 1; END".to_owned(),
            ),
            (
                "e".to_owned(),
                "CREATE TRIGGER e AFTER INSERT ON threads BEGIN UPDATE threads SET x = 1; END"
                    .to_owned(),
            ),
            (
                "update_x".to_owned(),
                "CREATE TRIGGER IF NOT EXISTS update_x AFTER INSERT ON threads BEGIN SELECT 1; END"
                    .to_owned(),
            ),
            (
                "f".to_owned(),
                "CREATE TEMP TRIGGER f UPDATE OF model_provider ON threads BEGIN SELECT 1; END"
                    .to_owned(),
            ),
        ];
        assert_eq!(
            update_triggers(&triggers, &[PROVIDER_COL.to_owned()]),
            vec!["b".to_owned(), "c".to_owned(), "f".to_owned()]
        );
    }
}
