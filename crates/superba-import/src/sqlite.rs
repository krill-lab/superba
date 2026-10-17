//! Read-only access to another program's SQLite database.
//!
//! The other tool may be running, and its database belongs to it: Superba never
//! writes to it, never creates `-shm` / `-wal` / `-journal` files next to it and
//! never replays its journal. Per journal mode:
//!
//! - **rollback journal** (cc-switch): opened in place with `mode=ro` and read in
//!   one read transaction (a shared lock for a few milliseconds, which the
//!   tool's writer simply waits out). A hot `-journal` (a crash in the middle of
//!   a write) cannot be rolled back read-only, so then the file and its journal
//!   are copied and the copy is read.
//! - **WAL** (AI Toolbox): without pending changes (no `-wal`, or an empty one)
//!   the file is complete and is opened `immutable=1` (no locks, no side files).
//!   Otherwise recent commits sit only in the `-wal` file, so the database and
//!   its `-wal` are copied into a private scratch directory and the copy is
//!   opened (SQLite applies the WAL to the copy). The WAL header is compared
//!   before and after copying: a checkpoint that restarted the WAL meanwhile
//!   makes the copy inconsistent, and the copy is retried.
//!
//! Copies live in a 0700 directory under Superba's scratch directory and are
//! removed when the [`Snapshot`] is dropped (they contain keys).

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

/// Why a database could not be opened.
#[derive(Debug)]
pub enum OpenError {
    /// The file is not an SQLite database (wrong header).
    NotSqlite,
    /// Reading or copying failed.
    Io(std::io::Error),
    /// SQLite refused it.
    Sqlite(rusqlite::Error),
    /// The tool kept changing the database while it was copied.
    Busy,
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::NotSqlite => f.write_str("not an SQLite database"),
            OpenError::Io(e) => write!(f, "{e}"),
            OpenError::Sqlite(e) => write!(f, "{e}"),
            OpenError::Busy => f.write_str("the database kept changing while it was read"),
        }
    }
}

impl From<std::io::Error> for OpenError {
    fn from(e: std::io::Error) -> Self {
        OpenError::Io(e)
    }
}

impl From<rusqlite::Error> for OpenError {
    fn from(e: rusqlite::Error) -> Self {
        OpenError::Sqlite(e)
    }
}

/// How a snapshot was opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenMode {
    /// In place, read-only, inside one read transaction.
    ReadOnly,
    /// In place, `immutable=1`.
    Immutable,
    /// A private copy (pending changes in a `-wal` / `-journal` file).
    Copy,
}

/// An open, read-only view of a database. Dropping it closes the connection and
/// removes the private copy, if one was made.
#[derive(Debug)]
pub struct Snapshot {
    conn: Option<Connection>,
    copy_dir: Option<PathBuf>,
    mode: OpenMode,
}

impl Snapshot {
    /// The connection (`PRAGMA query_only` is on).
    pub fn conn(&self) -> &Connection {
        self.conn.as_ref().expect("the connection lives until drop")
    }

