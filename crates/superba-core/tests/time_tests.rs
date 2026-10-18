#![allow(missing_docs)]

use std::time::{Duration, UNIX_EPOCH};

use superba_core::{Clock, Countdown, ManualClock, Timestamp};

#[test]
fn timestamps_render_epoch_and_negative_milliseconds() {
    for (millis, expected) in [
        (0, "1970-01-01T00:00:00.000Z"),
        (-1, "1969-12-31T23:59:59.999Z"),
        (-1_001, "1969-12-31T23:59:58.999Z"),
        (-86_400_000, "1969-12-31T00:00:00.000Z"),
    ] {
        let timestamp = Timestamp::from_millis(millis);
        assert_eq!(timestamp.as_millis(), millis);
        assert_eq!(timestamp.to_rfc3339(), expected);
        assert_eq!(timestamp.to_string(), expected);
    }
}

#[test]
fn timestamps_render_leap_day_and_october_2026() {
    const DAY_MS: i64 = 86_400_000;
    // 1970..2024 contains 54 ordinary years plus 13 leap days; Feb 29 is day 59.
    let leap_day_ms = (54 * 365 + 13 + 31 + 28) * DAY_MS;
    assert_eq!(leap_day_ms, 1_709_164_800_000);
    assert_eq!(
        Timestamp(leap_day_ms).to_rfc3339(),
        "2024-02-29T00:00:00.000Z"
    );
    assert_eq!(
        Timestamp(leap_day_ms + DAY_MS - 1).to_rfc3339(),
        "2024-02-29T23:59:59.999Z"
    );
    assert_eq!(
        Timestamp(leap_day_ms + DAY_MS).to_rfc3339(),
        "2024-03-01T00:00:00.000Z"
    );

    // 1970..2026: 56 * 365 + 14 leap days. Jan..Sep: 273 days, then 15 days.
    let october_ms = (56 * 365 + 14 + 273 + 15) * DAY_MS + (8 * 3600 + 30 * 60) * 1000 + 123;
    assert_eq!(october_ms, 1_792_139_400_123);
    assert_eq!(
        Timestamp(october_ms).to_rfc3339(),
        "2026-10-16T08:30:00.123Z"
    );
}

#[test]
fn timestamps_round_trip_through_system_time_on_both_sides_of_epoch() {
    for (system_time, millis) in [
        (UNIX_EPOCH, 0),
        (
            UNIX_EPOCH + Duration::from_millis(1_792_139_400_123),
            1_792_139_400_123,
        ),
        (UNIX_EPOCH - Duration::from_millis(1), -1),
        (UNIX_EPOCH - Duration::from_millis(86_400_123), -86_400_123),
    ] {
        let timestamp = Timestamp::from_system_time(system_time);
        assert_eq!(timestamp, Timestamp(millis));
        assert_eq!(timestamp.to_system_time(), system_time);
        assert_eq!(
            Timestamp::from_system_time(timestamp.to_system_time()),
            timestamp
        );
    }
}

#[test]
fn duration_since_handles_equal_reversed_and_cross_epoch_times() {
    assert_eq!(
        Timestamp(123).duration_since(Timestamp(123)),
        Some(Duration::ZERO)
    );
    assert_eq!(
        Timestamp(123).duration_since(Timestamp(-877)),
        Some(Duration::from_secs(1))
    );
    assert_eq!(
        Timestamp(-1).duration_since(Timestamp(-1_001)),
        Some(Duration::from_secs(1))
    );
    assert_eq!(Timestamp(123).duration_since(Timestamp(124)), None);
    assert_eq!(
        Timestamp(i64::MAX).duration_since(Timestamp(i64::MIN)),
        None
    );
}

#[test]
fn timestamp_arithmetic_saturates_at_integer_bounds() {
    assert_eq!(
        Timestamp(10).saturating_add(Duration::from_millis(5)),
        Timestamp(15)
    );
    assert_eq!(
        Timestamp(10).saturating_sub(Duration::from_millis(15)),
        Timestamp(-5)
    );
    assert_eq!(
        Timestamp(i64::MAX - 1).saturating_add(Duration::from_millis(2)),
        Timestamp(i64::MAX)
    );
    assert_eq!(
        Timestamp(i64::MIN + 1).saturating_sub(Duration::from_millis(2)),
        Timestamp(i64::MIN)
    );
}

