//! Regressions from an adversarial review (Codex gpt-6-astra): each test pins an
//! invariant an earlier version violated. All data, locks, and any executables
//! created here belong to tempfile fixtures.

#![allow(missing_docs)]

mod common;

use std::fs;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Fixture, Thread, rollout_provider, row_provider};
use rusqlite::{Connection, params};
use superba_core::{
    Clock, CodexHome, ConsumersStopped, DataDir, ManualClock, MigrationRequest, MigrationSelector,
    SessionMigrator, Timestamp,
};
use superba_sessions::{CodexSessionMigrator, FaultPlan, FaultPoint, takeover_request};

fn request(from: &str, to: &str) -> MigrationRequest {
    MigrationRequest {
        to_provider: to.into(),
        selectors: vec![MigrationSelector::Provider {
            provider: from.into(),
        }],
        include_archived: true,
        exclude: vec![],
    }
}

fn stopped(f: &Fixture) -> ConsumersStopped {
    ConsumersStopped::new(&f.home, Timestamp::now())
}

fn one(t: Thread) -> Fixture {
    let mut f = Fixture::new();
    f.add(t);
    f
}

// Keep the synthetic history valid when a test changes only the metadata line.
fn adjust_history(f: &Fixture, id: &str, delta: i64) {
    let c = Connection::open(f.history_db()).unwrap();
    c.execute(
        "UPDATE thread_turns SET rollout_byte_offset = rollout_byte_offset + ?1,
         rollout_end_byte_offset = rollout_end_byte_offset + ?1 WHERE thread_id = ?2",
        params![delta, id],
    )
    .unwrap();
    c.execute(
        "UPDATE thread_history_projection_state
         SET next_rollout_byte_offset = next_rollout_byte_offset + ?1 WHERE thread_id = ?2",
        params![delta, id],
    )
    .unwrap();
}

#[tokio::test]
async fn failure_loses_trigger_deleted_rows() {
    let f = one(Thread::new(1, "krill"));
    let c = Connection::open(f.state_db()).unwrap();
    c.execute_batch(
        "CREATE TABLE thread_payloads (thread_id TEXT PRIMARY KEY, body TEXT NOT NULL);
         INSERT INTO thread_payloads SELECT id, 'irreplaceable content' FROM threads;
         CREATE TRIGGER clear_payload AFTER UPDATE OF model_provider ON threads
         BEGIN DELETE FROM thread_payloads WHERE thread_id = NEW.id; END;",
    )
    .unwrap();
    drop(c);
    let before = f.snapshot();
    let m = f.migrator().with_faults(FaultPlan {
        fail_at: Some(FaultPoint::BeforeComplete),
        crash: false,
    });
    let plan = m.plan(&f.home, &request("krill", "openai")).await.unwrap();
    assert!(
        m.migrate_stopped(&f.home, &plan, &stopped(&f))
            .await
            .is_err()
    );
    // Restoring model_provider alone does not resurrect rows deleted by its trigger.
    before.assert_same(&f.snapshot());
}

#[tokio::test]
async fn mutated_primary_key_strands_changes() {
    let f = one(Thread::new(1, "krill"));
    let c = Connection::open(f.state_db()).unwrap();
    c.execute_batch(
        "CREATE TABLE provider_cache (
             thread_id TEXT NOT NULL, model_provider TEXT NOT NULL,
             PRIMARY KEY (thread_id, model_provider)
         ) WITHOUT ROWID;
         INSERT INTO provider_cache SELECT id, model_provider FROM threads;",
    )
    .unwrap();
    drop(c);
    let before = f.snapshot();
    let m = f.migrator().with_faults(FaultPlan {
        fail_at: Some(FaultPoint::BeforeComplete),
        crash: false,
    });
    let plan = m.plan(&f.home, &request("krill", "openai")).await.unwrap();
    assert!(
        m.migrate_stopped(&f.home, &plan, &stopped(&f))
            .await
            .is_err()
    );
    // The ledger's old key cannot locate the row after model_provider changes.
    before.assert_same(&f.snapshot());
}

