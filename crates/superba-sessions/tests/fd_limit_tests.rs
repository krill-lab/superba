//! Open-file limits and lock conflicts: a migration keeps a small, bounded number
//! of files open whatever the session count, "too many open files" is reported as
//! such, and only a genuinely held Codex lock is reported as one.
//!
//! The tests that lower `RLIMIT_NOFILE` run in a child process (this test binary
//! started again for one test), so the limit never affects other tests.

#![allow(missing_docs)]

mod common;

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use common::{Fixture, Thread, rollout_provider, row_provider};
use superba_core::*;

fn request() -> MigrationRequest {
    MigrationRequest {
        to_provider: "openai".into(),
        selectors: vec![MigrationSelector::Provider {
            provider: "krill".into(),
        }],
        include_archived: true,
        exclude: vec![],
    }
}

/// Codex's writer-lock folder with a free lock file for every session, as a
/// home used by a Codex with writer locks can have.
fn add_writer_locks(f: &Fixture) {
    let dir = f.home.root().join("thread-writer-locks");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(".coordination.lock"), b"").unwrap();
    for t in &f.threads {
        fs::write(dir.join(format!("{}.lock", t.id)), b"").unwrap();
    }
}

/// Paths a child process needs to rebuild the fixture's migrator.
#[cfg(unix)]
fn child_env(f: &Fixture) -> [(&'static str, PathBuf); 3] {
    [
        ("SUPERBA_FD_CHILD_CODEX_HOME", f.home.root().to_path_buf()),
        ("SUPERBA_FD_CHILD_HOME_DIR", f.home_dir.clone()),
        ("SUPERBA_FD_CHILD_DATA", f.tmp.path().join("data")),
    ]
}

/// The fixture as seen from the child: home and migrator.
#[cfg(unix)]
fn child_fixture() -> Option<(CodexHome, superba_sessions::CodexSessionMigrator)> {
    let var = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let root = var("SUPERBA_FD_CHILD_CODEX_HOME")?;
    let home = CodexHome::new(root);
    let m =
        superba_sessions::CodexSessionMigrator::new(DataDir::new(var("SUPERBA_FD_CHILD_DATA")?))
            .with_home_dir(var("SUPERBA_FD_CHILD_HOME_DIR")?);
    Some((home, m))
}

/// Run the single test `name` of this binary in a child process with the
/// fixture's paths; returns its output after asserting it passed.
#[cfg(unix)]
fn run_child(name: &str, f: &Fixture) -> String {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", name, "--nocapture", "--test-threads=1"]);
    for (k, v) in child_env(f) {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "child {name} failed:\n{text}");
    println!("{text}");
    text
}

/// Lower this process's soft open-file limit (the hard one is kept).
#[cfg(unix)]
fn set_soft_limit(soft: u64) {
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
    let now = getrlimit(Resource::Nofile);
    setrlimit(
        Resource::Nofile,
        Rlimit {
            current: Some(soft),
            maximum: now.maximum,
        },
    )
    .unwrap();
    assert_eq!(fdlimit::open_files_limit(), Some(soft));
}

#[cfg(unix)]
const SESSIONS: u32 = 2_000;
#[cfg(unix)]
const MACOS_GUI_LIMIT: u64 = 256;

/// 2,000 sessions migrate and roll back under macOS's GUI soft limit of 256 open
/// files, with Codex's writer-lock folder present (the case that failed at about
/// session 235 when every session's lock stayed open).
#[cfg(unix)]
#[tokio::test]
async fn two_thousand_sessions_migrate_under_a_256_open_file_limit() {
    const NAME: &str = "two_thousand_sessions_migrate_under_a_256_open_file_limit";
    if let Some((home, m)) = child_fixture() {
        set_soft_limit(MACOS_GUI_LIMIT);
        let plan = m.plan(&home, &request()).await.unwrap();
        assert_eq!(plan.items.len() as u32, SESSIONS);
        let token = ConsumersStopped::new(&home, Timestamp::now());
        let receipt = m.migrate_stopped(&home, &plan, &token).await.unwrap();
        assert_eq!(receipt.threads, SESSIONS);
        assert_eq!(receipt.rollouts, SESSIONS);
        // Nothing raised the limit behind the test's back.
        assert_eq!(fdlimit::open_files_limit(), Some(MACOS_GUI_LIMIT));
        println!("CHILD-MIGRATED {}", plan.id);
        let report = m.rollback_stopped(&home, &plan.id, &token).await.unwrap();
        assert_eq!(report.restored_rollouts, SESSIONS);
        let plan = m.plan(&home, &request()).await.unwrap();
        m.migrate_stopped(&home, &plan, &token).await.unwrap();
        assert_eq!(fdlimit::open_files_limit(), Some(MACOS_GUI_LIMIT));
        println!("CHILD-DONE");
        return;
    }
    let mut f = Fixture::new();
    for i in 0..SESSIONS {
        let mut t = Thread::new(i, "krill");
        if i % 4 == 0 {
            t = t.zst();
        }
        if i % 9 == 0 {
            t = t.archived();
        }
        f.add(t);
    }
    add_writer_locks(&f);
    let out = run_child(NAME, &f);
    assert!(out.contains("CHILD-DONE"), "{out}");
    for t in f.threads.iter().step_by(97) {
        assert_eq!(
            rollout_provider(&f.rollout_path(t)).as_deref(),
            Some("openai")
        );
        assert_eq!(
            row_provider(&f.state_db(), &t.id).as_deref(),
            Some("openai")
        );
    }
    f.assert_projection_consistent();
}

/// When no file can be opened any more, the error says so (kind, OS code and
/// the limit), never that a Codex process holds a lock; nothing is changed.
#[cfg(unix)]
#[tokio::test]
async fn too_many_open_files_is_reported_as_such() {
    const NAME: &str = "too_many_open_files_is_reported_as_such";
    const LIMIT: u64 = 64;
    if let Some((home, m)) = child_fixture() {
        let plan = m.plan(&home, &request()).await.unwrap();
        let token = ConsumersStopped::new(&home, Timestamp::now());
        set_soft_limit(LIMIT);
        // Use up every descriptor the limit allows.
        let mut hog = Vec::new();
        while let Ok(file) = fs::File::open(home.root()) {
            hog.push(file);
        }
        let err = m.migrate_stopped(&home, &plan, &token).await.unwrap_err();
        drop(hog);
        println!("CHILD-ERROR {err}");
        assert_eq!(err.code(), "io", "{err:?}");
        let Error::Io {
            kind,
            message,
            open_files_limit,
            ..
        } = &err
        else {
            panic!("{err:?}");
        };
        assert_eq!(kind, fdlimit::IO_KIND_TOO_MANY_OPEN_FILES);
        assert_eq!(*open_files_limit, Some(LIMIT));
        let emfile = rustix::io::Errno::MFILE.raw_os_error();
        assert!(message.contains(&format!("os error {emfile}")), "{message}");
        assert!(!err.to_string().contains("Codex"), "{err}");
        let zh = Catalog::new(Locale::ZhCn).text(&err.to_text());
        assert!(zh.starts_with("打开的文件太多（系统上限 64）"), "{zh}");
        let en = Catalog::new(Locale::En).text(&err.to_text());
        assert!(
            en.starts_with("Too many open files (system limit 64)"),
            "{en}"
        );
        // With descriptors back, the same plan runs.
        let receipt = m.migrate_stopped(&home, &plan, &token).await.unwrap();
        println!("CHILD-DONE {}", receipt.threads);
        return;
    }
    let mut f = Fixture::new();
    f.add(Thread::new(1, "krill"));
    f.add(Thread::new(2, "krill").zst());
    add_writer_locks(&f);
    let out = run_child(NAME, &f);
    assert!(out.contains("CHILD-DONE 2"), "{out}");
}

/// Wherever a migration runs out of descriptors (each step from the first open
/// on), the error names "too many open files", never a lock conflict, and the
/// home can be restored exactly.
#[cfg(unix)]
#[tokio::test]
async fn running_out_of_descriptors_at_any_step_is_never_a_lock_conflict() {
    const NAME: &str = "running_out_of_descriptors_at_any_step_is_never_a_lock_conflict";
    if let Some((home, m)) = child_fixture() {
        let token = ConsumersStopped::new(&home, Timestamp::now());
        set_soft_limit(64);
        let mut failures = 0;
        for spare in 0..40 {
            let plan = m.plan(&home, &request()).await.unwrap();
            let mut hog = Vec::new();
            while let Ok(file) = fs::File::open(home.root()) {
                hog.push(file);
            }
            hog.truncate(hog.len().saturating_sub(spare));
            let result = m.migrate_stopped(&home, &plan, &token).await;
            drop(hog);
            match result {
                Ok(receipt) => {
                    println!("CHILD-SUCCEEDED-WITH {spare} {}", receipt.threads);
                    m.rollback_stopped(&home, &plan.id, &token).await.unwrap();
                    break;
                }
                Err(err) => {
                    failures += 1;
                    println!("CHILD-FAILED-WITH {spare}: {err}");
                    assert_ne!(err.code(), "locked", "spare {spare}: {err}");
                    assert!(
                        err.to_string().contains("Too many open files"),
                        "spare {spare}: {err:?}"
                    );
                    // A rollback that itself ran out of descriptors left a
                    // pending ledger: recovery with descriptors restores it.
                    m.recover_stopped(&home, &token).await.unwrap();
                }
            }
        }
        assert!(failures > 0);
        println!("CHILD-DONE");
        return;
    }
    let mut f = Fixture::new();
    f.add(Thread::new(1, "krill"));
    f.add(Thread::new(2, "krill").zst());
    f.add(Thread::new(3, "krill").archived());
    add_writer_locks(&f);
    let before = f.snapshot();
    let out = run_child(NAME, &f);
    assert!(out.contains("CHILD-SUCCEEDED-WITH"), "{out}");
    f.snapshot().assert_same(&before);
}

/// A lock really held by another process is still a conflict: the error names
/// the session, its rollout file, the lock file and (Linux) the holder.
#[tokio::test]
async fn held_session_lock_names_the_session_file_and_holder() {
    const NAME: &str = "held_session_lock_names_the_session_file_and_holder";
    if let Some(lock) = std::env::var_os("SUPERBA_FD_CHILD_HOLD") {
        // Child: hold the lock until stdin closes.
        let file = fs::File::create(PathBuf::from(lock)).unwrap();
        file.lock().unwrap();
        println!("held");
        let mut sink = String::new();
        let _ = std::io::stdin().read_line(&mut sink);
        return;
    }
    let mut f = Fixture::new();
    f.add(Thread::new(1, "krill"));
    f.add(Thread::new(2, "krill").zst());
    add_writer_locks(&f);
    let lock = f
        .home
        .root()
        .join("thread-writer-locks")
        .join(format!("{}.lock", f.threads[1].id));
    let mut holder = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
        .env("SUPERBA_FD_CHILD_HOLD", &lock)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_line(&mut holder, "held");
    let m = f.migrator();
    let plan = m.plan(&f.home, &request()).await.unwrap();
    let before = f.snapshot();
    let token = ConsumersStopped::new(&f.home, Timestamp::now());
    let err = m.migrate_stopped(&f.home, &plan, &token).await.unwrap_err();
    // Only the process this test started, by PID.
    drop(holder.stdin.take());
    let _ = holder.wait();
    assert_eq!(err.code(), "locked", "{err:?}");
    let text = err.to_string();
    assert!(text.contains("a Codex process"), "{text}");
    assert!(text.contains(&f.threads[1].id), "{text}");
    assert!(
        text.contains(&f.rollout_path(&f.threads[1]).display().to_string()),
        "{text}"
    );
    assert!(text.contains(&lock.display().to_string()), "{text}");
    if cfg!(target_os = "linux") {
        assert!(text.contains(&format!("pid {}", holder.id())), "{text}");
    }
    f.snapshot().assert_same(&before);
    // Released: the same plan runs.
    m.migrate_stopped(&f.home, &plan, &token).await.unwrap();
}

/// A Codex that starts writing while sessions are migrated (Codex's protocol:
/// coordination lock, then the thread lock) waits until the migration is over
/// and then sees the finished state, never a half-migrated session.
#[tokio::test]
async fn codex_writer_waits_for_the_migration_and_sees_it_finished() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    let mut f = Fixture::new();
    for i in 0..40 {
        f.add(Thread::new(i, "krill"));
    }
    add_writer_locks(&f);
    let dir = f.home.root().join("thread-writer-locks");
    let target = f.threads[7].clone();
    let rollout = f.rollout_path(&target);
    let state_db = f.state_db();
    let blocked = Arc::new(AtomicBool::new(false));
    // (rollout provider, threads row provider) the writer saw.
    type Seen = Option<(Option<String>, Option<String>)>;
    let seen: Arc<Mutex<Seen>> = Arc::default();
    let writer: Arc<Mutex<Option<std::thread::JoinHandle<()>>>> = Arc::default();
    let hook = {
        let (blocked, seen, writer) = (blocked.clone(), seen.clone(), writer.clone());
        let (dir, rollout, state_db, id) = (
            dir.clone(),
            rollout.clone(),
            state_db.clone(),
            target.id.clone(),
        );
        move || {
            // Runs inside the migration, after its locks and checks.
            let coordination = fs::File::options()
                .read(true)
                .write(true)
                .open(dir.join(".coordination.lock"))
                .unwrap();
            let held = matches!(coordination.try_lock(), Err(fs::TryLockError::WouldBlock));
            blocked.store(held, Ordering::SeqCst);
            let handle = std::thread::spawn(move || {
                coordination.lock().unwrap();
                let thread_lock = fs::File::options()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(dir.join(format!("{id}.lock")))
                    .unwrap();
                thread_lock.try_lock().unwrap();
                *seen.lock().unwrap() =
                    Some((rollout_provider(&rollout), row_provider(&state_db, &id)));
            });
            *writer.lock().unwrap() = Some(handle);
        }
    };
    let plan = f.migrator().plan(&f.home, &request()).await.unwrap();
    let m = f.migrator().with_clock(AfterChecksClock::new(hook));
    let token = ConsumersStopped::new(&f.home, Timestamp::now());
    m.migrate_stopped(&f.home, &plan, &token).await.unwrap();
    let handle = writer.lock().unwrap().take().expect("the hook ran");
    handle.join().unwrap();
    assert!(
        blocked.load(Ordering::SeqCst),
        "the coordination lock was free during the migration"
    );
    let seen = seen.lock().unwrap().clone().unwrap();
    assert_eq!(seen, (Some("openai".to_owned()), Some("openai".to_owned())));
}

