//! The comment-keeping YAML editor (dsh patch files): every edit changes only
//! its entry, keeps comments, `!!js` expressions and layout, and is refused
//! (nothing changed) when it cannot be made safely.

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use superba_config::harness::slot::SlotPath;
use superba_config::harness::yaml::{YamlDoc, to_value};

fn fixture(name: &str) -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/harness/dsh")
            .join(name),
    )
    .unwrap()
}

fn provider_path(id: &str) -> SlotPath {
    SlotPath::find(&[("id", "llm-pi-ai")])
        .then_key("config")
        .then_key("providers")
        .then_key(id)
}

fn superba_provider() -> Value {
    json!({
        "displayName": "Superba · Krill",
        "apiKeyEnv": "SUPERBA_API_KEY",
        "api": "openai-responses",
        "baseURL": "https://line.example/codex/v1",
        "models": [
            { "id": "gpt-5.6-luna", "name": "GPT-5.6 Luna", "contextWindow": 272000,
              "input": ["text", "image"],
              "reasoningEfforts": { "low": "low", "medium": "medium", "high": "high", "xhigh": "xhigh" } },
            { "id": "4o" }
        ]
    })
}

fn default_model() -> Value {
    json!({ "id": "agent-default-model", "config": { "provider": "superba", "model": "gpt-5.6-luna" } })
}

/// Set then remove (and take out the containers the set created, as a
/// restore does): the text must come back byte for byte.
fn round_trip(text: &str, path: &SlotPath, value: &Value) -> String {
    use superba_config::harness::slot;
    let mut doc = YamlDoc::parse(text).unwrap();
    let created = slot::missing_containers(doc.value(), path);
    doc.set(path, Some(value)).unwrap();
    let written = doc.text().to_owned();
    assert_eq!(
        slot::get(&to_value(&written).unwrap(), path),
        Some(value),
        "{written}"
    );
    doc.set(path, None).unwrap();
    for c in created.iter().rev() {
        let v = slot::get(doc.value(), c).cloned().unwrap();
        assert!(slot::is_skeleton(&v, c.last()), "{c} = {v}");
        doc.set(c, None).unwrap();
    }
    assert_eq!(doc.text(), text, "after removing again");
    written
}

#[test]
fn fresh_profile_patch_gets_a_block_and_back_to_its_empty_list() {
    let text = fixture("fresh-profile-patch.yml");
    assert!(text.contains("[]"));
    let mut doc = YamlDoc::parse(&text).unwrap();
    doc.set(&provider_path("superba"), Some(&superba_provider()))
        .unwrap();
    doc.set(
        &SlotPath::find(&[("id", "agent-default-model")]),
        Some(&default_model()),
    )
    .unwrap();
    let written = doc.text().to_owned();
    // The comments stay at the top, the list becomes a block.
    let comments: Vec<&str> = text.lines().filter(|l| l.starts_with('#')).collect();
    assert!(written.starts_with(&comments.join("\n")));
    assert!(!written.contains("[]"));
    assert!(written.contains("- id: llm-pi-ai\n  config:\n    providers:\n      superba:\n        displayName: Superba · Krill\n"), "{written}");
    assert!(
        written.contains(
            "        models:\n          - id: gpt-5.6-luna\n            name: GPT-5.6 Luna\n"
        ),
        "{written}"
    );
    // A model id that looks like a number is quoted.
    assert!(written.contains("- id: \"4o\""), "{written}");
    assert!(
        written.contains(
            "- id: agent-default-model\n  config:\n    provider: superba\n    model: gpt-5.6-luna\n"
        ),
        "{written}"
    );
    let v = to_value(&written).unwrap();
    assert_eq!(v[0]["config"]["providers"]["superba"], superba_provider());
    assert_eq!(v[1], default_model());
    // Taking both out again gives the file dsh created, byte for byte.
    doc.set(&SlotPath::find(&[("id", "agent-default-model")]), None)
        .unwrap();
    doc.set(&provider_path("superba"), None).unwrap();
    doc.set(
        &SlotPath::find(&[("id", "llm-pi-ai")])
            .then_key("config")
            .then_key("providers"),
        None,
    )
    .unwrap();
    doc.set(
        &SlotPath::find(&[("id", "llm-pi-ai")]).then_key("config"),
        None,
    )
    .unwrap();
    doc.set(&SlotPath::find(&[("id", "llm-pi-ai")]), None)
        .unwrap();
    assert_eq!(doc.text(), text);
}

