//! One-key edits of the tools' JSON / JSONC files keep everything else.

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use superba_tools::jsonedit::{
    get_path, jsonc_get, jsonc_object, jsonc_remove, jsonc_set, parse_object, print_object,
    remove_path, set_path,
};

#[test]
fn strict_json_paths() {
    let mut obj = parse_object(
        Some(br#"{"model":"x","env":{"A":"1"},"permissions":{"allow":[]}}"#),
        "s",
    )
    .unwrap();
    set_path(&mut obj, "env.DISABLE_AUTOUPDATER", json!("1")).unwrap();
    assert_eq!(get_path(&obj, "env.DISABLE_AUTOUPDATER"), Some(&json!("1")));
    let text = String::from_utf8(print_object(&obj)).unwrap();
    // Order kept, new key last in its object.
    let model = text.find("\"model\"").unwrap();
    let env = text.find("\"env\"").unwrap();
    let perms = text.find("\"permissions\"").unwrap();
    assert!(model < env && env < perms, "{text}");
    assert!(text.find("\"A\"").unwrap() < text.find("DISABLE_AUTOUPDATER").unwrap());
    assert_eq!(
        remove_path(&mut obj, "env.DISABLE_AUTOUPDATER", true),
        Some(json!("1"))
    );
    assert_eq!(
        obj.get("env"),
        Some(&json!({"A": "1"})),
        "env kept: it has A"
    );
    remove_path(&mut obj, "env.A", true);
    assert!(obj.get("env").is_none(), "an emptied object is pruned");
    // Not an object on the way: refused.
    let mut bad = parse_object(Some(br#"{"env":"x"}"#), "s").unwrap();
    assert!(set_path(&mut bad, "env.K", json!("1")).is_err());
    assert!(parse_object(Some(b"[1]"), "s").is_err());
    assert!(parse_object(Some(b"{broken"), "s").is_err());
    assert!(parse_object(None, "s").unwrap().is_empty());
}

const OPENCODE: &str = r#"{
  // my settings
  "$schema": "https://opencode.ai/config.json",
  "model": "krill/gpt-5.6-luna", /* inline */
  "provider": {
    "krill": { "options": { "baseURL": "https://x/v1" } },
  },
}
"#;

#[test]
fn jsonc_set_inserts_first_and_keeps_comments() {
    let out = jsonc_set(OPENCODE, "update", "\"disable\"", "oc").unwrap();
    assert!(out.contains("// my settings"));
    assert!(out.contains("/* inline */"));
    assert!(out.contains("\"provider\""));
    let obj = jsonc_object(&out, "oc").unwrap();
    assert_eq!(obj.get("update"), Some(&json!("disable")));
    assert_eq!(obj.get("model"), Some(&json!("krill/gpt-5.6-luna")));
    assert_eq!(jsonc_get(&out, "update").as_deref(), Some("\"disable\""));
    // Set again: replaced in place.
    let again = jsonc_set(&out, "update", "\"notify\"", "oc").unwrap();
    assert_eq!(again.matches("\"update\"").count(), 1);
    assert_eq!(
        jsonc_object(&again, "oc").unwrap().get("update"),
        Some(&json!("notify"))
    );
    // Remove: back to the original text.
    let back = jsonc_remove(&again, "update", "oc").unwrap();
    assert_eq!(back, OPENCODE);
}

#[test]
fn jsonc_remove_handles_first_middle_last() {
    let text = "{\n  \"a\": 1,\n  \"b\": {\"x\": [1, 2]},\n  \"c\": \"z\"\n}\n";
    for key in ["a", "b", "c"] {
        let out = jsonc_remove(text, key, "t").unwrap();
        let obj = jsonc_object(&out, "t").unwrap();
        assert!(obj.get(key).is_none(), "{key}: {out}");
        assert_eq!(obj.len(), 2, "{key}: {out}");
    }
    let one = "{ \"autoupdate\": false }";
    let out = jsonc_remove(one, "autoupdate", "t").unwrap();
    assert!(jsonc_object(&out, "t").unwrap().is_empty(), "{out}");
    assert_eq!(jsonc_remove(one, "missing", "t").unwrap(), one);
}

#[test]
fn jsonc_set_on_empty_and_new_files() {
    let out = jsonc_set("", "update", "\"disable\"", "t").unwrap();
    assert_eq!(
        jsonc_object(&out, "t").unwrap().get("update"),
        Some(&json!("disable"))
    );
    let out = jsonc_set("{}", "autoupdate", "false", "t").unwrap();
    assert_eq!(
        jsonc_object(&out, "t").unwrap().get("autoupdate"),
        Some(&Value::Bool(false))
    );
    assert!(jsonc_set("[1,2]", "k", "1", "t").is_err());
    assert!(jsonc_set("{ \"a\": ", "k", "1", "t").is_err());
}

#[test]
fn strings_with_braces_and_comment_markers_are_not_confused() {
    let text = r#"{"url": "http://x//y/*z*/", "k": "}{", "n": 1}"#;
    let out = jsonc_set(text, "update", "\"disable\"", "t").unwrap();
    let obj = jsonc_object(&out, "t").unwrap();
    assert_eq!(obj.get("url"), Some(&json!("http://x//y/*z*/")));
    assert_eq!(obj.get("k"), Some(&json!("}{")));
    let out = jsonc_remove(&out, "k", "t").unwrap();
    let obj = jsonc_object(&out, "t").unwrap();
    assert!(obj.get("k").is_none());
    assert_eq!(obj.get("n"), Some(&json!(1)));
}
