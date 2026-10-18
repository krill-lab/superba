//! Local time for people to read (user bug 55(b): the CLI and, on Windows, the
//! GUI showed times in UTC).
//!
//! [`offset_secs`] reads the local time zone's offset from UTC at an instant
//! from the system's time-zone rules, so a past instant gets the daylight
//! saving state it had:
//!
//! - Unix: `localtime_r` (`TZ`, else `/etc/localtime`). Superba never changes
//!   its own environment at run time, so `localtime_r` reading `TZ` cannot race
//!   a `setenv` (the reason the `time` crate refuses local offsets in
//!   multi-threaded Unix programs);
//! - Windows: the dynamic time-zone information of the signed-in user's zone
//!   (`GetDynamicTimeZoneInformation`, including the per-year daylight rules)
//!   applied with `SystemTimeToTzSpecificLocalTimeEx`;
//! - elsewhere: unknown, and times are shown in UTC with a "UTC" suffix.
//!
//! [`local`] pairs a [`Timestamp`] with that offset as a
//! [`superba_core::LocalTime`] (`2026-10-18 07:10:23`). Machine output (JSON,
//! logs, journals) keeps RFC 3339 UTC.

use superba_core::{LocalTime, Timestamp};

/// The local time zone's offset from UTC at `t`, in seconds east of UTC;
/// `None` where it cannot be read.
pub fn offset_secs(t: Timestamp) -> Option<i64> {
    imp::offset_secs(t)
}

/// `t` as local wall-clock time.
pub fn local(t: Timestamp) -> LocalTime {
    LocalTime::with_offset(t, offset_secs(t))
}

/// The local UTC offset at `t` in whole minutes (0 where unknown), for
/// grouping by local day.
pub fn offset_minutes(t: Timestamp) -> i32 {
    offset_secs(t)
        .and_then(|s| i32::try_from(s.div_euclid(60)).ok())
        .unwrap_or(0)
}

#[cfg(unix)]
mod imp {
    use superba_core::Timestamp;

    pub(super) fn offset_secs(t: Timestamp) -> Option<i64> {
        let secs = libc::time_t::try_from(t.as_unix_secs()).ok()?;
        let mut tm = std::mem::MaybeUninit::<libc::tm>::zeroed();
        // SAFETY: localtime_r is the reentrant variant: it reads `secs` and
        // writes only the caller's `tm`, which is valid for writes.
        let r = unsafe { libc::localtime_r(&secs, tm.as_mut_ptr()) };
        if r.is_null() {
            return None;
        }
        // SAFETY: localtime_r succeeded, so it filled `tm` (zeroed before).
        let tm = unsafe { tm.assume_init() };
        // `c_long`: i64 on the 64-bit targets, i32 on 32-bit ones.
        #[allow(clippy::useless_conversion)]
        let offset = i64::from(tm.tm_gmtoff);
        Some(offset)
    }
}

#[cfg(windows)]
mod imp {
    use superba_core::Timestamp;
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{
        DYNAMIC_TIME_ZONE_INFORMATION, FileTimeToSystemTime, GetDynamicTimeZoneInformation,
        SystemTimeToFileTime, SystemTimeToTzSpecificLocalTimeEx, TIME_ZONE_ID_INVALID,
    };

    /// 100 ns intervals from 1601-01-01 (FILETIME's epoch) to 1970-01-01.
    const UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;

    fn ticks_of(ft: FILETIME) -> i64 {
        (i64::from(ft.dwHighDateTime) << 32) | i64::from(ft.dwLowDateTime)
    }

    pub(super) fn offset_secs(t: Timestamp) -> Option<i64> {
        let ticks = t
            .as_unix_secs()
            .checked_mul(10_000_000)?
            .checked_add(UNIX_EPOCH_TICKS)
            .filter(|v| *v >= 0)?;
        let utc_ft = FILETIME {
            dwLowDateTime: (ticks & 0xFFFF_FFFF) as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        };
        let mut tz = DYNAMIC_TIME_ZONE_INFORMATION::default();
        let mut utc = SYSTEMTIME::default();
        let mut local = SYSTEMTIME::default();
        let mut local_ft = FILETIME::default();
        // SAFETY: every pointer is to a live, properly sized local; the calls
        // only read their inputs and write their outputs.
        let ok = unsafe {
            GetDynamicTimeZoneInformation(&mut tz) != TIME_ZONE_ID_INVALID
                && FileTimeToSystemTime(&utc_ft, &mut utc) != 0
                && SystemTimeToTzSpecificLocalTimeEx(&tz, &utc, &mut local) != 0
                && SystemTimeToFileTime(&local, &mut local_ft) != 0
        };
        ok.then(|| (ticks_of(local_ft) - ticks).div_euclid(10_000_000))
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use superba_core::Timestamp;

    pub(super) fn offset_secs(_t: Timestamp) -> Option<i64> {
        None
    }
}
