//! Claude Code transcript usage index over a fixture configuration directory.

use std::io::Write;

use superba_core::Timestamp;
use superba_sessions::claude_usage::{ClaudeUsageIndex, ID_CEIL, ID_FLOOR};

fn line(id: &str, session: &str, ts: &str, out: u64) -> String {
    format!(
        r#"{{"type":"assistant","timestamp":"{ts}","sessionId":"{session}","requestId":"req_{id}","isSidechain":false,"message":{{"id":"{id}","model":"claude-sonnet-4-5","content":[{{"type":"text","text":"do not keep"}}],"usage":{{"input_tokens":5,"cache_creation_input_tokens":0,"cache_read_input_tokens":100,"output_tokens":{out}}}}}}}"#
    )
}

fn ts(ms: i64) -> String {
    // Fixed dates are fine: the mtime filter only uses `from`.
    let t = Timestamp::from_millis(ms);
    t.to_rfc3339()
}

#[test]
fn reads_main_and_subagent_transcripts_incrementally() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("projects").join("-home-x-proj");
    let sub = project.join("s1").join("subagents");
    std::fs::create_dir_all(&sub).unwrap();
    let now = Timestamp::now().as_millis();
    let main = project.join("s1.jsonl");
    let mut text = String::new();
    text += &line("msg_old", "s1", &ts(now - 3 * 3_600_000), 1);
    text += "\n";
    text += &line("msg_1", "s1", &ts(now - 60_000), 2);
    text += "\n";
    // An oversize line (over 16 MiB) is skipped without breaking the next one.
    text += &"x".repeat(17 * 1024 * 1024);
    text += "\n{garbage\n";
    text += &line("msg_1", "s1", &ts(now - 59_000), 9);
    text += "\n";
    std::fs::write(&main, text).unwrap();
    std::fs::write(
        sub.join("agent-a.jsonl"),
        line("msg_2", "s1", &ts(now - 30_000), 3) + "\n",
    )
    .unwrap();

    let mut index = ClaudeUsageIndex::new(dir.path().to_path_buf());
    let from = Some(Timestamp::from_millis(now - 3_600_000));
    let page = index.requests(from, None);
    assert_eq!(page.files, 2);
    let ids: Vec<&str> = page.rows.iter().map(|r| r.message_id.as_str()).collect();
    assert_eq!(ids, ["msg_1", "msg_2"]);
    assert_eq!(page.rows[0].usage.output, 9);
    assert_eq!(page.rows[0].usage.input, 105);
    assert_eq!(page.rows[0].duration_ms(), 1000);
    assert!(!page.rows[0].sidechain);
    assert!(page.rows[1].sidechain);
    assert!(
        page.rows
            .iter()
            .all(|r| (ID_FLOOR..ID_CEIL).contains(&r.id))
    );
    assert_eq!(index.row(page.rows[1].id).unwrap().message_id, "msg_2");

    // Without a range the old row is there too.
    assert_eq!(index.requests(None, None).rows.len(), 3);

    // Appended lines are picked up (a partial last line waits).
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&main)
        .unwrap();
    write!(
        f,
        "{}\n{{\"type\":",
        line("msg_3", "s1", &ts(now - 1_000), 4)
    )
    .unwrap();
    drop(f);
    std::thread::sleep(std::time::Duration::from_millis(2100));
    let page = index.requests(from, None);
    assert_eq!(page.rows.len(), 3);
}
