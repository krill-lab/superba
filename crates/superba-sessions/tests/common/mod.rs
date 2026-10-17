//! Synthetic Codex homes for the migration tests.
//!
//! The SQLite schemas are copied from a real Codex 0.162 home (`state_5.sqlite`
//! `threads` with its indexes and triggers, `thread_history_1.sqlite` with its
//! byte-offset projection). Rollouts look like real ones: a large
//! `session_meta` first line, then records carrying an `ordinal`.

#![allow(dead_code, missing_docs)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rusqlite::Connection;
use superba_core::{CodexHome, DataDir};
use superba_sessions::CodexSessionMigrator;

pub const THREADS_DDL: &str = r#"
CREATE TABLE threads (
    id TEXT PRIMARY KEY,
    rollout_path TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    source TEXT NOT NULL,
    model_provider TEXT NOT NULL,
    cwd TEXT NOT NULL,
    title TEXT NOT NULL,
    sandbox_policy TEXT NOT NULL,
    approval_mode TEXT NOT NULL,
    tokens_used INTEGER NOT NULL DEFAULT 0,
    has_user_event INTEGER NOT NULL DEFAULT 0,
    archived INTEGER NOT NULL DEFAULT 0,
    archived_at INTEGER,
    git_sha TEXT,
    git_branch TEXT,
    git_origin_url TEXT
, cli_version TEXT NOT NULL DEFAULT '', first_user_message TEXT NOT NULL DEFAULT '', agent_nickname TEXT, agent_role TEXT, memory_mode TEXT NOT NULL DEFAULT 'enabled', model TEXT, reasoning_effort TEXT, agent_path TEXT, created_at_ms INTEGER, updated_at_ms INTEGER, thread_source TEXT, preview TEXT NOT NULL DEFAULT '', recency_at INTEGER NOT NULL DEFAULT 0, recency_at_ms INTEGER NOT NULL DEFAULT 0, history_mode TEXT NOT NULL DEFAULT 'legacy', name TEXT, is_pinned INTEGER NOT NULL DEFAULT 0, thread_section_id TEXT, section_position INTEGER, section_entered_at_ms INTEGER, project_id TEXT, originator TEXT, daybreak_enabled BOOLEAN, creator_user_id TEXT, creator_account_id TEXT);
CREATE INDEX idx_threads_created_at ON threads(created_at DESC, id DESC);
CREATE INDEX idx_threads_provider ON threads(model_provider);
CREATE INDEX idx_threads_archived ON threads(archived);
CREATE TRIGGER threads_created_at_ms_after_insert
AFTER INSERT ON threads
WHEN NEW.created_at_ms IS NULL
BEGIN
    UPDATE threads SET created_at_ms = NEW.created_at * 1000 WHERE id = NEW.id;
END;
CREATE TRIGGER threads_updated_at_ms_after_update
AFTER UPDATE OF updated_at ON threads
WHEN NEW.updated_at != OLD.updated_at AND NEW.updated_at_ms IS OLD.updated_at_ms
BEGIN
    UPDATE threads SET updated_at_ms = NEW.updated_at * 1000 WHERE id = NEW.id;
END;
CREATE TABLE external_agent_config_imports (import_id TEXT PRIMARY KEY, completed_at_ms INTEGER, successes INTEGER, failures INTEGER, provider_id TEXT);
INSERT INTO external_agent_config_imports VALUES ('imp-1', 1, 0, 0, 'krill');
"#;

pub const HISTORY_DDL: &str = r#"
CREATE TABLE thread_turns (
    thread_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    rollout_ordinal INTEGER NOT NULL,
    status TEXT NOT NULL,
    error_json TEXT,
    started_at INTEGER,
    completed_at INTEGER,
    duration_ms INTEGER,
    first_user_item_id TEXT,
    final_agent_item_id TEXT, rollout_byte_offset INTEGER, rollout_end_ordinal INTEGER, rollout_end_byte_offset INTEGER, root_turn_id TEXT,
    PRIMARY KEY (thread_id, turn_id)
);
CREATE UNIQUE INDEX idx_thread_turns_page ON thread_turns(thread_id, rollout_ordinal);
CREATE TABLE thread_items (
    thread_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    rollout_ordinal INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    item_json TEXT NOT NULL, item_type TEXT NOT NULL DEFAULT '', updated_at_ordinal INTEGER NOT NULL DEFAULT 0, started_at_ms INTEGER, completed_at_ms INTEGER,
    PRIMARY KEY (thread_id, turn_id, item_id)
);
CREATE TABLE thread_history_projection_state (
    thread_id TEXT PRIMARY KEY,
    next_rollout_byte_offset INTEGER NOT NULL,
    next_rollout_ordinal INTEGER NOT NULL
);
CREATE TABLE thread_realtime_items (
    thread_id TEXT NOT NULL,
    item_id TEXT NOT NULL,
    rollout_ordinal INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    item_type TEXT NOT NULL,
    item_json TEXT NOT NULL,
    PRIMARY KEY (thread_id, item_id)
);
CREATE TRIGGER thread_realtime_items_projection_cleanup
    AFTER DELETE ON thread_history_projection_state
