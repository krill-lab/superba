//! User bugs 135/136 follow-up: the overview's offer of folders the app
//! dropped, and the answer (queue the ticked folders, never offer the list
//! again).

#![allow(missing_docs)]

use std::fs;
use std::path::PathBuf;

use pretty_assertions::assert_eq;
use rusqlite::{Connection, params};
use superba_core::{CodexHome, Error, LostProjectsAnswer, Timestamp};
use superba_switch::lost_projects::{answer, discover_cached, discover_cached_for, discover_for};

struct World {
    _tmp: tempfile::TempDir,
    home_dir: PathBuf,
    codex: CodexHome,
    app_state: PathBuf,
}

fn world() -> World {
    // Not under the temporary directory: folders there are never offered.
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let root = superba_core::paths::canonical(tmp.path()).unwrap();
    let home_dir = root.join("home");
    let codex = CodexHome::new(home_dir.join(".codex"));
    fs::create_dir_all(codex.root()).unwrap();
    let conn = Connection::open(codex.state_db()).unwrap();
    conn.execute_batch(
        "CREATE TABLE projects (id TEXT PRIMARY KEY, name TEXT NOT NULL, metadata TEXT NOT NULL DEFAULT '{}', \
           position INTEGER NOT NULL, created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL);
         CREATE TABLE project_roots (project_id TEXT NOT NULL, position INTEGER NOT NULL, path TEXT NOT NULL);
         CREATE TABLE project_idempotency_keys (key TEXT PRIMARY KEY, project_id TEXT NOT NULL, created_at_ms INTEGER NOT NULL);",
    )
    .unwrap();
    for (i, name) in ["proj-a", "proj-c", "proj-d"].iter().enumerate() {
        let dir = home_dir.join("work").join(name);
        fs::create_dir_all(&dir).unwrap();
        conn.execute(
            "INSERT INTO projects (id, name, position, created_at_ms, updated_at_ms) VALUES (?1, ?2, ?3, ?3, ?3)",
            params![format!("s-{name}"), name, i as i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO project_roots VALUES (?1, 0, ?2)",
            params![format!("s-{name}"), dir.to_str().unwrap()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO project_idempotency_keys VALUES (?1, ?2, 0)",
            params![format!("k-{name}"), format!("s-{name}")],
        )
        .unwrap();
    }
    // The app knows proj-a (and nothing else, as after the wipe).
    let a = home_dir.join("work/proj-a");
    fs::write(
        codex.global_state_json(),
        serde_json::to_vec(&serde_json::json!({"local-projects": {"local-a": {"rootPaths": [a]}}}))
            .unwrap(),
    )
    .unwrap();
    World {
        app_state: root.join("data/app-state"),
        _tmp: tmp,
        home_dir,
        codex,
    }
}

#[test]
fn the_offer_lists_what_the_app_forgot_and_the_answer_queues_the_ticked_ones() {
    let w = world();
    let lost = discover_for(&w.codex, &w.home_dir, &w.app_state).unwrap();
    let names: Vec<_> = lost.candidates.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["proj-d", "proj-c"]);
    assert_eq!(lost.codex_home, w.codex.root());
    assert_eq!(
        discover_cached(&w.codex, &w.home_dir, &w.app_state),
        Some(lost.clone())
    );

    // A stale list is refused.
    let stale = answer(
        &w.codex,
        &w.home_dir,
        &w.app_state,
        "not-this-one",
        &LostProjectsAnswer::Dismiss,
        Timestamp::EPOCH,
    );
    assert!(matches!(stale, Err(Error::Conflict { .. })), "{stale:?}");

    let c = lost.candidates[1].path.clone();
    let queued = answer(
        &w.codex,
        &w.home_dir,
        &w.app_state,
        &lost.fingerprint,
        &LostProjectsAnswer::Restore {
            paths: vec![c.clone()],
        },
        Timestamp::from_millis(7),
    )
    .unwrap();
    assert_eq!(queued, 1);
    let pending = superba_appctl::project_restore::pending(&w.app_state, w.codex.root())
        .unwrap()
        .unwrap();
    assert_eq!(pending.items.len(), 1);
    assert_eq!(pending.items[0].path, c);
    assert_eq!(pending.items[0].app_id.as_deref(), Some("k-proj-c"));
    // Answered: the whole list is gone, also through the cache.
    assert_eq!(discover_for(&w.codex, &w.home_dir, &w.app_state), None);
    assert_eq!(discover_cached(&w.codex, &w.home_dir, &w.app_state), None);
}

#[test]
fn dismissing_queues_nothing_and_ends_the_offer() {
    let w = world();
    let lost = discover_for(&w.codex, &w.home_dir, &w.app_state).unwrap();
    let queued = answer(
        &w.codex,
        &w.home_dir,
        &w.app_state,
        &lost.fingerprint,
        &LostProjectsAnswer::Dismiss,
        Timestamp::EPOCH,
    )
    .unwrap();
    assert_eq!(queued, 0);
    assert_eq!(
        superba_appctl::project_restore::pending(&w.app_state, w.codex.root()).unwrap(),
        None
    );
    assert_eq!(discover_for(&w.codex, &w.home_dir, &w.app_state), None);
}

#[test]
fn without_the_app_state_nothing_is_offered() {
    let w = world();
    fs::remove_file(w.codex.global_state_json()).unwrap();
    assert_eq!(discover_for(&w.codex, &w.home_dir, &w.app_state), None);
    fs::write(w.codex.global_state_json(), b"\xEF\xBB\xBF{}").unwrap();
    assert_eq!(discover_for(&w.codex, &w.home_dir, &w.app_state), None);
}

fn make_db(path: &std::path::Path) -> Connection {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE projects (id TEXT PRIMARY KEY, name TEXT NOT NULL, metadata TEXT NOT NULL DEFAULT '{}', \
           position INTEGER NOT NULL, created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL);
         CREATE TABLE project_roots (project_id TEXT NOT NULL, position INTEGER NOT NULL, path TEXT NOT NULL);",
    )
    .unwrap();
    conn
}

