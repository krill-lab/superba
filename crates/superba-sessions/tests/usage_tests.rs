//! Model requests read from rollout fixtures (modelled on Codex 0.162 / 0.163
//! files written by the desktop app): per-response usage records in tool loops,
//! compaction, failed and cancelled turns, the token-count fallback of older
//! Codex, partial last lines, unknown records, compressed and archived files.

use std::io::Write;

use superba_core::{CodexHome, RequestKind, RequestStatus, Timestamp};
use superba_sessions::usage::{MAX_LINE_BYTES, RolloutUsage, UsageIndex, parse_rollout};

const T1: &str = "01a1461c-18fa-73b2-8eb4-7917b838bd3e";
const T2: &str = "01a14622-ae42-72c3-ac7e-c37d351480e0";

fn ts(sec: u32, ms: u32) -> String {
    format!("2026-10-16T19:{:02}:{:02}.{ms:03}Z", sec / 60, sec % 60)
}

fn at(sec: u32, ms: u32) -> Timestamp {
    // 2026-10-16T19:00:00Z
    Timestamp::from_millis(1_792_177_200_000 + i64::from(sec) * 1000 + i64::from(ms))
}

fn line(t: &str, kind: &str, payload: serde_json::Value) -> String {
    serde_json::json!({"timestamp": t, "type": kind, "payload": payload}).to_string() + "\n"
}

fn meta(t: &str, thread: &str) -> String {
    line(
        t,
        "session_meta",
        serde_json::json!({
            "session_id": thread, "id": thread, "timestamp": t, "cwd": "/w",
            "originator": "Codex Desktop", "cli_version": "0.163.0", "source": "vscode",
            "thread_source": "user", "model_provider": "openai",
            "base_instructions": {"text": "SECRET PROMPT TEXT"}
        }),
    )
}

fn event(t: &str, payload: serde_json::Value) -> String {
    line(t, "event_msg", payload)
}

fn item(t: &str, kind: &str, role: Option<&str>) -> String {
    let mut p = serde_json::json!({"type": kind, "content": [{"type": "input_text", "text": "SECRET USER TEXT"}]});
    if let Some(r) = role {
        p["role"] = r.into();
    }
    line(t, "response_item", p)
}

fn usage(input: u64, cached: u64, output: u64) -> serde_json::Value {
    serde_json::json!({
        "input_tokens": input, "cached_input_tokens": cached, "cache_write_input_tokens": 0,
        "output_tokens": output, "reasoning_output_tokens": 0, "total_tokens": input + output
    })
}

fn record(t: &str, thread: &str, turn: &str, resp: &str, u: serde_json::Value) -> String {
    line(
        t,
        "token_usage_record",
        serde_json::json!({
            "thread_id": thread, "turn_id": turn, "session_id": thread, "root_turn_id": turn,
            "response_id": resp, "usage": u, "turn_token_usage": u, "thread_token_usage": u
        }),
    )
}

fn token_count(t: &str, total: serde_json::Value, last: serde_json::Value) -> String {
    event(
        t,
        serde_json::json!({
            "type": "token_count",
            "info": {"total_token_usage": total, "last_token_usage": last, "model_context_window": 258400},
            "rate_limits": {"limit_id": "codex"}
        }),
    )
}

fn turn_context(t: &str, turn: &str, model: &str) -> String {
    line(
        t,
        "turn_context",
        serde_json::json!({
            "turn_id": turn, "cwd": "/w", "model": model, "approval_policy": "never",
            "sandbox_policy": {"type": "read-only"},
            "collaboration_mode": {"mode": "default", "settings": {"model": model, "reasoning_effort": "low", "developer_instructions": "SECRET"}}
        }),
    )
}

fn started(t: &str, turn: &str) -> String {
    event(
        t,
        serde_json::json!({"type": "task_started", "turn_id": turn, "started_at": 1792177612, "model_context_window": 258400}),
    )
}

fn complete(t: &str, turn: &str) -> String {
    event(
        t,
        serde_json::json!({"type": "task_complete", "turn_id": turn, "last_agent_message": "SECRET ANSWER", "duration_ms": 4025}),
    )
}

