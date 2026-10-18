#![allow(missing_docs)]
//! Local time zone offsets (user bug 55(b): times were shown in UTC on Windows).

use superba_core::Timestamp;
use superba_switch::localtime;

/// 2026-01-15T12:00Z and 2026-07-15T12:00Z: winter and summer in both
/// hemispheres' daylight saving zones.
const WINTER: Timestamp = Timestamp(1_768_478_400_000);
const SUMMER: Timestamp = Timestamp(1_784_116_800_000);

#[test]
fn local_offsets_are_read_from_the_system_time_zone() {
    let now = Timestamp::now();
    let offset = localtime::offset_secs(now);
    if !cfg!(any(unix, windows)) {
        assert_eq!(offset, None);
        return;
    }
    let o = offset.expect("the local offset is known on Unix and Windows");
    assert!(o.abs() <= 14 * 3600, "offset {o} s out of range");
    assert_eq!(o % 900, 0, "offset {o} s is not a quarter hour");
    assert_eq!(localtime::local(now).offset_secs(), Some(o));
    assert_eq!(i64::from(localtime::offset_minutes(now)) * 60, o);
    // The system's own report agrees, also for a winter and a summer instant
    // (the offset of an instant uses the daylight saving state it had).
    for t in [now, WINTER, SUMMER] {
        if let Some(expected) = system_offset_at(t) {
            assert_eq!(
                localtime::offset_secs(t),
                Some(expected),
                "offset at {t} vs the system's own report"
            );
        }
    }
}

/// The system's own report of the UTC offset at `t`: .NET's
/// `TimeZoneInfo.Local` on Windows, `date +%z` on Linux and macOS. `None`
/// when the tool is missing or says something else.
fn system_offset_at(t: Timestamp) -> Option<i64> {
    let secs = t.as_unix_secs();
    let out = if cfg!(windows) {
        let script = format!(
            "[int][System.TimeZoneInfo]::Local.GetUtcOffset(\
             [DateTimeOffset]::FromUnixTimeSeconds({secs}).UtcDateTime).TotalSeconds"
        );
        let out = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
            .ok()?;
        return String::from_utf8_lossy(&out.stdout).trim().parse().ok();
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("date")
            .args(["-r", &secs.to_string(), "+%z"])
            .output()
            .ok()?
    } else {
        std::process::Command::new("date")
            .args(["-d", &format!("@{secs}"), "+%z"])
            .output()
            .ok()?
    };
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let (sign, digits) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => return None,
    };
    let h: i64 = digits.get(..2)?.parse().ok()?;
    let m: i64 = digits.get(2..4)?.parse().ok()?;
    Some(sign * (h * 3600 + m * 60))
}

/// A zone with daylight saving set through `TZ` in a child process: winter
/// and summer instants get their own offsets (America/Los_Angeles: -8 h /
/// -7 h), so a past time is not shown with today's offset.
#[cfg(unix)]
#[test]
fn local_offsets_follow_daylight_saving_of_the_instant() {
    if std::env::var_os("SUPERBA_TZ_CHILD").is_some() {
        let winter = localtime::offset_secs(WINTER);
        let summer = localtime::offset_secs(SUMMER);
        println!("offsets {winter:?} {summer:?}");
        println!(
            "shown {} {}",
            localtime::local(WINTER),
            localtime::local(SUMMER)
        );
        return;
    }
    if !std::path::Path::new("/usr/share/zoneinfo/America/Los_Angeles").exists() {
        return; // no zone database on this machine
    }
    let exe = std::env::current_exe().unwrap();
    let out = std::process::Command::new(exe)
        .args([
            "--exact",
            "local_offsets_follow_daylight_saving_of_the_instant",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("SUPERBA_TZ_CHILD", "1")
        .env("TZ", "America/Los_Angeles")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("offsets Some(-28800) Some(-25200)"),
        "child output: {stdout}"
    );
    assert!(
        stdout.contains("shown 2026-01-15 04:00:00 2026-07-15 05:00:00"),
        "child output: {stdout}"
    );
}
