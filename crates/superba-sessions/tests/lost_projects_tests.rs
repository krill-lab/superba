//! User bugs 135/136 follow-up: which folders the app dropped can be found
//! again. The schema follows `state_5.sqlite` of Codex 0.167 (app 26.1103):
//! `projects`, `project_roots`, `project_idempotency_keys`, `threads`.

#![allow(missing_docs)]

use std::fs;
use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use rusqlite::{Connection, params};
use superba_core::{CodexHome, LostProjectSource, Timestamp};
use superba_sessions::lost_projects::{self, Filter, MAX_CHAT_FOLDERS, ProjectRow, discover};

struct World {
    _tmp: tempfile::TempDir,
    home_dir: PathBuf,
    codex: CodexHome,
}

impl World {
    fn new() -> Self {
        // Not under the temporary directory: folders there are never offered.
        let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        let root = superba_core::paths::canonical(tmp.path()).unwrap();
        let home_dir = root.join("home");
        let codex = CodexHome::new(home_dir.join(".codex"));
        fs::create_dir_all(codex.root()).unwrap();
        Self {
            _tmp: tmp,
            home_dir,
            codex,
        }
    }

    fn dir(&self, rel: &str) -> PathBuf {
        let p = self.home_dir.join(rel);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn db(&self) -> Connection {
        let conn = Connection::open(self.codex.state_db()).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, name TEXT NOT NULL, \
               metadata TEXT NOT NULL DEFAULT '{}', position INTEGER NOT NULL, \
               created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS project_roots (project_id TEXT NOT NULL, \
               position INTEGER NOT NULL, path TEXT NOT NULL, PRIMARY KEY (project_id, position));
             CREATE TABLE IF NOT EXISTS project_idempotency_keys (key TEXT PRIMARY KEY, \
               project_id TEXT NOT NULL, created_at_ms INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS threads (id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL, \
               created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, cwd TEXT NOT NULL, \
               archived INTEGER NOT NULL DEFAULT 0, updated_at_ms INTEGER);",
        )
        .unwrap();
        conn
    }

    fn project(&self, id: &str, name: &str, roots: &[&Path], updated_ms: i64, key: Option<&str>) {
        let conn = self.db();
        conn.execute(
            "INSERT INTO projects (id, name, position, created_at_ms, updated_at_ms) VALUES (?1, ?2, 0, ?3, ?3)",
            params![id, name, updated_ms],
        )
        .unwrap();
        for (i, r) in roots.iter().enumerate() {
            conn.execute(
                "INSERT INTO project_roots VALUES (?1, ?2, ?3)",
                params![id, i as i64, r.to_str().unwrap()],
            )
            .unwrap();
        }
        if let Some(k) = key {
            conn.execute(
                "INSERT INTO project_idempotency_keys VALUES (?1, ?2, ?3)",
                params![k, id, updated_ms],
            )
            .unwrap();
        }
    }

    fn thread(&self, id: &str, cwd: &Path, updated_s: i64, archived: bool) {
        self.db()
            .execute(
                "INSERT INTO threads (id, rollout_path, created_at, updated_at, cwd, archived) \
                 VALUES (?1, 'r', ?2, ?2, ?3, ?4)",
                params![id, updated_s, cwd.to_str().unwrap(), archived as i64],
            )
            .unwrap();
    }

    fn discover(&self, known: &[PathBuf], answered: &[PathBuf]) -> Vec<superba_core::LostProject> {
        discover(&self.codex, &self.home_dir, known, answered)
    }
}

#[test]
fn the_app_server_project_list_brings_back_what_the_app_forgot() {
    let w = World::new();
    let a = w.dir("work/proj-a");
    let c = w.dir("work/proj-c");
    let d = w.dir("work/proj-d");
    w.project("s-a", "proj-a", &[&a], 1_000, Some("local-aaaa"));
    w.project("s-c", "proj-c", &[&c], 3_000, Some("877476ef-uuid"));
    let assets = w.dir("work/proj-d-assets");
    w.project("s-d", "", &[&d, &a, &assets], 2_000, None);
    w.thread("t1", &c, 5, false);
    w.thread("t2", &c, 4, false);
    w.thread("t3", &c, 9, true);
    // proj-a is still a project of the app (any account).
    let got = w.discover(std::slice::from_ref(&a), &[]);
    let paths: Vec<_> = got.iter().map(|p| p.path.clone()).collect();
    // Most recent use first: proj-c (chat at 5 s) before proj-d (2 s).
    assert_eq!(paths, [c.clone(), d.clone()]);
    let pc = &got[0];
    assert_eq!(pc.name, "proj-c");
    assert_eq!(pc.app_id.as_deref(), Some("877476ef-uuid"));
    assert_eq!(
        pc.sources,
        [
            LostProjectSource::ProjectList,
            LostProjectSource::Chats { count: 2 }
        ]
    );
    assert_eq!(pc.last_used, Some(Timestamp::from_millis(5_000)));
    assert!(pc.recommended());
    let pd = &got[1];
    // A project without a name gets its folder's; further roots are kept,
    // except one some project already has (proj-a).
    assert_eq!(pd.name, "proj-d");
    assert_eq!(pd.extra_roots, std::slice::from_ref(&assets));
    assert_eq!(pd.app_id, None);
    // Answered folders are not offered again.
    let got = w.discover(std::slice::from_ref(&a), &[c.clone(), d.clone()]);
    assert!(got.is_empty(), "{got:?}");
}

#[test]
fn chats_alone_are_no_sign_of_a_loss() {
    let w = World::new();
    let repo = w.dir("src/repo");
    w.thread("t1", &repo, 10, false);
    assert!(w.discover(&[], &[]).is_empty());
    // Next to a lost project, chat folders are offered too, unticked, after it.
    let p = w.dir("work/proj");
    w.project("s-p", "proj", &[&p], 1, Some("k"));
    let got = w.discover(&[], &[]);
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].path, p);
    assert_eq!(got[1].path, repo);
    assert!(!got[1].recommended());
    assert_eq!(got[1].sources, [LostProjectSource::Chats { count: 1 }]);
    assert_eq!(got[1].name, "repo");
}

