//! Session browser list and exports (Markdown, Claude Code, pi) against a
//! fixture rollout.

use std::fs;
use std::path::Path;

use rusqlite::Connection;
use superba_core::{CodexHome, SessionExportFormat, Timestamp};
use superba_sessions::browse;
use superba_sessions::export::{self, Block, Role, Targets};

const FIXTURE: &str = include_str!("fixtures/rollout-export.jsonl");
const TID: &str = "019a2f5b-6a7b-7d46-8e85-4f0a6b8c2d05";

fn now() -> Timestamp {
    Timestamp(1_792_400_000_000)
}

#[test]
fn rollout_parses_into_a_lossy_transcript() {
    let t = export::parse(FIXTURE.as_bytes());
    assert_eq!(t.id, TID);
    assert_eq!(t.cwd, "/home/u/w/superba");
    assert_eq!(t.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(t.compactions, 1);
    assert_eq!(t.encrypted, 1);
    // Summary note, the user's question (+ image), then one assistant message.
    assert_eq!(t.messages.len(), 2);
    assert_eq!(t.messages[0].role, Role::User);
    let user = &t.messages[0].blocks;
    assert!(matches!(&user[0], Block::Text(s) if s.contains("table users was renamed")));
    assert!(matches!(&user[1], Block::Text(s) if s == "Plan the database migration"));
    assert_eq!(user[2], Block::Image);
    let text = format!("{:?}", t.messages);
    assert!(
        !text.contains("Old question"),
        "compaction drops older turns"
    );
    assert!(!text.contains("AGENTS.md") && !text.contains("environment_context"));
    assert!(
        !text.contains("gAAAA"),
        "encrypted reasoning is never exported"
    );
    let tools: Vec<_> = t.messages[1]
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Tool { name, output, .. } => Some((name.as_str(), output.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(tools[0].0, "shell");
    assert!(tools[0].1.as_deref().unwrap().contains("002_users.sql"));
    assert_eq!(tools[1].0, "apply_patch");
}

#[test]
fn markdown_has_turns_tools_and_reasoning_summary() {
    let md = export::markdown(
        &export::parse(FIXTURE.as_bytes()),
        "Plan the database migration",
    );
    assert!(md.starts_with("# Plan the database migration\n"));
    assert!(md.contains(&format!("- Codex thread: `{TID}`")));
    assert!(md.contains("## User") && md.contains("## Assistant"));
    assert!(md.contains("> **Planning** the steps"));
    assert!(md.contains("**Tool `shell`**"));
    assert!(md.contains("Added `003.sql`"));
    assert!(!md.contains("gAAAA"));
}

#[test]
fn claude_code_lines_chain_and_alternate() {
    let t = export::parse(FIXTURE.as_bytes());
    let out = export::claude_jsonl(&t, "sess-1", now());
    let lines: Vec<serde_json::Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["type"], "user");
    assert_eq!(lines[0]["parentUuid"], serde_json::Value::Null);
    assert_eq!(lines[1]["type"], "assistant");
    assert_eq!(lines[1]["parentUuid"], lines[0]["uuid"]);
    assert_eq!(lines[1]["sessionId"], "sess-1");
    assert_eq!(lines[1]["cwd"], "/home/u/w/superba");
    assert_eq!(lines[1]["message"]["role"], "assistant");
    let text = lines[1]["message"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("[tool call: shell]") && text.contains("Added `003.sql`"));
    assert!(!text.contains("Planning"), "reasoning is not sent as text");
    assert_eq!(
        export::claude_slug("/home/u/w/super_ba.x"),
        "-home-u-w-super-ba-x"
    );
}

#[test]
fn pi_session_has_header_and_parent_chain() {
    let t = export::parse(FIXTURE.as_bytes());
    let out = export::pi_jsonl(&t, "sess-2", now());
    let lines: Vec<serde_json::Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines[0]["type"], "session");
    assert_eq!(lines[0]["version"], 3);
    assert_eq!(lines[0]["id"], "sess-2");
    assert_eq!(lines[1]["type"], "message");
    assert_eq!(lines[1]["parentId"], serde_json::Value::Null);
    assert_eq!(lines[1]["message"]["role"], "user");
    assert_eq!(lines[2]["parentId"], lines[1]["id"]);
    assert_eq!(lines[2]["message"]["role"], "assistant");
    assert_eq!(export::pi_slug("/home/u/w"), "--home-u-w--");
    assert_eq!(export::pi_slug("C:\\Users\\u"), "--C--Users-u--");
}

fn home_with_rollout(dir: &Path) -> CodexHome {
    let codex = dir.join(".codex");
    let day = codex.join("sessions/2026/10/20");
    fs::create_dir_all(&day).unwrap();
    fs::write(
        day.join(format!("rollout-2026-10-20T08-00-00-{TID}.jsonl")),
        FIXTURE,
    )
    .unwrap();
    CodexHome::new(codex)
}

#[test]
fn exports_write_new_files_only_outside_codex() {
    let dir = tempfile::tempdir().unwrap();
    let home = home_with_rollout(dir.path());
    let before = fs::read(home.sessions_dir().join(format!(
        "2026/10/20/rollout-2026-10-20T08-00-00-{TID}.jsonl"
    )))
    .unwrap();
    let targets = Targets {
        claude_dir: dir.path().join(".claude"),
        pi_dir: dir.path().join(".pi/agent"),
    };
    let md = export::export(
        &home,
        TID,
        None,
        "T",
        SessionExportFormat::Markdown,
        &targets,
        now(),
    )
    .unwrap();
    assert!(md.path.is_none() && md.text.starts_with("# T"));
    let cc = export::export(
        &home,
        TID,
        None,
        "T",
        SessionExportFormat::ClaudeCode,
        &targets,
        now(),
    )
    .unwrap();
    let path = cc.path.unwrap();
    assert!(path.starts_with(dir.path().join(".claude/projects/-home-u-w-superba")));
    assert!(
        cc.text
            .ends_with(&format!("claude --resume {}", cc.session_id.unwrap()))
    );
    let pi = export::export(
        &home,
        TID,
        None,
        "T",
        SessionExportFormat::Pi,
        &targets,
        now(),
    )
    .unwrap();
    assert!(
        pi.path
            .unwrap()
            .starts_with(dir.path().join(".pi/agent/sessions/--home-u-w-superba--"))
    );
    let after = fs::read(home.sessions_dir().join(format!(
        "2026/10/20/rollout-2026-10-20T08-00-00-{TID}.jsonl"
    )))
    .unwrap();
    assert_eq!(before, after, "the rollout is never changed");
    assert!(
        export::export(
            &home,
            "missing",
            None,
            "",
            SessionExportFormat::Markdown,
            &targets,
            now()
        )
        .is_err()
    );
}

#[test]
fn list_merges_state_threads_and_the_app_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let home = home_with_rollout(dir.path());
    let state = Connection::open(home.root().join("state_5.sqlite")).unwrap();
    state
        .execute_batch(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT, created_at INTEGER,
               updated_at INTEGER, model_provider TEXT, cwd TEXT, title TEXT, name TEXT,
               first_user_message TEXT, model TEXT, tokens_used INTEGER, archived INTEGER);
             INSERT INTO threads VALUES ('t-local', '/r.jsonl', 1792300000, 1792300600, 'openai',
               '/w/a', '', NULL, 'Fix   flaky\ntest', 'gpt-5.5', 1234, 0);
             INSERT INTO threads VALUES ('t-old', NULL, 1700000000, 1700000000, 'krill',
               '/w/b', 'Old', 'Named', '', NULL, 0, 1);",
        )
        .unwrap();
    drop(state);
    fs::create_dir_all(home.root().join("sqlite")).unwrap();
    let app = Connection::open(home.root().join("sqlite/codex-dev.db")).unwrap();
    app.execute_batch(
        "CREATE TABLE local_thread_catalog (host_id TEXT NOT NULL, thread_id TEXT NOT NULL,
           display_title TEXT NOT NULL, source_created_at REAL NOT NULL,
           source_updated_at REAL NOT NULL, cwd TEXT NOT NULL, source_kind TEXT NOT NULL,
           source_detail TEXT, model_provider TEXT NOT NULL, git_branch TEXT,
           observation_sequence INTEGER NOT NULL, missing_candidate INTEGER NOT NULL DEFAULT 0,
           PRIMARY KEY (host_id, thread_id));
         CREATE TABLE local_thread_catalog_hosts (host_id TEXT PRIMARY KEY, host_kind TEXT NOT NULL);
         INSERT INTO local_thread_catalog_hosts VALUES ('remote-ssh-discovered:bj', 'ssh');
         INSERT INTO local_thread_catalog VALUES ('local', 't-local', 'ignored', 1792300000,
           1792300900, '/w/a', 'vscode', NULL, 'openai', 'main', 1, 0);
         INSERT INTO local_thread_catalog VALUES ('remote-ssh-discovered:bj', 't-remote',
           'Remote work', 1792300000, 1792301000, '/srv/app', 'vscode', NULL, 'openai', NULL, 2, 0);
         INSERT INTO local_thread_catalog VALUES ('remote-ssh-discovered:bj', 't-gone',
           'Gone', 1, 2, '/x', 'vscode', NULL, 'openai', NULL, 3, 1);",
    )
    .unwrap();
    drop(app);
    let list = browse::list(&home, None);
    let ids: Vec<&str> = list.entries.iter().map(|e| e.thread_id.as_str()).collect();
    assert_eq!(ids, ["t-remote", "t-local", "t-old"]);
    let local = &list.entries[1];
    assert_eq!(local.title, "Fix flaky test");
    assert_eq!(local.tokens_used, Some(1234));
    assert_eq!(
        local.git_branch.as_deref(),
        Some("main"),
        "filled from the catalog"
    );
    assert_eq!(local.updated_at, Some(Timestamp(1_792_300_900_000)));
    assert_eq!(local.host_kind.as_deref(), Some("local"));
    let remote = &list.entries[0];
    assert_eq!(remote.host_kind.as_deref(), Some("ssh"));
    assert!(!remote.is_local());
    assert_eq!(list.entries[2].title, "Named");
    assert!(list.entries[2].archived);
    assert!(!list.truncated);
}
