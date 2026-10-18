//! Hook payloads of each harness and the hook entries Superba writes.

use pretty_assertions::assert_eq;
use superba_core::{EventKind, Timestamp};
use superba_notify::hooks::{
    self, NotifyShape, classify_notify, codex_notify_argv, is_our_hook_command, split_codex_args,
};

const NOW: Timestamp = Timestamp::from_millis(1_792_335_642_000);

/// Verbatim shape of Codex 0.140 – 0.163 `notify` (paths shortened).
const CODEX_NOTIFY: &str = r#"{"type":"agent-turn-complete","thread-id":"01a14f86-95e7-7c72-b48f-f899e9ef96c2","turn-id":"01a14f86-9636-7b23-9f7b-5c4cd4a2379d","cwd":"/home/me/src/superba","client":"codex_exec","input-messages":["say hi"],"last-assistant-message":"Done: the mock task finished."}"#;

#[test]
fn codex_notify_payload_is_a_finished_turn() {
    let e = hooks::parse("codex", CODEX_NOTIFY, NOW).unwrap().unwrap();
    assert_eq!(e.kind, EventKind::TurnComplete);
    assert_eq!(e.harness, "codex");
    assert_eq!(
        e.thread_id.as_deref(),
        Some("01a14f86-95e7-7c72-b48f-f899e9ef96c2")
    );
    assert_eq!(
        e.turn_id.as_deref(),
        Some("01a14f86-9636-7b23-9f7b-5c4cd4a2379d")
    );
    assert_eq!(e.client.as_deref(), Some("codex_exec"));
    assert_eq!(e.project().as_deref(), Some("superba"));
    assert_eq!(e.summary.as_deref(), Some("Done: the mock task finished."));
    // The prompt (input-messages) is never kept.
    assert!(!serde_json::to_string(&e).unwrap().contains("say hi"));
    // Other notification types are ignored, broken JSON is an error.
    assert_eq!(
        hooks::parse("codex", r#"{"type":"something-else"}"#, NOW).unwrap(),
        None
    );
    assert!(hooks::parse("codex", "not json", NOW).is_err());
}

#[test]
fn codex_lifecycle_hooks() {
    // Codex 0.163 Stop hook input (verbatim keys).
    let stop = r#"{"session_id":"s1","turn_id":"t1","transcript_path":"/x/rollout.jsonl","cwd":"/home/me/p","hook_event_name":"Stop","model":"gpt-6","permission_mode":"bypassPermissions","stop_hook_active":false,"last_assistant_message":"done"}"#;
    let e = hooks::parse("codex", stop, NOW).unwrap().unwrap();
    assert_eq!(
        (e.kind, e.turn_id.as_deref(), e.model.as_deref()),
        (EventKind::TurnComplete, Some("t1"), Some("gpt-6"))
    );
    assert_eq!(
        hooks::transcript_path(stop).as_deref(),
        Some("/x/rollout.jsonl")
    );
    let perm = r#"{"session_id":"s1","turn_id":"t2","cwd":"/p","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"rm -rf build"}}"#;
    let e = hooks::parse("codex", perm, NOW).unwrap().unwrap();
    assert_eq!(e.kind, EventKind::NeedsApproval);
    assert_eq!(e.detail.as_deref(), Some("Bash"));
    // The command is content: it goes out only with the summary option.
    assert_eq!(e.summary.as_deref(), Some("rm -rf build"));
    let array = r#"{"hook_event_name":"PermissionRequest","tool_name":"shell","tool_input":{"command":["git","push"]}}"#;
    assert_eq!(
        hooks::parse("codex", array, NOW)
            .unwrap()
            .unwrap()
            .summary
            .as_deref(),
        Some("git push")
    );
}

#[test]
fn claude_code_hooks() {
    // Claude Code 2.1.302 Stop / StopFailure / Notification inputs.
    let stop = r#"{"session_id":"abc","transcript_path":"/t.jsonl","cwd":"/home/me/web","permission_mode":"default","hook_event_name":"Stop","stop_hook_active":false,"last_assistant_message":"All tests pass."}"#;
    let e = hooks::parse("claude-code", stop, NOW).unwrap().unwrap();
    assert_eq!(
        (e.kind, e.harness.as_str()),
        (EventKind::TurnComplete, "claude-code")
    );
    assert_eq!(e.thread_id.as_deref(), Some("abc"));
    assert_eq!(e.turn_id, None);
    let fail = r#"{"session_id":"abc","hook_event_name":"StopFailure","error":"rate_limit","error_details":"429 from upstream","last_assistant_message":null}"#;
    let e = hooks::parse("claude-code", fail, NOW).unwrap().unwrap();
    assert_eq!(
        (e.kind, e.detail.as_deref()),
        (EventKind::TurnFailed, Some("rate_limit"))
    );
    let perm = r#"{"session_id":"abc","hook_event_name":"Notification","message":"Claude needs your permission to use Bash","notification_type":"permission_prompt"}"#;
    let e = hooks::parse("claude-code", perm, NOW).unwrap().unwrap();
    assert_eq!(
        (e.kind, e.detail.as_deref()),
        (
            EventKind::NeedsApproval,
            Some("Claude needs your permission to use Bash")
        )
    );
    let idle = r#"{"hook_event_name":"Notification","message":"Claude is waiting for your input","notification_type":"idle_prompt"}"#;
    assert_eq!(
        hooks::parse("claude-code", idle, NOW)
            .unwrap()
            .unwrap()
            .kind,
        EventKind::NeedsInput
    );
    for ignored in [
        r#"{"hook_event_name":"Notification","notification_type":"auth_success","message":"ok"}"#,
        r#"{"hook_event_name":"SubagentStop","session_id":"a"}"#,
        r#"{"hook_event_name":"SessionStart","session_id":"a"}"#,
    ] {
        assert_eq!(
            hooks::parse("claude-code", ignored, NOW).unwrap(),
            None,
            "{ignored}"
        );
    }
}

#[test]
fn generic_payload_for_other_harnesses() {
    let p = r#"{"event":"turn-complete","thread_id":"s","turn_id":"t","cwd":"/w/app","title":"Refactor","duration_ms":61000}"#;
    let e = hooks::parse("opencode", p, NOW).unwrap().unwrap();
    assert_eq!(
        (
            e.kind,
            e.harness.as_str(),
            e.title.as_deref(),
            e.duration_ms
        ),
        (
            EventKind::TurnComplete,
            "opencode",
            Some("Refactor"),
            Some(61_000)
        )
    );
    assert_eq!(
        hooks::parse("pi", r#"{"event":"nope"}"#, NOW).unwrap(),
        None
    );
}

#[test]
fn codex_notify_values_are_classified_and_chained() {
    let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
    assert_eq!(classify_notify(&[]), NotifyShape::Missing);
    assert_eq!(
        classify_notify(&s(&["/opt/superba/spb", "notify", "codex"])),
        NotifyShape::Ours {
            program: "/opt/superba/spb".into(),
            chained: vec![]
        }
    );
    assert_eq!(
        classify_notify(&s(&[
            r"C:\Program Files\Superba\spb.exe",
            "notify",
            "codex",
            "--",
            "notify-send",
            "-a"
        ])),
        NotifyShape::Ours {
            program: r"C:\Program Files\Superba\spb.exe".into(),
            chained: s(&["notify-send", "-a"])
        }
    );
    assert_eq!(
        classify_notify(&s(&["python3", "/x/notify.py"])),
        NotifyShape::Foreign(s(&["python3", "/x/notify.py"]))
    );
    // `spb` doing something else is not the hook.
    assert!(matches!(
        classify_notify(&s(&["spb", "status"])),
        NotifyShape::Foreign(_)
    ));
    assert_eq!(
        codex_notify_argv("/b/spb", &[]),
        s(&["/b/spb", "notify", "codex"])
    );
    assert_eq!(
        codex_notify_argv("/b/spb", &s(&["py", "n.py"])),
        s(&["/b/spb", "notify", "codex", "--", "py", "n.py"])
    );
    // What Codex runs: our argv plus the JSON.
    assert_eq!(
        split_codex_args(&s(&["--", "py", "n.py", "{}"])),
        (s(&["py", "n.py"]), Some("{}".into()))
    );
    assert_eq!(split_codex_args(&s(&["{}"])), (vec![], Some("{}".into())));
    assert_eq!(split_codex_args(&[]), (vec![], None));
}

#[test]
fn claude_code_hook_entries_run_in_the_background() {
    let v = hooks::claude_code_hooks("/Applications/Superba.app/Contents/MacOS/spb");
    for event in ["Stop", "StopFailure", "Notification"] {
        let handler = &v[event][0]["hooks"][0];
        assert_eq!(handler["type"], "command", "{event}");
        assert_eq!(handler["async"], true, "{event}");
        let cmd = handler["command"].as_str().unwrap();
        assert!(cmd.ends_with(" notify hook claude-code"), "{cmd}");
        assert!(is_our_hook_command(cmd), "{cmd}");
    }
    assert_eq!(
        v["Notification"][0]["matcher"],
        "permission_prompt|idle_prompt|elicitation_dialog"
    );
    assert!(!is_our_hook_command(
        "afplay /System/Library/Sounds/Glass.aiff"
    ));
    assert!(is_our_hook_command("'/a b/spb' notify hook claude-code"));
    if !cfg!(windows) {
        assert_eq!(hooks::shell_quote("/a b/spb"), "'/a b/spb'");
        assert_eq!(hooks::shell_quote("/usr/bin/spb"), "/usr/bin/spb");
    }
}
