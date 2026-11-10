use std::fs;

use super::{ATOMS, COMPLETED, ensure_completed};
use serde_json::{Value, json};
use superba_core::CodexHome;

fn fixture(bytes: &[u8]) -> (tempfile::TempDir, CodexHome) {
    let tmp = tempfile::tempdir().unwrap();
    let home = CodexHome::new(tmp.path());
    fs::write(home.global_state_json(), bytes).unwrap();
    (tmp, home)
}

#[test]
fn adds_only_missing_flag_preserving_values_and_key_order() {
    let before = r#"{"z":1,"electron-persisted-atom-state":{"z":[1,{"b":2,"a":1}],"a":"中文"},"a":{"y":false,"x":null}}"#;
    let (_tmp, home) = fixture(before.as_bytes());
    let backup = home.global_state_json().with_extension("json.bak");
    fs::write(&backup, b"backup stays untouched").unwrap();
    ensure_completed(&home).unwrap();
    let after = fs::read_to_string(home.global_state_json()).unwrap();
    assert_eq!(
        after,
        r#"{"z":1,"electron-persisted-atom-state":{"z":[1,{"b":2,"a":1}],"a":"中文","electron:onboarding-projectless-completed":true},"a":{"y":false,"x":null}}"#
    );
    assert_eq!(fs::read(&backup).unwrap(), b"backup stays untouched");
    assert_eq!(fs::read_dir(home.root()).unwrap().count(), 2);
}

#[test]
fn creates_atom_object_without_reordering_other_keys() {
    let (_tmp, home) = fixture(br#"{"z":{},"a":42}"#);
    ensure_completed(&home).unwrap();
    assert_eq!(
        fs::read_to_string(home.global_state_json()).unwrap(),
        r#"{"z":{},"a":42,"electron-persisted-atom-state":{"electron:onboarding-projectless-completed":true}}"#
    );
}

#[test]
fn existing_flag_of_any_value_is_byte_for_byte_unchanged() {
    for value in ["true", "false", "null", "0", "[]", "{}", "\"yes\""] {
        let before = format!("{{\n  \"{ATOMS}\": {{ \"{COMPLETED}\": {value} }}\n}}\n");
        let (_tmp, home) = fixture(before.as_bytes());
        ensure_completed(&home).unwrap();
        assert_eq!(
            fs::read(home.global_state_json()).unwrap(),
            before.as_bytes()
        );
        assert_eq!(fs::read_dir(home.root()).unwrap().count(), 1);
    }
}

#[test]
fn invalid_json_and_non_objects_are_not_replaced() {
    for before in [
        "{broken",
        "",
        "null",
        "[]",
        "42",
        "\"text\"",
        "{\"a\":1} trailing",
    ] {
        let (_tmp, home) = fixture(before.as_bytes());
        let backup = home.global_state_json().with_extension("json.bak");
        fs::write(&backup, b"{}").unwrap();
        assert!(ensure_completed(&home).is_err());
        assert_eq!(
            fs::read(home.global_state_json()).unwrap(),
            before.as_bytes()
        );
        assert_eq!(fs::read(backup).unwrap(), b"{}");
        assert_eq!(fs::read_dir(home.root()).unwrap().count(), 2);
    }
}

#[test]
fn non_object_atom_state_is_not_destroyed() {
    for value in ["null", "false", "[]", "1", "\"text\""] {
        let before = format!(r#"{{"{ATOMS}":{value},"other":true}}"#);
        let (_tmp, home) = fixture(before.as_bytes());
        assert!(ensure_completed(&home).is_err());
        assert_eq!(
            fs::read(home.global_state_json()).unwrap(),
            before.as_bytes()
        );
    }
}

#[test]
fn bom_is_invalid_for_the_app_and_is_left_intact() {
    for body in [
        "{}",
        r#"{"electron-persisted-atom-state":{"electron:onboarding-projectless-completed":false}}"#,
    ] {
        let before = format!("\u{feff}{body}");
        let (_tmp, home) = fixture(before.as_bytes());
        assert!(ensure_completed(&home).is_err());
        assert_eq!(
            fs::read(home.global_state_json()).unwrap(),
            before.as_bytes()
        );
    }
}

#[test]
fn missing_home_gets_a_minimal_utf8_file_without_a_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let home = CodexHome::new(tmp.path().join("new/.codex"));
    ensure_completed(&home).unwrap();
    let bytes = fs::read(home.global_state_json()).unwrap();
    assert!(!bytes.starts_with(b"\xef\xbb\xbf"));
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap(),
        json!({ATOMS: {COMPLETED: true}})
    );
    assert_eq!(fs::read_dir(home.root()).unwrap().count(), 1);
    ensure_completed(&home).unwrap();
    assert_eq!(fs::read(home.global_state_json()).unwrap(), bytes);
}

#[test]
fn missing_primary_does_not_read_or_modify_an_existing_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let home = CodexHome::new(tmp.path());
    let backup = home.global_state_json().with_extension("json.bak");
    let before = b"this is not the primary";
    fs::write(&backup, before).unwrap();
    ensure_completed(&home).unwrap();
    assert_eq!(fs::read(backup).unwrap(), before);
    assert_eq!(fs::read_dir(home.root()).unwrap().count(), 2);
}

#[cfg(unix)]
#[test]
fn preserves_original_file_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, home) = fixture(b"{}");
    fs::set_permissions(home.global_state_json(), fs::Permissions::from_mode(0o640)).unwrap();
    ensure_completed(&home).unwrap();
    assert_eq!(
        fs::metadata(home.global_state_json())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o640
    );
}

#[test]
fn io_failure_leaves_the_existing_path_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let home = CodexHome::new(tmp.path());
    fs::create_dir(home.global_state_json()).unwrap();
    assert!(ensure_completed(&home).is_err());
    assert!(home.global_state_json().is_dir());
    assert_eq!(fs::read_dir(home.root()).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn a_symlinked_state_file_is_written_through_and_stays_a_link() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let tmp = tempfile::tempdir().unwrap();
    let home = CodexHome::new(tmp.path().join(".codex"));
    let elsewhere = tmp.path().join("dotfiles");
    fs::create_dir_all(home.root()).unwrap();
    fs::create_dir_all(&elsewhere).unwrap();
    let target = elsewhere.join("state.json");
    fs::write(&target, br#"{"a":1}"#).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
    // A relative link, resolved against the link's own directory.
    symlink("../dotfiles/state.json", home.global_state_json()).unwrap();
    ensure_completed(&home).unwrap();
    let link = fs::symlink_metadata(home.global_state_json()).unwrap();
    assert!(link.file_type().is_symlink());
    assert_eq!(
        fs::read_link(home.global_state_json()).unwrap(),
        std::path::Path::new("../dotfiles/state.json")
    );
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        format!(r#"{{"a":1,"{ATOMS}":{{"{COMPLETED}":true}}}}"#)
    );
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o640
    );
    // No temporary file left in either directory.
    assert_eq!(fs::read_dir(home.root()).unwrap().count(), 1);
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn a_dangling_state_file_link_is_left_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let home = CodexHome::new(tmp.path());
    let missing = tmp.path().join("gone/state.json");
    std::os::unix::fs::symlink(&missing, home.global_state_json()).unwrap();
    assert!(ensure_completed(&home).is_err());
    assert_eq!(fs::read_link(home.global_state_json()).unwrap(), missing);
    assert!(!missing.exists());
    assert_eq!(fs::read_dir(home.root()).unwrap().count(), 1);
}