#[test]
fn manual_clock_advances_both_clocks_and_wall_jumps_leave_monotonic_time_alone() {
    let clock = ManualClock::new(Timestamp(-500));
    assert_eq!(clock.now(), Timestamp(-500));
    assert_eq!(clock.monotonic(), Duration::ZERO);
    clock.advance(Duration::from_millis(1_250));
    assert_eq!(clock.now(), Timestamp(750));
    assert_eq!(clock.monotonic(), Duration::from_millis(1_250));
    clock.set_now(Timestamp(10_000));
    assert_eq!(clock.now(), Timestamp(10_000));
    assert_eq!(clock.monotonic(), Duration::from_millis(1_250));
    clock.advance(Duration::from_millis(750));
    assert_eq!(clock.now(), Timestamp(10_750));
    assert_eq!(clock.monotonic(), Duration::from_secs(2));
}

#[test]
fn countdown_tracks_progress_and_clamps_after_expiry() {
    let clock = ManualClock::new(Timestamp::EPOCH);
    clock.advance(Duration::from_secs(20));
    let countdown = Countdown::start(Duration::from_secs(4), clock.monotonic());
    assert_eq!(countdown.total(), Duration::from_secs(4));
    assert_eq!(countdown.elapsed(clock.monotonic()), Duration::ZERO);
    assert_eq!(
        countdown.remaining(clock.monotonic()),
        Duration::from_secs(4)
    );
    assert_eq!(countdown.progress(clock.monotonic()), 0.0);
    assert!(!countdown.is_expired(clock.monotonic()));
    clock.advance(Duration::from_secs(1));
    assert_eq!(countdown.elapsed(clock.monotonic()), Duration::from_secs(1));
    assert_eq!(
        countdown.remaining(clock.monotonic()),
        Duration::from_secs(3)
    );
    assert_eq!(countdown.progress(clock.monotonic()), 0.25);
    clock.advance(Duration::from_secs(3));
    assert!(countdown.is_expired(clock.monotonic()));
    clock.advance(Duration::from_secs(10));
    assert_eq!(countdown.remaining(clock.monotonic()), Duration::ZERO);
    assert_eq!(countdown.progress(clock.monotonic()), 1.0);
}

#[test]
fn countdown_excludes_multiple_pauses_and_pause_resume_are_idempotent() {
    let clock = ManualClock::new(Timestamp::EPOCH);
    let mut countdown = Countdown::start(Duration::from_secs(8), clock.monotonic());
    countdown.resume(clock.monotonic());
    clock.advance(Duration::from_secs(2));
    countdown.pause(clock.monotonic());
    clock.advance(Duration::from_secs(50));
    countdown.pause(clock.monotonic());
    assert!(countdown.is_paused());
    assert!(!countdown.is_expired(clock.monotonic()));
    assert_eq!(countdown.elapsed(clock.monotonic()), Duration::from_secs(2));
    assert_eq!(
        countdown.remaining(clock.monotonic()),
        Duration::from_secs(6)
    );
    assert_eq!(countdown.progress(clock.monotonic()), 0.25);
    countdown.resume(clock.monotonic());
    clock.advance(Duration::from_secs(2));
    countdown.resume(clock.monotonic());
    countdown.pause(clock.monotonic());
    clock.advance(Duration::from_secs(100));
    countdown.resume(clock.monotonic());
    assert!(!countdown.is_paused());
    assert_eq!(countdown.elapsed(clock.monotonic()), Duration::from_secs(4));
    clock.advance(Duration::from_secs(4));
    assert!(countdown.is_expired(clock.monotonic()));
}

#[test]
fn countdown_restart_restores_full_duration_and_clears_pause_history() {
    let clock = ManualClock::new(Timestamp::EPOCH);
    let mut countdown = Countdown::start(Duration::from_secs(5), clock.monotonic());
    clock.advance(Duration::from_secs(2));
    countdown.pause(clock.monotonic());
    clock.advance(Duration::from_secs(20));
    countdown.restart(clock.monotonic());
    assert!(!countdown.is_paused());
    assert_eq!(countdown.elapsed(clock.monotonic()), Duration::ZERO);
    assert_eq!(
        countdown.remaining(clock.monotonic()),
        Duration::from_secs(5)
    );
    clock.advance(Duration::from_secs(5));
    assert!(countdown.is_expired(clock.monotonic()));
    countdown.restart(clock.monotonic());
    assert!(!countdown.is_expired(clock.monotonic()));
    assert_eq!(countdown.remaining(clock.monotonic()), countdown.total());
}