#[tokio::test]
async fn offset_overflow_leaves_partial_migration() {
    let f = one(Thread::new(1, "krill"));
    let c = Connection::open(f.history_db()).unwrap();
    c.execute(
        "UPDATE thread_turns SET rollout_byte_offset = ?1",
        [i64::MAX],
    )
    .unwrap();
    drop(c);
    let before = f.snapshot();
    let m = f.migrator();
    let plan = m.plan(&f.home, &request("krill", "openai")).await.unwrap();
    assert!(
        m.migrate_stopped(&f.home, &plan, &stopped(&f))
            .await
            .is_err()
    );
    // Overflow must be a normal failure that restores earlier files/DB commits.
    before.assert_same(&f.snapshot());
}

#[tokio::test]
async fn locked_history_database_is_skipped() {
    let f = one(Thread::new(1, "krill"));
    let m = f.migrator();
    let plan = m.plan(&f.home, &request("krill", "openai")).await.unwrap();
    let before = f.snapshot();
    let holder = Connection::open(f.history_db()).unwrap();
    holder
        .pragma_update(None, "journal_mode", "DELETE")
        .unwrap();
    holder.execute_batch("BEGIN EXCLUSIVE").unwrap();
    // A token must not bypass SQLite busy checks. The lock is held by this test.
    let result = m.migrate_stopped(&f.home, &plan, &stopped(&f)).await;
    holder.execute_batch("ROLLBACK").unwrap();
    drop(holder);
    assert!(
        result.is_err(),
        "migration succeeded while the history DB was unreadable: {result:?}"
    );
    before.assert_same(&f.snapshot());
}

#[tokio::test]
async fn offsets_from_inactive_rollout() {
    let f = one(Thread::new(1, "krill").rollout_provider(Some("openai")));
    let t = &f.threads[0];
    let live = f.rollout_path(t);
    let live_before = fs::read(&live).unwrap();
    // The projection and threads.rollout_path follow the unchanged plain file.
    // An older compressed copy still needs a provider rewrite.
    let old = String::from_utf8(live_before.clone()).unwrap().replacen(
        "\"model_provider\":\"openai\"",
        "\"model_provider\":\"krill\"",
        1,
    );
    let compressed = live.with_extension("jsonl.zst");
    fs::write(
        &compressed,
        zstd::stream::encode_all(old.as_bytes(), 3).unwrap(),
    )
    .unwrap();
    f.assert_projection_consistent();
    let m = f.migrator();
    let plan = m.plan(&f.home, &request("krill", "openai")).await.unwrap();
    m.migrate_stopped(&f.home, &plan, &stopped(&f))
        .await
        .unwrap();
    assert_eq!(fs::read(&live).unwrap(), live_before);
    assert_eq!(rollout_provider(&compressed).as_deref(), Some("openai"));
    f.assert_projection_consistent();
}

#[tokio::test]
async fn new_migration_with_pending_ledger() {
    let f = one(Thread::new(1, "krill"));
    let crash = f.migrator().with_faults(FaultPlan {
        fail_at: Some(FaultPoint::AfterRollout(0)),
        crash: true,
    });
    let plan = crash
        .plan(&f.home, &request("krill", "openai"))
        .await
        .unwrap();
    crash
        .migrate_stopped(&f.home, &plan, &stopped(&f))
        .await
        .unwrap_err();
    let m = f.migrator();
    let next = m.plan(&f.home, &request("krill", "openai")).await.unwrap();
    // Either recover first or refuse. A successful second migration must leave
    // the projection correct, even though its rollout was already rewritten.
    if m.migrate_stopped(&f.home, &next, &stopped(&f))
        .await
        .is_ok()
    {
        f.assert_projection_consistent();
    }
}

// Engine consults the clock in prepare_write, then when the pending ledger is
// assembled (after preflight, inventory and backups, before the first write).
// This hook makes that interleaving deterministic without sleep-based races.
struct AtLedgerClock {
    time: Timestamp,
    calls: AtomicUsize,
    hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl std::fmt::Debug for AtLedgerClock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AtLedgerClock").finish_non_exhaustive()
    }
}

