//! Turn facts from Codex rollouts and Claude Code transcripts, and the
//! watcher that finds failed Codex turns.

use pretty_assertions::assert_eq;
use superba_core::{EventKind, Timestamp, TurnTokens};
use superba_notify::enrich::{self, claude_turn, codex_turn, find_rollout, parse_rfc3339};
use superba_notify::policy::PolicyState;
use superba_notify::watch;

const THREAD: &str = "01a14f87-7d0b-70d1-8c96-5892b6d8dcd5";

/// Two turns in the shape of Codex 0.163 (0.140 / 0.150 have `turn_id` at the
/// top of `task_started` and no `turn_attribution`, both handled).
fn rollout() -> String {
    [
        r#"{"timestamp":"2026-10-18T15:00:00.000Z","type":"session_meta","payload":{"id":"01a14f87-7d0b-70d1-8c96-5892b6d8dcd5","cwd":"/w/superba"}}"#,
        r#"{"timestamp":"2026-10-18T15:00:01.000Z","type":"event_msg","payload":{"type":"task_started","turn_id":"t1","started_at":1792335601}}"#,
        r#"{"timestamp":"2026-10-18T15:00:01.010Z","type":"turn_context","payload":{"turn_id":"t1","cwd":"/w/superba","model":"gpt-6.1-sol"}}"#,
        r#"{"timestamp":"2026-10-18T15:00:05.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":50}}}}"#,
        r#"{"timestamp":"2026-10-18T15:00:06.000Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t1","last_agent_message":"first","duration_ms":5000}}"#,
        r#"{"timestamp":"2026-10-18T15:01:00.000Z","type":"event_msg","payload":{"type":"task_started","turn_attribution":{"turn_id":"t2"},"turn_id":"t2"}}"#,
        r#"{"timestamp":"2026-10-18T15:01:00.010Z","type":"turn_context","payload":{"turn_id":"t2","cwd":"/w/superba","model":"gpt-6.1-sol"}}"#,
        r#"{"timestamp":"2026-10-18T15:01:30.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":2500,"cached_input_tokens":1800,"output_tokens":350}}}}"#,
        r#"{"timestamp":"2026-10-18T15:03:12.500Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t2","last_agent_message":"second"}}"#,
    ]
    .join("\n")
        + "\n"
}

#[test]
fn codex_turn_facts() {
    let text = rollout();
    let t2 = codex_turn(&text, Some("t2"));
    assert!(t2.started && t2.ended);
    // No duration_ms: from the line timestamps.
    assert_eq!(t2.duration_ms, Some(132_500));
    assert_eq!(
        t2.tokens,
        Some(TurnTokens {
            input: 1500,
            cached: 1000,
            output: 300
        })
    );
    assert_eq!(t2.model.as_deref(), Some("gpt-6.1-sol"));
    assert_eq!(t2.cwd.as_deref(), Some("/w/superba"));
    assert_eq!(t2.last_message.as_deref(), Some("second"));
    let t1 = codex_turn(&text, Some("t1"));
    assert_eq!(
        (t1.duration_ms, t1.tokens.map(|t| t.output)),
        (Some(5000), Some(50))
    );
    // The last turn when no id is given; a turn still running has no end.
    assert_eq!(codex_turn(&text, None).turn_id.as_deref(), Some("t2"));
    let running: String = text.lines().take(8).collect::<Vec<_>>().join("\n");
    let r = codex_turn(&running, Some("t2"));
    assert!(r.started && !r.ended);
}

#[test]
fn rollouts_are_found_by_thread_id() {
    let dir = tempfile::tempdir().unwrap();
    let day = dir.path().join("2026/10/18");
    std::fs::create_dir_all(&day).unwrap();
    std::fs::create_dir_all(dir.path().join("2026/10/17")).unwrap();
    let file = day.join(format!("rollout-2026-10-18T15-00-41-{THREAD}.jsonl"));
    std::fs::write(&file, rollout()).unwrap();
    assert_eq!(find_rollout(dir.path(), THREAD, 7), Some(file.clone()));
    assert_eq!(find_rollout(dir.path(), "../../etc", 7), None);
    assert_eq!(
        find_rollout(dir.path(), "01a14f87-0000-0000-0000-000000000000", 7),
        None
    );
    assert_eq!(
        watch::thread_id_from_stem(&format!("rollout-2026-10-18T15-00-41-{THREAD}")).as_deref(),
        Some(THREAD)
    );
    // The tail reader drops a partial first line.
    let tail = enrich::read_tail(&file).unwrap();
    assert!(tail.ends_with('\n'));
}