#[test]
fn home_roots_temp_codex_home_app_chats_and_missing_folders_are_never_offered() {
    let w = World::new();
    let keep = w.dir("work/keep");
    w.project("s-keep", "keep", &[&keep], 1, None);
    let app_chats = w.dir("Documents/Codex/2026-11-10/hello");
    let tmp = std::env::temp_dir().join("superba-lost-projects-test");
    fs::create_dir_all(&tmp).unwrap();
    let inside_codex = w.codex.root().join("worktrees/x");
    fs::create_dir_all(&inside_codex).unwrap();
    let gone = w.home_dir.join("work/gone");
    for (i, p) in [
        w.home_dir.clone(),
        PathBuf::from("/"),
        app_chats,
        tmp.clone(),
        inside_codex,
        gone.clone(),
        PathBuf::from("relative/dir"),
    ]
    .iter()
    .enumerate()
    {
        w.project(&format!("s{i}"), "x", &[p], 10 + i as i64, None);
        w.thread(&format!("t{i}"), p, 10, false);
    }
    let got = w.discover(&[], &[]);
    let paths: Vec<_> = got.iter().map(|p| p.path.clone()).collect();
    assert_eq!(paths, [keep]);
    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn known_folders_match_with_trailing_separators() {
    let w = World::new();
    let a = w.dir("work/a");
    w.project("s-a", "a", &[&a], 1, None);
    let mut with_slash = a.as_os_str().to_owned();
    with_slash.push("/");
    assert!(w.discover(&[PathBuf::from(with_slash)], &[]).is_empty());
    assert_eq!(
        lost_projects::key(Path::new("/x/y/")),
        lost_projects::key(Path::new("/x/y"))
    );
}

#[test]
fn chat_folders_are_capped_and_ranked() {
    let w = World::new();
    let p = w.dir("work/p");
    let rows = vec![ProjectRow {
        app_id: None,
        name: "p".into(),
        roots: vec![p.clone()],
        updated_ms: Some(1),
    }];
    let mut chats = Vec::new();
    for i in 0..(MAX_CHAT_FOLDERS + 5) {
        let d = w.dir(&format!("chats/c{i:02}"));
        chats.push((d, 1, Some(i as i64)));
    }
    let filter = Filter::new(&w.codex, &w.home_dir, &[], &[]);
    let got = lost_projects::candidates(rows, chats, &filter);
    assert_eq!(got.len(), 1 + MAX_CHAT_FOLDERS);
    assert_eq!(got[0].path, p);
    // The newest chat folders, newest first.
    assert!(
        got[1]
            .path
            .ends_with(format!("c{:02}", MAX_CHAT_FOLDERS + 4))
    );
    assert!(got.last().unwrap().path.ends_with("c05"));
}

#[test]
fn no_databases_no_candidates() {
    let w = World::new();
    assert!(w.discover(&[], &[]).is_empty());
}

#[test]
fn every_root_of_a_project_passes_the_same_filter() {
    let w = World::new();
    let m = w.dir("work/proj-m");
    let other = w.dir("work/proj-m-docs");
    let gone = w.home_dir.join("work/gone");
    let home = w.home_dir.clone();
    w.project("s-m", "proj-m", &[&m, &home, &gone, &other], 5, None);
    let got = w.discover(&[], &[]);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].path, m);
    // The home directory and the missing folder are left out of the project.
    assert_eq!(got[0].extra_roots, std::slice::from_ref(&other));
    // A further root some project already has is left out too.
    let got = w.discover(std::slice::from_ref(&other), &[]);
    assert!(got[0].extra_roots.is_empty());
}