/// A tool-loop turn with two responses, then a turn with a compaction, a failed
/// turn and a cancelled one; plus unknown records and a broken line.
fn session_one() -> String {
    let mut s = String::new();
    s += &meta(&ts(0, 0), T1);
    s += &event(
        &ts(0, 5),
        serde_json::json!({"type": "thread_settings_applied", "thread_settings": {"model": "gpt-6-luna", "model_provider_id": "openai", "service_tier": "priority"}}),
    );
    // Turn 1: request 1 calls a tool, request 2 answers.
    s += &started(&ts(1, 0), "turn-1");
    s += &turn_context(&ts(1, 10), "turn-1", "gpt-6-luna");
    s += &item(&ts(1, 20), "message", Some("user"));
    s += &item(&ts(3, 0), "custom_tool_call", None);
    s += &record(&ts(3, 50), T1, "turn-1", "resp_a", usage(15_818, 0, 75));
    s += &token_count(&ts(3, 51), usage(15_818, 0, 75), usage(15_818, 0, 75));
    s += &item(&ts(5, 0), "custom_tool_call_output", None);
    s += &item(&ts(7, 0), "message", Some("assistant"));
    s += &record(
        &ts(7, 100),
        T1,
        "turn-1",
        "resp_b",
        usage(16_000, 15_800, 30),
    );
    s += &token_count(
        &ts(7, 101),
        usage(31_818, 15_800, 105),
        usage(16_000, 15_800, 30),
    );
    // Rate-limit refresh: the same totals again.
    s += &token_count(
        &ts(7, 102),
        usage(31_818, 15_800, 105),
        usage(16_000, 15_800, 30),
    );
    s += &complete(&ts(7, 120), "turn-1");
    // Unknown records and garbage are skipped.
    s += &line(
        &ts(8, 0),
        "world_state",
        serde_json::json!({"full": true, "state": {}}),
    );
    s += &event(
        &ts(8, 1),
        serde_json::json!({"type": "brand_new_event", "x": [1, 2]}),
    );
    s += "{not json\n";
    s += &line(
        &ts(8, 2),
        "future_record_kind",
        serde_json::json!({"anything": true}),
    );
    // Turn 2: an answer, then a compaction response.
    s += &started(&ts(10, 0), "turn-2");
    s += &turn_context(&ts(10, 5), "turn-2", "gpt-6-astra");
    s += &item(&ts(10, 10), "message", Some("user"));
    s += &item(&ts(12, 0), "message", Some("assistant"));
    s += &record(
        &ts(12, 10),
        T1,
        "turn-2",
        "resp_c",
        usage(20_000, 16_000, 40),
    );
    s += &record(
        &ts(15, 0),
        T1,
        "turn-2",
        "resp_compact",
        usage(20_100, 0, 900),
    );
    s += &line(
        &ts(15, 5),
        "compacted",
        serde_json::json!({"message": "SECRET SUMMARY", "replacement_history": [], "compaction_response_id": "resp_compact"}),
    );
    s += &complete(&ts(15, 10), "turn-2");
    // Turn 3 fails without usage.
    s += &started(&ts(20, 0), "turn-3");
    s += &item(&ts(20, 10), "message", Some("user"));
    s += &event(
        &ts(25, 0),
        serde_json::json!({"type": "task_complete", "turn_id": "turn-3", "last_agent_message": null,
            "error": {"message": "stream disconnected: SECRET", "codex_error_info": {"response_stream_disconnected": {"http_status_code": 502}}}}),
    );
    // Turn 4 is stopped while the model works; the abort re-sends the token count.
    s += &started(&ts(30, 0), "turn-4");
    s += &item(&ts(30, 40), "message", Some("user"));
    s += &token_count(
        &ts(35, 0),
        usage(51_918, 31_800, 145),
        usage(20_100, 0, 900),
    );
    s += &item(&ts(35, 20), "message", Some("developer"));
    s += &event(
        &ts(35, 30),
        serde_json::json!({"type": "turn_aborted", "turn_id": "turn-4", "reason": "interrupted", "duration_ms": 5030}),
    );
    s
}

