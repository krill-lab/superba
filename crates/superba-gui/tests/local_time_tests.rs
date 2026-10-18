//! The GUI shows times in local time on every platform (user bug 62(b): on
//! Windows every time was shown in UTC with a "UTC" suffix).

#![allow(missing_docs)]

use superba_core::Timestamp;
use superba_gui::pages::{self, router::requests};

#[test]
fn clock_and_table_times_use_the_system_time_zone() {
    let t = Timestamp(1_792_332_623_456); // 2026-10-18T14:10:23.456Z
    let local = superba_switch::localtime::local(t);
    assert_eq!(pages::clock_time(t, true), local.time_of_day(true));
    assert_eq!(pages::clock_time(t, false), local.time_of_day(false));
    assert_eq!(requests::full_time(t), local.to_string());
    assert_eq!(requests::local_day(t), local.date());
    assert_eq!(
        pages::local_offset_secs(t),
        superba_switch::localtime::offset_secs(t)
    );
    if cfg!(any(unix, windows)) {
        assert!(pages::local_offset_secs(t).is_some());
        assert!(!pages::clock_time(t, true).ends_with("UTC"));
    }
}
