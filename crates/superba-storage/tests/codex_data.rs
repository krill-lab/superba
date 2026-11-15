//! Codex data location (GitHub issue 15): temp homes only, links made by the
//! production code (symlinks here, junctions on Windows CI).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use superba_core::data::{CodexDirPlacement, DataMovePhase, DataMoveProgressSink};
use superba_core::{CodexHome, Error, LocalContext, MapEnv};
use superba_storage::codex_data::{self, CodexDataFs, JOURNAL_FILE, Recovered, Scope};

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    ctx: LocalContext,
    home: CodexHome,
    fs: CodexDataFs,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = superba_core::paths::canonical(temp.path()).unwrap();
        let mut ctx = LocalContext::for_tests(&root);
        ctx.env = Arc::new(MapEnv::new());
        fs::create_dir_all(ctx.data_dir.root()).unwrap();
        let home = ctx.codex_home();
        fs::create_dir_all(home.root()).unwrap();
        fs::write(home.config_toml(), "model = \"gpt-5\"\n").unwrap();
        let day = home.sessions_dir().join("2026/11/16");
        fs::create_dir_all(&day).unwrap();
        fs::write(
            day.join("rollout-2026-11-16T10-00-00-a.jsonl"),
            b"{\"a\":1}\n",
        )
        .unwrap();
        fs::write(
            day.join("rollout-2026-11-16T11-00-00-b.jsonl"),
            vec![b'x'; 70_000],
        )
        .unwrap();
        fs::create_dir_all(home.archived_sessions_dir()).unwrap();
        fs::write(
            home.archived_sessions_dir()
                .join("rollout-2026-01-01T00-00-00-c.jsonl"),
            "会话\n",
        )
        .unwrap();
        Self {
            _temp: temp,
            root,
            ctx,
            home,
            fs: CodexDataFs::default(),
        }
    }

    fn scope(&self) -> Scope<'_> {
        Scope {
            home: &self.home,
            data: &self.ctx.data_dir,
            env: self.ctx.env.as_ref(),
            fs: &self.fs,
        }
    }

    fn dest(&self, name: &str) -> PathBuf {
        self.root.join("disk-d").join(name)
    }

    fn rollout(&self) -> PathBuf {
        self.home
            .sessions_dir()
            .join("2026/11/16/rollout-2026-11-16T10-00-00-a.jsonl")
    }
}

fn quiet() -> DataMoveProgressSink {
    Arc::new(|_| true)
}

fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| superba_storage::validation::is_link(&m))
}

fn leftovers_in(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".superba-codex"))
        .collect()
}

#[test]
fn move_out_links_both_folders_and_codex_paths_stay_valid() {
    let f = Fixture::new();
    let dest = f.dest("Codex 数据");
    let plan = codex_data::plan(f.scope(), Some(&dest)).unwrap();
    assert!(plan.ready(), "{:?}", plan.blockers);
    assert_eq!(plan.moves.len(), 2);
    assert_eq!(plan.total_bytes, 8 + 70_000 + "会话\n".len() as u64);
    let report = codex_data::execute(f.scope(), Some(&dest), &quiet()).unwrap();
    assert!(report.leftovers.is_empty(), "{:?}", report.leftovers);
    assert_eq!(report.files, 3);
    for dir in [f.home.sessions_dir(), f.home.archived_sessions_dir()] {
        assert!(is_link(&dir), "{}", dir.display());
    }
    // The path Codex recorded still opens the same bytes, now on the other disk.
    assert_eq!(fs::read(f.rollout()).unwrap(), b"{\"a\":1}\n");
    assert!(
        dest.join("sessions/2026/11/16/rollout-2026-11-16T10-00-00-a.jsonl")
            .is_file()
    );
    assert_eq!(
        fs::read_to_string(dest.join("archived_sessions/rollout-2026-01-01T00-00-00-c.jsonl"))
            .unwrap(),
        "会话\n"
    );
    // Old copies, staging and the journal are gone; the rest of the home stays.
    assert!(leftovers_in(f.home.root()).is_empty());
    assert!(
        leftovers_in(&dest)
            .iter()
            .all(|n| n == ".superba-codex-data")
    );
    assert!(!f.ctx.data_dir.root().join(JOURNAL_FILE).exists());
    assert!(f.home.config_toml().is_file());
    let location = codex_data::inspect(&f.home, &f.ctx.data_dir);
    assert_eq!(location.directory.as_deref(), Some(dest.as_path()));
    assert!(!location.unreachable());
    for dir in &location.dirs {
        assert!(
            matches!(
                dir.placement,
                CodexDirPlacement::Linked {
                    managed: true,
                    reachable: true,
                    ..
                }
            ),
            "{dir:?}"
        );
    }
    assert_eq!(location.dirs[0].files, 2);
    // A new rollout written through the link lands on the other disk, and
    // archiving (a plain rename between the two folders) keeps working.
    let new = f.home.sessions_dir().join("2026/11/16/rollout-new.jsonl");
    fs::write(&new, b"new").unwrap();
    fs::rename(
        &new,
        f.home.archived_sessions_dir().join("rollout-new.jsonl"),
    )
    .unwrap();
    assert!(dest.join("archived_sessions/rollout-new.jsonl").is_file());
    // Storage use counts what the links point to.
    let usage = superba_storage::usage(&f.ctx);
    assert_eq!(usage.codex_sessions_bytes, plan.total_bytes + 3);
}

