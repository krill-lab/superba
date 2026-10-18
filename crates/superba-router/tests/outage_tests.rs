//! Member outages reported for notifications (GH#12): only long ones, once.

use std::time::{Duration, Instant};

use superba_router::outage::{DOWN_FOR, MemberHealth, OutageTracker, spb_next_to};

fn member(id: &str, usable: bool) -> MemberHealth {
    MemberHealth {
        id: id.into(),
        name: format!("{id} name"),
        usable,
        error: (!usable).then(|| "probe: cannot connect".into()),
    }
}

#[test]
fn a_member_is_reported_once_per_long_outage() {
    let mut t = OutageTracker::default();
    let t0 = Instant::now();
    let down = [member("krill", false), member("other", true)];
    assert!(t.observe(&down, t0, 1_000).is_empty());
    assert!(
        t.observe(&down, t0 + DOWN_FOR - Duration::from_secs(1), 1_000)
            .is_empty()
    );
    let reported = t.observe(&down, t0 + DOWN_FOR, 1_000);
    assert_eq!(reported.len(), 1);
    assert_eq!(reported[0].name, "krill name");
    assert_eq!(reported[0].error.as_deref(), Some("probe: cannot connect"));
    assert_eq!(reported[0].key, "krill@1000");
    // Still down: not again.
    assert!(t.observe(&down, t0 + DOWN_FOR * 3, 1_000).is_empty());
    // Back, then down again: a new outage with a new key.
    assert!(
        t.observe(&[member("krill", true)], t0 + DOWN_FOR * 4, 2_000)
            .is_empty()
    );
    let t1 = t0 + DOWN_FOR * 5;
    assert!(t.observe(&[member("krill", false)], t1, 3_000).is_empty());
    let again = t.observe(&[member("krill", false)], t1 + DOWN_FOR, 3_000);
    assert_eq!(again[0].key, "krill@3000");
}

#[test]
fn a_short_failure_is_not_reported() {
    let mut t = OutageTracker::default();
    let t0 = Instant::now();
    assert!(t.observe(&[member("a", false)], t0, 1).is_empty());
    assert!(
        t.observe(&[member("a", true)], t0 + Duration::from_secs(30), 1)
            .is_empty()
    );
    assert!(
        t.observe(&[member("a", false)], t0 + DOWN_FOR, 2)
            .is_empty()
    );
}

#[test]
fn spb_is_found_next_to_the_router() {
    let dir = tempfile::tempdir().unwrap();
    let router = dir.path().join("superba-router");
    assert_eq!(spb_next_to(&router), None);
    let name = if cfg!(windows) { "spb.exe" } else { "spb" };
    std::fs::write(dir.path().join(name), b"").unwrap();
    assert_eq!(spb_next_to(&router), Some(dir.path().join(name)));
}
