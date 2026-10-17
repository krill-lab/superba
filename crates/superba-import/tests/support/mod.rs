//! Fixture homes for the import tests: each test gets its own temporary home with
//! the other tools' files placed where they keep them, and a scratch directory
//! standing in for Superba's cache.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use superba_core::{EnvSource, MapEnv};
use superba_import::{Os, ScanEnv, SourceRead};

/// Directory of the fixture files.
pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// A fixture file's text.
pub fn fixture(rel: &str) -> String {
    std::fs::read_to_string(fixtures().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// `path` (written with `/`) as [`superba_import::ScanEnv::display`] shows it on
/// this system: `\`-separated on Windows.
pub fn native(path: &str) -> String {
    path.replace('/', std::path::MAIN_SEPARATOR_STR)
}

/// A temporary home.
pub struct Home {
    pub tmp: tempfile::TempDir,
    pub env: Arc<MapEnv>,
}

impl Home {
    pub fn new() -> Self {
        Self {
            tmp: tempfile::tempdir().unwrap(),
            env: Arc::new(MapEnv::new()),
        }
    }

    pub fn with_env(mut self, key: &str, value: impl Into<std::ffi::OsString>) -> Self {
        let env = (*self.env).clone().with(key, value);
        self.env = Arc::new(env);
        self
    }

    pub fn home(&self) -> PathBuf {
        self.tmp.path().join("home")
    }

    pub fn scratch(&self) -> PathBuf {
        self.tmp.path().join("superba-cache")
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.home().join(rel)
    }

    pub fn write(&self, rel: &str, text: &str) -> PathBuf {
        let p = self.path(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
        p
    }

    /// Create an SQLite database at `rel` from `sql`; `wal` switches it to WAL.
    pub fn db(&self, rel: &str, sql: &str, wal: bool) -> PathBuf {
        let p = self.path(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let conn = rusqlite::Connection::open(&p).unwrap();
        conn.execute_batch(sql).unwrap();
        if wal {
            let mode: String = conn
                .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
                .unwrap();
            assert_eq!(mode, "wal");
        }
        drop(conn);
        p
    }

    pub fn scan_env(&self, os: Os) -> (PathBuf, PathBuf, Arc<MapEnv>, Os) {
        (self.home(), self.scratch(), self.env.clone(), os)
    }

    /// Read `source` as Superba would on `os`.
    pub fn read(&self, source: superba_core::ImportSource, os: Os) -> SourceRead {
        let home = self.home();
        let scratch = self.scratch();
        let env: &dyn EnvSource = self.env.as_ref();
        let scan = ScanEnv {
            home: &home,
            env,
            os,
            scratch: &scratch,
        };
        superba_import::read_source(source, &scan)
    }
}

/// Every file under `dir` with its size and modification time (read-only
/// checks).
pub fn listing(dir: &Path) -> BTreeMap<PathBuf, (u64, std::time::SystemTime, Vec<u8>)> {
    let mut out = BTreeMap::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(listing(&p));
            } else if let Ok(m) = e.metadata() {
                out.insert(
                    p.clone(),
                    (m.len(), m.modified().unwrap(), std::fs::read(&p).unwrap()),
                );
            }
        }
    }
    out
}