#[test]
fn adding_next_to_the_users_providers_keeps_js_comments_and_layout() {
    let original = format!(
        "# my dsh overrides\n{}",
        fixture("research-js-patch.yml").replacen(
            "      krill:\n",
            "      # the relay I use\n      krill:   # mine\n",
            1
        )
    );
    let written = round_trip(&original, &provider_path("superba"), &superba_provider());
    // Every original line is still there, in order; only new lines appear.
    let mut lines = written.lines();
    for l in original.lines() {
        assert!(lines.any(|w| w == l), "lost line {l:?} in\n{written}");
    }
    assert!(written.contains("baseURL: !!js process.env.KRILL_BASE + '/codex/v1'"));
    let v = to_value(&written).unwrap();
    assert_eq!(
        v[0]["config"]["providers"]["krill"]["baseURL"],
        json!({ "$yaml_tag": "!!js", "value": "process.env.KRILL_BASE + '/codex/v1'" })
    );
    // Superba's provider comes after the user's, at the same indentation.
    let k = written.find("      krill:").unwrap();
    let s = written.find("      superba:").unwrap();
    assert!(s > k);
}

#[test]
fn replacing_superbas_provider_changes_only_its_lines() {
    let text = fixture("research-patch.yml");
    let mut doc = YamlDoc::parse(&text).unwrap();
    doc.set(&provider_path("superba"), Some(&superba_provider()))
        .unwrap();
    let first = doc.text().to_owned();
    let mut next = superba_provider();
    next["models"] = json!([{ "id": "gpt-5.5" }]);
    next["api"] = json!("openai-completions");
    doc.set(&provider_path("superba"), Some(&next)).unwrap();
    let second = doc.text().to_owned();
    assert!(second.contains("api: openai-completions"));
    assert!(!second.contains("gpt-5.6-luna\n            name"));
    // The user's three providers and the default-model entry are untouched.
    for id in ["superba-resp", "superba-chat", "superba-anth"] {
        assert_eq!(
            to_value(&second).unwrap()[0]["config"]["providers"][id],
            to_value(&text).unwrap()[0]["config"]["providers"][id]
        );
    }
    assert!(second.ends_with(
        "- id: agent-default-model\n  config:\n    provider: superba-resp\n    model: fake-gpt\n"
    ));
    doc.set(&provider_path("superba"), None).unwrap();
    assert_eq!(doc.text(), text);
    assert_ne!(first, second);
}

#[test]
fn whole_entries_replace_and_come_back_as_values() {
    let text = fixture("research-efforts-patch.yml");
    let mut doc = YamlDoc::parse(&text).unwrap();
    let before = to_value(&text).unwrap()[1].clone();
    doc.set(
        &SlotPath::find(&[("id", "agent-default-model")]),
        Some(&default_model()),
    )
    .unwrap();
    assert!(doc.text().contains("    model: gpt-5.6-luna\n"));
    assert!(
        doc.text()
            .contains("reasoningEfforts:\n              low: low")
    );
    // Restoring the user's entry from its value.
    doc.set(
        &SlotPath::find(&[("id", "agent-default-model")]),
        Some(&before),
    )
    .unwrap();
    assert_eq!(doc.text(), text);
}

#[test]
fn insert_entries_and_flow_style_neighbours_are_left_alone() {
    let text = fixture("research-insert-patch.yml");
    // A top-level `- insert:` item is not an id entry: a new llm-pi-ai entry
    // is added after it.
    let written = round_trip(&text, &provider_path("superba"), &superba_provider());
    assert!(written.starts_with("# Managed by Superba — whole file is regenerated.\n- insert:\n"));
}

#[test]
fn flow_style_in_the_way_is_refused_without_changes() {
    let text = "- id: llm-pi-ai\n  config: {providers: {mine: {api: openai-responses}}}\n";
    let mut doc = YamlDoc::parse(text).unwrap();
    let err = doc
        .set(&provider_path("superba"), Some(&superba_provider()))
        .unwrap_err();
    assert!(err.contains("flow style"), "{err}");
    assert_eq!(doc.text(), text);
}

