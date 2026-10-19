use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::json;
use superba_core::LocalContext;
#[cfg(unix)]
use superba_core::MapEnv;
use superba_core::data::DataMoveProgressSink;
use superba_storage::{
    DataLock, LOCATION_FILE, MOVE_FILE, MoveJournal, Pointer, PreparedMove, resolve,
};

fn fixture() -> (tempfile::TempDir, LocalContext) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let ctx = LocalContext::for_tests(&root);
    fs::create_dir_all(ctx.data_dir.root()).unwrap();
    fs::create_dir_all(&ctx.home_dir).unwrap();
    (temp, ctx)
}

fn accept() -> DataMoveProgressSink {
    Arc::new(|_| true)
}

#[test]
fn successful_move_verifies_bytes_rewrites_owned_paths_and_keeps_fixed_pointer() {
    let (_temp, ctx) = fixture();
    let old = ctx.data_dir.root();
    fs::create_dir_all(old.join("secrets")).unwrap();
    fs::create_dir_all(old.join("backups/migrate-one/rollouts")).unwrap();
    fs::write(old.join("secrets/vault-key"), "sample vault key").unwrap();
    superba_storage::write_json(
        &old.join("secrets/vault.json"),
        &json!({"location":{"file":{"path":old.join("secrets/vault-key")}}}),
    )
    .unwrap();
    let original =
        json!({"path": old.join("secrets/vault-key"), "message": "a session backup is opaque"})
            .to_string();
    fs::write(
        old.join("backups/migrate-one/rollouts/keep.json"),
        &original,
    )
    .unwrap();
    let target = old.parent().unwrap().join("数据 有空格");
    let _lock = DataLock::exclusive(old).unwrap();
    let mut move_ = PreparedMove::prepare(&ctx, &target).unwrap();
    move_.begin(false).unwrap();
    let reports = Arc::new(Mutex::new(Vec::new()));
    let record = reports.clone();
    let progress: DataMoveProgressSink = Arc::new(move |progress| {
        record.lock().unwrap().push(progress);
        true
    });
    move_.copy(&progress).unwrap();
    assert_eq!(resolve(old).unwrap().root(), old);
    assert!(!target.exists());
    move_.publish().unwrap();
    let active = resolve(old).unwrap();
    assert_eq!(active.root(), move_.journal.target);
    assert_eq!(active.anchor(), old);
    assert!(
        old.join("secrets/vault-key").exists(),
        "source kept until router restart succeeds"
    );
    let meta: serde_json::Value =
        serde_json::from_slice(&fs::read(target.join("secrets/vault.json")).unwrap()).unwrap();
    assert_eq!(
        meta["location"]["file"]["path"],
        json!(move_.journal.target.join("secrets/vault-key"))
    );
    assert_eq!(
        fs::read_to_string(target.join("backups/migrate-one/rollouts/keep.json")).unwrap(),
        original
    );
    assert!(reports.lock().unwrap().last().unwrap().copied_bytes > 0);
    superba_storage::cleanup_source(old, old).unwrap();
    assert!(old.join(LOCATION_FILE).is_file());
    assert!(old.join(MOVE_FILE).is_file());
    assert!(!old.join("secrets").exists());
}

#[test]
fn repeated_moves_use_the_original_pointer_and_rollback_restores_it() {
    let (_temp, ctx) = fixture();
    let old = ctx.data_dir.root();
    fs::write(old.join("data.bin"), b"data").unwrap();
    let a = old.parent().unwrap().join("first");
    let b = old.parent().unwrap().join("second");
    let mut first = PreparedMove::prepare(&ctx, &a).unwrap();
    first.begin(false).unwrap();
    first.copy(&accept()).unwrap();
    first.publish().unwrap();
    MoveJournal::clear(old).unwrap();
    first.journal.forget_owner();
    let mut moved = ctx.clone();
    moved.data_dir = resolve(old).unwrap();
    let mut second = PreparedMove::prepare(&moved, &b).unwrap();
    second.begin(false).unwrap();
    second.copy(&accept()).unwrap();
    second.publish().unwrap();
    assert_eq!(resolve(old).unwrap().root(), second.journal.target);
    assert!(!a.join(LOCATION_FILE).exists());
    second.journal.restore_pointer(old).unwrap();
    second.journal.discard_copies().unwrap();
    assert_eq!(resolve(old).unwrap().root(), first.journal.target);
    assert!(!b.exists());
    assert_eq!(fs::read(a.join("data.bin")).unwrap(), b"data");
}

#[test]
fn cancellation_keeps_source_pointer_and_removes_owned_staging() {
    let (_temp, ctx) = fixture();
    let old = ctx.data_dir.root();
    fs::write(old.join("large"), vec![9u8; 2 * 1024 * 1024]).unwrap();
    let target = old.parent().unwrap().join("cancelled");
    let staging;
    {
        let mut move_ = PreparedMove::prepare(&ctx, &target).unwrap();
        move_.begin(false).unwrap();
        staging = move_.journal.staging.clone();
        let cancel: DataMoveProgressSink = Arc::new(|_| false);
        assert!(matches!(
            move_.copy(&cancel),
            Err(superba_core::Error::Cancelled)
        ));
    }
    assert!(!staging.exists());
    assert!(!target.exists());
    assert!(!old.join(LOCATION_FILE).exists());
    assert_eq!(
        fs::metadata(old.join("large")).unwrap().len(),
        2 * 1024 * 1024
    );
    assert!(MoveJournal::read(old).unwrap().is_some());
}

