//! Rollout-based turn detection.

use std::time::{Duration, SystemTime};

use superba_appctl::activity::{
    ScanOpts, analyze_tail, busy_threads, parse_rfc3339, scan_rollouts, thread_id_from_file_name,
};
use superba_core::{ActivitySignal, Timestamp};

const ID: &str = "01a143a6-f4e0-7ef2-93b7-ce25598382d2";

fn line(ts: &str, kind: &str, turn: &str) -> String {
    format!(
        r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"{kind}","turn_id":"{turn}"}}}}"#
    )
}

#[test]
fn rfc3339() {
    assert_eq!(
        parse_rfc3339("1970-01-01T00:00:00Z"),
        Some(Timestamp::from_millis(0))
    );
    assert_eq!(
        parse_rfc3339("2026-10-16T07:39:40.633Z"),
        Some(Timestamp::from_millis(1_792_136_380_633))
    );
    assert_eq!(
        parse_rfc3339("2026-10-16T15:39:40.633+08:00"),
        parse_rfc3339("2026-10-16T07:39:40.633Z")
    );
    assert_eq!(
        parse_rfc3339("2000-02-29T12:00:00.5Z").map(|t| t.as_millis() % 1000),
        Some(500)
    );
    assert_eq!(parse_rfc3339("not a date"), None);
    assert_eq!(parse_rfc3339("2026-13-01T00:00:00Z"), None);
}

#[test]
fn tail_analysis() {
    let started = [
        r#"{"timestamp":"2026-10-16T07:39:40.633Z","type":"session_meta","payload":{"id":"x"}}"#.to_owned(),
        line("2026-10-16T07:39:40.633Z", "task_started", "t1"),
        r#"{"timestamp":"2026-10-16T07:39:40.751Z","type":"response_item","payload":{"type":"message"}}"#
            .to_owned(),
        line("2026-10-16T07:39:40.828Z", "item_completed", "t1"),
    ]
    .join("\n");
    let a = analyze_tail(&started);
    assert!(a.conclusive);
    assert_eq!(a.open_turn_id.as_deref(), Some("t1"));
    assert_eq!(
        a.open_turn_started_at,
        parse_rfc3339("2026-10-16T07:39:40.633Z")
    );

    let done = format!(
        "{started}\n{}\n{}",
        line("2026-10-16T07:44:51.803Z", "task_complete", "t1"),
        r#"{"timestamp":"2026-10-16T07:46:17.705Z","type":"event_msg","payload":{"type":"thread_settings_applied"}}"#
    );
    let a = analyze_tail(&done);
    assert!(a.conclusive);
    assert_eq!(a.open_turn_started_at, None);
    assert_eq!(a.last_event_at, parse_rfc3339("2026-10-16T07:46:17.705Z"));

    let aborted = format!(
        "{}\n{}\n{}",
        line("2026-10-16T08:00:00Z", "turn_started", "a"),
        line("2026-10-16T08:00:05Z", "turn_aborted", "a"),
        line("2026-10-16T08:00:06Z", "task_started", "b"),
    );
    assert_eq!(analyze_tail(&aborted).open_turn_id.as_deref(), Some("b"));

    // A partial first line and no turn events: inconclusive.
    let a = analyze_tail(
        "ial\":1}\n{\"timestamp\":\"2026-10-16T08:00:00Z\",\"type\":\"response_item\"}",
    );
    assert!(!a.conclusive);
    assert!(a.last_event_at.is_some());
}

#[test]
fn thread_ids_from_names() {
    assert_eq!(
        thread_id_from_file_name(&format!("rollout-2026-10-16T15-39-37-{ID}.jsonl")).as_deref(),
        Some(ID)
    );
    assert_eq!(
        thread_id_from_file_name(&format!("rollout-2026-10-16T15-39-37-{ID}.jsonl.zst")).as_deref(),
        Some(ID)
    );
    assert_eq!(thread_id_from_file_name("rollout-short.jsonl"), None);
    assert_eq!(thread_id_from_file_name("notes.jsonl"), None);
}

fn write_rollout(dir: &std::path::Path, id: &str, body: &str, age: Duration) -> std::path::PathBuf {
    let day = dir.join("2026/10/16");
    std::fs::create_dir_all(&day).unwrap();
    let p = day.join(format!("rollout-2026-10-16T15-39-37-{id}.jsonl"));
    std::fs::write(&p, body).unwrap();
    let f = std::fs::File::options().write(true).open(&p).unwrap();
    f.set_modified(SystemTime::now() - age).unwrap();
    p
}

#[test]
fn scan_and_busy_rules() {
    let tmp = tempfile::tempdir().unwrap();
    let sessions = tmp.path().join("sessions");
    let now = Timestamp::now();
    let started_at = now.saturating_sub(Duration::from_secs(60));
    let open = line(&started_at.to_rfc3339(), "task_started", "t1");
    let id_open = "11111111-1111-4111-8111-111111111111";
    let id_old = "22222222-2222-4222-8222-222222222222";
    let id_quiet = "33333333-3333-4333-8333-333333333333";
    write_rollout(&sessions, id_open, &open, Duration::from_secs(5));
    write_rollout(&sessions, id_old, &open, Duration::from_secs(3600));
    write_rollout(
        &sessions,
        id_quiet,
        r#"{"timestamp":"2026-10-16T08:00:00Z","type":"response_item"}"#,
        Duration::from_secs(2),
    );
    std::fs::write(sessions.join("2026/10/16/other.txt"), "x").unwrap();

    let opts = ScanOpts::at(now);
    let findings = scan_rollouts(&sessions, &opts).unwrap();
    let ids: Vec<_> = findings
        .iter()
        .filter_map(|f| f.thread_id.clone())
        .collect();
    assert!(ids.contains(&id_open.to_owned()));
    assert!(ids.contains(&id_quiet.to_owned()));
    assert!(!ids.contains(&id_old.to_owned()), "outside the tail window");

    // No live consumer: nothing can be in flight.
    assert!(busy_threads(&findings, &[], &opts).is_empty());

    // Live process older than the turn: busy (tail), plus the quiet file (mtime).
    let live_old = [Some(started_at.saturating_sub(Duration::from_secs(600)))];
    let busy = busy_threads(&findings, &live_old, &opts);
    let open_t = busy
        .iter()
        .find(|t| t.thread_id.as_deref() == Some(id_open))
        .unwrap();
    assert_eq!(open_t.signal, ActivitySignal::RolloutTail);
    let quiet_t = busy
        .iter()
        .find(|t| t.thread_id.as_deref() == Some(id_quiet))
        .unwrap();
    assert_eq!(quiet_t.signal, ActivitySignal::RolloutMtime);

    // Only processes started after the turn (a restart killed its owner): the open
    // turn is dead, not busy.
    let live_new = [Some(now)];
    let busy = busy_threads(&findings, &live_new, &opts);
    assert!(busy.iter().all(|t| t.thread_id.as_deref() != Some(id_open)));

    // Unknown start times count as old.
    let busy = busy_threads(&findings, &[None], &opts);
    assert!(busy.iter().any(|t| t.thread_id.as_deref() == Some(id_open)));

    // Missing dir is not an error.
    assert!(
        scan_rollouts(&tmp.path().join("nope"), &opts)
            .unwrap()
            .is_empty()
    );
}