#[test]
fn moving_back_restores_real_folders_and_removes_the_other_copy() {
    let f = Fixture::new();
    let dest = f.dest("codex");
    codex_data::execute(f.scope(), Some(&dest), &quiet()).unwrap();
    let plan = codex_data::plan(f.scope(), None).unwrap();
    assert!(plan.ready(), "{:?}", plan.blockers);
    let report = codex_data::execute(f.scope(), None, &quiet()).unwrap();
    assert!(report.leftovers.is_empty(), "{:?}", report.leftovers);
    for dir in [f.home.sessions_dir(), f.home.archived_sessions_dir()] {
        assert!(!is_link(&dir) && dir.is_dir(), "{}", dir.display());
    }
    assert_eq!(fs::read(f.rollout()).unwrap(), b"{\"a\":1}\n");
    // The folder Superba made holds nothing anymore and is removed.
    assert!(!dest.exists());
    let location = codex_data::inspect(&f.home, &f.ctx.data_dir);
    assert_eq!(location.directory, None);
    assert!(
        location
            .dirs
            .iter()
            .all(|d| d.placement == CodexDirPlacement::InHome)
    );
    // Nothing is left to move back.
    let again = codex_data::plan(f.scope(), None).unwrap();
    assert_eq!(again.blockers[0].key, "data.codex.not_moved");
}

#[test]
fn moving_between_two_other_folders_switches_the_links() {
    let f = Fixture::new();
    let first = f.dest("one");
    let second = f.root.join("disk-e").join("two");
    codex_data::execute(f.scope(), Some(&first), &quiet()).unwrap();
    codex_data::execute(f.scope(), Some(&second), &quiet()).unwrap();
    assert_eq!(fs::read(f.rollout()).unwrap(), b"{\"a\":1}\n");
    assert!(second.join("sessions/2026/11/16").is_dir());
    assert!(!first.exists(), "the first folder only held Superba's data");
    assert_eq!(
        superba_core::paths::canonical(f.home.sessions_dir()).unwrap(),
        second.join("sessions")
    );
}

#[test]
fn insufficient_space_blocks_and_changes_nothing() {
    let mut f = Fixture::new();
    f.fs.available = Arc::new(|_| Ok(1024));
    let dest = f.dest("small");
    let plan = codex_data::plan(f.scope(), Some(&dest)).unwrap();
    assert_eq!(plan.available_bytes, Some(1024));
    assert!(
        plan.blockers.iter().any(|b| b.key == "data.path.space"),
        "{:?}",
        plan.blockers
    );
    let error = codex_data::execute(f.scope(), Some(&dest), &quiet()).unwrap_err();
    assert_eq!(error.detail(), "data.path.space");
    assert!(!dest.exists());
    assert!(!is_link(&f.home.sessions_dir()));
    assert!(!f.ctx.data_dir.root().join(JOURNAL_FILE).exists());
}