#[test]
fn empty_flow_and_empty_values_become_blocks_and_back() {
    for text in [
        "- id: llm-pi-ai\n  config:\n    providers: {}\n",
        "- id: llm-pi-ai\n  config:\n    providers: {}   # none yet\n- id: other\n",
    ] {
        let mut doc = YamlDoc::parse(text).unwrap();
        doc.set(&provider_path("superba"), Some(&superba_provider()))
            .unwrap();
        // A comment after the empty value stays on the key's line.
        let comment = if text.contains("# none yet") {
            "   # none yet"
        } else {
            ""
        };
        assert!(
            doc.text()
                .contains(&format!("    providers:{comment}\n      superba:\n")),
            "{}",
            doc.text()
        );
        doc.set(&provider_path("superba"), None).unwrap();
        assert_eq!(doc.text(), text);
    }
    // An empty value (null) takes a block too.
    let text = "- id: llm-pi-ai\n  config:\n- id: next\n";
    let mut doc = YamlDoc::parse(text).unwrap();
    doc.set(&provider_path("superba"), Some(&superba_provider()))
        .unwrap();
    assert!(
        doc.text()
            .starts_with("- id: llm-pi-ai\n  config:\n    providers:\n      superba:\n")
    );
    assert!(doc.text().ends_with("- id: next\n"));
}

#[test]
fn non_ascii_comments_and_crlf_files() {
    let text = "# 我的配置 — überall\r\n- id: llm-pi-ai   # 注释\r\n  config:\r\n    providers:\r\n      mine:\r\n        api: openai-responses\r\n";
    let written = round_trip(text, &provider_path("superba"), &superba_provider());
    assert!(
        !written.replace("\r\n", "").contains('\n'),
        "only CRLF: {written:?}"
    );
    assert!(written.contains("      superba:\r\n        displayName: Superba · Krill\r\n"));
}

#[test]
fn comments_only_and_empty_files() {
    for text in ["", "# nothing yet\n", "# nothing yet"] {
        let mut doc = YamlDoc::parse(text).unwrap();
        doc.set(
            &SlotPath::find(&[("id", "agent-default-model")]),
            Some(&default_model()),
        )
        .unwrap();
        let w = doc.text().to_owned();
        assert!(
            w.contains("- id: agent-default-model\n  config:\n"),
            "{w:?}"
        );
        assert!(w.starts_with(text.trim_end_matches('\n')));
        assert_eq!(to_value(&w).unwrap(), json!([default_model()]));
    }
}

#[test]
fn removing_the_only_entry_leaves_an_empty_list() {
    let text = "# keep me\n- id: agent-default-model\n  config:\n    provider: a\n    model: b\n";
    let mut doc = YamlDoc::parse(text).unwrap();
    doc.set(&SlotPath::find(&[("id", "agent-default-model")]), None)
        .unwrap();
    assert_eq!(doc.text(), "# keep me\n[]\n");
}

#[test]
fn anchors_aliases_and_several_documents_are_refused() {
    for text in [
        "- &a {id: x}\n- *a\n",
        "- id: llm-pi-ai\n  config: &c\n    providers: {}\n",
        "- id: a\n---\n- id: b\n",
    ] {
        assert!(YamlDoc::parse(text).is_err(), "{text}");
    }
}

#[test]
fn duplicate_entries_are_refused() {
    let text = "- id: llm-pi-ai\n  config: {}\n- id: llm-pi-ai\n  disabled: true\n";
    let mut doc = YamlDoc::parse(text).unwrap();
    let err = doc
        .set(&provider_path("superba"), Some(&superba_provider()))
        .unwrap_err();
    assert!(err.contains("several"), "{err}");
    assert_eq!(doc.text(), text);
}

#[test]
fn block_scalars_before_the_insert_point_keep_their_text() {
    let text = "- id: llm-pi-ai\n  note: |\n    line one\n    line two\n  config:\n    providers:\n      mine:\n        prompt: >-\n          folded\n          text\n# trailing comment\n";
    let written = round_trip(text, &provider_path("superba"), &superba_provider());
    assert!(written.ends_with("# trailing comment\n"));
    let v = to_value(&written).unwrap();
    assert_eq!(v[0]["note"], json!("line one\nline two\n"));
    assert_eq!(
        v[0]["config"]["providers"]["mine"]["prompt"],
        json!("folded text")
    );
}

