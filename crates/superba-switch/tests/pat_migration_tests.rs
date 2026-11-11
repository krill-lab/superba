//! One-off alpha.16 PAT→API-key migration (user bug 139; remove with
//! `pat_migration`): a home alpha.15 left on a personal access token is
//! written once more with the same key as an API key at the first start of
//! Superba's window, write-only (a running app is not interrupted), and never
//! again. A line that does not take the key asks for one; a failure is tried
//! once more, then given up.

mod fakes;

use fakes::*;
use superba_core::{
    ActiveState, AuthShape, Error, KeyAccess, PatMigration, Severity, SwitchService, Timestamp,
};

/// The local home as alpha.15 left it: `work` applied on its personal access
/// token (the app runs, opened through Superba).
fn alpha15() -> World {
    let w = World::new();
    w.set_session(Some(session(Some(OVERSEAS), true)));
    let mut st = w.config.state.lock().unwrap();
    st.auth = AuthShape::Pat {
        key_last4: "1111".into(),
    };
    st.active = Some(ActiveState {
        profile_id: Some("work".into()),
        site_id: Some("krill".into()),
        line_id: Some("overseas".into()),
        // alpha.15 stored "pat", which reads as an API key.
        auth_mode: serde_json::from_value(serde_json::json!("pat")).unwrap(),
        generation: 1,
        applied_at: Timestamp(T0),
    });
    drop(st);
    w.log.lock().unwrap().clear();
    w
}

/// The window starts: the once-per-start call, then it shows the notices.
async fn start(sw: &superba_switch::Switcher) -> Vec<(Severity, String)> {
    let notes = sw.analytics_reapply().await.unwrap();
    sw.analytics_reapply_delivered().await.unwrap();
    notes
        .into_iter()
        .map(|n| (n.severity, n.text.key))
        .collect()
}

fn state(w: &World) -> Option<PatMigration> {
    w.profiles
        .set
        .lock()
        .unwrap()
        .settings
        .pat_migration
        .clone()
}

#[tokio::test]
async fn a_pat_home_is_written_once_with_the_key_as_an_api_key() {
    let w = alpha15();
    let sw = w.switcher();
    assert_eq!(
        start(&sw).await,
        [(
            Severity::Info,
            "switch.pat_migration.done_restart".to_owned()
        )]
    );
    // Written with the API-key form of the same profile and line.
    assert_eq!(
        w.config.last_desired.lock().unwrap().as_deref(),
        Some("token:ApiKey:https://api.kbsfdxc.net/backend-api/codex")
    );
    let writes = w.writes();
    assert!(writes.contains(&"config.apply".to_owned()), "{writes:?}");
    // The running app is not interrupted.
    assert!(!writes.iter().any(|c| c.starts_with("app.")), "{writes:?}");
    assert_eq!(
        state(&w).map(|s| (s.settled, s.undelivered)),
        Some((true, None))
    );
    // Exactly once: the next starts do nothing, even with the token back.
    w.log.lock().unwrap().clear();
    w.config.state.lock().unwrap().auth = AuthShape::Pat {
        key_last4: "1111".into(),
    };
    assert!(start(&sw).await.is_empty());
    assert!(start(&sw).await.is_empty());
    assert!(w.writes().is_empty(), "{:?}", w.writes());
}

#[tokio::test]
async fn without_the_app_running_it_says_nothing_was_restarted() {
    let w = alpha15();
    w.set_session(None);
    let sw = w.switcher();
    assert_eq!(
        start(&sw).await,
        [(Severity::Info, "switch.pat_migration.done".to_owned())]
    );
    assert!(!w.writes().iter().any(|c| c.starts_with("app.")));
}

#[tokio::test]
async fn nothing_happens_without_a_superba_token_in_auth_json() {
    for case in ["api_key", "not_applied", "handed_off"] {
        let w = alpha15();
        {
            let mut st = w.config.state.lock().unwrap();
            match case {
                "api_key" => {
                    st.auth = AuthShape::ApiKey {
                        key_last4: "1111".into(),
                    }
                }
                "not_applied" => st.active = None,
                _ => {
                    st.handed_off = Some(superba_core::HandOffInfo {
                        at: Timestamp(T0),
                        to: Default::default(),
                        generation: 2,
                    })
                }
            }
        }
        let sw = w.switcher();
        assert!(start(&sw).await.is_empty(), "{case}");
        assert!(w.writes().is_empty(), "{case}: {:?}", w.writes());
        assert_eq!(
            state(&w).map(|s| s.settled),
            Some(true),
            "{case}: checked once, settled"
        );
    }
}

#[tokio::test]
async fn a_line_that_refuses_the_key_asks_for_one_instead_of_guessing() {
    let w = alpha15();
    let mut caps = good_caps("krill", "overseas");
    caps.key_access = Some(KeyAccess::InvalidKey);
    w.prober
        .caps
        .lock()
        .unwrap()
        .insert("overseas".into(), caps);
    let sw = w.switcher();
    assert_eq!(
        start(&sw).await,
        [(
            Severity::Warning,
            "switch.pat_migration.needs_key".to_owned()
        )]
    );
    assert!(w.writes().is_empty(), "{:?}", w.writes());
    // Shown once, then settled.
    assert!(start(&sw).await.is_empty());
}