#[test]
fn cancelling_the_copy_keeps_the_old_data_and_removes_the_partial_copy() {
    let f = Fixture::new();
    let dest = f.dest("cancelled");
    let progress: DataMoveProgressSink = Arc::new(|p| p.phase != DataMovePhase::Copying);
    let error = codex_data::execute(f.scope(), Some(&dest), &progress).unwrap_err();
    assert_eq!(error, Error::Cancelled);
    assert!(!dest.exists(), "a folder the move created is removed again");
    assert!(!is_link(&f.home.sessions_dir()));
    assert_eq!(fs::read(f.rollout()).unwrap(), b"{\"a\":1}\n");
    assert!(leftovers_in(f.home.root()).is_empty());
    assert!(!f.ctx.data_dir.root().join(JOURNAL_FILE).exists());
}

#[test]
fn a_failed_link_rolls_back_before_the_codex_home_changes() {
    let mut f = Fixture::new();
    f.fs.link = Arc::new(|_, _| Err(io::Error::from(io::ErrorKind::PermissionDenied)));
    let dest = f.dest("nolinks");
    let error = codex_data::execute(f.scope(), Some(&dest), &quiet()).unwrap_err();
    assert_eq!(error.detail(), "data.codex.links");
    assert!(!dest.exists());
    assert!(f.home.sessions_dir().is_dir() && !is_link(&f.home.sessions_dir()));
    assert_eq!(fs::read(f.rollout()).unwrap(), b"{\"a\":1}\n");
}

#[test]
fn a_crash_while_switching_is_rolled_back_at_the_next_start() {
    let mut f = Fixture::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    // The process dies after the copies were published and one link exists.
    f.fs.link = Arc::new(move |target, link| {
        if seen.fetch_add(1, Ordering::SeqCst) == 1 {
            panic!("simulated crash");
        }
        codex_data::create_dir_link(target, link)
    });
    let dest = f.dest("crash");
    let scope = f.scope();
    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        codex_data::execute(scope, Some(&dest), &quiet())
    }));
    assert!(crashed.is_err());
    assert!(f.ctx.data_dir.root().join(JOURNAL_FILE).exists());
    assert!(codex_data::inspect(&f.home, &f.ctx.data_dir).pending);
    assert_eq!(
        codex_data::plan(f.scope(), Some(&f.dest("other")))
            .unwrap()
            .blockers[0]
            .key,
        "data.codex.pending"
    );
    assert_eq!(
        codex_data::recover(&f.ctx.data_dir).unwrap(),
        Some(Recovered::RolledBack)
    );
    assert!(!dest.exists());
    assert!(leftovers_in(f.home.root()).is_empty());
    assert!(!is_link(&f.home.sessions_dir()));
    assert_eq!(fs::read(f.rollout()).unwrap(), b"{\"a\":1}\n");
    assert_eq!(codex_data::recover(&f.ctx.data_dir).unwrap(), None);
}

#[test]
fn a_crash_after_the_commit_finishes_the_cleanup_at_the_next_start() {
    let f = Fixture::new();
    let dest = f.dest("committed");
    let progress: DataMoveProgressSink = Arc::new(|p| {
        if p.phase == DataMovePhase::Cleaning {
            panic!("simulated crash");
        }
        true
    });
    let scope = f.scope();
    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        codex_data::execute(scope, Some(&dest), &progress)
    }));
    assert!(crashed.is_err());
    assert_eq!(
        leftovers_in(f.home.root()).len(),
        2,
        "old copies renamed aside"
    );
    assert_eq!(
        codex_data::recover(&f.ctx.data_dir).unwrap(),
        Some(Recovered::Completed { leftovers: vec![] })
    );
    assert!(leftovers_in(f.home.root()).is_empty());
    assert!(is_link(&f.home.sessions_dir()));
    assert_eq!(fs::read(f.rollout()).unwrap(), b"{\"a\":1}\n");
}

#[test]
fn an_old_copy_that_changed_after_the_copy_is_kept() {
    let f = Fixture::new();
    let dest = f.dest("changed");
    let home = f.home.root().to_path_buf();
    // Something writes into the old folder after the switch (e.g. a Codex
    // that kept a file open): the old copy no longer matches and stays.
    let progress: DataMoveProgressSink = Arc::new(move |p| {
        if p.phase == DataMovePhase::Cleaning {
            let aside = fs::read_dir(&home)
                .unwrap()
                .flatten()
                .find(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    n.starts_with(".superba-codex-old-") && n.ends_with("-archived_sessions")
                })
                .unwrap()
                .path();
            fs::write(aside.join("late.jsonl"), b"late").unwrap();
        }
        true
    });
    let report = codex_data::execute(f.scope(), Some(&dest), &progress).unwrap();
    assert_eq!(report.leftovers.len(), 1, "{:?}", report.leftovers);
    assert!(report.leftovers[0].join("late.jsonl").is_file());
    assert_eq!(
        codex_data::inspect(&f.home, &f.ctx.data_dir).leftovers,
        report.leftovers
    );
}