impl AtLedgerClock {
    fn new(hook: impl FnOnce() + Send + 'static) -> Arc<Self> {
        Arc::new(Self {
            time: Timestamp::now(),
            calls: AtomicUsize::new(0),
            hook: Mutex::new(Some(Box::new(hook))),
        })
    }
}

impl Clock for AtLedgerClock {
    fn now(&self) -> Timestamp {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
            let hook = self.hook.lock().unwrap().take().unwrap();
            hook();
        }
        self.time
    }

    fn monotonic(&self) -> Duration {
        Duration::ZERO
    }
}

#[tokio::test]
async fn writer_lock_after_preflight() {
    let f = one(Thread::new(1, "krill"));
    let dir = f.home.root().join("thread-writer-locks");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{}.lock", f.threads[0].id));
    let holder = Arc::new(fs::File::create(&path).unwrap());
    let acquired = Arc::new(AtomicBool::new(false));
    let hook_holder = holder.clone();
    let hook_acquired = acquired.clone();
    let clock = AtLedgerClock::new(move || {
        hook_acquired.store(hook_holder.try_lock().is_ok(), Ordering::SeqCst);
    });
    let plan = f
        .migrator()
        .plan(&f.home, &request("krill", "openai"))
        .await
        .unwrap();
    let before = f.snapshot();
    let m = f.migrator().with_clock(clock);
    let result = m.migrate_stopped(&f.home, &plan, &stopped(&f)).await;
    let lock_was_acquired = acquired.load(Ordering::SeqCst);
    if lock_was_acquired {
        holder.unlock().unwrap();
        assert!(
            result.is_err(),
            "a writer acquired the lock after preflight and migration still succeeded"
        );
        before.assert_same(&f.snapshot());
    }
}

#[tokio::test]
async fn same_size_mtime_edit_is_overwritten() {
    let f = one(Thread::new(1, "krill"));
    let path = f.rollout_path(&f.threads[0]);
    let hook_path = path.clone();
    let clock = AtLedgerClock::new(move || {
        let mtime = fs::metadata(&hook_path).unwrap().modified().unwrap();
        let old = fs::read_to_string(&hook_path).unwrap();
        let new = old.replacen("done 0", "edit 0", 1);
        assert_ne!(old, new);
        assert_eq!(old.len(), new.len());
        fs::write(&hook_path, new).unwrap();
        fs::File::options()
            .write(true)
            .open(&hook_path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(mtime))
            .unwrap();
    });
    let plan = f
        .migrator()
        .plan(&f.home, &request("krill", "openai"))
        .await
        .unwrap();
    let m = f.migrator().with_clock(clock);
    let _result = m.migrate_stopped(&f.home, &plan, &stopped(&f)).await;
    assert!(
        fs::read_to_string(&path).unwrap().contains("edit 0"),
        "the unrelated edit made after the backup was silently overwritten"
    );
}

#[tokio::test]
async fn relative_home_ledger_not_recovered() {
    const CHILD: &str = "SUPERBA_REVIEW_RELATIVE_HOME_CHILD";
    if let Some(root) = std::env::var_os(CHILD) {
        // This branch runs only in the child started below, with its cwd in the
        // parent's tempfile. All persisted relative paths refer to that cwd.
        let root = std::path::PathBuf::from(root);
        let home = CodexHome::new("home/.codex");
        let m = CodexSessionMigrator::new(DataDir::new(root.join("data")))
            .with_home_dir(root.join("home"))
            .with_faults(FaultPlan {
                fail_at: Some(FaultPoint::AfterRollout(0)),
                crash: true,
            });
        let plan = m.plan(&home, &request("krill", "openai")).await.unwrap();
        m.migrate_stopped(
            &home,
            &plan,
            &ConsumersStopped::new(&home, Timestamp::now()),
        )
        .await
        .unwrap_err();
        return;
    }
    let f = one(Thread::new(1, "krill"));
    let before = f.snapshot();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "relative_home_ledger_not_recovered",
            "--nocapture",
        ])
        .current_dir(f.tmp.path())
        .env(CHILD, f.tmp.path())
        .env("HOME", &f.home_dir)
        .env("USERPROFILE", &f.home_dir)
        .env("CODEX_HOME", f.home.root())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child setup failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        rollout_provider(&f.rollout_path(&f.threads[0])).as_deref(),
        Some("openai")
    );
    let report = f
        .migrator()
        .recover_stopped(&f.home, &stopped(&f))
        .await
        .unwrap();
    assert_eq!(
        report.restored_rollouts, 1,
        "recovery lost a pending ledger when the working directory changed"
    );
    before.assert_same(&f.snapshot());
}