#[tokio::test]
async fn a_failure_is_tried_once_more_then_given_up() {
    let w = alpha15();
    *w.config.fail_apply.lock().unwrap() = Some(Error::other("disk full"));
    let sw = w.switcher();
    let first = start(&sw).await;
    assert_eq!(first.len(), 1);
    assert_eq!(
        first[0],
        (Severity::Danger, "switch.pat_migration.retry".to_owned())
    );
    let second = start(&sw).await;
    assert_eq!(
        second,
        [(Severity::Danger, "switch.pat_migration.given_up".to_owned())]
    );
    assert!(start(&sw).await.is_empty(), "nothing loops");
    assert_eq!(state(&w).map(|s| s.settled), Some(true));
}

#[tokio::test]
async fn an_unshown_notice_comes_back_without_writing_again() {
    let w = alpha15();
    let sw = w.switcher();
    let first = sw.analytics_reapply().await.unwrap();
    assert_eq!(first.len(), 1);
    // The window closed before showing it.
    w.log.lock().unwrap().clear();
    assert_eq!(sw.analytics_reapply().await.unwrap(), first);
    assert!(w.writes().is_empty(), "{:?}", w.writes());
    sw.analytics_reapply_delivered().await.unwrap();
    assert!(sw.analytics_reapply().await.unwrap().is_empty());
}

/// `spb` before a command: its notices are printed, then settled.
async fn spb(sw: &superba_switch::Switcher) -> Vec<(Severity, String)> {
    let notes = SwitchService::pat_migration(sw).await.unwrap();
    if !notes.is_empty() {
        SwitchService::pat_migration_delivered(sw).await.unwrap();
    }
    notes
        .into_iter()
        .map(|n| (n.severity, n.text.key))
        .collect()
}

fn applies(w: &World) -> usize {
    w.writes().iter().filter(|c| *c == "config.apply").count()
}

#[tokio::test]
async fn spb_migrates_once_and_a_later_window_start_does_not_again() {
    let w = alpha15();
    let sw = w.switcher();
    assert_eq!(
        spb(&sw).await,
        [(
            Severity::Info,
            "switch.pat_migration.done_restart".to_owned()
        )]
    );
    assert_eq!(applies(&w), 1);
    assert!(!w.writes().iter().any(|c| c.starts_with("app.")));
    assert!(spb(&sw).await.is_empty());
    // The window starts later (the token back in auth.json changes nothing).
    w.config.state.lock().unwrap().auth = AuthShape::Pat {
        key_last4: "1111".into(),
    };
    assert!(start(&w.switcher()).await.is_empty());
    assert_eq!(applies(&w), 1, "{:?}", w.writes());
}

#[tokio::test]
async fn a_window_start_first_leaves_nothing_for_spb() {
    let w = alpha15();
    assert_eq!(start(&w.switcher()).await.len(), 1);
    assert_eq!(applies(&w), 1);
    w.config.state.lock().unwrap().auth = AuthShape::Pat {
        key_last4: "1111".into(),
    };
    assert!(spb(&w.switcher()).await.is_empty());
    assert_eq!(applies(&w), 1);
}

#[tokio::test]
async fn an_spb_failure_is_retried_once_then_given_up() {
    let w = alpha15();
    *w.config.fail_apply.lock().unwrap() = Some(Error::other("disk full"));
    let sw = w.switcher();
    assert_eq!(
        spb(&sw).await,
        [(Severity::Danger, "switch.pat_migration.retry".to_owned())]
    );
    assert_eq!(
        spb(&sw).await,
        [(Severity::Danger, "switch.pat_migration.given_up".to_owned())]
    );
    assert!(spb(&sw).await.is_empty());
    assert!(start(&sw).await.is_empty(), "the window does not try again");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_window_and_spb_at_the_same_time_migrate_once() {
    for _ in 0..8 {
        let w = alpha15();
        let (window, cli) = (w.switcher(), w.switcher());
        let (a, b) = tokio::join!(async { window.analytics_reapply().await.unwrap() }, async {
            SwitchService::pat_migration(&cli).await.unwrap()
        },);
        assert_eq!(applies(&w), 1, "{:?}", w.writes());
        // Both may show the one owed notice (the second reads it owed); it is
        // settled once shown.
        assert!(a.len() + b.len() >= 1);
        assert!(
            a.iter()
                .chain(&b)
                .all(|n| n.text.key == "switch.pat_migration.done_restart"),
            "{a:?} {b:?}"
        );
        SwitchService::pat_migration_delivered(&cli).await.unwrap();
        assert!(spb(&cli).await.is_empty());
        assert!(start(&window).await.is_empty());
        assert_eq!(applies(&w), 1);
    }
}