#[test]
fn strings_that_yaml_would_read_otherwise_are_quoted() {
    let tricky = [
        "yes",
        "No",
        "on",
        "null",
        "~",
        "123",
        "1.5",
        "0x1f",
        "1e3",
        "-fast",
        "a: b",
        "x #y",
        "[x]",
        "{x}",
        "*a",
        "&a",
        "!tag",
        "",
        " lead",
        "trail ",
        "multi\nline",
        "it's",
        "\"q\"",
        "12:30",
        "1_000",
        ".inf",
        "@at",
        "%p",
        "`b`",
        "a,b",
        "café: ok",
    ];
    for s in tricky {
        let mut doc = YamlDoc::parse("[]\n").unwrap();
        let v = json!({ "id": "x", "value": s });
        doc.set(&SlotPath::find(&[("id", "x")]), Some(&v)).unwrap();
        assert_eq!(
            to_value(doc.text()).unwrap(),
            json!([v]),
            "{s:?} -> {}",
            doc.text()
        );
    }
}

#[test]
fn numbers_booleans_and_nested_lists_round_trip() {
    let v = json!({
        "id": "llm-pi-ai",
        "config": { "providers": { "superba": {
            "contextWindow": 272000, "ratio": 0.5, "on": true, "off": false, "nothing": null,
            "list": [[1, 2], [], {}, { "a": [ { "b": 1 } ] }],
            "empty": {}, "none": []
        } } }
    });
    let mut doc = YamlDoc::parse("# c\n[]\n").unwrap();
    doc.set(&SlotPath::find(&[("id", "llm-pi-ai")]), Some(&v))
        .unwrap();
    assert_eq!(to_value(doc.text()).unwrap(), json!([v]), "{}", doc.text());
    doc.set(&SlotPath::find(&[("id", "llm-pi-ai")]), None)
        .unwrap();
    assert_eq!(doc.text(), "# c\n[]\n");
}

#[test]
fn tagged_values_are_written_back_with_their_tag() {
    let v = json!({ "id": "x", "config": { "root": { "$yaml_tag": "!!js", "value": "dshHomePath('sessions')" } } });
    let mut doc = YamlDoc::parse("[]\n").unwrap();
    doc.set(&SlotPath::find(&[("id", "x")]), Some(&v)).unwrap();
    assert!(
        doc.text()
            .contains("root: !!js \"dshHomePath('sessions')\""),
        "{}",
        doc.text()
    );
    assert_eq!(to_value(doc.text()).unwrap(), json!([v]));
}

#[test]
fn a_last_line_without_newline_is_handled() {
    let text = "- id: a\n  config:\n    providers:\n      mine:\n        api: x";
    let written = round_trip(text, &provider_path("superba"), &superba_provider());
    assert!(written.contains("        api: x\n"), "{written}");
    let text = "- id: llm-pi-ai\n  config:\n    providers:\n      mine:\n        api: x";
    let written = round_trip(text, &provider_path("superba"), &superba_provider());
    assert!(
        written.contains("      mine:\n        api: x\n      superba:"),
        "{written}"
    );
}

#[test]
fn a_key_after_a_list_dash_is_not_removed_by_line() {
    // `id` shares its line with `- `: removing the entry's first key would
    // need re-indenting; the editor refuses rather than guesses.
    let text = "- id: x\n  y: 1\n";
    let mut doc = YamlDoc::parse(text).unwrap();
    let err = doc
        .set(&SlotPath::find(&[("id", "x")]).then_key("id"), None)
        .unwrap_err();
    assert!(!err.is_empty());
    assert_eq!(doc.text(), text);
}

#[test]
fn every_research_patch_round_trips() {
    for name in [
        "research-patch.yml",
        "research-js-patch.yml",
        "research-efforts-patch.yml",
        "research-insert-patch.yml",
        "fresh-profile-patch.yml",
    ] {
        let text = fixture(name);
        round_trip(&text, &provider_path("superba"), &superba_provider());
        round_trip(
            &text,
            &SlotPath::find(&[("id", "session-telemetry-otel")]),
            &json!({ "id": "session-telemetry-otel", "disabled": true }),
        );
    }
}