fn add_project(conn: &Connection, id: &str, dir: &std::path::Path) {
    conn.execute(
        "INSERT INTO projects (id, name, position, created_at_ms, updated_at_ms) VALUES (?1, ?1, 0, 1, 1)",
        params![id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_roots VALUES (?1, 0, ?2)",
        params![id, dir.to_str().unwrap()],
    )
    .unwrap();
}

/// A bare home: the app's state knows nothing, no database yet.
fn bare() -> World {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let root = superba_core::paths::canonical(tmp.path()).unwrap();
    let home_dir = root.join("home");
    let codex = CodexHome::new(home_dir.join(".codex"));
    fs::create_dir_all(codex.root()).unwrap();
    fs::write(codex.global_state_json(), br#"{"local-projects": {}}"#).unwrap();
    World {
        app_state: root.join("data/app-state"),
        _tmp: tmp,
        home_dir,
        codex,
    }
}

#[test]
fn a_change_in_a_configured_sqlite_home_reaches_the_cache() {
    let w = bare();
    let db_dir = w.home_dir.join("elsewhere-state");
    fs::create_dir_all(&db_dir).unwrap();
    let a = w.home_dir.join("work/proj-a");
    let b = w.home_dir.join("work/proj-b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    fs::write(
        w.codex.config_toml(),
        format!("sqlite_home = {:?}\n", db_dir.display().to_string()),
    )
    .unwrap();
    let conn = make_db(&db_dir.join("state_5.sqlite"));
    add_project(&conn, "s-a", &a);
    let first = discover_cached(&w.codex, &w.home_dir, &w.app_state).unwrap();
    assert_eq!(first.candidates.len(), 1);
    // The app-server adds a project there; nothing under the Codex home changes.
    add_project(&conn, "s-b", &b);
    let cached = discover_cached(&w.codex, &w.home_dir, &w.app_state).unwrap();
    assert_eq!(cached.candidates.len(), 2);
}

/// Same size and same time hide a change from the stamp; the age limit
/// still brings it in (here with a zero age, the default is a minute).
#[test]
fn a_change_the_stamp_cannot_see_is_seen_once_the_result_is_too_old() {
    let w = bare();
    let x = w.home_dir.join("work/proj-x");
    let c = w.home_dir.join("work/proj-c");
    fs::create_dir_all(&x).unwrap();
    fs::create_dir_all(&c).unwrap();
    let db = w.codex.state_db();
    let conn = make_db(&db);
    add_project(&conn, "s-c", &c);
    let pin = |t| {
        fs::OpenOptions::new()
            .write(true)
            .open(&db)
            .unwrap()
            .set_modified(t)
            .unwrap()
    };
    let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000);
    pin(t);
    let first = discover_cached(&w.codex, &w.home_dir, &w.app_state).unwrap();
    assert_eq!(first.candidates[0].path, c);
    let len = fs::metadata(&db).unwrap().len();
    conn.execute(
        "UPDATE project_roots SET path = ?1 WHERE project_id = 's-c'",
        params![x.to_str().unwrap()],
    )
    .unwrap();
    pin(t);
    assert_eq!(fs::metadata(&db).unwrap().len(), len);
    // Within the age limit the stamp hides it ...
    assert_eq!(
        discover_cached(&w.codex, &w.home_dir, &w.app_state)
            .unwrap()
            .candidates[0]
            .path,
        c
    );
    // ... past it, it is read again.
    let fresh = discover_cached_for(
        &w.codex,
        &w.home_dir,
        &w.app_state,
        std::time::Duration::ZERO,
    )
    .unwrap();
    assert_eq!(fresh.candidates[0].path, x);
}

/// An answers file of an unknown shape does not block "don't bring back".
#[test]
fn a_scalar_answers_file_does_not_block_a_dismissal() {
    let w = world();
    fs::create_dir_all(&w.app_state).unwrap();
    fs::write(w.app_state.join("lost-projects-answered.json"), b"\"x\"").unwrap();
    let lost = discover_for(&w.codex, &w.home_dir, &w.app_state).unwrap();
    answer(
        &w.codex,
        &w.home_dir,
        &w.app_state,
        &lost.fingerprint,
        &LostProjectsAnswer::Dismiss,
        Timestamp::EPOCH,
    )
    .unwrap();
    assert_eq!(discover_for(&w.codex, &w.home_dir, &w.app_state), None);
}

/// Acceptance repro (880500b7): a chat folder left unticked while another
/// project comes back must not hide a project lost later that starts there.
#[test]
fn an_unticked_chat_folder_does_not_hide_a_project_lost_later() {
    let w = bare();
    let (z, a, b) = (
        w.home_dir.join("work/z"),
        w.home_dir.join("work/a"),
        w.home_dir.join("work/b"),
    );
    for d in [&z, &a, &b] {
        fs::create_dir_all(d).unwrap();
    }
    let conn = make_db(&w.codex.state_db());
    conn.execute_batch(
        "CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL, created_at INTEGER NOT NULL, \
           updated_at INTEGER NOT NULL, cwd TEXT NOT NULL, archived INTEGER NOT NULL DEFAULT 0, updated_at_ms INTEGER);",
    )
    .unwrap();
    add_project(&conn, "s-z", &z);
    conn.execute(
        "INSERT INTO threads (id, rollout_path, created_at, updated_at, cwd, archived) VALUES ('t1', 'r', 10, 10, ?1, 0)",
        params![a.to_str().unwrap()],
    )
    .unwrap();
    let offer = discover_for(&w.codex, &w.home_dir, &w.app_state).unwrap();
    assert!(
        offer
            .candidates
            .iter()
            .any(|c| c.path == a && !c.recommended())
    );
    answer(
        &w.codex,
        &w.home_dir,
        &w.app_state,
        &offer.fingerprint,
        &LostProjectsAnswer::Restore {
            paths: vec![z.clone()],
        },
        Timestamp::EPOCH,
    )
    .unwrap();
    // z is queued, the chat folder a was not offered as a project: nothing
    // to offer right now.
    assert_eq!(discover_for(&w.codex, &w.home_dir, &w.app_state), None);
    // Later the app-server has a project starting in a, which the app lost.
    conn.execute(
        "INSERT INTO projects (id, name, position, created_at_ms, updated_at_ms) VALUES ('s-p', 'proj-p', 0, 20, 20)",
        [],
    )
    .unwrap();
    for (i, r) in [&a, &b].iter().enumerate() {
        conn.execute(
            "INSERT INTO project_roots VALUES ('s-p', ?1, ?2)",
            params![i as i64, r.to_str().unwrap()],
        )
        .unwrap();
    }
    let later = discover_for(&w.codex, &w.home_dir, &w.app_state)
        .expect("a project lost with folders a and b must still be offered");
    let p = later
        .candidates
        .iter()
        .find(|c| c.project_id.as_deref() == Some("s-p"))
        .unwrap();
    assert_eq!(p.path, a);
    assert_eq!(p.extra_roots, std::slice::from_ref(&b));

    // Answered: the unticked project of the list is settled by its id and
    // does not come back through either folder.
    answer(
        &w.codex,
        &w.home_dir,
        &w.app_state,
        &later.fingerprint,
        &LostProjectsAnswer::Restore { paths: Vec::new() },
        Timestamp::EPOCH,
    )
    .unwrap();
    assert_eq!(discover_for(&w.codex, &w.home_dir, &w.app_state), None);
}

/// "Don't bring back" turns down the chat folders of the list as well.
#[test]
fn dismissing_also_turns_down_the_chat_folders() {
    let w = bare();
    let (z, a) = (w.home_dir.join("work/z"), w.home_dir.join("work/a"));
    for d in [&z, &a] {
        fs::create_dir_all(d).unwrap();
    }
    let conn = make_db(&w.codex.state_db());
    conn.execute_batch(
        "CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL, created_at INTEGER NOT NULL, \
           updated_at INTEGER NOT NULL, cwd TEXT NOT NULL, archived INTEGER NOT NULL DEFAULT 0, updated_at_ms INTEGER);",
    )
    .unwrap();
    add_project(&conn, "s-z", &z);
    conn.execute(
        "INSERT INTO threads (id, rollout_path, created_at, updated_at, cwd, archived) VALUES ('t1', 'r', 10, 10, ?1, 0)",
        params![a.to_str().unwrap()],
    )
    .unwrap();
    let offer = discover_for(&w.codex, &w.home_dir, &w.app_state).unwrap();
    answer(
        &w.codex,
        &w.home_dir,
        &w.app_state,
        &offer.fingerprint,
        &LostProjectsAnswer::Dismiss,
        Timestamp::EPOCH,
    )
    .unwrap();
    let got = superba_appctl::project_restore::answered(&w.app_state, w.codex.root()).unwrap();
    assert_eq!(got.project_ids, ["s-z"]);
    assert_eq!(got.folders, std::slice::from_ref(&a));
    // A new lost project there would now come with folder a left out.
    add_project(&conn, "s-q", &w.home_dir.join("work/q"));
    fs::create_dir_all(w.home_dir.join("work/q")).unwrap();
    let later = discover_for(&w.codex, &w.home_dir, &w.app_state).unwrap();
    assert!(later.candidates.iter().all(|c| c.path != a));
}