#[test]
fn copy_io_failure_is_non_destructive() {
    let (_temp, ctx) = fixture();
    fs::write(ctx.data_dir.root().join("file"), b"untouched").unwrap();
    let target = ctx.home_dir.parent().unwrap().join("destination");
    let mut move_ = PreparedMove::prepare(&ctx, &target).unwrap();
    fs::write(move_.journal.staging.join("file"), b"preexisting").unwrap();
    assert!(move_.copy(&accept()).is_err());
    assert_eq!(
        fs::read(ctx.data_dir.root().join("file")).unwrap(),
        b"untouched"
    );
    assert!(!target.exists());
    assert_eq!(
        resolve(ctx.data_dir.anchor()).unwrap().root(),
        ctx.data_dir.root()
    );
}

#[test]
fn new_destination_contents_are_never_overwritten_or_removed() {
    let (_temp, ctx) = fixture();
    let target = ctx.home_dir.parent().unwrap().join("destination");
    let mut move_ = PreparedMove::prepare(&ctx, &target).unwrap();
    move_.copy(&accept()).unwrap();
    fs::create_dir(&target).unwrap();
    fs::write(target.join("owner.txt"), "belongs to user").unwrap();
    assert!(move_.publish().is_err());
    move_.journal.discard_copies().unwrap();
    assert_eq!(
        fs::read_to_string(target.join("owner.txt")).unwrap(),
        "belongs to user"
    );
}

#[test]
fn pointer_write_failure_can_restore_the_source_after_directory_publication() {
    let (_temp, ctx) = fixture();
    let target = ctx.home_dir.parent().unwrap().join("destination");
    let mut move_ = PreparedMove::prepare(&ctx, &target).unwrap();
    move_.copy(&accept()).unwrap();
    fs::create_dir(ctx.data_dir.root().join(LOCATION_FILE)).unwrap();
    assert!(move_.publish().is_err());
    assert!(move_.journal.owns_target());
    fs::remove_dir(ctx.data_dir.root().join(LOCATION_FILE)).unwrap();
    move_
        .journal
        .restore_pointer(ctx.data_dir.anchor())
        .unwrap();
    move_.journal.discard_copies().unwrap();
    assert!(!target.exists());
    assert!(ctx.data_dir.root().is_dir());
}

#[test]
fn corrupt_or_unavailable_pointer_fails_closed() {
    let (_temp, ctx) = fixture();
    let path = ctx.data_dir.root().join(LOCATION_FILE);
    for text in [
        "not json",
        r#"{"schema":2,"directory":"relative"}"#,
        r#"{"schema":1,"directory":"relative"}"#,
    ] {
        fs::write(&path, text).unwrap();
        assert!(resolve(ctx.data_dir.anchor()).is_err());
    }
    superba_storage::write_json(
        &path,
        &Pointer {
            schema: 1,
            directory: ctx.home_dir.join("missing"),
        },
    )
    .unwrap();
    assert!(resolve(ctx.data_dir.anchor()).is_err());
    assert!(!ctx.home_dir.join("missing").exists());
}

#[test]
fn rejects_overlaps_nonempty_relative_file_home_and_codex_destinations() {
    let (_temp, ctx) = fixture();
    let nonempty = ctx.home_dir.parent().unwrap().join("nonempty");
    fs::create_dir(&nonempty).unwrap();
    fs::write(nonempty.join("keep"), b"keep").unwrap();
    let file = ctx.home_dir.parent().unwrap().join("file");
    fs::write(&file, b"keep").unwrap();
    for target in [
        ctx.data_dir.root().to_path_buf(),
        ctx.data_dir.root().join("below"),
        ctx.data_dir.root().parent().unwrap().to_path_buf(),
        ctx.home_dir.clone(),
        ctx.codex_home().root().to_path_buf(),
        ctx.codex_home().sessions_dir(),
        nonempty.clone(),
        file,
        Path::new("relative").into(),
        // PathBuf::join normalizes `..` for verbatim Windows paths; preserve
        // the user's raw spelling so the validator actually sees ParentDir.
        std::path::PathBuf::from(format!(
            "{}/../other",
            superba_core::paths::display_path(&ctx.home_dir)
        )),
    ] {
        assert!(
            PreparedMove::prepare(&ctx, &target).is_err(),
            "{}",
            target.display()
        );
    }
    assert_eq!(fs::read(nonempty.join("keep")).unwrap(), b"keep");
}

