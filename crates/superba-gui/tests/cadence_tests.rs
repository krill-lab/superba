//! The shell's cadence (user request 34): the overview every 5 s while shown,
//! nothing but the router watchdog while hidden, window events for the
//! environment monitor, the host list on focus and a long interval.

#![allow(missing_docs)]

use std::time::{Duration, Instant};

use superba_core::EnvironmentEvent;
use superba_gui::cadence::{
    Cadence, Due, Inputs, OVERVIEW_EVERY, ROUTER_WATCH_EVERY, SELF_UPDATE_EVERY,
};

fn shown() -> Inputs {
    Inputs {
        visible: true,
        focused: true,
        entered: true,
        ..Inputs::default()
    }
}

fn count(dues: &[Due], what: Due) -> usize {
    dues.iter().filter(|d| **d == what).count()
}

/// Run `seconds` of 1 s frames from `t`, collecting everything due.
fn run(cadence: &mut Cadence, t: &mut Instant, seconds: u64, inputs: Inputs) -> Vec<Due> {
    let mut all = Vec::new();
    for _ in 0..seconds {
        *t += Duration::from_secs(1);
        all.extend(cadence.tick(*t, inputs));
    }
    all
}

#[test]
fn shown_polls_the_overview_every_five_seconds_and_nothing_slow() {
    let mut t = Instant::now();
    let mut cadence = Cadence::new(t);
    let first = cadence.tick(t, shown());
    assert!(first.contains(&Due::Environment(EnvironmentEvent::Shown)));
    let dues = run(&mut cadence, &mut t, 600, shown());
    assert_eq!(count(&dues, Due::Overview), 600 / 5);
    // The host list every 5 minutes (it was every minute), no update check yet.
    assert_eq!(count(&dues, Due::Hosts), 2);
    assert_eq!(count(&dues, Due::SelfUpdate), 0);
    assert_eq!(count(&dues, Due::RouterWatch), 0, "router mode is off");
    // No window event while nothing changes.
    assert!(!dues.iter().any(|d| matches!(d, Due::Environment(_))));
    assert_eq!(cadence.next_wake(t, shown()), Some(OVERVIEW_EVERY));
}

#[test]
fn hidden_runs_only_the_router_watchdog() {
    let mut t = Instant::now();
    let mut cadence = Cadence::new(t);
    cadence.tick(t, shown());
    let hidden = Inputs {
        visible: false,
        focused: false,
        ..shown()
    };
    let dues = run(&mut cadence, &mut t, 3600, hidden);
    assert_eq!(dues, vec![Due::Environment(EnvironmentEvent::Hidden)]);
    assert_eq!(
        cadence.next_wake(t, hidden),
        None,
        "nothing wakes the window"
    );

    // Router mode on: the watchdog keeps its 5 s tick while hidden, nothing else.
    let watched = Inputs {
        router_watch: true,
        ..hidden
    };
    let dues = run(&mut cadence, &mut t, 600, watched);
    assert_eq!(count(&dues, Due::RouterWatch), 600 / 5);
    assert_eq!(dues.len(), count(&dues, Due::RouterWatch));
    assert!(cadence.next_wake(t, watched).unwrap() <= ROUTER_WATCH_EVERY);

    // Shown again: an event for the monitor and the overview at once.
    t += Duration::from_secs(1);
    let back = cadence.tick(t, shown());
    assert!(back.contains(&Due::Environment(EnvironmentEvent::Shown)));
    assert!(back.contains(&Due::Environment(EnvironmentEvent::FocusGained)));
    assert!(back.contains(&Due::Overview));
    assert!(back.contains(&Due::Hosts), "away for over a minute");
}

#[test]
fn focus_comes_back_reads_at_once_but_not_more_than_once_a_second() {
    let mut t = Instant::now();
    let mut cadence = Cadence::new(t);
    cadence.tick(t, shown());
    let unfocused = Inputs {
        focused: false,
        ..shown()
    };
    t += Duration::from_secs(2);
    assert_eq!(cadence.tick(t, unfocused), Vec::<Due>::new());
    t += Duration::from_secs(1);
    let dues = cadence.tick(t, shown());
    assert_eq!(
        dues,
        vec![
            Due::Environment(EnvironmentEvent::FocusGained),
            Due::Overview
        ]
    );
    // Alt-tab back and forth within the second: no second read.
    cadence.tick(t, unfocused);
    let dues = cadence.tick(t + Duration::from_millis(500), shown());
    assert_eq!(dues, vec![Due::Environment(EnvironmentEvent::FocusGained)]);
}

#[test]
fn no_poll_during_a_switch_or_an_overview_in_flight() {
    let mut t = Instant::now();
    let mut cadence = Cadence::new(t);
    cadence.tick(t, shown());
    for inputs in [
        Inputs {
            switching: true,
            ..shown()
        },
        Inputs {
            overview_loading: true,
            hosts_loading: true,
            ..shown()
        },
    ] {
        let dues = run(&mut cadence, &mut t, 120, inputs);
        assert_eq!(count(&dues, Due::Overview), 0);
        assert_eq!(count(&dues, Due::Hosts), 0);
    }
}

#[test]
fn a_new_environment_generation_reloads_the_views_once() {
    let mut t = Instant::now();
    let mut cadence = Cadence::new(t);
    cadence.tick(t, shown());
    let next = Inputs {
        environment_generation: 1,
        ..shown()
    };
    t += Duration::from_secs(1);
    assert!(cadence.tick(t, next).contains(&Due::EnvironmentChanged));
    let dues = run(&mut cadence, &mut t, 60, next);
    assert_eq!(count(&dues, Due::EnvironmentChanged), 0);
}

#[test]
fn the_update_check_runs_daily_while_shown() {
    let mut t = Instant::now();
    let mut cadence = Cadence::new(t);
    cadence.tick(t, shown());
    let hidden = Inputs {
        visible: false,
        ..shown()
    };
    // A day hidden: not checked while hidden ...
    t += SELF_UPDATE_EVERY + Duration::from_secs(1);
    assert_eq!(count(&cadence.tick(t, hidden), Due::SelfUpdate), 0);
    // ... but as soon as the window is shown.
    t += Duration::from_secs(1);
    assert_eq!(count(&cadence.tick(t, shown()), Due::SelfUpdate), 1);
    assert_eq!(
        count(&run(&mut cadence, &mut t, 3600, shown()), Due::SelfUpdate),
        0
    );
}
