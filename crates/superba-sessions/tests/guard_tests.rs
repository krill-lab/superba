//! Safety checks before writes: live Codex processes, Codex thread writer locks,
//! the migrate lock, locks of other tools and the processes-stopped token.

#![allow(missing_docs)]

mod common;

use std::fs;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use common::{Fixture, Thread};
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

fn fixture() -> Fixture {
    let mut f = Fixture::new();
    f.add(Thread::new(1, "krill"));
    f.add(Thread::new(2, "krill").zst());
    f
}

/// A child process whose executable is named `codex` (a copy of `sleep`).
#[cfg(target_os = "linux")]
struct FakeCodex(std::process::Child);

#[cfg(target_os = "linux")]
impl FakeCodex {
    fn spawn(f: &Fixture, codex_home: &std::path::Path) -> Self {
        use std::os::unix::process::CommandExt;
        let bin = f.tmp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("codex");
        if !exe.exists() {
            let sleep = ["/bin/sleep", "/usr/bin/sleep"]
                .into_iter()
                .find(|p| std::path::Path::new(p).exists())
                .expect("sleep binary");
            fs::copy(sleep, &exe).unwrap();
        }
        // Another test thread forking while the copy's write fd is still open
        // makes exec fail with ETXTBSY for a moment: retry briefly.
        let mut attempts = 0;
        let child = loop {
            attempts += 1;
            match std::process::Command::new(&exe)
                .arg0("sleep")
                .arg("60")
                .env_clear()
                .env("HOME", &f.home_dir)
                .env("CODEX_HOME", codex_home)
                .spawn()
            {
                Ok(child) => break child,
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempts < 40 => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!("cannot start the fake codex: {e}"),
            }
        };
        // Let the kernel publish the new process image.
        std::thread::sleep(Duration::from_millis(200));
        FakeCodex(child)
    }
}

