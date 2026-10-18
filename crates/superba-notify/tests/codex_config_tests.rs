//! Superba's hooks in Codex's config.toml: install, chain, update, remove.

use pretty_assertions::assert_eq;
use superba_notify::codex_config::{read, set_notify, set_permission_hook, set_trusted};
use superba_notify::hooks::NotifyShape;

const PROGRAM: &str = "/opt/superba/spb";

#[test]
fn notify_is_added_before_the_tables_and_removed_again() {
    let original = "# my config\nmodel = \"gpt-6\"\n\n[model_providers.x]\nname = \"x\"\n";
    let installed = set_notify(original, PROGRAM, true).unwrap().unwrap();
    assert_eq!(
        installed,
        "# my config\nmodel = \"gpt-6\"\nnotify = [\"/opt/superba/spb\", \"notify\", \"codex\"]\n\n[model_providers.x]\nname = \"x\"\n"
    );
    let facts = read(&installed, "/h/.codex/config.toml").unwrap();
    assert!(matches!(facts.notify, NotifyShape::Ours { ref chained, .. } if chained.is_empty()));
    // Installing again changes nothing.
    assert_eq!(set_notify(&installed, PROGRAM, true).unwrap(), None);
    let removed = set_notify(&installed, PROGRAM, false).unwrap().unwrap();
    assert_eq!(removed, original);
    assert_eq!(set_notify(original, PROGRAM, false).unwrap(), None);
}

#[test]
fn a_users_notify_program_is_chained_and_restored() {
    let original =
        "notify = [\"python3\", \"/home/me/notify.py\"]  # desktop toast\nmodel = \"m\"\n";
    let installed = set_notify(original, PROGRAM, true).unwrap().unwrap();
    assert!(installed.starts_with(
        "notify = [\"/opt/superba/spb\", \"notify\", \"codex\", \"--\", \"python3\", \"/home/me/notify.py\"]"
    ), "{installed}");
    // A new location of spb (an update moved it) keeps the chain.
    let moved = set_notify(&installed, "/new/spb", true).unwrap().unwrap();
    assert!(
        moved.contains("[\"/new/spb\", \"notify\", \"codex\", \"--\", \"python3\""),
        "{moved}"
    );
    let restored = set_notify(&moved, PROGRAM, false).unwrap().unwrap();
    assert!(
        restored.starts_with("notify = [\"python3\", \"/home/me/notify.py\"]"),
        "{restored}"
    );
    // Foreign programs are never removed.
    assert_eq!(set_notify(original, PROGRAM, false).unwrap(), None);
    // A notify of another type is refused, not overwritten.
    assert!(set_notify("notify = \"x\"\n", PROGRAM, true).is_err());
    assert!(set_notify("not = = toml", PROGRAM, true).is_err());
}

#[test]
fn permission_hook_is_added_next_to_the_users_hooks() {
    let original = "model = \"m\"\n\n[[hooks.PermissionRequest]]\nmatcher = \"Bash\"\n[[hooks.PermissionRequest.hooks]]\ntype = \"command\"\ncommand = \"/usr/local/bin/my-guard\"\n";
    let installed = set_permission_hook(original, PROGRAM, true, false)
        .unwrap()
        .unwrap();
    assert!(
        installed.contains("command = \"/opt/superba/spb notify hook codex\""),
        "{installed}"
    );
    assert!(installed.contains("/usr/local/bin/my-guard"));
    let doc: toml::Value = toml::from_str(&installed).unwrap();
    let groups = doc["hooks"]["PermissionRequest"].as_array().unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[1]["hooks"][0]["timeout"].as_integer(), Some(30));
    let path = "/h/.codex/config.toml";
    let facts = read(&installed, path).unwrap();
    assert!(facts.permission_hook && !facts.permission_trusted);
    // Codex's trust record for group 1, handler 0.
    let trusted = format!(
        "{installed}\n[hooks.state.\"{path}:permission_request:1:0\"]\ntrusted_hash = \"sha256:abc\"\n"
    );
    assert!(read(&trusted, path).unwrap().permission_trusted);
    assert_eq!(
        set_permission_hook(&installed, PROGRAM, true, false).unwrap(),
        None
    );
    let removed = set_permission_hook(&installed, PROGRAM, false, false)
        .unwrap()
        .unwrap();
    let doc: toml::Value = toml::from_str(&removed).unwrap();
    assert_eq!(
        doc["hooks"]["PermissionRequest"].as_array().unwrap().len(),
        1
    );
    assert!(removed.contains("/usr/local/bin/my-guard"));
    // From an empty file and back.
    let fresh = set_permission_hook("", PROGRAM, true, true)
        .unwrap()
        .unwrap();
    let back = set_permission_hook(&fresh, PROGRAM, false, false)
        .unwrap()
        .unwrap();
    assert_eq!(back.trim(), "");
}

#[test]
fn trust_is_recorded_like_codex_does() {
    let path = "/h/.codex/config.toml";
    let installed = set_permission_hook("", PROGRAM, true, true)
        .unwrap()
        .unwrap();
    assert!(installed.contains("async = true"));
    let key = format!("{path}:permission_request:0:0");
    let trusted = set_trusted(&installed, &[(key.clone(), "sha256:feed".into())])
        .unwrap()
        .unwrap();
    let doc: toml::Value = toml::from_str(&trusted).unwrap();
    assert_eq!(
        doc["hooks"]["state"][&key]["trusted_hash"].as_str(),
        Some("sha256:feed")
    );
    assert!(read(&trusted, path).unwrap().permission_trusted);
    assert_eq!(
        set_trusted(&trusted, &[(key, "sha256:feed".into())]).unwrap(),
        None
    );
}
