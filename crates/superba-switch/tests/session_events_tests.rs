//! The session migration counter names its stage and agrees with the overall
//! percentage (`session_events`, shared by the GUI dialog and `spb`).

#![allow(missing_docs)]

use pretty_assertions::assert_eq;
use superba_core::{Locale, SessionCurrent, SessionDirection, SessionProgress, SessionStage};
use superba_switch::messages;
use superba_switch::session_events::{session_count, session_percent, session_progress_line};

fn report(
    direction: SessionDirection,
    stage: SessionStage,
    (step_done, step_total): (u32, u32),
    done: u32,
) -> SessionProgress {
    SessionProgress {
        direction,
        stage,
        done,
        total: 600,
        step_done,
        step_total,
        current: None,
        can_stop: false,
    }
}

fn render(locale: Locale, p: &SessionProgress) -> String {
    messages::catalog(locale).text(&session_progress_line(p))
}

/// A migration of 600 sessions (601 files to back up: 600 rollouts and a
/// database; the Moving stage counts files 1:1 but a session without a
/// rollout is counted only when the databases are done).
fn migration() -> Vec<SessionProgress> {
    use SessionDirection::Migrate;
    use SessionStage::*;
    vec![
        report(Migrate, Reading, (0, 600), 0),
        report(Migrate, Reading, (200, 600), 0),
        report(Migrate, BackingUp, (300, 601), 0),
        report(Migrate, Moving, (290, 580), 300),
        report(Migrate, Moving, (580, 580), 580),
        report(Migrate, Databases, (1, 2), 580),
        report(Migrate, Verifying, (290, 580), 580),
        report(Migrate, Done, (580, 580), 600),
    ]
}

#[test]
fn every_stage_names_its_own_counter_with_the_matching_percentage() {
    let zh: Vec<String> = migration()
        .iter()
        .map(|p| render(Locale::ZhCn, p))
        .collect();
    assert_eq!(
        zh,
        [
            "读取中 0 / 600 个会话 · 0%",
            "读取中 200 / 600 个会话 · 11%",
            "已备份 300 / 601 个文件 · 47%",
            "已迁移 300 / 600 个会话 · 75%",
            "已迁移 580 / 600 个会话 · 89%",
            "写入数据库 1 / 2 · 91%",
            "已校验 290 / 580 个文件 · 96%",
            "已迁移 600 / 600 个会话 · 100%",
        ]
    );
    let en: Vec<String> = migration().iter().map(|p| render(Locale::En, p)).collect();
    assert_eq!(en[1], "Reading 200 of 600 sessions · 11%");
    assert_eq!(en[2], "Backed up 300 of 601 files · 47%");
    assert_eq!(en[3], "Migrated 300 of 600 sessions · 75%");
    assert_eq!(en[5], "Updating databases: 1 of 2 · 91%");
    assert_eq!(en[6], "Checked 290 of 580 files · 96%");
}

#[test]
fn the_percentage_is_the_counters_ratio_within_its_stages_share() {
    // Shares: reading .35, backing up .25, moving .3, databases .03,
    // verifying .07 (restore: checking .3, databases .1, moving .6).
    let before = |s: SessionStage| match s {
        SessionStage::Reading => (0.0, 0.35),
        SessionStage::BackingUp => (0.35, 0.25),
        SessionStage::Moving => (0.6, 0.3),
        SessionStage::Databases => (0.9, 0.03),
        _ => (0.93, 0.07),
    };
    let mut last = 0;
    for p in migration() {
        let (n, of) = p.counter();
        let percent = session_percent(&p);
        if p.stage != SessionStage::Done {
            let (start, share) = before(p.stage);
            let expected = ((start + share * f64::from(n) / f64::from(of)) * 100.0).floor() as u32;
            assert!(
                percent.abs_diff(expected) <= 1,
                "{p:?}: {percent} vs {expected}"
            );
        }
        // The counter shown is the counter the percentage uses.
        let text = messages::catalog(Locale::En).text(&session_count(&p));
        assert!(text.contains(&format!("{n} of {of}")), "{text}");
        assert!(percent >= last, "never goes back: {p:?}");
        last = percent;
    }
    assert_eq!(last, 100);
    // While rewriting, sessions count (not files).
    let p = report(
        SessionDirection::Migrate,
        SessionStage::Moving,
        (100, 100),
        50,
    );
    assert_eq!(p.counter(), (50, 600));
}

#[test]
fn a_restore_checks_files_then_puts_sessions_back() {
    use SessionDirection::Restore;
    let checking = report(Restore, SessionStage::Reading, (150, 600), 0);
    assert_eq!(
        render(Locale::ZhCn, &checking),
        "检查中 150 / 600 个文件 · 7%"
    );
    let moving = report(Restore, SessionStage::Moving, (300, 600), 300);
    assert_eq!(
        render(Locale::ZhCn, &moving),
        "已恢复 300 / 600 个会话 · 70%"
    );
    let mut done = report(Restore, SessionStage::Done, (600, 600), 600);
    done.current = Some(SessionCurrent {
        title: Some("把路由页的统计改成按小时".into()),
        file: None,
    });
    assert_eq!(
        render(Locale::En, &done),
        "Restored 600 of 600 sessions · 100% · 把路由页的统计改成按小时"
    );
}