#[test]
fn one_row_per_response_with_derived_times() {
    let rows = parse_rollout(session_one().as_bytes());
    let ids: Vec<_> = rows.iter().map(|r| r.response_id.as_deref()).collect();
    assert_eq!(
        ids,
        [
            Some("resp_a"),
            Some("resp_b"),
            Some("resp_c"),
            Some("resp_compact"),
            None,
            None
        ]
    );
    let a = &rows[0];
    assert_eq!(a.thread_id, T1);
    assert_eq!(a.turn_id.as_deref(), Some("turn-1"));
    assert_eq!(a.model.as_deref(), Some("gpt-6-luna"));
    assert_eq!(a.effort.as_deref(), Some("low"));
    assert_eq!(a.service_tier.as_deref(), Some("priority"));
    assert_eq!(a.model_provider.as_deref(), Some("openai"));
    assert_eq!(a.originator.as_deref(), Some("Codex Desktop"));
    assert_eq!(a.cli_version.as_deref(), Some("0.163.0"));
    assert_eq!(a.thread_source.as_deref(), Some("user"));
    assert_eq!(a.status, RequestStatus::Ok);
    assert_eq!(a.kind, RequestKind::Turn);
    assert!(!a.per_turn);
    assert_eq!(
        a.usage,
        Some(RolloutUsage {
            input: 15_818,
            cached: 0,
            cache_write: 0,
            output: 75,
            reasoning: 0
        })
    );
    // Request 1 starts after the user message, request 2 after the tool output.
    assert_eq!(a.started_at, at(1, 20));
    assert_eq!(a.completed_at, at(3, 50));
    assert_eq!(a.duration_ms(), 2030);
    let b = &rows[1];
    assert_eq!(b.started_at, at(5, 0));
    assert_eq!(b.completed_at, at(7, 100));
    assert_eq!(b.usage.unwrap().cached, 15_800);
    // The model of the second turn; the compaction response is marked.
    assert_eq!(rows[2].model.as_deref(), Some("gpt-6-astra"));
    assert_eq!(rows[2].kind, RequestKind::Turn);
    assert_eq!(rows[3].kind, RequestKind::Compaction);
    assert_eq!(rows[3].started_at, at(12, 10));
    // The failed turn: one row with the error code, no usage.
    let failed = &rows[4];
    assert_eq!(
        failed.status,
        RequestStatus::Failed {
            code: Some("response_stream_disconnected".into())
        }
    );
    assert_eq!(failed.usage, None);
    assert!(failed.per_turn);
    assert_eq!(failed.started_at, at(20, 10));
    assert_eq!(failed.completed_at, at(25, 0));
    // The stopped turn: cancelled from the user message to the abort.
    let cancelled = &rows[5];
    assert_eq!(cancelled.status, RequestStatus::Cancelled);
    assert_eq!(cancelled.turn_id.as_deref(), Some("turn-4"));
    assert_eq!(cancelled.started_at, at(30, 40));
    assert_eq!(cancelled.completed_at, at(35, 30));
    // Ids are negative, distinct and stable.
    assert!(rows.iter().all(|r| r.id < 0));
    let again = parse_rollout(session_one().as_bytes());
    assert_eq!(rows, again);
    let mut unique: Vec<_> = rows.iter().map(|r| r.id).collect();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), rows.len());
    // Nothing of the content is kept.
    let dump = format!("{rows:?}");
    assert!(!dump.contains("SECRET"), "{dump}");
}

#[test]
fn older_codex_without_records_uses_token_counts_once() {
    let mut s = String::new();
    s += &meta(&ts(0, 0), T2);
    s += &started(&ts(1, 0), "t1");
    s += &turn_context(&ts(1, 1), "t1", "gpt-5.5");
    s += &item(&ts(1, 2), "message", Some("user"));
    s += &item(&ts(2, 0), "function_call", None);
    s += &token_count(&ts(2, 10), usage(1000, 0, 10), usage(1000, 0, 10));
    s += &item(&ts(3, 0), "function_call_output", None);
    s += &token_count(&ts(4, 0), usage(2100, 900, 30), usage(1100, 900, 20));
    // Re-sent on a rate-limit update and with an estimate (zeros) on resume.
    s += &token_count(&ts(4, 5), usage(2100, 900, 30), usage(1100, 900, 20));
    s += &token_count(&ts(4, 6), usage(2100, 900, 30), usage(0, 0, 0));
    s += &complete(&ts(4, 20), "t1");
    let rows = parse_rollout(s.as_bytes());
    assert_eq!(rows.len(), 2, "{rows:#?}");
    assert!(rows.iter().all(|r| r.per_turn && r.response_id.is_none()));
    assert_eq!(rows[0].usage.unwrap().input, 1000);
    assert_eq!(rows[1].usage.unwrap().input, 1100);
    assert_eq!(rows[1].started_at, at(3, 0));
    assert_eq!(rows[1].model.as_deref(), Some("gpt-5.5"));
}

#[test]
fn records_copied_from_a_parent_thread_are_not_counted() {
    let mut s = String::new();
    s += &meta(&ts(0, 0), T2);
    s += &record(&ts(0, 1), T1, "old", "resp_parent", usage(100, 0, 1));
    s += &token_count(&ts(0, 2), usage(100, 0, 1), usage(100, 0, 1));
    s += &started(&ts(1, 0), "t1");
    s += &record(&ts(2, 0), T2, "t1", "resp_child", usage(200, 0, 2));
    let rows = parse_rollout(s.as_bytes());
    let ids: Vec<_> = rows
        .iter()
        .map(|r| r.response_id.clone().unwrap())
        .collect();
    assert_eq!(ids, ["resp_child"]);
    assert_eq!(rows[0].thread_id, T2);
}