#[test]
fn claude_transcript_facts() {
    let text = [
        r#"{"type":"user","uuid":"u0","timestamp":"2026-10-18T10:00:00.000Z","message":{"role":"user","content":"old prompt"}}"#,
        r#"{"type":"user","uuid":"u1","timestamp":"2026-10-18T10:05:00.000Z","cwd":"/w/web","message":{"role":"user","content":[{"type":"text","text":"fix it"}]}}"#,
        r#"{"type":"assistant","timestamp":"2026-10-18T10:05:10.000Z","message":{"model":"gpt-6.1-sol","usage":{"input_tokens":100,"cache_read_input_tokens":900,"cache_creation_input_tokens":0,"output_tokens":40}}}"#,
        r#"{"type":"user","uuid":"u2","timestamp":"2026-10-18T10:05:20.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"x","content":"ok"}]}}"#,
        r#"{"type":"assistant","timestamp":"2026-10-18T10:06:00.000Z","message":{"model":"gpt-6.1-sol","usage":{"input_tokens":50,"cache_read_input_tokens":1000,"cache_creation_input_tokens":20,"output_tokens":60}}}"#,
    ]
    .join("\n");
    let now = parse_rfc3339("2026-10-18T10:06:30Z").unwrap();
    let f = claude_turn(&text, now);
    assert_eq!(f.turn_id.as_deref(), Some("u1"));
    assert_eq!(f.duration_ms, Some(90_000));
    assert_eq!(
        f.tokens,
        Some(TurnTokens {
            input: 2070,
            cached: 1900,
            output: 100
        })
    );
    assert_eq!(f.model.as_deref(), Some("gpt-6.1-sol"));
    assert_eq!(f.cwd.as_deref(), Some("/w/web"));
}

#[test]
fn rfc3339_parsing() {
    assert_eq!(
        parse_rfc3339("1970-01-01T00:00:00Z"),
        Some(Timestamp::from_millis(0))
    );
    assert_eq!(
        parse_rfc3339("2026-10-18T15:00:41.974Z").map(|t| t.as_millis()),
        Some(1_792_335_641_974)
    );
    assert_eq!(
        parse_rfc3339("2026-10-18T23:00:41.974+08:00").map(|t| t.as_millis()),
        Some(1_792_335_641_974)
    );
    assert_eq!(parse_rfc3339("garbage"), None);
}

#[test]
fn watcher_reports_failed_turns_once() {
    let dir = tempfile::tempdir().unwrap();
    let day = dir.path().join("2026/10/18");
    std::fs::create_dir_all(&day).unwrap();
    let file = day.join(format!("rollout-2026-10-18T15-00-41-{THREAD}.jsonl"));
    let now = Timestamp::now();
    let ts = now.to_rfc3339();
    std::fs::write(
        &file,
        format!("{{\"timestamp\":\"{ts}\",\"type\":\"session_meta\",\"payload\":{{}}}}\n"),
    )
    .unwrap();
    let mut state = PolicyState::default();
    // The first scan only takes bookmarks.
    assert!(watch::scan(dir.path(), &mut state, now).is_empty());
    let failed = format!(
        "{{\"timestamp\":\"{ts}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_complete\",\"turn_id\":\"tf\",\"last_agent_message\":null,\"error\":{{\"message\":\"We're currently experiencing high demand\"}}}}}}\n\
         {{\"timestamp\":\"{ts}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_complete\",\"turn_id\":\"tok\",\"last_agent_message\":\"fine\"}}}}\n"
    );
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .unwrap();
    std::io::Write::write_all(&mut f, failed.as_bytes()).unwrap();
    let later = Timestamp::now();
    let events = watch::scan(dir.path(), &mut state, later);
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert_eq!(e.kind, EventKind::TurnFailed);
    assert_eq!(e.thread_id.as_deref(), Some(THREAD));
    assert_eq!(e.turn_id.as_deref(), Some("tf"));
    assert_eq!(
        e.detail.as_deref(),
        Some("We're currently experiencing high demand")
    );
    // Nothing new: nothing reported.
    assert!(watch::scan(dir.path(), &mut state, Timestamp::now()).is_empty());
    // A new rollout created after the last scan is read from its start.
    std::thread::sleep(std::time::Duration::from_millis(30));
    let file2 = day.join("rollout-2026-10-18T15-10-00-01a14f87-7d0b-70d1-8c96-000000000002.jsonl");
    std::fs::write(&file2, failed).unwrap();
    let events = watch::scan(dir.path(), &mut state, Timestamp::now());
    assert_eq!(
        events
            .iter()
            .map(|e| e.turn_id.clone().unwrap())
            .collect::<Vec<_>>(),
        vec!["tf"]
    );
}