BEGIN
    DELETE FROM thread_realtime_items WHERE thread_id = OLD.thread_id;
END;
"#;

/// The app's catalog table (Codex++ observed it in `sqlite/codex-dev.db`).
pub const CATALOG_DDL: &str = r#"
CREATE TABLE local_thread_catalog (
    host_id TEXT NOT NULL, thread_id TEXT NOT NULL, display_title TEXT NOT NULL,
    source_created_at REAL NOT NULL, source_updated_at REAL NOT NULL,
    cwd TEXT NOT NULL, source_kind TEXT NOT NULL, source_detail TEXT,
    model_provider TEXT NOT NULL, git_branch TEXT,
    observation_sequence INTEGER NOT NULL,
    missing_candidate INTEGER NOT NULL DEFAULT 0, thread_source TEXT,
    PRIMARY KEY (host_id, thread_id)) WITHOUT ROWID;
"#;

/// Where a session's rollout lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Store {
    Live,
    Archived,
    /// No rollout file (sqlite row only).
    None,
}

/// One synthetic session.
#[derive(Clone, Debug)]
pub struct Thread {
    pub id: String,
    pub provider: String,
    /// Provider of the rollout's session_meta when it differs from the row
    /// (`Some(None)` = no `model_provider` key at all).
    pub rollout_provider: Option<Option<String>>,
    pub store: Store,
    pub compressed: bool,
    pub sqlite: bool,
    pub catalog: bool,
    pub created_ms: i64,
    pub title: String,
    pub turns: usize,
    pub crlf: bool,
}

impl Thread {
    pub fn new(n: u32, provider: &str) -> Self {
        // UUIDv7-shaped ids whose timestamp is 2026-10-01 + n minutes.
        let ms = 1_790_812_800_000i64 + i64::from(n) * 60_000;
        let id = format!(
            "{:08x}-{:04x}-7{:03x}-8{:03x}-{:012x}",
            (ms >> 16) as u32,
            (ms & 0xffff) as u16,
            n % 0xfff,
            n % 0xfff,
            u64::from(n) * 7919
        );
        Thread {
            id,
            provider: provider.to_owned(),
            rollout_provider: None,
            store: Store::Live,
            compressed: false,
            sqlite: true,
            catalog: false,
            created_ms: ms,
            title: format!("会话 {n} — fix the build"),
            turns: 2,
            crlf: false,
        }
    }
    pub fn archived(mut self) -> Self {
        self.store = Store::Archived;
        self
    }
    pub fn zst(mut self) -> Self {
        self.compressed = true;
        self
    }
    pub fn no_sqlite(mut self) -> Self {
        self.sqlite = false;
        self
    }
    pub fn no_rollout(mut self) -> Self {
        self.store = Store::None;
        self
    }
    pub fn rollout_provider(mut self, p: Option<&str>) -> Self {
        self.rollout_provider = Some(p.map(str::to_owned));
        self
    }
    pub fn catalog(mut self) -> Self {
        self.catalog = true;
        self
    }
    pub fn crlf(mut self) -> Self {
        self.crlf = true;
        self
    }
    pub fn turns(mut self, n: usize) -> Self {
        self.turns = n;
        self
    }
}

pub struct Fixture {
    pub tmp: tempfile::TempDir,
    pub home: CodexHome,
    pub home_dir: PathBuf,
    pub data: DataDir,
    pub threads: Vec<Thread>,
}

fn mtime_for(n: usize) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + n as u64 * 3600)
}

