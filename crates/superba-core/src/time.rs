//! Time: wall-clock [`Timestamp`], the injectable [`Clock`] and the pure [`Countdown`].
//!
//! Countdowns and "waited N seconds" displays are driven by polling a [`Clock`]
//! (immediate-mode UI ticks, CLI loops) instead of sleeping, so tests advance a
//! [`ManualClock`] and assert state transitions deterministically. Async waits
//! inside implementations use `tokio::time` (testable with paused time).

use std::fmt;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Milliseconds since the Unix epoch, UTC. Serialized as a plain integer.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Timestamp(pub i64);

impl Timestamp {
    /// The Unix epoch.
    pub const EPOCH: Timestamp = Timestamp(0);

    /// From milliseconds since the epoch.
    pub const fn from_millis(ms: i64) -> Self {
        Timestamp(ms)
    }

    /// Milliseconds since the epoch.
    pub const fn as_millis(self) -> i64 {
        self.0
    }

    /// Current system time. Prefer [`Clock::now`] in code that needs tests.
    pub fn now() -> Self {
        Self::from_system_time(SystemTime::now())
    }

    /// Convert from `SystemTime` (times before the epoch become negative).
    pub fn from_system_time(t: SystemTime) -> Self {
        match t.duration_since(UNIX_EPOCH) {
            Ok(d) => Timestamp(i64::try_from(d.as_millis()).unwrap_or(i64::MAX)),
            Err(e) => Timestamp(-i64::try_from(e.duration().as_millis()).unwrap_or(i64::MAX)),
        }
    }

    /// Convert to `SystemTime`.
    pub fn to_system_time(self) -> SystemTime {
        if self.0 >= 0 {
            UNIX_EPOCH + Duration::from_millis(self.0.unsigned_abs())
        } else {
            UNIX_EPOCH - Duration::from_millis(self.0.unsigned_abs())
        }
    }

    /// `self + d`, saturating.
    pub fn saturating_add(self, d: Duration) -> Self {
        Timestamp(
            self.0
                .saturating_add(i64::try_from(d.as_millis()).unwrap_or(i64::MAX)),
        )
    }

    /// `self - d`, saturating.
    pub fn saturating_sub(self, d: Duration) -> Self {
        Timestamp(
            self.0
                .saturating_sub(i64::try_from(d.as_millis()).unwrap_or(i64::MAX)),
        )
    }

    /// Elapsed time from `earlier` to `self`, or `None` if `earlier` is later.
    pub fn duration_since(self, earlier: Timestamp) -> Option<Duration> {
        u64::try_from(self.0.checked_sub(earlier.0)?)
            .ok()
            .map(Duration::from_millis)
    }

    /// RFC 3339 / ISO 8601 UTC rendering with millisecond precision,
    /// e.g. `2026-10-16T08:30:00.123Z`.
    pub fn to_rfc3339(self) -> String {
        let ms = self.0.rem_euclid(1000);
        let secs = self.0.div_euclid(1000);
        let days = secs.div_euclid(86_400);
        let sod = secs.rem_euclid(86_400);
        let (y, m, d) = civil_from_days(days);
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}Z",
            sod / 3600,
            (sod % 3600) / 60,
            sod % 60
        )
    }

    /// From whole seconds since the epoch (saturating).
    pub const fn from_unix_secs(secs: i64) -> Self {
        Timestamp(secs.saturating_mul(1000))
    }

    /// Whole seconds since the epoch (rounded towards negative infinity).
    pub const fn as_unix_secs(self) -> i64 {
        self.0.div_euclid(1000)
    }

    /// The UTC calendar date `(year, month, day)` of this instant.
    pub fn utc_date(self) -> (i64, u32, u32) {
        civil_from_days(self.0.div_euclid(86_400_000))
    }

    /// Midnight UTC of the day this instant falls on.
    pub fn utc_day_start(self) -> Self {
        Timestamp(self.0.div_euclid(86_400_000).saturating_mul(86_400_000))
    }

    /// Midnight UTC of a calendar date, or `None` for an invalid date.
    pub fn from_utc_date(year: i64, month: u32, day: u32) -> Option<Self> {
        let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
        let days_in_month = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => return None,
        };
        if day == 0 || day > days_in_month {
            return None;
        }
        Some(Timestamp(
            days_from_civil(year, month, day).checked_mul(86_400_000)?,
        ))
    }

    /// The UTC date as `YYYY-MM-DD`.
    pub fn utc_date_string(self) -> String {
        let (y, m, d) = self.utc_date();
        format!("{y:04}-{m:02}-{d:02}")
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_rfc3339())
    }
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 -> (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Howard Hinnant's `days_from_civil`: (year, month, day) -> days since 1970-01-01.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let (m, d) = (i64::from(m), i64::from(d));
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A [`Timestamp`] shown as local wall-clock time for people to read:
/// `YYYY-MM-DD HH:MM:SS`, or the UTC time with a ` UTC` suffix where the local
/// offset is unknown. [`Display`](fmt::Display) of a `Timestamp` itself stays
/// RFC 3339 UTC for logs and machine output. The offset comes from the
/// system's time-zone rules (`superba_switch::localtime`, which may use FFI;
/// this crate forbids `unsafe`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    at: Timestamp,
    offset_secs: Option<i64>,
}

impl LocalTime {
    /// `at` shown with a given UTC offset (`None` = unknown, shown in UTC).
    pub const fn with_offset(at: Timestamp, offset_secs: Option<i64>) -> Self {
        Self { at, offset_secs }
    }