#[tokio::test]
async fn recovery_swallows_unsupported_ledger() {
    let f = one(Thread::new(1, "krill"));
    let m = f.migrator().with_faults(FaultPlan {
        fail_at: Some(FaultPoint::AfterRollout(0)),
        crash: true,
    });
    let plan = m.plan(&f.home, &request("krill", "openai")).await.unwrap();
    m.migrate_stopped(&f.home, &plan, &stopped(&f))
        .await
        .unwrap_err();
    // Model running an older Superba after a newer one crashed mid-migration.
    let path = plan.backup_dir.join("ledger.json");
    let mut ledger: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    ledger["version"] = serde_json::json!(2);
    fs::write(&path, serde_json::to_vec(&ledger).unwrap()).unwrap();
    let result = f.migrator().recover_stopped(&f.home, &stopped(&f)).await;
    assert!(
        result.is_err(),
        "recovery reported success while ignoring the pending ledger: {result:?}"
    );
}

#[cfg(target_os = "linux")]
struct OwnedChild(std::process::Child);

#[cfg(target_os = "linux")]
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn relative_process_home_bypasses_guard() {
    let f = one(Thread::new(1, "krill"));
    let m = f.migrator();
    let plan = m.plan(&f.home, &request("krill", "openai")).await.unwrap();
    let executable = f.tmp.path().join("codex");
    let sleep = ["/bin/sleep", "/usr/bin/sleep"]
        .into_iter()
        .find(|p| std::path::Path::new(p).is_file())
        .unwrap();
    fs::copy(sleep, &executable).unwrap();
    let mut child = OwnedChild(
        std::process::Command::new(&executable)
            .arg("60")
            .current_dir(f.tmp.path())
            .env_clear()
            .env("HOME", &f.home_dir)
            .env("CODEX_HOME", "home/.codex")
            .spawn()
            .unwrap(),
    );
    assert!(child.0.try_wait().unwrap().is_none());
    let result = m.migrate(&f.home, &plan).await;
    assert!(
        result.as_ref().is_err_and(|e| e.code() == "locked"),
        "a live Codex process using this home through a relative path was missed: {result:?}"
    );
}

#[tokio::test]
async fn provider_selector_union_loses_source() {
    let f = one(Thread::new(1, "krill").rollout_provider(Some("codex")));
    let m = f.migrator();
    let mut req = request("krill", "openai");
    req.selectors.push(MigrationSelector::Provider {
        provider: "codex".into(),
    });
    let plan = m.plan(&f.home, &req).await.unwrap();
    m.migrate_stopped(&f.home, &plan, &stopped(&f))
        .await
        .unwrap();
    assert_eq!(
        row_provider(&f.state_db(), &f.threads[0].id).as_deref(),
        Some("openai")
    );
    assert_eq!(
        rollout_provider(&f.rollout_path(&f.threads[0])).as_deref(),
        Some("openai"),
        "the union of krill and codex selectors must migrate both storage forms"
    );
}

