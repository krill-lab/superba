//! Progress reports of the writing operations and stopping a migration
//! (`migrate_stopped_with_progress`, `rollback_stopped_with_progress`).

#![allow(missing_docs)]

mod common;

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use common::{Fixture, Thread};
use superba_core::*;

/// Which reports make the recorder ask to stop.
type StopWhen = Box<dyn Fn(&SessionProgress) -> bool + Send + Sync>;

/// Records every report; asks to stop once `stop_when` matches a report,
/// after `stop_after` more polls (the engine polls before each file).
struct Recorder {
    reports: Mutex<Vec<SessionProgress>>,
    stop_when: Option<StopWhen>,
    stop_after: u32,
    polls: AtomicU32,
}

impl Recorder {
    fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            reports: Mutex::new(Vec::new()),
            stop_when: None,
            stop_after: 0,
            polls: AtomicU32::new(0),
        })
    }

    fn stopping(
        when: impl Fn(&SessionProgress) -> bool + Send + Sync + 'static,
    ) -> std::sync::Arc<Self> {
        Self::stopping_after(when, 0)
    }

    fn stopping_after(
        when: impl Fn(&SessionProgress) -> bool + Send + Sync + 'static,
        polls: u32,
    ) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            reports: Mutex::new(Vec::new()),
            stop_when: Some(Box::new(when)),
            stop_after: polls,
            polls: AtomicU32::new(0),
        })
    }

    fn reports(&self) -> Vec<SessionProgress> {
        self.reports.lock().unwrap().clone()
    }
}

impl SessionProgressSink for Recorder {
    fn report(&self, progress: SessionProgress) {
        self.reports.lock().unwrap().push(progress);
    }

    fn stop_requested(&self) -> bool {
        let Some(when) = &self.stop_when else {
            return false;
        };
        if !self.reports.lock().unwrap().iter().any(when) {
            return false;
        }
        self.polls.fetch_add(1, Ordering::SeqCst) >= self.stop_after
    }
}

fn build(n: u32) -> Fixture {
    let mut f = Fixture::new();
    for i in 0..n {
        let mut t = Thread::new(i, if i % 9 == 0 { "openai" } else { "krill" });
        if i % 4 == 0 {
            t = t.zst();
        }
        if i % 11 == 0 {
            t = t.archived();
        }
        if i % 13 == 0 {
            t = t.no_rollout();
        }
        f.add(t);
    }
    f
}

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

fn stopped(f: &Fixture) -> ConsumersStopped {
    ConsumersStopped::new(&f.home, Timestamp::now())
}

fn stage_index(s: SessionStage) -> usize {
    [
        SessionStage::Reading,
        SessionStage::BackingUp,
        SessionStage::Moving,
        SessionStage::Databases,
        SessionStage::Verifying,
        SessionStage::Done,
    ]
    .iter()
    .position(|x| *x == s)
    .unwrap()
}

/// Done, total, stages and the overall fraction only move forward, and the
/// last report is `Done` with every session counted.
fn assert_forward(reports: &[SessionProgress], direction: SessionDirection, total: u32) {
    assert!(!reports.is_empty(), "no reports");
    let restore_order = |s: SessionStage| match s {
        SessionStage::Reading => 0,
        SessionStage::Databases => 1,
        SessionStage::Moving => 2,
        _ => 3,
    };
    for pair in reports.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        assert_eq!(b.direction, direction);
        assert_eq!(a.total, b.total, "total never changes");
        assert!(b.done >= a.done, "done never decreases: {a:?} -> {b:?}");
        let order = |s| match direction {
            SessionDirection::Migrate => stage_index(s),
            SessionDirection::Restore => restore_order(s),
        };
        assert!(
            order(b.stage) >= order(a.stage),
            "stages advance: {a:?} -> {b:?}"
        );
        assert!(
            b.fraction() + 1e-6 >= a.fraction(),
            "fraction never decreases: {a:?} -> {b:?}"
        );
        assert!(b.step_done <= b.step_total.max(b.step_done));
    }
    let last = reports.last().unwrap();
    assert_eq!(last.stage, SessionStage::Done);
    assert_eq!(last.done, total);
    assert_eq!(last.total, total);
    assert_eq!(last.fraction(), 1.0);
}