#[test]
fn a_fresh_home_gets_links_to_empty_folders() {
    let temp = tempfile::tempdir().unwrap();
    let root = superba_core::paths::canonical(temp.path()).unwrap();
    let ctx = LocalContext::for_tests(&root);
    fs::create_dir_all(ctx.data_dir.root()).unwrap();
    let home = ctx.codex_home();
    fs::create_dir_all(home.root()).unwrap();
    let fs_ops = CodexDataFs::default();
    let scope = Scope {
        home: &home,
        data: &ctx.data_dir,
        env: ctx.env.as_ref(),
        fs: &fs_ops,
    };
    let dest = root.join("d").join("codex");
    codex_data::execute(scope, Some(&dest), &quiet()).unwrap();
    assert!(is_link(&home.sessions_dir()));
    fs::create_dir_all(home.sessions_dir().join("2026")).unwrap();
    assert!(dest.join("sessions/2026").is_dir());
}

#[test]
fn unsafe_destinations_and_foreign_links_are_refused() {
    let f = Fixture::new();
    let blockers = |dest: &Path| -> Vec<String> {
        codex_data::plan(f.scope(), Some(dest))
            .unwrap()
            .blockers
            .into_iter()
            .map(|b| b.key)
            .collect()
    };
    assert!(blockers(&f.home.root().join("inside")).contains(&"data.codex.overlap".to_owned()));
    assert!(blockers(&f.ctx.data_dir.root().join("x")).contains(&"data.codex.overlap".to_owned()));
    assert!(blockers(&f.dest("sessions")).contains(&"data.codex.name".to_owned()));
    let full = f.dest("full");
    fs::create_dir_all(&full).unwrap();
    fs::write(full.join("mine.txt"), b"keep").unwrap();
    assert!(blockers(&full).contains(&"data.path.not_empty".to_owned()));
    assert!(blockers(Path::new("relative/dir")).contains(&"data.path.absolute".to_owned()));
    // A link Superba did not make is left alone.
    let elsewhere = f.root.join("elsewhere");
    fs::rename(f.home.archived_sessions_dir(), &elsewhere).unwrap();
    codex_data::create_dir_link(&elsewhere, &f.home.archived_sessions_dir()).unwrap();
    assert!(blockers(&f.dest("ok")).contains(&"data.codex.unmanaged".to_owned()));
    assert!(fs::read_dir(&elsewhere).unwrap().next().is_some());
}

#[test]
fn an_unplugged_drive_is_reported_and_blocks_moves() {
    let f = Fixture::new();
    let dest = f.dest("usb");
    codex_data::execute(f.scope(), Some(&dest), &quiet()).unwrap();
    // Simulate the drive going away.
    let parked = f.root.join("parked");
    fs::rename(&dest, &parked).unwrap();
    let location = codex_data::inspect(&f.home, &f.ctx.data_dir);
    assert!(location.unreachable());
    let blockers = codex_data::plan(f.scope(), None).unwrap().blockers;
    assert_eq!(blockers[0].key, "data.codex.unreachable");
    // Back again: everything is where it was.
    fs::rename(&parked, &dest).unwrap();
    assert!(!codex_data::inspect(&f.home, &f.ctx.data_dir).unreachable());
    assert_eq!(fs::read(f.rollout()).unwrap(), b"{\"a\":1}\n");
}

#[test]
fn the_marker_keeps_links_recognised_without_superbas_record() {
    let f = Fixture::new();
    let dest = f.dest("marked");
    codex_data::execute(f.scope(), Some(&dest), &quiet()).unwrap();
    fs::remove_file(f.ctx.data_dir.root().join(codex_data::RECORD_FILE)).unwrap();
    let location = codex_data::inspect(&f.home, &f.ctx.data_dir);
    assert_eq!(location.directory.as_deref(), Some(dest.as_path()));
    assert!(codex_data::plan(f.scope(), None).unwrap().ready());
}
