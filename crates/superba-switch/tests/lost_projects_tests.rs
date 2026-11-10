//! User bugs 135/136 follow-up: the overview's offer of folders the app
//! dropped, and the answer (queue the ticked folders, never offer the list
//! again).

#![allow(missing_docs)]

use std::fs;
use std::path::PathBuf;

use pretty_assertions::assert_eq;
use rusqlite::{Connection, params};
use superba_core::{CodexHome, Error, LostProjectsAnswer, Timestamp};
use superba_switch::lost_projects::{answer, discover_cached, discover_for};

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