    /// How it was opened.
    pub fn mode(&self) -> OpenMode {
        self.mode
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            if self.mode == OpenMode::ReadOnly {
                let _ = conn.execute_batch("COMMIT");
            }
            drop(conn);
        }
        if let Some(dir) = self.copy_dir.take() {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

const HEADER: &[u8; 16] = b"SQLite format 3\0";

/// The first `n` bytes of `path` (fewer when the file is shorter).
fn head(path: &Path, n: usize) -> std::io::Result<Vec<u8>> {
    let mut f = fs::File::open(path)?;
    let mut buf = vec![0u8; n];
    let mut read = 0;
    while read < n {
        match f.read(&mut buf[read..])? {
            0 => break,
            k => read += k,
        }
    }
    buf.truncate(read);
    Ok(buf)
}

/// True when `path` starts with the SQLite header.
pub fn is_sqlite(path: &Path) -> std::io::Result<bool> {
    Ok(head(path, 16)?.as_slice() == HEADER)
}

/// True when the database header says WAL mode (file format versions 2).
fn wal_mode(path: &Path) -> std::io::Result<bool> {
    let h = head(path, 20)?;
    Ok(h.len() == 20 && h[18] == 2 && h[19] == 2)
}

fn side_file(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn has_data(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

/// Open `path` read-only (see the module docs). `scratch` is a directory of
/// Superba's own for private copies (created when needed).
pub fn open(path: &Path, scratch: &Path) -> Result<Snapshot, OpenError> {
    if !is_sqlite(path)? {
        return Err(OpenError::NotSqlite);
    }
    if wal_mode(path)? {
        let wal = side_file(path, "-wal");
        if !has_data(&wal)
            && let Some(uri) = file_uri(path, "immutable=1")
        {
            let conn = Connection::open_with_flags(
                uri,
                OpenFlags::SQLITE_OPEN_READ_ONLY
                    | OpenFlags::SQLITE_OPEN_URI
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            conn.pragma_update(None, "query_only", true)?;
            return Ok(Snapshot {
                conn: Some(conn),
                copy_dir: None,
                mode: OpenMode::Immutable,
            });
        }
        return copy_and_open(path, scratch, "-wal");
    }
    let journal = side_file(path, "-journal");
    if !has_data(&journal)
        && let Some(uri) = file_uri(path, "mode=ro")
    {
        match open_read_only(&uri) {
            Ok(conn) => {
                return Ok(Snapshot {
                    conn: Some(conn),
                    copy_dir: None,
                    mode: OpenMode::ReadOnly,
                });
            }
            Err(e) => tracing::debug!("read-only open failed ({e}); reading a copy"),
        }
    }
    copy_and_open(path, scratch, "-journal")
}

/// Open in place read-only and start the read transaction (its shared lock is
/// taken by the first read and held until the snapshot is dropped).
fn open_read_only(uri: &str) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_secs(3))?;
    conn.pragma_update(None, "query_only", true)?;
    conn.execute_batch("BEGIN DEFERRED")?;
    // Touch the schema now: a hot journal fails here, not halfway through.
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    })?;
    Ok(conn)
}

/// The first 32 bytes of a WAL file (magic, version, page size, checkpoint
/// sequence, salts, checksum): they change when a checkpoint restarts the WAL.
fn wal_header(path: &Path) -> Option<Vec<u8>> {
    head(path, 32).ok().filter(|h| h.len() == 32)
}

fn copy_and_open(path: &Path, scratch: &Path, suffix: &str) -> Result<Snapshot, OpenError> {
    let side = side_file(path, suffix);
    let mut last: Option<OpenError> = None;
    for _ in 0..3 {
        let dir = private_dir(scratch)?;
        let attempt = (|| {
            let copy = dir.join("store.db");
            let before = wal_header(&side);
            fs::copy(path, &copy)?;
            // The side file may vanish meanwhile (the tool checkpointed): the
            // main file alone is then complete.
            match fs::copy(&side, side_file(&copy, suffix)) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(OpenError::Io(e)),
            }
            if suffix == "-wal" && wal_header(&side) != before {
                return Err(OpenError::Busy);
            }
            let conn = Connection::open_with_flags(
                &copy,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            conn.pragma_update(None, "query_only", true)?;
            let ok: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
            if ok != "ok" {
                return Err(OpenError::Busy);
            }
            Ok(conn)
        })();
        match attempt {
            Ok(conn) => {
                return Ok(Snapshot {
                    conn: Some(conn),
                    copy_dir: Some(dir),
                    mode: OpenMode::Copy,
                });
            }
            Err(e) => {
                let _ = fs::remove_dir_all(&dir);
                let retry = matches!(e, OpenError::Busy | OpenError::Sqlite(_));
                last = Some(e);
                if !retry {
                    break;
                }
                std::thread::sleep(Duration::from_millis(150));
            }
        }
    }
    Err(last.unwrap_or(OpenError::Busy))
}

/// A new private directory under `scratch` (0700 on Unix: the copy holds keys).
fn private_dir(scratch: &Path) -> std::io::Result<PathBuf> {
    fs::create_dir_all(scratch)?;
    let dir = scratch.join(format!("import-{}", uuid::Uuid::new_v4().simple()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(&dir)?;
    }
    #[cfg(not(unix))]
    fs::create_dir(&dir)?;
    Ok(dir)
}

/// `file:` URI of `path` with `query`; `None` when the path is not UTF-8 (the
/// caller copies the file instead).
pub fn file_uri(path: &Path, query: &str) -> Option<String> {
    let text = path.to_str()?;
    let mut uri = String::from("file:");
    let normalized = text.replace('\\', "/");
    // Windows drive paths: file:///C:/... ; UNC paths keep their two slashes.
    if normalized.len() >= 2 && normalized.as_bytes()[1] == b':' {
        uri.push_str("///");
    } else if normalized.starts_with("//") {
        uri.push_str("//");
    }
    for c in normalized.chars() {
        match c {
            '%' => uri.push_str("%25"),
            '?' => uri.push_str("%3f"),
            '#' => uri.push_str("%23"),
            ' ' => uri.push_str("%20"),
            c => uri.push(c),
        }
    }
    uri.push('?');
    uri.push_str(query);
    Some(uri)
}

/// True when `table` exists.
pub fn has_table(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
}

/// Column names of `table` (empty when it does not exist).
pub fn columns(conn: &Connection, table: &str) -> rusqlite::Result<Vec<String>> {
    // Table names come from this crate's constants, never from the database.
    let mut stmt = conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    rows.collect()
}

/// `PRAGMA user_version`.
pub fn user_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
}
