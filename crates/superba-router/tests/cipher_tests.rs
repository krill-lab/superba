//! Ciphertexts another provider made (GitHub issue 11): finding them in a
//! Codex request, recognising a refusal, neutralising exactly the chosen
//! items byte-preservingly, the plan after a refusal and the ledger.

mod common;

use std::collections::BTreeSet;

use common::ciphers::*;
use serde_json::{Value, json};
use superba_router::cipher::rewrite::{COMPACTION_NOTE, OMITTED};
use superba_router::cipher::{
    self, Ledger, Named, Place, Plan, Verdict, neutralise, rejection, scan,
};
use superba_router::compaction::fingerprint;

fn bytes(v: &Value) -> Vec<u8> {
    serde_json::to_vec(v).unwrap()
}

/// The parent conversation after a third-party provider: its reasoning, the
/// spawn call, a notes output, an assistant message with an encrypted copy.
fn parent() -> Value {
    body(vec![
        developer("<permissions instructions>…"),
        user("Spawn a worker agent to count the files, then report."),
        reasoning("rs_r01", &cipher("third-party-r01")),
        spawn_call(TASK, None),
        notes_call("call_n1"),
        notes_output("fco_01a14f90-4268", "call_n1", &cipher("notes-out")),
        assistant("The repository has 3 files.", Some(&cipher("replay-msg"))),
        user("Continue."),
    ])
}

/// The sub-agent's own request: its task is the last input item.
fn child(payload: &str) -> Value {
    body(vec![
        developer("<collaboration_mode>…"),
        user("Spawn a worker agent to count the files, then report."),
        agent_task(TASK_ID, payload),
    ])
}

#[test]
fn every_kind_of_ciphertext_is_found_where_codex_puts_it() {
    let s = scan(&bytes(&parent()));
    let places: Vec<(usize, Place)> = s.ciphers.iter().map(|c| (c.item, c.place)).collect();
    assert_eq!(
        places,
        vec![
            (2, Place::Reasoning),
            (5, Place::Part { plaintext: false }),
            (6, Place::Field),
        ]
    );
    assert_eq!(s.ciphers[0].item_id.as_deref(), Some("rs_r01"));
    assert_eq!(s.ciphers[0].fp, fingerprint(&cipher("third-party-r01")));
    assert_eq!(
        s.task_item, None,
        "the parent's last input is a user message"
    );

    // The sub-agent: the task is found and its plain text is recognised.
    let s = scan(&bytes(&child(TASK)));
    assert_eq!(s.ciphers.len(), 1);
    assert_eq!(s.ciphers[0].place, Place::Part { plaintext: true });
    assert_eq!(s.ciphers[0].item_id.as_deref(), Some(TASK_ID));
    assert_eq!(s.task_item, Some(2));
    // A trailing request control does not hide the task.
    let mut b = child(&cipher("task"));
    b["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type": "compaction_trigger"}));
    let s = scan(&bytes(&b));
    assert_eq!(s.ciphers[0].place, Place::Part { plaintext: false });
    assert_eq!(s.task_item, Some(2));

    // Encrypted arguments (an OpenAI backend's spawn call) and compaction.
    let b = body(vec![
        user("go"),
        spawn_call(&cipher("spawn-arg"), Some(&["message"])),
        compaction(&cipher("cmp")),
    ]);
    let s = scan(&bytes(&b));
    let places: Vec<Place> = s.ciphers.iter().map(|c| c.place).collect();
    assert_eq!(places, vec![Place::Argument, Place::Compaction]);
    assert_eq!(s.ciphers[0].fp, fingerprint(&cipher("spawn-arg")));

    // Nothing encrypted: nothing parsed, nothing found.
    assert!(scan(&bytes(&body(vec![user("hi")]))).is_empty());
    assert!(scan(b"not json encrypted").is_empty());
}