    /// The instant.
    pub const fn timestamp(&self) -> Timestamp {
        self.at
    }

    /// The UTC offset used, in seconds east of UTC (`None` = unknown).
    pub const fn offset_secs(&self) -> Option<i64> {
        self.offset_secs
    }

    /// The wall-clock time as RFC 3339 digits without a zone
    /// (`2026-10-18T07:10:23.000`; UTC where the offset is unknown).
    fn wall(&self) -> String {
        let shifted = Timestamp(
            self.at
                .0
                .saturating_add(self.offset_secs.unwrap_or(0).saturating_mul(1000)),
        );
        let s = shifted.to_rfc3339();
        s.strip_suffix('Z').unwrap_or(&s).to_owned()
    }

    /// The local calendar date, `YYYY-MM-DD` (UTC where the offset is unknown).
    pub fn date(&self) -> String {
        let s = self.wall();
        s.get(..10).unwrap_or(&s).to_owned()
    }

    /// The local time of day, `HH:MM:SS` (or `HH:MM`), with ` UTC` where the
    /// offset is unknown.
    pub fn time_of_day(&self, seconds: bool) -> String {
        let s = self.wall();
        let end = if seconds { 19 } else { 16 };
        format!("{}{}", s.get(11..end).unwrap_or(&s), self.suffix())
    }

    fn suffix(&self) -> &'static str {
        if self.offset_secs.is_some() {
            ""
        } else {
            " UTC"
        }
    }
}

impl fmt::Display for LocalTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self.wall();
        write!(
            f,
            "{} {}{}",
            s.get(..10).unwrap_or(&s),
            s.get(11..19).unwrap_or(""),
            self.suffix()
        )
    }
}

/// Source of time. Inject it wherever behaviour depends on "now" (countdowns, stash
/// age, turn quiet periods, generation timestamps) so tests can control it.
pub trait Clock: Send + Sync + fmt::Debug {
    /// Wall-clock time (persisted timestamps, ages).
    fn now(&self) -> Timestamp;
    /// Monotonic time since an arbitrary fixed origin (countdowns, timeouts).
    fn monotonic(&self) -> Duration;
}

/// The real clock.
#[derive(Debug, Clone)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    /// Create a clock whose monotonic origin is "now".
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }

    fn monotonic(&self) -> Duration {
        self.origin.elapsed()
    }
}

/// A clock that only moves when told to. For tests.
#[derive(Debug)]
pub struct ManualClock {
    state: Mutex<(Timestamp, Duration)>,
}

impl ManualClock {
    /// Start at the given wall time with monotonic time zero.
    pub fn new(start: Timestamp) -> Self {
        Self {
            state: Mutex::new((start, Duration::ZERO)),
        }
    }

    /// Advance both wall and monotonic time.
    pub fn advance(&self, d: Duration) {
        let mut s = self.state.lock().unwrap_or_else(|p| p.into_inner());
        s.0 = s.0.saturating_add(d);
        s.1 += d;
    }

    /// Jump the wall clock (monotonic time is unaffected).
    pub fn set_now(&self, t: Timestamp) {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).0 = t;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Timestamp {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).0
    }

    fn monotonic(&self) -> Duration {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).1
    }
}

/// A pausable countdown over monotonic time (plan 7.2 `Armed` / `WaitingTurn`).
///
/// Pure value type: every method takes `now` (from [`Clock::monotonic`]) so it is
/// trivially testable and can live inside UI state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Countdown {
    total: Duration,
    started: Duration,
    paused_at: Option<Duration>,
    paused_for: Duration,
}

impl Countdown {
    /// Start a countdown of `total` at monotonic time `now`.
    pub fn start(total: Duration, now: Duration) -> Self {
        Self {
            total,
            started: now,
            paused_at: None,
            paused_for: Duration::ZERO,
        }
    }

    /// Configured length.
    pub fn total(&self) -> Duration {
        self.total
    }

    /// Time counted so far (pauses excluded).
    pub fn elapsed(&self, now: Duration) -> Duration {
        let end = self.paused_at.unwrap_or(now);
        end.saturating_sub(self.started)
            .saturating_sub(self.paused_for)
    }

    /// Time left; zero once expired.
    pub fn remaining(&self, now: Duration) -> Duration {
        self.total.saturating_sub(self.elapsed(now))
    }

    /// Elapsed fraction in `0.0..=1.0` (progress bar). A zero-length countdown is 1.0.
    pub fn progress(&self, now: Duration) -> f32 {
        if self.total.is_zero() {
            return 1.0;
        }
        (self.elapsed(now).as_secs_f32() / self.total.as_secs_f32()).clamp(0.0, 1.0)
    }

    /// True when the remaining time is zero and the countdown is not paused.
    pub fn is_expired(&self, now: Duration) -> bool {
        self.paused_at.is_none() && self.remaining(now).is_zero()
    }

    /// True while paused.
    pub fn is_paused(&self) -> bool {
        self.paused_at.is_some()
    }

    /// Pause (no-op when already paused).
    pub fn pause(&mut self, now: Duration) {
        if self.paused_at.is_none() {
            self.paused_at = Some(now);
        }
    }

    /// Resume (no-op when running).
    pub fn resume(&mut self, now: Duration) {
        if let Some(p) = self.paused_at.take() {
            self.paused_for += now.saturating_sub(p);
        }
    }

    /// Restart from the full length (re-selection while armed restarts the timer).
    pub fn restart(&mut self, now: Duration) {
        *self = Self::start(self.total, now);
    }
}
