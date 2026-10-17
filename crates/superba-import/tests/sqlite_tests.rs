//! Read-only database access: how each journal mode is opened, and that the
//! other program's files never change.

mod support;

use superba_import::sqlite::{self, OpenMode};
use support::{Home, listing};

#[test]
fn rollback_journal_databases_are_read_in_place_read_only() {
    let h = Home::new();
    let db = h.db(
        "tool/store.db",
        "CREATE TABLE t (x); INSERT INTO t VALUES (1);",
        false,
    );
    let before = listing(&h.path("tool"));
    let snap = sqlite::open(&db, &h.scratch()).unwrap();
    assert_eq!(snap.mode(), OpenMode::ReadOnly);
    let n: i64 = snap
        .conn()
        .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
    assert!(snap.conn().execute("INSERT INTO t VALUES (2)", []).is_err());
    drop(snap);
    assert_eq!(listing(&h.path("tool")), before);
}

#[test]
fn a_clean_wal_database_is_opened_immutable_and_a_pending_one_is_copied() {
    let h = Home::new();
    let db = h.db(
        "tool/store.db",
        "CREATE TABLE t (x); INSERT INTO t VALUES (1);",
        true,
    );
    let before = listing(&h.path("tool"));
    let snap = sqlite::open(&db, &h.scratch()).unwrap();
    assert_eq!(snap.mode(), OpenMode::Immutable);
    drop(snap);
    assert_eq!(listing(&h.path("tool")), before);

    let writer = rusqlite::Connection::open(&db).unwrap();
    writer
        .execute_batch("PRAGMA wal_autocheckpoint = 0; INSERT INTO t VALUES (2);")
        .unwrap();
    let before = listing(&h.path("tool"));
    let snap = sqlite::open(&db, &h.scratch()).unwrap();
    assert_eq!(snap.mode(), OpenMode::Copy);
    let n: i64 = snap
        .conn()
        .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 2, "the commit in the WAL is visible");
    drop(snap);
    assert_eq!(listing(&h.path("tool")), before);
    assert_eq!(std::fs::read_dir(h.scratch()).unwrap().count(), 0);
    drop(writer);
}

#[test]
fn not_a_database() {
    let h = Home::new();
    let p = h.write("tool/store.db", "{\"json\": true}");
    assert!(matches!(
        sqlite::open(&p, &h.scratch()),
        Err(sqlite::OpenError::NotSqlite)
    ));
}

#[test]
fn file_uris_escape_their_path() {
    assert_eq!(
        sqlite::file_uri(std::path::Path::new("/home/a b/x?#%.db"), "mode=ro").unwrap(),
        "file:/home/a%20b/x%3f%23%25.db?mode=ro"
    );
    assert_eq!(
        sqlite::file_uri(std::path::Path::new("C:\\Users\\al\\x.db"), "immutable=1").unwrap(),
        "file:///C:/Users/al/x.db?immutable=1"
    );
}