/// A failed migration in a home with Codex writer locks rolls back by itself
/// (it used to collide with the locks the migration itself held).
#[tokio::test]
async fn failed_migration_rolls_back_in_a_home_with_writer_locks() {
    use superba_sessions::{FaultPlan, FaultPoint};
    for point in [
        FaultPoint::AfterLedger,
        FaultPoint::AfterRollout(1),
        FaultPoint::AfterDatabase(0),
        FaultPoint::BeforeComplete,
    ] {
        let mut f = Fixture::new();
        f.add(Thread::new(1, "krill"));
        f.add(Thread::new(2, "krill").zst());
        f.add(Thread::new(3, "krill").archived());
        add_writer_locks(&f);
        let before = f.snapshot();
        let m = f.migrator().with_faults(FaultPlan {
            fail_at: Some(point),
            crash: false,
        });
        let plan = m.plan(&f.home, &request()).await.unwrap();
        let token = ConsumersStopped::new(&f.home, Timestamp::now());
        let err = m.migrate_stopped(&f.home, &plan, &token).await.unwrap_err();
        assert!(
            !err.to_string().contains("rolling back failed"),
            "{point:?}: {err}"
        );
        f.snapshot().assert_same(&before);
        assert_eq!(
            m.recover_stopped(&f.home, &token).await.unwrap(),
            RollbackReport::default(),
            "{point:?}"
        );
    }
}

/// Calls `hook` on the second `now()`: inside `migrate`, after the locks and
/// safety checks, before any write.
struct AfterChecksClock {
    time: Timestamp,
    calls: std::sync::atomic::AtomicUsize,
    hook: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl std::fmt::Debug for AfterChecksClock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AfterChecksClock").finish_non_exhaustive()
    }
}

impl AfterChecksClock {
    fn new(hook: impl FnOnce() + Send + 'static) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            time: Timestamp::now(),
            calls: std::sync::atomic::AtomicUsize::new(0),
            hook: std::sync::Mutex::new(Some(Box::new(hook))),
        })
    }
}

impl Clock for AfterChecksClock {
    fn now(&self) -> Timestamp {
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1
            && let Some(hook) = self.hook.lock().unwrap().take()
        {
            hook();
        }
        self.time
    }

    fn monotonic(&self) -> std::time::Duration {
        std::time::Duration::ZERO
    }
}

fn wait_for_line(child: &mut std::process::Child, want: &str) {
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(child.stdout.as_mut().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert!(reader.read_line(&mut line).unwrap() > 0, "child exited");
        // libtest may print `test <name> ... ` on the same line first.
        if line.trim_end().ends_with(want) {
            return;
        }
    }
}
