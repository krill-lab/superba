//! A hot restart while the desktop app finishes its first-start Windows
//! setup left the app stuck on "Finishing Windows setup" (user bug 59): the
//! setup counts as busy, so a switch waits for it like for a running turn.

#![allow(missing_docs)]

mod fakes;

use std::sync::Arc;

use fakes::*;
use pretty_assertions::assert_eq;
use superba_core::{
    ActivitySignal, ExecuteOptions, Locale, OutcomeStatus, ProfileId, SwitchRequest, SwitchService,
    TargetId, ThreadActivity,
};
use superba_switch::messages;

fn setup() -> ThreadActivity {
    ThreadActivity {
        target_id: TargetId::local(),
        thread_id: None,
        title: None,
        rollout_path: None,
        last_event_at: None,
        signal: ActivitySignal::AppSetup,
        originator: None,
        cwd: None,
    }
}

#[tokio::test(start_paused = true)]
async fn a_hot_switch_waits_for_the_app_setup_before_restarting_the_core() {
    let w = World::new();
    // Plan-time check and three polls see the setup, then it is done.
    w.push_turns(vec![
        vec![setup()],
        vec![setup()],
        vec![setup()],
        vec![setup()],
    ]);
    let d = ScriptedDecider::with(&[]);
    *d.hang_on_wait.lock().unwrap() = true;
    let sw = w.switcher();
    let plan = sw
        .plan(SwitchRequest::profile(ProfileId::new("work")))
        .await
        .unwrap();
    let o = sw
        .execute(
            plan,
            ExecuteOptions::default(),
            d.clone(),
            Arc::new(RecordingEvents::default()),
        )
        .await
        .unwrap();
    assert_eq!(o.status, OutcomeStatus::Switched, "{:?}", o.notices);
    assert_eq!(d.asked_kinds(), vec!["wait_for_turns"]);
    // The restart came after the setup ended.
    let calls = w.calls();
    let restart = calls
        .iter()
        .position(|c| c.starts_with("app.hot_restart"))
        .unwrap();
    assert!(restart > 0);
}

#[test]
fn the_setup_is_named_as_such_not_as_an_untitled_conversation() {
    let en = messages::catalog(Locale::En);
    let zh = messages::catalog(Locale::ZhCn);
    let t = messages::busy_thread(&setup());
    assert_eq!(t.key, "switch.waiting.app_setup");
    assert!(
        en.text(&t).contains("finishing its Windows setup"),
        "{}",
        en.text(&t)
    );
    assert!(zh.text(&t).contains("Windows 设置"), "{}", zh.text(&t));
}