#[tokio::test]
async fn migration_reports_forward_progress_and_ends_done() {
    let f = build(120);
    let m = f.migrator();
    let plan = m.plan(&f.home, &request()).await.unwrap();
    let total = plan.items.len() as u32;
    let rec = Recorder::new();
    let receipt = m
        .migrate_stopped_with_progress(&f.home, &plan, &stopped(&f), rec.clone())
        .await
        .unwrap();
    assert_eq!(receipt.threads, total);
    let reports = rec.reports();
    assert_forward(&reports, SessionDirection::Migrate, total);
    // Every stage of a migration is reported, in order.
    let mut stages: Vec<SessionStage> = reports.iter().map(|r| r.stage).collect();
    stages.dedup();
    assert_eq!(
        stages,
        [
            SessionStage::Reading,
            SessionStage::BackingUp,
            SessionStage::Moving,
            SessionStage::Databases,
            SessionStage::Verifying,
            SessionStage::Done
        ]
    );
    // Stopping is offered only before the databases are written.
    for r in &reports {
        let early = matches!(
            r.stage,
            SessionStage::Reading | SessionStage::BackingUp | SessionStage::Moving
        );
        assert_eq!(r.can_stop, early, "{r:?}");
    }
    // The session being worked on is named, by title and file.
    assert!(reports.iter().any(|r| {
        r.current.as_ref().is_some_and(|c| {
            c.title
                .as_deref()
                .is_some_and(|t| t.contains("fix the build"))
                && c.file.is_some()
        })
    }));
    // A new stage starts without the previous stage's file.
    for pair in reports.windows(2) {
        if pair[0].stage != pair[1].stage {
            assert_eq!(pair[1].current, None, "{:?}", pair[1]);
        }
    }
    // Throttled: far fewer reports than files touched.
    assert!(
        reports.len() < 4 * total as usize,
        "{} reports",
        reports.len()
    );
}

#[tokio::test]
async fn stopping_while_moving_puts_every_session_back() {
    let f = build(60);
    let before = f.snapshot();
    let m = f.migrator();
    let plan = m.plan(&f.home, &request()).await.unwrap();
    let total = plan.items.len() as u32;
    let rec = Recorder::stopping_after(|p| p.stage == SessionStage::Moving, 12);
    let err = m
        .migrate_stopped_with_progress(&f.home, &plan, &stopped(&f), rec.clone())
        .await
        .unwrap_err();
    assert_eq!(err, Error::Cancelled);
    f.snapshot().assert_same(&before);
    let reports = rec.reports();
    let split = reports
        .iter()
        .position(|r| r.direction == SessionDirection::Restore)
        .expect("the undo reports its progress");
    // The migration got under way, then stopped.
    assert!(
        reports[..split]
            .iter()
            .any(|r| r.stage == SessionStage::Moving)
    );
    assert!(
        reports[..split]
            .iter()
            .all(|r| r.stage != SessionStage::Databases)
    );
    // The undo never offers to stop and ends done.
    assert!(reports[split..].iter().all(|r| !r.can_stop));
    let restore = &reports[split..];
    assert_eq!(restore.last().unwrap().stage, SessionStage::Done);
    // Twelve rollouts had been rewritten: the undo put them back.
    assert!(
        restore
            .iter()
            .any(|r| r.stage == SessionStage::Moving && r.step_total >= 12),
        "{restore:?}"
    );
    assert!(restore.last().unwrap().done >= restore.first().unwrap().done);
    assert!(total > 0);
    // Nothing is left to recover and the attempt is not history.
    assert_eq!(m.recover(&f.home).await.unwrap(), RollbackReport::default());
    assert!(m.history(&f.home).await.unwrap().is_empty());
}

#[tokio::test]
async fn stopping_while_backing_up_writes_nothing() {
    let f = build(40);
    let before = f.snapshot();
    let m = f.migrator();
    let plan = m.plan(&f.home, &request()).await.unwrap();
    let rec = Recorder::stopping(|p| p.stage == SessionStage::BackingUp);
    let err = m
        .migrate_stopped_with_progress(&f.home, &plan, &stopped(&f), rec.clone())
        .await
        .unwrap_err();
    assert_eq!(err, Error::Cancelled);
    f.snapshot().assert_same(&before);
    assert!(!plan.backup_dir.exists(), "the partial backup is removed");
    assert!(
        rec.reports()
            .iter()
            .all(|r| r.direction == SessionDirection::Migrate),
        "nothing to undo"
    );
}

#[tokio::test]
async fn rollback_reports_restore_progress_and_cannot_be_stopped() {
    let f = build(50);
    let before = f.snapshot();
    let m = f.migrator();
    let plan = m.plan(&f.home, &request()).await.unwrap();
    m.migrate(&f.home, &plan).await.unwrap();
    // Asks to stop all along: a restore ignores it.
    let rec = Recorder::stopping(|_| true);
    let report = m
        .rollback_stopped_with_progress(&f.home, &plan.id, &stopped(&f), rec.clone())
        .await
        .unwrap();
    assert!(report.restored_threads > 0);
    f.snapshot().assert_same(&before);
    let reports = rec.reports();
    let total = reports.first().unwrap().total;
    assert_eq!(total, plan.items.len() as u32);
    assert_forward(&reports, SessionDirection::Restore, total);
    assert!(reports.iter().all(|r| !r.can_stop));
    // Titles come from Codex's database.
    assert!(reports.iter().any(|r| {
        r.current
            .as_ref()
            .and_then(|c| c.title.as_deref())
            .is_some_and(|t| t.contains("fix the build"))
    }));
}

#[tokio::test]
async fn plain_migrate_reports_nothing_and_is_unchanged() {
    let f = build(10);
    let m = f.migrator();
    let plan = m.plan(&f.home, &request()).await.unwrap();
    let receipt = m.migrate(&f.home, &plan).await.unwrap();
    assert_eq!(receipt.threads, plan.items.len() as u32);
}
