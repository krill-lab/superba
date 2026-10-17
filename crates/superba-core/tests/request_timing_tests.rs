//! Request timing marks (user bug 12): normalization never lets a mark go
//! before an earlier one and keeps 0 ms phases; TPS has a guard for windows of
//! zero or a few ms.

use superba_core::{RequestTiming, TPS_MIN_OUTPUT, TPS_MIN_WINDOW_MS, tps_milli};

fn marks(t: &RequestTiming) -> [Option<u64>; 6] {
    [
        t.sent_ms,
        t.headers_ms,
        t.first_event_ms,
        t.first_token_ms,
        t.last_delta_ms,
        t.end_ms,
    ]
}

#[test]
fn equal_marks_are_valid_and_kept() {
    // Headers, first event and first token read in one go.
    let t = RequestTiming {
        sent_ms: Some(3),
        headers_ms: Some(640),
        first_event_ms: Some(640),
        first_token_ms: Some(640),
        last_delta_ms: Some(4210),
        end_ms: Some(4226),
    };
    assert_eq!(t.normalized(), t);
    assert_eq!(t.tps_window_ms(), Some(3586));
}

#[test]
fn marks_out_of_order_are_raised_never_negative() {
    // Raw marks out of order: first event before the headers, first token
    // before the first event.
    let t = RequestTiming {
        sent_ms: Some(5),
        headers_ms: Some(700),
        first_event_ms: Some(660),
        first_token_ms: Some(650),
        last_delta_ms: None,
        end_ms: Some(690),
    };
    let n = t.normalized();
    assert_eq!(
        marks(&n),
        [Some(5), Some(700), Some(700), Some(700), None, Some(700)]
    );
    let known: Vec<u64> = marks(&n).into_iter().flatten().collect();
    assert!(known.windows(2).all(|w| w[0] <= w[1]), "{known:?}");
    assert_eq!(n.tps_window_ms(), Some(0));
    // Normalizing twice changes nothing.
    assert_eq!(n.normalized(), n);
}

#[test]
fn missing_marks_stay_missing() {
    // A 429 without events: headers and end only; a WebSocket request has no
    // headers of its own.
    let t = RequestTiming {
        sent_ms: Some(1),
        headers_ms: Some(180),
        end_ms: Some(181),
        ..RequestTiming::default()
    };
    assert_eq!(t.normalized(), t);
    assert_eq!(t.tps_window_ms(), None);
    let ws = RequestTiming {
        sent_ms: Some(0),
        first_event_ms: Some(410),
        first_token_ms: Some(430),
        last_delta_ms: Some(2100),
        end_ms: Some(2110),
        ..RequestTiming::default()
    };
    assert_eq!(ws.normalized(), ws);
}

#[test]
fn tps_needs_a_real_window_and_some_output() {
    assert_eq!(tps_milli(287, 640, 4226), Some(287 * 1_000_000 / 3586));
    // Everything in one read: no window at all.
    assert_eq!(tps_milli(287, 640, 640), None);
    // A few ms (would be tens of thousands of tok/s).
    assert_eq!(tps_milli(287, 640, 652), None);
    assert_eq!(tps_milli(287, 640, 640 + TPS_MIN_WINDOW_MS - 1), None);
    assert_eq!(
        tps_milli(287, 640, 640 + TPS_MIN_WINDOW_MS),
        Some(287 * 1_000_000 / TPS_MIN_WINDOW_MS)
    );
    // Out of order never divides.
    assert_eq!(tps_milli(287, 900, 640), None);
    assert_eq!(tps_milli(TPS_MIN_OUTPUT - 1, 0, 5000), None);
    assert_eq!(tps_milli(TPS_MIN_OUTPUT, 0, 5000), Some(2000));
    // Huge numbers saturate instead of wrapping.
    assert_eq!(tps_milli(u64::MAX, 0, 200), None);
}
