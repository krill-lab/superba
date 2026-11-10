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
    w.project("s-d", "", &[&d, &a], 2_000, None);
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
    // A project without a name gets its folder's; further roots are kept.
    assert_eq!(pd.name, "proj-d");
    assert_eq!(pd.extra_roots, std::slice::from_ref(&a));
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