#[test]
fn shared_readers_exclude_move_and_move_excludes_readers() {
    let (_temp, ctx) = fixture();
    let first = DataLock::shared(ctx.data_dir.anchor()).unwrap();
    let second = DataLock::shared(ctx.data_dir.anchor()).unwrap();
    assert!(DataLock::exclusive(ctx.data_dir.anchor()).is_err());
    drop(first);
    drop(second);
    let exclusive = DataLock::exclusive(ctx.data_dir.anchor()).unwrap();
    assert!(DataLock::shared(ctx.data_dir.anchor()).is_err());
    assert!(DataLock::exclusive(ctx.data_dir.anchor()).is_err());
    drop(exclusive);
    assert!(DataLock::shared(ctx.data_dir.anchor()).is_ok());
}

#[test]
fn insufficient_space_and_overflow_are_refused() {
    assert!(superba_storage::check_space(100, 1).is_err());
    assert!(superba_storage::check_space(u64::MAX, u64::MAX).is_err());
    assert!(superba_storage::check_space(100, 32 * 1024 * 1024).is_ok());
}

#[test]
fn windows_policy_rejects_unc_devices_system_roots_reserved_names_and_streams() {
    for path in [
        r"\server\share",
        r"\\server\share",
        r"\\?\UNC\server\share",
        r"\\.\C:\data",
        r"C:\",
        r"C:relative",
        r"C:\Windows\data",
        r"D:\Program Files\Superba",
        r"C:\ProgramData\data",
        r"D:\data:stream",
        r"D:\NUL",
        r"D:\COM1.txt",
        r"D:\trailing.\data",
        r"D:\foo\..\bar",
    ] {
        assert!(
            !superba_storage::validation::windows_path_allowed(path),
            "{path}"
        );
    }
    assert!(superba_storage::validation::windows_path_allowed(
        r"D:\我的 数据\Superba"
    ));
    assert!(superba_storage::validation::windows_path_allowed(
        r"C:\Users\me\AppData\Local\Programs\Superba"
    ));
}

#[test]
fn usage_is_disjoint_and_codex_is_separate() {
    let (_temp, ctx) = fixture();
    for (relative, bytes) in [
        ("router/usage.sqlite3", 10),
        ("router/usage.sqlite3-wal", 20),
        ("router/router.log", 30),
        ("backups/migrate-id/rollouts/a.jsonl", 40),
        ("backups/switch/x.enc", 50),
        ("cache/usage/x", 60),
        ("downloads/setup.msi", 70),
        ("codex/version/codex", 80),
        ("imported-usage.sqlite3", 90),
        ("profiles.json", 100),
    ] {
        let path = ctx.data_dir.root().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![0; bytes]).unwrap();
    }
    fs::create_dir_all(ctx.codex_home().sessions_dir()).unwrap();
    fs::write(
        ctx.codex_home().sessions_dir().join("a.jsonl"),
        vec![0; 150],
    )
    .unwrap();
    fs::write(ctx.codex_home().config_toml(), vec![0; 25]).unwrap();
    let usage = superba_storage::usage(&ctx);
    assert_eq!(usage.total_bytes, 550);
    assert_eq!(usage.codex_sessions_bytes, 150);
    assert_eq!(usage.codex_home_bytes, 175);
    assert_eq!(
        usage
            .categories
            .iter()
            .find(|c| c.category == "requests")
            .unwrap()
            .bytes,
        30
    );
    assert_eq!(usage.skipped, 0);
}

#[test]
fn path_rewrite_does_not_change_user_strings_or_prefix_neighbors() {
    let (_temp, ctx) = fixture();
    let old = ctx.data_dir.root();
    let new = ctx.home_dir.join("new");
    let mut value = json!({"path":old.join("key"),"message":old.join("key"),"nested":{"path":old.with_extension("other").join("key")}});
    assert!(superba_storage::rewrite_paths(&mut value, old, &new));
    assert_eq!(value["path"], json!(new.join("key")));
    assert_eq!(value["message"], json!(old.join("key")));
    assert_eq!(
        value["nested"]["path"],
        json!(old.with_extension("other").join("key"))
    );
}

#[cfg(unix)]
#[test]
fn symlink_sources_and_destinations_are_refused_without_touching_link_target() {
    use std::os::unix::fs::symlink;
    let (_temp, ctx) = fixture();
    let external = ctx.home_dir.join("external");
    fs::create_dir(&external).unwrap();
    fs::write(external.join("key"), b"outside").unwrap();
    let link = ctx.home_dir.join("linked");
    symlink(&external, &link).unwrap();
    assert!(PreparedMove::prepare(&ctx, &link).is_err());
    symlink(&external, ctx.data_dir.root().join("linked")).unwrap();
    assert!(PreparedMove::prepare(&ctx, &ctx.home_dir.join("new")).is_err());
    assert_eq!(fs::read(external.join("key")).unwrap(), b"outside");
}

#[cfg(unix)]
#[test]
fn system_targets_and_unwritable_targets_are_refused() {
    use std::os::unix::fs::PermissionsExt;
    let (_temp, ctx) = fixture();
    assert!(
        superba_storage::validation::validate_directory(
            Path::new("/usr/superba-data"),
            &MapEnv::new()
        )
        .is_err()
    );
    let locked = ctx.home_dir.join("locked");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(locked.join("probe"), b"x").is_err() {
        assert!(PreparedMove::prepare(&ctx, &locked).is_err());
    }
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
}