#[cfg(target_os = "linux")]
impl Drop for FakeCodex {
    fn drop(&mut self) {
        // Only the process this test started, by PID.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn live_codex_process_blocks_unless_the_caller_vouches() {
    let f = fixture();
    let m = f.migrator();
    let plan = m.plan(&f.home, &request()).await.unwrap();
    let before = f.snapshot();
    let fake = FakeCodex::spawn(&f, f.home.root());
    let err = m.migrate(&f.home, &plan).await.unwrap_err();
    assert_eq!(err.code(), "locked", "{err}");
    assert!(err.to_string().contains("codex"), "{err}");
    f.snapshot().assert_same(&before);
    assert!(!plan.backup_dir.exists());

    // The caller listed and stopped the consumers itself.
    let token = ConsumersStopped::new(&f.home, Timestamp::now());
    let receipt = m.migrate_stopped(&f.home, &plan, &token).await.unwrap();
    assert_eq!(receipt.rollouts, 2);
    // Rollback is guarded the same way.
    let err = m.rollback(&f.home, &plan.id).await.unwrap_err();
    assert_eq!(err.code(), "locked");
    m.rollback_stopped(&f.home, &plan.id, &token).await.unwrap();
    drop(fake);
    // Without the process the plain methods work again.
    let plan = m.plan(&f.home, &request()).await.unwrap();
    m.migrate(&f.home, &plan).await.unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn codex_process_of_another_home_is_ignored() {
    let f = fixture();
    let elsewhere = f.tmp.path().join("other-codex-home");
    fs::create_dir_all(&elsewhere).unwrap();
    let _fake = FakeCodex::spawn(&f, &elsewhere);
    let m = f.migrator();
    let plan = m.plan(&f.home, &request()).await.unwrap();
    m.migrate(&f.home, &plan).await.unwrap();
}

#[tokio::test]
async fn held_thread_writer_lock_blocks_even_with_a_token() {
    let f = fixture();
    let dir = f.home.root().join("thread-writer-locks");
    fs::create_dir_all(&dir).unwrap();
    // A stale lock file (nobody holds it) is fine.
    fs::write(dir.join(".coordination.lock"), b"").unwrap();
    let path = dir.join(format!("{}.lock", f.threads[0].id));
    let holder = fs::File::create(&path).unwrap();
    holder.lock().unwrap();
    let m = f.migrator();
    let plan = m.plan(&f.home, &request()).await.unwrap();
    let token = ConsumersStopped::new(&f.home, Timestamp::now());
    let err = m.migrate_stopped(&f.home, &plan, &token).await.unwrap_err();
    assert_eq!(err.code(), "locked");
    assert!(err.to_string().contains(&f.threads[0].id), "{err}");
    holder.unlock().unwrap();
    drop(holder);
    m.migrate_stopped(&f.home, &plan, &token).await.unwrap();
}

#[tokio::test]
async fn second_migrator_waits_for_the_migrate_lock() {
    let f = fixture();
    let lock_path = f.home.migrate_lock();
    fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    let held = fs::File::create(&lock_path).unwrap();
    held.lock().unwrap();
    let m = f.migrator();
    let plan = m.plan(&f.home, &request()).await.unwrap();
    let err = m.migrate(&f.home, &plan).await.unwrap_err();
    assert_eq!(err.code(), "locked");
    assert_eq!(m.recover(&f.home).await.unwrap(), RollbackReport::default());
    drop(held);
    m.migrate(&f.home, &plan).await.unwrap();
}

#[tokio::test]
async fn fresh_lock_of_another_tool_blocks_and_stale_one_does_not() {
    let f = fixture();
    let lock = f.home.tmp_dir().join("provider-sync.lock");
    fs::create_dir_all(f.home.tmp_dir()).unwrap();
    fs::write(&lock, b"{}").unwrap();
    let m = f.migrator();
    let plan = m.plan(&f.home, &request()).await.unwrap();
    let err = m.migrate(&f.home, &plan).await.unwrap_err();
    assert_eq!(err.code(), "locked");
    let old = SystemTime::now() - Duration::from_secs(2 * 3600);
    fs::File::options()
        .write(true)
        .open(&lock)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(old))
        .unwrap();
    m.migrate(&f.home, &plan).await.unwrap();
}

#[tokio::test]
async fn tokens_must_match_the_home_and_be_recent() {
    let f = fixture();
    let clock = Arc::new(ManualClock::new(Timestamp(1_800_000_000_000)));
    let m = f.migrator().with_clock(clock.clone());
    let plan = m.plan(&f.home, &request()).await.unwrap();
    let other = ConsumersStopped::new(&CodexHome::new("/elsewhere/.codex"), clock.now());
    assert_eq!(
        m.migrate_stopped(&f.home, &plan, &other)
            .await
            .unwrap_err()
            .code(),
        "invalid"
    );
    let stale = ConsumersStopped::new(&f.home, clock.now());
    clock.advance(Duration::from_secs(20 * 60));
    assert_eq!(
        m.migrate_stopped(&f.home, &plan, &stale)
            .await
            .unwrap_err()
            .code(),
        "invalid"
    );
    let fresh = ConsumersStopped::new(&f.home, clock.now());
    m.migrate_stopped(&f.home, &plan, &fresh).await.unwrap();
    let receipts = m.history(&f.home).await.unwrap();
    assert_eq!(receipts[0].migrated_at, clock.now());
}

/// Any process holding the home's SQLite database open (here a Python sqlite3
/// connection, not named codex) is found through `/proc/locks`.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn process_holding_the_database_open_blocks() {
    let Some(python) = ["/usr/bin/python3", "/bin/python3"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
    else {
        eprintln!("python3 not found; skipped");
        return;
    };
    let f = fixture();
    let m = f.migrator();
    let plan = m.plan(&f.home, &request()).await.unwrap();
    let mut child = std::process::Command::new(python)
        .arg("-I")
        .arg("-c")
        .arg(
            "import sqlite3,sys,time\n\
             c=sqlite3.connect(sys.argv[1])\n\
             c.execute('select count(*) from threads').fetchall()\n\
             print('ready', flush=True)\n\
             time.sleep(60)",
        )
        .arg(f.state_db())
        .env_clear()
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::BufRead;
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.as_mut().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(line.trim(), "ready");
    }
    let err = m.migrate(&f.home, &plan).await.unwrap_err();
    assert_eq!(err.code(), "locked", "{err}");
    assert!(err.to_string().contains(&child.id().to_string()), "{err}");
    // Only the process this test started, by PID.
    child.kill().unwrap();
    child.wait().unwrap();
    m.migrate(&f.home, &plan).await.unwrap();
}