fn home() -> (tempfile::TempDir, CodexHome) {
    let tmp = tempfile::tempdir().unwrap();
    let home = CodexHome::new(tmp.path().join(".codex"));
    std::fs::create_dir_all(home.sessions_dir().join("2026/10/17")).unwrap();
    std::fs::create_dir_all(home.archived_sessions_dir()).unwrap();
    (tmp, home)
}

#[test]
fn index_tails_new_lines_and_waits_for_a_partial_last_line() {
    let (_tmp, home) = home();
    let path = home
        .sessions_dir()
        .join(format!("2026/10/17/rollout-2026-10-17T03-06-48-{T1}.jsonl"));
    let mut head = String::new();
    head += &meta(&ts(0, 0), T1);
    head += &started(&ts(1, 0), "turn-1");
    head += &item(&ts(1, 20), "message", Some("user"));
    let rec = record(&ts(3, 50), T1, "turn-1", "resp_a", usage(10, 0, 1));
    let (first, rest) = rec.split_at(40);
    std::fs::write(&path, format!("{head}{first}")).unwrap();
    let mut index = UsageIndex::new(home.clone());
    assert!(index.requests(None, None).rows.is_empty());
    // The rest of the line arrives: the row appears, read from the offset.
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    f.write_all(rest.as_bytes()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2100));
    let page = index.requests(None, None);
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.files, 1);
    // A second response in the same turn after a tool output.
    f.write_all(item(&ts(4, 0), "function_call_output", None).as_bytes())
        .unwrap();
    f.write_all(record(&ts(6, 0), T1, "turn-1", "resp_b", usage(20, 10, 2)).as_bytes())
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2100));
    let page = index.requests(None, None);
    assert_eq!(page.rows.len(), 2);
    assert_eq!(page.rows[1].started_at, at(4, 0));
    assert_eq!(
        index.row(page.rows[1].id).unwrap().response_id.as_deref(),
        Some("resp_b")
    );
    // Range filters by derived start.
    assert_eq!(index.requests(Some(at(4, 0)), None).rows.len(), 1);
    assert_eq!(index.requests(None, Some(at(4, 0))).rows.len(), 1);
}

#[test]
fn index_reads_compressed_and_archived_rollouts_and_skips_old_files() {
    let (_tmp, home) = home();
    let plain = session_one();
    let zst = zstd::stream::encode_all(plain.as_bytes(), 3).unwrap();
    std::fs::write(
        home.archived_sessions_dir()
            .join(format!("rollout-2026-10-16T19-00-00-{T1}.jsonl.zst")),
        zst,
    )
    .unwrap();
    let mut other = String::new();
    other += &meta(&ts(0, 0), T2);
    other += &started(&ts(40, 0), "x");
    other += &record(&ts(41, 0), T2, "x", "resp_x", usage(5, 0, 1));
    std::fs::write(
        home.sessions_dir()
            .join(format!("2026/10/17/rollout-2026-10-17T03-14-00-{T2}.jsonl")),
        other,
    )
    .unwrap();
    // Not a rollout name.
    std::fs::write(home.sessions_dir().join("2026/10/17/notes.jsonl"), "{}\n").unwrap();
    let mut index = UsageIndex::new(home.clone());
    let page = index.requests(None, None);
    assert_eq!(page.files, 2);
    assert_eq!(page.rows.len(), 7);
    assert!(!page.truncated);
    // Files last modified long before the range start are not read.
    let future = Timestamp::now().saturating_add(std::time::Duration::from_secs(3600));
    let page = index.requests(Some(future), None);
    assert_eq!(page.files, 0);
    assert!(page.rows.is_empty());
}

#[test]
fn overlong_lines_are_skipped_without_losing_the_next_ones() {
    let mut s = String::new();
    s += &meta(&ts(0, 0), T1);
    s += &started(&ts(1, 0), "turn-1");
    let big = "x".repeat(MAX_LINE_BYTES + 10);
    s += &line(
        &ts(1, 5),
        "response_item",
        serde_json::json!({"type": "function_call_output", "output": big}),
    );
    s += &record(&ts(2, 0), T1, "turn-1", "resp_a", usage(10, 0, 1));
    let rows = parse_rollout(s.as_bytes());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].started_at, at(1, 0));
}