impl Fixture {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home_dir = tmp.path().join("home");
        let root = home_dir.join(".codex");
        fs::create_dir_all(&root).unwrap();
        Self {
            home: CodexHome::new(&root),
            home_dir,
            data: DataDir::new(tmp.path().join("data")),
            tmp,
            threads: Vec::new(),
        }
    }

    /// Use a separate sqlite directory (`CODEX_SQLITE_HOME`).
    pub fn with_sqlite_home(mut self) -> Self {
        let dir = self.tmp.path().join("sqlite-home");
        fs::create_dir_all(&dir).unwrap();
        self.home = self.home.with_sqlite_home(dir);
        self
    }

    pub fn migrator(&self) -> CodexSessionMigrator {
        CodexSessionMigrator::new(self.data.clone()).with_home_dir(&self.home_dir)
    }

    pub fn state_db(&self) -> PathBuf {
        self.home.state_db()
    }

    pub fn history_db(&self) -> PathBuf {
        self.home.sqlite_home().join("thread_history_1.sqlite")
    }

    pub fn catalog_db(&self) -> PathBuf {
        self.home.sqlite_home().join("sqlite").join("codex-dev.db")
    }

    fn ensure_dbs(&self) {
        if !self.state_db().exists() {
            let c = Connection::open(self.state_db()).unwrap();
            c.pragma_update(None, "journal_mode", "WAL").unwrap();
            c.execute_batch(THREADS_DDL).unwrap();
        }
        if !self.history_db().exists() {
            let c = Connection::open(self.history_db()).unwrap();
            c.pragma_update(None, "journal_mode", "WAL").unwrap();
            c.execute_batch(HISTORY_DDL).unwrap();
        }
    }

    fn ensure_catalog(&self) {
        if !self.catalog_db().exists() {
            fs::create_dir_all(self.catalog_db().parent().unwrap()).unwrap();
            let c = Connection::open(self.catalog_db()).unwrap();
            c.execute_batch(CATALOG_DDL).unwrap();
        }
    }

    pub fn rollout_path(&self, t: &Thread) -> PathBuf {
        let name = format!("rollout-2026-10-01T08-00-00-{}.jsonl", t.id);
        let base = match t.store {
            Store::Archived => self.home.archived_sessions_dir(),
            _ => self.home.sessions_dir().join("2026").join("10").join("01"),
        };
        let path = base.join(name);
        if t.compressed {
            let mut s = path.into_os_string();
            s.push(".zst");
            PathBuf::from(s)
        } else {
            path
        }
    }

    /// Plain rollout content and the byte offset of every line.
    pub fn rollout_content(t: &Thread) -> (String, Vec<usize>) {
        let nl = if t.crlf { "\r\n" } else { "\n" };
        let provider = match &t.rollout_provider {
            None => format!(",\"model_provider\":\"{}\"", t.provider),
            Some(Some(p)) => format!(",\"model_provider\":\"{p}\""),
            Some(None) => String::new(),
        };
        let mut lines = vec![format!(
            r#"{{"timestamp":"2026-10-01T08:00:00.000Z","ordinal":0,"type":"session_meta","payload":{{"session_id":"{id}","id":"{id}","timestamp":"2026-10-01T08:00:00.000Z","cwd":"/work/项目","originator":"codex_exec","cli_version":"0.162.0","source":"exec","thread_source":"user"{provider},"base_instructions":{{"text":"You are Codex. Never write \"model_provider\":\"krill\" or {{\"type\":\"session_meta\"}} — 中文说明 ✓"}},"history_mode":"paginated","context_window":null}}}}"#,
            id = t.id
        )];
        let mut ordinal = 1;
        for turn in 0..t.turns {
            lines.push(format!(
                r#"{{"timestamp":"2026-10-01T08:0{turn}:01.000Z","ordinal":{ordinal},"type":"event_msg","payload":{{"type":"user_message","message":"第 {turn} 轮: please fix \"it\""}}}}"#
            ));
            ordinal += 1;
            lines.push(format!(
                r#"{{"timestamp":"2026-10-01T08:0{turn}:02.000Z","ordinal":{ordinal},"type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"done {turn} {}"}}]}}}}"#,
                "x".repeat(40 * (turn + 1))
            ));
            ordinal += 1;
        }
        let mut content = String::new();
        let mut offsets = Vec::new();
        for line in &lines {
            offsets.push(content.len());
            content.push_str(line);
            content.push_str(nl);
        }
        (content, offsets)
    }

    pub fn add(&mut self, t: Thread) {
        self.ensure_dbs();
        let n = self.threads.len();
        let (content, offsets) = Self::rollout_content(&t);
        let path = self.rollout_path(&t);
        if t.store != Store::None {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let bytes = if t.compressed {
                zstd::stream::encode_all(content.as_bytes(), 19).unwrap()
            } else {
                content.clone().into_bytes()
            };
            fs::write(&path, bytes).unwrap();
            let f = fs::File::options().write(true).open(&path).unwrap();
            f.set_times(fs::FileTimes::new().set_modified(mtime_for(n)))
                .unwrap();
        }
        if t.sqlite {
            let c = Connection::open(self.state_db()).unwrap();
            c.execute(
                "INSERT INTO threads (id, rollout_path, created_at, updated_at, source, model_provider, cwd, title, sandbox_policy, approval_mode, archived, created_at_ms, history_mode, preview)
                 VALUES (?1, ?2, ?3, ?3, 'exec', ?4, '/work/项目', ?5, '{}', 'never', ?6, ?7, 'paginated', ?5)",
                rusqlite::params![
                    t.id,
                    path.to_string_lossy(),
                    t.created_ms / 1000,
                    t.provider,
                    t.title,
                    i64::from(t.store == Store::Archived),
                    t.created_ms,
                ],
            )
            .unwrap();
        }
        if t.store != Store::None {
            // Projection: one turn row per user turn (offset of its first line and
            // of its last line), plus the projection state (end of file).
            let c = Connection::open(self.history_db()).unwrap();
            for turn in 0..t.turns {
                let first = 1 + 2 * turn;
                let last = first + 1;
                c.execute(
                    "INSERT INTO thread_turns (thread_id, turn_id, rollout_ordinal, status, rollout_byte_offset, rollout_end_ordinal, rollout_end_byte_offset)
                     VALUES (?1, ?2, ?3, 'completed', ?4, ?5, ?6)",
                    rusqlite::params![
                        t.id,
                        format!("turn-{turn}"),
                        first as i64,
                        offsets[first] as i64,
                        last as i64,
                        offsets[last] as i64,
                    ],
                )
                .unwrap();
                c.execute(
                    "INSERT INTO thread_items (thread_id, turn_id, item_id, rollout_ordinal, created_at_ms, item_json, item_type)
                     VALUES (?1, ?2, ?3, ?4, 0, '{}', 'userMessage')",
                    rusqlite::params![t.id, format!("turn-{turn}"), format!("item-{turn}"), first as i64],
                )
                .unwrap();
            }
            c.execute(
                "INSERT INTO thread_history_projection_state VALUES (?1, ?2, ?3)",
                rusqlite::params![t.id, content.len() as i64, (1 + 2 * t.turns) as i64],
            )
            .unwrap();
        }
        if t.catalog {
            self.ensure_catalog();
            let c = Connection::open(self.catalog_db()).unwrap();
            c.execute(
                "INSERT INTO local_thread_catalog VALUES ('local', ?1, ?2, 1.0, 2.0, '/w', 'vscode', NULL, ?3, 'main', ?4, 0, 'user')",
                rusqlite::params![t.id, t.title, t.provider, n as i64],
            )
            .unwrap();
        }
        self.threads.push(t);
    }

    /// Decompress `raw` when `compressed`.
    pub fn plain_bytes(raw: &[u8], compressed: bool) -> Vec<u8> {
        if compressed {
            zstd::stream::decode_all(raw).unwrap()
        } else {
            raw.to_vec()
        }
    }

    /// Plain (decompressed) content of a rollout.
    pub fn plain(path: &Path) -> Vec<u8> {
        let raw = fs::read(path).unwrap();
        if path.to_string_lossy().ends_with(".zst") {
            zstd::stream::decode_all(raw.as_slice()).unwrap()
        } else {
            raw
        }
    }

    /// Every projection offset of every thread points at the start of the line
    /// carrying the expected ordinal, and the projection end equals the file size.
    pub fn assert_projection_consistent(&self) {
        let c = Connection::open(self.history_db()).unwrap();
        for t in &self.threads {
            if t.store == Store::None {
                continue;
            }
            let plain = Self::plain(&self.rollout_path(t));
            let mut stmt = c
                .prepare("SELECT rollout_ordinal, rollout_byte_offset, rollout_end_ordinal, rollout_end_byte_offset FROM thread_turns WHERE thread_id = ?1")
                .unwrap();
            let rows: Vec<(i64, i64, i64, i64)> = stmt
                .query_map([&t.id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            assert_eq!(rows.len(), t.turns);
            for (ord, off, end_ord, end_off) in rows {
                assert_line_at(&plain, off as usize, ord, &t.id);
                assert_line_at(&plain, end_off as usize, end_ord, &t.id);
            }
            let next: i64 = c
                .query_row(
                    "SELECT next_rollout_byte_offset FROM thread_history_projection_state WHERE thread_id = ?1",
                    [&t.id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(next as usize, plain.len(), "projection end of {}", t.id);
        }
    }

    /// Byte-exact snapshot of the home: every regular file except SQLite files and
    /// lock/temp files (bytes + mtime), plus a logical dump of every database.
    pub fn snapshot(&self) -> Snapshot {
        let mut files = BTreeMap::new();
        let mut dbs = BTreeMap::new();
        let mut roots = vec![self.home.root().to_path_buf()];
        if self.home.sqlite_home() != self.home.root() {
            roots.push(self.home.sqlite_home().to_path_buf());
        }
        for root in roots {
            for entry in walkdir::WalkDir::new(&root).sort_by_file_name() {
                let entry = entry.unwrap();
                if !entry.file_type().is_file() {
                    continue;
                }
                let path = entry.path().to_path_buf();
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                if name.ends_with("-wal") || name.ends_with("-shm") || name.ends_with(".lock") {
                    continue;
                }
                if name.ends_with(".sqlite") || name.ends_with(".db") {
                    dbs.insert(path.clone(), dump_db(&path));
                    continue;
                }
                let meta = fs::metadata(&path).unwrap();
                files.insert(
                    path.clone(),
                    (fs::read(&path).unwrap(), meta.modified().unwrap()),
                );
            }
        }
        Snapshot { files, dbs }
    }
}

fn assert_line_at(plain: &[u8], off: usize, ordinal: i64, id: &str) {
    assert!(
        off <= plain.len(),
        "offset {off} beyond {} ({id})",
        plain.len()
    );
    assert!(
        off == 0 || plain[off - 1] == b'\n',
        "offset {off} is not a line start ({id})"
    );
    let end = plain[off..]
        .iter()
        .position(|b| *b == b'\n')
        .map_or(plain.len(), |p| off + p);
    let line = std::str::from_utf8(&plain[off..end])
        .unwrap()
        .trim_end_matches('\r');
    let v: serde_json::Value = serde_json::from_str(line)
        .unwrap_or_else(|e| panic!("line at {off} of {id} is not JSON ({e}): {line:.80}"));
    assert_eq!(
        v["ordinal"].as_i64(),
        Some(ordinal),
        "ordinal at {off} of {id}"
    );
}

/// Ordered dump of every table.
pub fn dump_db(path: &Path) -> BTreeMap<String, Vec<String>> {
    let c = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let tables: Vec<String> = c
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut out = BTreeMap::new();
    for t in tables {
        let mut stmt = c.prepare(&format!("SELECT * FROM \"{t}\"")).unwrap();
        let n = stmt.column_count();
        let mut rows: Vec<String> = stmt
            .query_map([], |r| {
                let mut parts = Vec::new();
                for i in 0..n {
                    let v: rusqlite::types::Value = r.get(i)?;
                    parts.push(format!("{v:?}"));
                }
                Ok(parts.join("|"))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        rows.sort();
        out.insert(t, rows);
    }
    out
}

#[derive(Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub files: BTreeMap<PathBuf, (Vec<u8>, SystemTime)>,
    pub dbs: BTreeMap<PathBuf, BTreeMap<String, Vec<String>>>,
}

impl Snapshot {
    /// Assert equality with a readable message instead of a giant dump.
    pub fn assert_same(&self, other: &Snapshot) {
        assert_eq!(
            self.files.keys().collect::<Vec<_>>(),
            other.files.keys().collect::<Vec<_>>(),
            "file sets differ"
        );
        for (path, (bytes, mtime)) in &self.files {
            let (b2, m2) = &other.files[path];
            assert!(bytes == b2, "content of {} differs", path.display());
            assert_eq!(mtime, m2, "mtime of {} differs", path.display());
        }
        assert_eq!(
            self.dbs.keys().collect::<Vec<_>>(),
            other.dbs.keys().collect::<Vec<_>>()
        );
        for (path, tables) in &self.dbs {
            for (t, rows) in tables {
                assert_eq!(rows, &other.dbs[path][t], "table {t} of {}", path.display());
            }
        }
    }
}

/// Provider recorded in the `threads` row of `id`.
pub fn row_provider(db: &Path, id: &str) -> Option<String> {
    let c = Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    c.query_row(
        "SELECT model_provider FROM threads WHERE id = ?1",
        [id],
        |r| r.get(0),
    )
    .ok()
}

/// Provider of the first session_meta of a rollout (`None` if absent).
pub fn rollout_provider(path: &Path) -> Option<String> {
    let plain = Fixture::plain(path);
    let first = plain.split(|b| *b == b'\n').next().unwrap();
    let v: serde_json::Value = serde_json::from_slice(first).unwrap();
    v["payload"]["model_provider"].as_str().map(str::to_owned)
}