/// Log lines written while `f` runs.
fn logged<R>(f: impl FnOnce() -> R) -> (R, String) {
    use std::sync::{Arc, Mutex};
    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf::default();
    let sink = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || sink.clone())
        .with_ansi(false)
        .finish();
    let out = tracing::subscriber::with_default(subscriber, f);
    let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    (out, text)
}

/// Codex keeps the state database in rollback-journal mode on network file
/// systems: a writer then blocks readers. The probe waits at most
/// [`lost_projects::PROBE_BUSY_TIMEOUT`] in all for a database (one read
/// transaction), skips it and says so.
#[test]
fn a_busy_database_is_skipped_quickly_and_logged() {
    let w = World::new();
    let p = w.dir("work/proj");
    {
        let conn = w.db();
        conn.pragma_update(None, "journal_mode", "delete").unwrap();
    }
    w.project("s-p", "proj", &[&p], 1, None);
    w.thread("t1", &p, 1, false);
    let writer = Connection::open(w.codex.state_db()).unwrap();
    writer.execute_batch("BEGIN EXCLUSIVE;").unwrap();
    let start = std::time::Instant::now();
    let (got, log) = logged(|| w.discover(&[], &[]));
    let took = start.elapsed();
    writer.execute_batch("ROLLBACK").unwrap();
    assert!(got.is_empty());
    assert!(
        took >= lost_projects::PROBE_BUSY_TIMEOUT
            && took < lost_projects::PROBE_BUSY_TIMEOUT + std::time::Duration::from_millis(400),
        "took {took:?}"
    );
    assert!(log.contains("WARN"), "{log}");
    assert!(
        log.contains("skipped a database while looking for dropped projects"),
        "{log}"
    );
    assert!(log.contains("state_5.sqlite"), "{log}");
    // Unlocked, it is read again, without a warning.
    let (got, log) = logged(|| w.discover(&[], &[]));
    assert_eq!(got.len(), 1);
    assert!(!log.contains("skipped a database"), "{log}");
}

#[test]
fn a_folder_two_projects_share_stays_with_the_first_and_the_second_keeps_the_rest() {
    let w = World::new();
    let shared = w.dir("work/shared");
    let x_own = w.dir("work/x-own");
    let y_own = w.dir("work/y-own");
    // Read by id: "s-x" before "s-y".
    w.project("s-x", "proj-x", &[&shared, &x_own], 2, None);
    w.project("s-y", "proj-y", &[&shared, &y_own], 1, None);
    // A third project with only the shared folder has nothing left.
    w.project("s-z", "proj-z", &[&shared], 3, None);
    let got = w.discover(&[], &[]);
    let names: Vec<_> = got.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["proj-x", "proj-y"]);
    assert_eq!(got[0].path, shared);
    assert_eq!(got[0].extra_roots, std::slice::from_ref(&x_own));
    // proj-y keeps its own folder as its first one.
    assert_eq!(got[1].path, y_own);
    assert!(got[1].extra_roots.is_empty());
}

#[test]
fn the_databases_of_a_configured_sqlite_home_are_read() {
    let w = World::new();
    let elsewhere = w.dir("state-elsewhere");
    fs::write(
        w.codex.config_toml(),
        format!("sqlite_home = \"{}\"\n", elsewhere.display()),
    )
    .unwrap();
    assert!(lost_projects::database_dirs(&w.codex, &w.home_dir).contains(&elsewhere));
    let p = w.dir("work/proj");
    let conn = Connection::open(elsewhere.join("state_5.sqlite")).unwrap();
    conn.execute_batch(
        "CREATE TABLE projects (id TEXT PRIMARY KEY, name TEXT NOT NULL, position INTEGER NOT NULL, \
           created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL);
         CREATE TABLE project_roots (project_id TEXT NOT NULL, position INTEGER NOT NULL, path TEXT NOT NULL);",
    )
    .unwrap();
    conn.execute("INSERT INTO projects VALUES ('s', 'proj', 0, 1, 1)", [])
        .unwrap();
    conn.execute(
        "INSERT INTO project_roots VALUES ('s', 0, ?1)",
        params![p.to_str().unwrap()],
    )
    .unwrap();
    assert_eq!(w.discover(&[], &[]).len(), 1);
}