#[test]
fn zero_countdown_is_complete_but_never_expired_while_paused() {
    let now = Duration::from_secs(42);
    let mut countdown = Countdown::start(Duration::ZERO, now);
    assert_eq!(countdown.remaining(now), Duration::ZERO);
    assert_eq!(countdown.progress(now), 1.0);
    assert!(countdown.is_expired(now));
    countdown.pause(now);
    assert!(!countdown.is_expired(now + Duration::from_secs(100)));
    countdown.resume(now + Duration::from_secs(100));
    assert!(countdown.is_expired(now + Duration::from_secs(100)));
}

#[test]
fn expired_nonzero_countdown_is_not_expired_while_paused() {
    let mut countdown = Countdown::start(Duration::from_secs(1), Duration::ZERO);
    let now = Duration::from_secs(2);
    assert!(countdown.is_expired(now));
    countdown.pause(now);
    assert!(!countdown.is_expired(now));
    countdown.resume(now);
    assert!(countdown.is_expired(now));
}

#[test]
fn utc_calendar_helpers_round_trip() {
    let t = Timestamp::from_utc_date(2026, 11, 1).unwrap();
    assert_eq!(t, Timestamp::from_millis(1_793_491_200_000));
    assert_eq!(t.utc_date(), (2026, 11, 1));
    assert_eq!(t.utc_date_string(), "2026-11-01");
    let later = t.saturating_add(Duration::from_secs(5 * 3600 + 7));
    assert_eq!(later.utc_day_start(), t);
    assert_eq!(later.as_unix_secs(), 1_793_491_200 + 5 * 3600 + 7);
    assert_eq!(Timestamp::from_unix_secs(1_793_491_200), t);
    assert_eq!(
        Timestamp::from_utc_date(2024, 2, 29).unwrap().utc_date(),
        (2024, 2, 29)
    );
    assert_eq!(Timestamp::from_utc_date(2025, 2, 29), None);
    assert_eq!(Timestamp::from_utc_date(2026, 13, 1), None);
    assert_eq!(Timestamp::from_utc_date(2026, 4, 0), None);
    let before_epoch = Timestamp::from_millis(-1);
    assert_eq!(before_epoch.utc_date_string(), "1969-12-31");
    assert_eq!(before_epoch.as_unix_secs(), -1);
}

// --- local time (user bug 55(b): times were shown in UTC on Windows) ---

/// 2026-10-18T14:10:23.456Z
const OCT18: i64 = 1_792_332_623_456;

#[test]
fn local_time_shows_the_wall_clock_of_the_offset() {
    use superba_core::LocalTime;
    let t = Timestamp(OCT18);
    assert_eq!(t.to_rfc3339(), "2026-10-18T14:10:23.456Z");
    // Pacific daylight time, the Windows test VMs' zone.
    let pdt = LocalTime::with_offset(t, Some(-7 * 3600));
    assert_eq!(pdt.to_string(), "2026-10-18 07:10:23");
    assert_eq!(pdt.date(), "2026-10-18");
    assert_eq!(pdt.time_of_day(false), "07:10");
    assert_eq!(pdt.time_of_day(true), "07:10:23");
    // East of UTC across midnight, with a half-hour zone.
    let late = Timestamp(OCT18 + 10 * 3_600_000);
    assert_eq!(
        LocalTime::with_offset(late, Some(5 * 3600 + 1800)).to_string(),
        "2026-10-19 05:40:23"
    );
    // West of UTC back across midnight.
    let early = Timestamp(OCT18 - 12 * 3_600_000);
    let ny = LocalTime::with_offset(early, Some(-4 * 3600));
    assert_eq!(ny.to_string(), "2026-10-17 22:10:23");
    assert_eq!(ny.date(), "2026-10-17");
    // UTC itself has no suffix; an unknown offset says UTC.
    assert_eq!(
        LocalTime::with_offset(t, Some(0)).to_string(),
        "2026-10-18 14:10:23"
    );
    let unknown = LocalTime::with_offset(t, None);
    assert_eq!(unknown.to_string(), "2026-10-18 14:10:23 UTC");
    assert_eq!(unknown.time_of_day(false), "14:10 UTC");
    assert_eq!(unknown.offset_secs(), None);
    assert_eq!(unknown.timestamp(), t);
}