#[tokio::test]
async fn catalog_source_is_not_selected() {
    let f = one(Thread::new(1, "openai").catalog());
    let c = Connection::open(f.catalog_db()).unwrap();
    c.execute(
        "UPDATE local_thread_catalog SET model_provider = 'krill'",
        [],
    )
    .unwrap();
    drop(c);
    let m = f.migrator();
    let plan = m.plan(&f.home, &request("krill", "openai")).await.unwrap();
    m.migrate_stopped(&f.home, &plan, &stopped(&f))
        .await
        .unwrap();
    let c = Connection::open(f.catalog_db()).unwrap();
    let provider: String = c
        .query_row("SELECT model_provider FROM local_thread_catalog", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        provider, "openai",
        "a catalog provider is ignored when another storage form knows this thread"
    );
}

#[tokio::test]
async fn escaped_session_meta_type_is_skipped() {
    let f = one(Thread::new(1, "krill"));
    let path = f.rollout_path(&f.threads[0]);
    let old = fs::read_to_string(&path).unwrap();
    // Escape every occurrence, including the decoy inside base_instructions,
    // so a raw substring search cannot accidentally find an unrelated match.
    let new = old.replace("session_meta", r"session\u005fmeta");
    fs::write(&path, &new).unwrap();
    adjust_history(&f, &f.threads[0].id, new.len() as i64 - old.len() as i64);
    f.assert_projection_consistent();
    let m = f.migrator();
    let plan = m.plan(&f.home, &request("krill", "openai")).await.unwrap();
    m.migrate_stopped(&f.home, &plan, &stopped(&f))
        .await
        .unwrap();
    assert_eq!(
        rollout_provider(&path).as_deref(),
        Some("openai"),
        "valid JSON escapes in the type must not hide the session_meta record"
    );
}

#[tokio::test]
async fn null_provider_creates_duplicate_member() {
    #[derive(serde::Deserialize)]
    struct Record {
        payload: Meta,
    }
    #[derive(serde::Deserialize)]
    struct Meta {
        model_provider: Option<String>,
    }

    let f = one(Thread::new(1, "openai"));
    let path = f.rollout_path(&f.threads[0]);
    let old = fs::read_to_string(&path).unwrap();
    let new = old.replacen(
        "\"model_provider\":\"openai\"",
        "\"model_provider\":null",
        1,
    );
    fs::write(&path, &new).unwrap();
    adjust_history(&f, &f.threads[0].id, new.len() as i64 - old.len() as i64);
    let original: Record = serde_json::from_str(new.lines().next().unwrap()).unwrap();
    assert_eq!(original.payload.model_provider, None);
    let m = f.migrator();
    let plan = m.plan(&f.home, &request("openai", "krill")).await.unwrap();
    m.migrate_stopped(&f.home, &plan, &stopped(&f))
        .await
        .unwrap();
    let after = fs::read_to_string(&path).unwrap();
    let parsed = serde_json::from_str::<Record>(after.lines().next().unwrap());
    assert!(
        parsed.is_ok(),
        "migration introduced a duplicate field rejected by typed JSON deserialization: {}",
        parsed.err().unwrap()
    );
    assert_eq!(
        after
            .lines()
            .next()
            .unwrap()
            .matches("\"model_provider\":")
            .count(),
        1,
        "a present null provider must be replaced, not followed by a duplicate field"
    );
}

#[tokio::test]
async fn open_creation_window_includes_future() {
    let f = one(Thread::new(1, "openai"));
    let now = Timestamp(f.threads[0].created_ms - 1);
    let m = f.migrator().with_clock(Arc::new(ManualClock::new(now)));
    let req = MigrationRequest {
        to_provider: "krill".into(),
        selectors: vec![MigrationSelector::CreatedBetween {
            provider: "openai".into(),
            from: Timestamp(0),
            to: None,
        }],
        include_archived: true,
        exclude: vec![],
    };
    let plan = m.plan(&f.home, &req).await.unwrap();
    assert!(
        plan.items.is_empty(),
        "to: None means now, not an unbounded future"
    );
}

