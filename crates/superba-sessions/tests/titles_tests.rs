//! Session names for request log keys from a fixture `state_5.sqlite` (both key
//! forms, every fallback, an old schema without `name`, a missing database).

mod common;

use std::collections::BTreeMap;

use rusqlite::Connection;
use superba_core::{CodexHome, TitleSource};
use superba_sessions::titles::{lookup, lookup_in};

const T1: &str = "019a0000-0000-7000-8000-000000000001";
const T2: &str = "019a0000-0000-7000-8000-000000000002";
const T3: &str = "019a0000-0000-7000-8000-000000000003";
const T4: &str = "019a0000-0000-7000-8000-000000000004";

fn insert(conn: &Connection, id: &str, title: &str, first: &str, name: Option<&str>) {
    conn.execute(
        "INSERT INTO threads (id, rollout_path, created_at, updated_at, source, model_provider, cwd, \
         title, sandbox_policy, approval_mode, first_user_message, name) \
         VALUES (?1, '/r', 1, 1, 'vscode', 'openai', '/w', ?2, 'x', 'x', ?3, ?4)",
        rusqlite::params![id, title, first, name],
    )
    .unwrap();
}

fn keys(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn names_titles_first_messages_and_short_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let home = CodexHome::new(tmp.path().join(".codex"));
    std::fs::create_dir_all(home.root()).unwrap();
    let conn = Connection::open(home.state_db()).unwrap();
    conn.execute_batch(common::THREADS_DDL).unwrap();
    insert(&conn, T1, "Refactor the parser", "please refactor", None);
    insert(&conn, T2, "", "  fix\nthe   build  ", None);
    insert(&conn, T3, "auto title", "x", Some("我的会话"));
    insert(&conn, T4, "", "", None);
    drop(conn);

    let got = lookup(
        &home,
        &keys(&[
            T1,
            T2,
            T3,
            T4,
            &format!("guardian:{T1}"),
            "missing-thread-id",
            "",
        ]),
    )
    .unwrap();
    let t = |k: &str| got.get(k).unwrap();
    assert_eq!(t(T1).title, "Refactor the parser");
    assert_eq!(t(T1).source, TitleSource::Title);
    assert_eq!(t(T1).thread_id.as_deref(), Some(T1));
    assert_eq!(t(T2).title, "fix the build");
    assert_eq!(t(T2).source, TitleSource::FirstUserMessage);
    assert_eq!(t(T3).title, "我的会话");
    assert_eq!(t(T4).title, "019a0000");
    assert_eq!(t(T4).source, TitleSource::ShortKey);
    // `<source>:<parent>` resolves to the parent thread.
    let g = t(&format!("guardian:{T1}"));
    assert_eq!(g.title, "Refactor the parser");
    assert_eq!(g.thread_id.as_deref(), Some(T1));
    assert_eq!(t("missing-thread-id").title, "missing-");
    assert_eq!(t("missing-thread-id").source, TitleSource::ShortKey);
    assert_eq!(t("missing-thread-id").thread_id, None);
    assert_eq!(t("").title, "");
}

#[test]
fn old_schema_without_name_and_sqlite_home() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join(".codex");
    let db_dir = tmp.path().join("db");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&db_dir).unwrap();
    let home = CodexHome::new(&root).with_sqlite_home(&db_dir);
    let conn = Connection::open(home.state_db()).unwrap();
    conn.execute_batch(
        "CREATE TABLE threads (id TEXT PRIMARY KEY, title TEXT NOT NULL, \
         first_user_message TEXT NOT NULL DEFAULT '');",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO threads (id, title, first_user_message) VALUES (?1, '', 'hello')",
        [T1],
    )
    .unwrap();
    drop(conn);
    let before = std::fs::read(home.state_db()).unwrap();
    let got = lookup_in(&home, Some(tmp.path()), &keys(&[T1])).unwrap();
    assert_eq!(got[T1].title, "hello");
    assert_eq!(got[T1].source, TitleSource::FirstUserMessage);
    // Read-only: the database is unchanged.
    assert_eq!(std::fs::read(home.state_db()).unwrap(), before);
}

#[test]
fn a_home_without_a_database_gives_short_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let home = CodexHome::new(tmp.path().join("none"));
    let got = lookup(&home, &keys(&[T1, &format!("memory:{T2}")])).unwrap();
    let expected: BTreeMap<String, (String, TitleSource)> = [
        (
            T1.to_owned(),
            ("019a0000".to_owned(), TitleSource::ShortKey),
        ),
        (
            format!("memory:{T2}"),
            ("memory:019a0000".to_owned(), TitleSource::ShortKey),
        ),
    ]
    .into_iter()
    .collect();
    let actual: BTreeMap<String, (String, TitleSource)> = got
        .into_iter()
        .map(|(k, v)| (k, (v.title, v.source)))
        .collect();
    assert_eq!(actual, expected);
}
