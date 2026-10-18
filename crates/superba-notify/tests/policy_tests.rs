//! Triggers, muted harnesses, minimum task length, quiet hours, dedupe,
//! per-channel filters and the shared state file.

use std::collections::BTreeSet;

use pretty_assertions::assert_eq;
use superba_core::notify::{hhmm, parse_hhmm};
use superba_core::{
    ChannelKind, EventKind, NotifyChannel, NotifyEvent, NotifySettings, NotifySkip, QuietHours,
    Timestamp,
};
use superba_notify::policy::{PolicyInput, PolicyState, StateFile, decide};

fn settings() -> NotifySettings {
    let mut s = NotifySettings {
        enabled: true,
        ..Default::default()
    };
    s.channels
        .push(NotifyChannel::new("a", ChannelKind::Telegram, "a"));
    s.channels
        .push(NotifyChannel::new("b", ChannelKind::Bark, "b"));
    s
}

fn ev(kind: EventKind, turn: &str) -> NotifyEvent {
    let mut e = NotifyEvent::new(kind, "codex", Timestamp::from_millis(0));
    e.turn_id = Some(turn.into());
    e
}

fn at(minute: u16) -> PolicyInput {
    PolicyInput {
        now_ms: 1_000_000,
        local_minute: Some(minute),
        only_channel: None,
        force: false,
    }
}

#[test]
fn quiet_hours_cross_midnight() {
    let q = QuietHours::parse("23:00-08:00").unwrap();
    assert!(q.enabled);
    assert!(q.contains(23 * 60));
    assert!(q.contains(2 * 60));
    assert!(!q.contains(8 * 60));
    assert!(!q.contains(12 * 60));
    let day = QuietHours::parse("12:30-13:30").unwrap();
    assert!(day.contains(12 * 60 + 30) && !day.contains(13 * 60 + 30));
    assert_eq!(q.display(), "23:00-08:00");
    assert_eq!(
        (parse_hhmm("7"), parse_hhmm("24:00"), hhmm(65)),
        (Some(420), None, "01:05".into())
    );
    assert!(!QuietHours::default().contains(0), "off by default");
}

#[test]
fn decisions_follow_the_preferences() {
    let mut state = PolicyState::default();
    let mut s = settings();
    // Master switch.
    s.enabled = false;
    assert_eq!(
        decide(&ev(EventKind::TurnComplete, "1"), &s, &at(600), &mut state).skipped,
        Some(NotifySkip::Disabled)
    );
    s.enabled = true;
    // Trigger off.
    s.triggers.switch_done = false;
    assert_eq!(
        decide(&ev(EventKind::SwitchDone, "2"), &s, &at(600), &mut state).skipped,
        Some(NotifySkip::TriggerOff)
    );
    // Muted harness.
    s.triggers.muted_harnesses = BTreeSet::from(["codex".to_owned()]);
    assert_eq!(
        decide(&ev(EventKind::TurnComplete, "3"), &s, &at(600), &mut state).skipped,
        Some(NotifySkip::HarnessMuted)
    );
    s.triggers.muted_harnesses.clear();
    // Minimum task length (unknown durations pass).
    s.triggers.min_turn_secs = 30;
    let mut short = ev(EventKind::TurnComplete, "4");
    short.duration_ms = Some(5_000);
    assert_eq!(
        decide(&short, &s, &at(600), &mut state).skipped,
        Some(NotifySkip::TooShort)
    );
    assert_eq!(
        decide(&ev(EventKind::TurnComplete, "5"), &s, &at(600), &mut state).skipped,
        None
    );
    // Quiet hours, with and without urgent events.
    s.quiet_hours = QuietHours::parse("22:00-07:00").unwrap();
    assert_eq!(
        decide(
            &ev(EventKind::TurnComplete, "6"),
            &s,
            &at(23 * 60),
            &mut state
        )
        .skipped,
        Some(NotifySkip::QuietHours)
    );
    assert_eq!(
        decide(
            &ev(EventKind::NeedsApproval, "7"),
            &s,
            &at(23 * 60),
            &mut state
        )
        .skipped,
        Some(NotifySkip::QuietHours)
    );
    s.quiet_hours.allow_urgent = true;
    assert_eq!(
        decide(
            &ev(EventKind::NeedsApproval, "8"),
            &s,
            &at(23 * 60),
            &mut state
        )
        .skipped,
        None
    );
    // Unknown local time: quiet hours cannot apply.
    let unknown = PolicyInput {
        local_minute: None,
        ..at(0)
    };
    assert_eq!(
        decide(&ev(EventKind::TurnComplete, "9"), &s, &unknown, &mut state).skipped,
        None
    );
    // Duplicate turn.
    assert_eq!(
        decide(&ev(EventKind::TurnComplete, "9"), &s, &at(600), &mut state).skipped,
        Some(NotifySkip::Duplicate)
    );
    // Without a turn id nothing is deduplicated.
    let mut no_id = ev(EventKind::TurnComplete, "x");
    no_id.turn_id = None;
    no_id.thread_id = Some("t".into());
    assert_eq!(decide(&no_id, &s, &at(600), &mut state).skipped, None);
    assert_eq!(decide(&no_id, &s, &at(600), &mut state).skipped, None);
}

#[test]
fn channel_selection_filters_and_force() {
    let mut state = PolicyState::default();
    let mut s = settings();
    s.channels[1].enabled = false;
    s.channels[0].only = BTreeSet::from([EventKind::NeedsApproval]);
    let d = decide(&ev(EventKind::TurnComplete, "1"), &s, &at(600), &mut state);
    assert_eq!(
        d.channels
            .iter()
            .map(|(c, v)| (c.id.as_str(), *v))
            .collect::<Vec<_>>(),
        vec![
            ("a", Err(NotifySkip::TriggerOff)),
            ("b", Err(NotifySkip::ChannelOff))
        ]
    );
    // The test button: one channel, even when it is off.
    let test = NotifyEvent::new(EventKind::Test, "superba", Timestamp::from_millis(0));
    let force = PolicyInput {
        only_channel: Some("b".into()),
        force: true,
        ..at(600)
    };
    let d = decide(&test, &s, &force, &mut state);
    assert_eq!(d.channels.len(), 1);
    assert_eq!(d.channels[0].1, Ok(0));
}

#[test]
fn state_file_is_shared_between_processes() {
    let dir = tempfile::tempdir().unwrap();
    let f = StateFile::new(dir.path().join("notify"));
    let s = settings();
    let first = f
        .update(|st| decide(&ev(EventKind::TurnComplete, "z"), &s, &at(600), st))
        .unwrap();
    assert_eq!(first.skipped, None);
    // A second "process" reads the same file.
    let g = StateFile::new(dir.path().join("notify"));
    let second = g
        .update(|st| decide(&ev(EventKind::TurnComplete, "z"), &s, &at(600), st))
        .unwrap();
    assert_eq!(second.skipped, Some(NotifySkip::Duplicate));
    // A corrupt file starts over instead of failing.
    std::fs::write(dir.path().join("notify/state.json"), b"{not json").unwrap();
    let third = g
        .update(|st| decide(&ev(EventKind::TurnComplete, "z"), &s, &at(600), st))
        .unwrap();
    assert_eq!(third.skipped, None);
}