#[tokio::test]
async fn nullable_primary_key_is_not_a_row_identity() {
    let f = one(Thread::new(1, "krill").no_rollout());
    let c = Connection::open(f.state_db()).unwrap();
    c.execute_batch(
        "CREATE TABLE provider_cache (
             host TEXT, thread_id TEXT, model_provider TEXT,
             PRIMARY KEY (host, thread_id)
         );
         INSERT INTO provider_cache SELECT NULL, id, model_provider FROM threads;
         INSERT INTO provider_cache SELECT NULL, id, model_provider FROM threads;",
    )
    .unwrap();
    drop(c);
    let m = f.migrator();
    let plan = m.plan(&f.home, &request("krill", "openai")).await.unwrap();
    let result = m.migrate_stopped(&f.home, &plan, &stopped(&f)).await;
    assert!(
        result.is_ok(),
        "valid duplicate NULL primary keys need distinct rowids: {result:?}"
    );
}

#[tokio::test]
async fn receipt_counts_untouched_session() {
    let f = one(Thread::new(1, "krill").no_rollout());
    let m = f.migrator();
    let plan = m.plan(&f.home, &request("krill", "openai")).await.unwrap();
    let c = Connection::open(f.state_db()).unwrap();
    c.execute("UPDATE threads SET model_provider = 'openai'", [])
        .unwrap();
    drop(c);
    let receipt = m
        .migrate_stopped(&f.home, &plan, &stopped(&f))
        .await
        .unwrap();
    assert_eq!((receipt.threads, receipt.rollouts), (0, 0));
    assert_eq!(
        receipt.by_source_provider.values().sum::<u32>(),
        0,
        "the completed receipt claims a source session although nothing was migrated"
    );
}

#[tokio::test]
async fn distinct_count_omits_inconsistent_rollout() {
    let f = one(Thread::new(1, "openai").rollout_provider(Some("krill")));
    let report = f.migrator().scan(&f.home).await.unwrap();
    assert_eq!(report.by_provider["krill"].rollouts, 1);
    assert_eq!(
        (
            report.by_provider["krill"].distinct,
            takeover_request(&report).is_some()
        ),
        (1, true),
        "the per-provider union must include the legacy rollout and allow takeover to repair it"
    );
}

#[tokio::test]
async fn created_at_overflow_panics_scan() {
    let f = one(Thread::new(1, "krill").no_rollout());
    let c = Connection::open(f.state_db()).unwrap();
    c.execute(
        "UPDATE threads SET created_at_ms = NULL, created_at = ?1",
        [i64::MAX],
    )
    .unwrap();
    drop(c);
    let result = f.migrator().scan(&f.home).await;
    assert!(
        result.is_ok(),
        "an out-of-range timestamp must produce missing metadata/a warning, not panic: {result:?}"
    );
}

#[tokio::test]
async fn token_timestamp_overflow_panics() {
    let f = one(Thread::new(1, "krill").no_rollout());
    let m = f.migrator();
    let plan = m.plan(&f.home, &request("krill", "openai")).await.unwrap();
    let token = ConsumersStopped::new(&f.home, Timestamp(i64::MIN));
    let error = m.migrate_stopped(&f.home, &plan, &token).await.unwrap_err();
    assert_eq!(
        error.code(),
        "invalid",
        "invalid extreme timestamps must not panic in the worker"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_database_is_silently_skipped() {
    let f = one(Thread::new(1, "krill"));
    let outside = f.tmp.path().join("actual-state.sqlite");
    fs::rename(f.state_db(), &outside).unwrap();
    std::os::unix::fs::symlink(&outside, f.state_db()).unwrap();
    let before = f.snapshot();
    let m = f.migrator();
    let plan = m.plan(&f.home, &request("krill", "openai")).await.unwrap();
    let result = m.migrate_stopped(&f.home, &plan, &stopped(&f)).await;
    if result.is_ok() {
        assert_eq!(
            row_provider(&outside, &f.threads[0].id).as_deref(),
            Some("openai"),
            "a successful migration changed the rollout but silently omitted its symlinked DB"
        );
    } else {
        before.assert_same(&f.snapshot());
    }
}