#[test]
fn only_the_chosen_items_change_and_everything_else_keeps_its_bytes() {
    let original = bytes(&parent());
    let all: BTreeSet<String> = scan(&original).ciphers.into_iter().map(|c| c.fp).collect();
    let (out, removed) = neutralise(&original, &|fp| all.contains(fp)).unwrap();
    assert_eq!(removed.reasoning, 1);
    assert_eq!(removed.parts_omitted, 1);
    assert_eq!(removed.fields, 1);
    assert_eq!(removed.total(), 3);

    let v: Value = serde_json::from_slice(&out).unwrap();
    let input = v["input"].as_array().unwrap();
    assert_eq!(input.len(), 7, "the reasoning item is left out");
    assert!(input.iter().all(|i| i["type"] != "reasoning"));
    assert_eq!(
        input[4]["output"],
        json!([{"type": "input_text", "text": OMITTED}])
    );
    assert_eq!(input[4]["id"], "fco_01a14f90-4268", "the item keeps its id");
    assert!(input[5].get("encrypted_content").is_none());
    assert_eq!(
        input[5]["content"][0]["text"],
        "The repository has 3 files."
    );

    // Untouched items and every other member are byte for byte the same.
    let text = String::from_utf8(out.clone()).unwrap();
    let orig = String::from_utf8(original.clone()).unwrap();
    for item in [0usize, 1, 3, 4, 7] {
        let raw = serde_json::to_string(&parent()["input"][item]).unwrap();
        assert!(orig.contains(&raw));
        assert!(text.contains(&raw), "item {item} keeps its bytes");
    }
    for key in ["tools", "client_metadata", "include", "text"] {
        let raw = serde_json::to_string(&parent()[key]).unwrap();
        assert!(text.contains(&format!("\"{key}\":{raw}")), "{key}");
    }
    let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
    let orig_v: Value = serde_json::from_slice(&original).unwrap();
    let orig_keys: Vec<&String> = orig_v.as_object().unwrap().keys().collect();
    assert_eq!(keys, orig_keys, "member order kept");

    // The same choice gives the same bytes every time (prompt cache).
    assert_eq!(
        neutralise(&original, &|fp| all.contains(fp)).unwrap().0,
        out
    );
    // Nothing chosen: no rewrite.
    assert!(neutralise(&original, &|_| false).is_none());
}

#[test]
fn a_task_in_plain_text_stays_readable_and_a_real_ciphertext_becomes_a_note() {
    let (out, removed) = neutralise(&bytes(&child(TASK)), &|_| true).unwrap();
    assert_eq!(removed.parts_as_text, 1);
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(
        v["input"][2]["content"],
        json!([{"type": "input_text", "text": TASK_HEADER}, {"type": "input_text", "text": TASK}])
    );
    assert_eq!(v["input"][2]["type"], "agent_message");
    assert_eq!(v["input"][2]["author"], "/root");

    // Encrypted arguments: replaced, no longer declared encrypted.
    let b = bytes(&body(vec![
        user("go"),
        spawn_call(&cipher("spawn-arg"), Some(&["message"])),
    ]));
    let (out, removed) = neutralise(&b, &|_| true).unwrap();
    assert_eq!(removed.arguments, 1);
    let v: Value = serde_json::from_slice(&out).unwrap();
    let call = &v["input"][1];
    assert!(call.get("encrypted_function_args").is_none());
    let args: Value = serde_json::from_str(call["arguments"].as_str().unwrap()).unwrap();
    assert_eq!(args, json!({"task_name": "worker", "message": OMITTED}));

    // A compaction item becomes a developer note.
    let b = bytes(&body(vec![
        user("hi"),
        compaction(&cipher("cmp")),
        user("go on"),
    ]));
    let (out, removed) = neutralise(&b, &|_| true).unwrap();
    assert_eq!(removed.compactions, 1);
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["input"][1]["role"], "developer");
    assert_eq!(v["input"][1]["content"][0]["text"], COMPACTION_NOTE);
    assert!(
        removed.describe().contains("compacted history"),
        "{}",
        removed.describe()
    );
}

#[test]
fn refusals_are_recognised_with_what_they_name() {
    let r = rejection::from_body(refusal_for_item(TASK_ID).as_bytes()).unwrap();
    assert_eq!(r.named, Named::Item(TASK_ID.into()));
    let r = rejection::from_body(refusal_for_item("rs_r01").as_bytes()).unwrap();
    assert_eq!(r.named, Named::Item("rs_r01".into()));
    let r = rejection::from_body(refusal_for_ends(&cipher("x")).as_bytes()).unwrap();
    let (head, tail) = ends(&cipher("x"));
    assert_eq!(r.named, Named::Ends(head, tail));
    assert!(r.message.contains("could not be verified"));
    let r = rejection::from_body(REFUSAL_FUNCTION_OUTPUT.as_bytes()).unwrap();
    assert_eq!(r.named, Named::Parts);

    // WebSocket events: `error` (nested or flat) and `response.failed`.
    let ev = refusal_for_ends(&cipher("x"));
    assert!(rejection::from_event(ev.as_bytes()).is_some());
    let flat = json!({"type": "error", "code": "invalid_encrypted_content",
                      "message": "Encrypted function output content could not be decrypted or decoded."});
    assert_eq!(
        rejection::from_event(flat.to_string().as_bytes())
            .unwrap()
            .named,
        Named::Parts
    );
    let failed = json!({"type": "response.failed", "response": {"id": "resp_1", "status": "failed",
        "error": {"code": "invalid_encrypted_content", "message": "The encrypted content for item rs_9 could not be verified."}}});
    assert_eq!(
        rejection::from_event(failed.to_string().as_bytes())
            .unwrap()
            .named,
        Named::Item("rs_9".into())
    );

    // Other errors are not refusals of a ciphertext.
    for other in [
        r#"{"error":{"message":"Rate limit reached","type":"rate_limit_error","code":"rate_limit_exceeded"}}"#,
        r#"{"error":{"message":"Invalid 'input[3].content'","type":"invalid_request_error","code":"invalid_value"}}"#,
        r#"{"detail":"encrypted"}"#,
        "Bad Gateway",
    ] {
        assert!(rejection::from_body(other.as_bytes()).is_none(), "{other}");
        assert!(rejection::from_event(other.as_bytes()).is_none(), "{other}");
    }
    assert!(rejection::from_event(br#"{"type":"response.completed","x":"encrypted"}"#).is_none());

    assert!(rejection::may_be(
        400,
        Some("application/json; charset=utf-8")
    ));
    assert!(rejection::may_be(502, Some("application/json")));
    assert!(!rejection::may_be(400, Some("text/event-stream")));
    assert!(!rejection::may_be(429, Some("application/json")));
    assert!(!rejection::may_be(200, Some("application/json")));
}

#[test]
fn a_refusal_leaves_out_what_it_names_and_what_the_member_never_read() {
    let b = bytes(&body(vec![
        user("go"),
        reasoning("rs_own", &cipher("own")),
        reasoning("rs_foreign", &cipher("foreign")),
        compaction(&cipher("cmp")),
        user("next"),
    ]));
    let s = scan(&b);
    let own = fingerprint(&cipher("own"));
    let foreign = fingerprint(&cipher("foreign"));
    let reads = |fp: &String| *fp == own;
    let r = rejection::from_body(refusal_for_item("rs_foreign").as_bytes()).unwrap();
    assert_eq!(
        cipher::plan(&s, &r, reads),
        Plan::Neutralise(BTreeSet::from([foreign.clone()])),
        "its own reasoning stays; compaction only when named"
    );
    // Unknown items go too (one retry must be enough).
    assert_eq!(
        cipher::plan(&s, &r, |_| false),
        Plan::Neutralise(BTreeSet::from([own.clone(), foreign.clone()]))
    );
    // A named compaction item is replaced.
    let r = rejection::from_body(refusal_for_ends(&cipher("cmp")).as_bytes()).unwrap();
    let Plan::Neutralise(set) = cipher::plan(&s, &r, |_| true) else {
        panic!("a plan");
    };
    assert_eq!(set, BTreeSet::from([fingerprint(&cipher("cmp"))]));
    // Four and four characters are not an identity: every ciphertext with
    // those ends is left out, and such a name never decides on the task.
    let twin = format!("{}twin{}", &cipher("cmp")[..60], &cipher("cmp")[60..]);
    let b2 = bytes(&body(vec![
        user("go"),
        reasoning("rs_twin", &twin),
        compaction(&cipher("cmp")),
    ]));
    let r = rejection::from_body(refusal_for_ends(&cipher("cmp")).as_bytes()).unwrap();
    let Plan::Neutralise(set) = cipher::plan(&scan(&b2), &r, |_| true) else {
        panic!("a plan");
    };
    assert_eq!(set.len(), 2);
    let mut task = child(&cipher("cmp"));
    task["input"]
        .as_array_mut()
        .unwrap()
        .insert(1, reasoning("rs_twin", &twin));
    assert_eq!(
        cipher::plan(&scan(&bytes(&task)), &r, |_| true),
        Plan::Neutralise(BTreeSet::from([fingerprint(&twin)])),
        "an ambiguous name keeps the task and leaves out the other one"
    );
    // Everything named is known readable and nothing else: pass it on.
    let r = rejection::from_body(refusal_for_item("rs_unknown").as_bytes()).unwrap();
    assert_eq!(cipher::plan(&s, &r, |_| true), Plan::PassThrough);
}

#[test]
fn a_sub_agent_keeps_its_task() {
    // Plain text in the task's part: neutralised into readable text.
    let s = scan(&bytes(&child(TASK)));
    let r = rejection::from_body(refusal_for_item(TASK_ID).as_bytes()).unwrap();
    assert_eq!(
        cipher::plan(&s, &r, |_| false),
        Plan::Neutralise(BTreeSet::from([fingerprint(TASK)]))
    );
    // A real ciphertext the member cannot read: never sent without it.
    let s = scan(&bytes(&child(&cipher("task"))));
    assert_eq!(cipher::plan(&s, &r, |_| false), Plan::TaskUnreadable);
    let parts = rejection::from_body(REFUSAL_FUNCTION_OUTPUT.as_bytes()).unwrap();
    assert_eq!(cipher::plan(&s, &parts, |_| false), Plan::TaskUnreadable);
    // Other items go, the task stays.
    let mut b = child(&cipher("task"));
    b["input"]
        .as_array_mut()
        .unwrap()
        .insert(1, reasoning("rs_x", &cipher("x")));
    let s = scan(&bytes(&b));
    assert_eq!(
        cipher::plan(&s, &parts, |_| false),
        Plan::Neutralise(BTreeSet::from([fingerprint(&cipher("x"))]))
    );
}

#[test]
fn the_ledger_remembers_refusals_over_reads() {
    let l = Ledger::new();
    let (a, b) = (fingerprint("a"), fingerprint("b"));
    assert_eq!(l.verdict("krill", &a), None);
    l.learn("krill", [&a, &b], Verdict::Reads);
    assert!(l.reads("krill", &a));
    assert!(!l.reads("site", &a), "per member");
    l.learn("krill", [&a], Verdict::Refuses);
    assert!(l.refuses("krill", &a));
    l.learn("krill", [&a], Verdict::Reads);
    assert!(l.refuses("krill", &a), "a read never replaces a refusal");
    assert_eq!(l.len(), 2);

    // `refused` remembers what its plan leaves out.
    let s = scan(&bytes(&body(vec![
        user("go"),
        reasoning("rs_1", &cipher("one")),
    ])));
    let r = rejection::from_body(refusal_for_item("rs_1").as_bytes()).unwrap();
    assert!(matches!(
        cipher::refused(&l, "site", &s, &r),
        Plan::Neutralise(_)
    ));
    assert!(l.refuses("site", &fingerprint(&cipher("one"))));
}

#[test]
fn the_ledger_is_bounded() {
    let l = Ledger::new();
    let fps: Vec<String> = (0..cipher::ledger::MAX_ENTRIES + 10)
        .map(|i| fingerprint(&i.to_string()))
        .collect();
    l.learn("krill", fps.iter(), Verdict::Reads);
    assert!(l.len() <= cipher::ledger::MAX_ENTRIES);
    // The most recent ones stay.
    assert!(l.reads("krill", fps.last().unwrap()));
}
