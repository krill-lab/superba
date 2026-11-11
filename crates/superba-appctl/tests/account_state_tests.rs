//! User bugs 135/136: the desktop's account-scoped state is folded back into
//! the top level before a launch without a ChatGPT sign-in. The fixtures
//! follow files written by app 26.1103.60933 (ChatGPT start, then PAT starts).

#![allow(missing_docs)]

use std::fs;
use std::path::Path;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use superba_appctl::account_state::{
    ACCOUNT_STATE_KEY, LoginKind, OWNER_KEY, Outcome, SIGNED_OUT_OWNER, flatten, login_kind,
    name_signed_out_owner, prepare, prepare_launch,
};
use superba_core::{CodexHome, Timestamp};

const PAT: &str = r#"{"personal_access_token":"nb_test","OPENAI_API_KEY":"nb_test"}"#;
const API_KEY: &str = r#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-test"}"#;
const CHATGPT: &str = r#"{"auth_mode":"chatgpt","OPENAI_API_KEY":null,"tokens":{"id_token":"a.b.c","access_token":"a.b.c","refresh_token":"r","account_id":"acc"},"last_refresh":"2026-11-10T00:00:00Z"}"#;
const HOST: &str = "local:/home/u/.codex";

/// After a ChatGPT start and a PAT start in which `proj-c` was added: the
/// ChatGPT account's projects are stashed, `proj-c` is about to be dropped.
fn broken_state() -> Value {
    json!({
        "electron-persisted-atom-state": {
            "electron:onboarding-projectless-completed": true
        },
        "electron-completed-local-data-migration-ids": ["2026-07-13-local-projects"],
        ACCOUNT_STATE_KEY: {
            "owner": null,
            "inactive": {
                "9219b2e4": {
                    "atoms": {
                        "flat-project-sidebar-preferences-v1": {"mode": "project", "initialized": true},
                        "composer-auto-context-enabled": false
                    },
                    "globals": {
                        "electron-saved-workspace-roots": ["/home/u/proj-a", "/home/u/proj-b"],
                        "local-projects": {
                            "local-a": {"id": "local-a", "name": "proj-a", "rootPaths": ["/home/u/proj-a"]},
                            "local-b": {"id": "local-b", "name": "proj-b", "rootPaths": ["/home/u/proj-b"]}
                        },
                        "app-server-project-id-by-legacy-project-id-by-host": {
                            HOST: {"local-a": "srv-a", "local-b": "srv-b"}
                        },
                        "selected-project": {"type": "local", "projectId": "local-a"}
                    }
                }
            }
        },
        "electron-main-window-bounds": {"x": 0, "y": 0, "width": 1280, "height": 820},
        "app-server-project-id-by-legacy-project-id-by-host": {HOST: {"proj-c-id": "srv-c"}},
        "local-projects": {
            "proj-c-id": {"id": "proj-c-id", "name": "proj-c", "rootPaths": ["/home/u/proj-c"]}
        },
        "project-order": ["proj-c-id"],
        "selected-project": {"type": "local", "projectId": "proj-c-id"}
    })
}

fn keys(v: &Value) -> Vec<&str> {
    v.as_object().unwrap().keys().map(String::as_str).collect()
}

/// PAT auth.json with ChatGPT-shaped tokens next to it (what a line issuing
/// an id_token at PAT sign-in would produce): Codex still picks the PAT.
const PAT_WITH_TOKENS: &str = r#"{"personal_access_token":"nb_test","OPENAI_API_KEY":"nb_test","tokens":{"id_token":"a.b.c","access_token":"a.b.c","refresh_token":"r","account_id":"acc"},"last_refresh":"2026-11-10T00:00:00Z"}"#;

#[test]
fn login_kinds() {
    assert_eq!(login_kind(PAT.as_bytes()), LoginKind::Token);
    assert_eq!(login_kind(API_KEY.as_bytes()), LoginKind::Token);
    assert_eq!(login_kind(CHATGPT.as_bytes()), LoginKind::ChatGpt);
    // Codex's precedence (AuthDotJson::resolved_mode): explicit auth_mode,
    // then personal access token, Bedrock, API key, and ChatGPT tokens last.
    for (auth, kind) in [
        (
            r#"{"OPENAI_API_KEY":null,"tokens":{"id_token":"x"}}"#,
            LoginKind::ChatGpt,
        ),
        (r#"{"OPENAI_API_KEY":"sk","tokens":null}"#, LoginKind::Token),
        (
            r#"{"OPENAI_API_KEY":"sk","tokens":{"id_token":"x"}}"#,
            LoginKind::Token,
        ),
        (
            r#"{"auth_mode":"chatgpt","OPENAI_API_KEY":"sk","tokens":{"id_token":"x"}}"#,
            LoginKind::ChatGpt,
        ),
        (r#"{"auth_mode":"chatgptAuthTokens"}"#, LoginKind::ChatGpt),
        (
            r#"{"auth_mode":"personalAccessToken","personal_access_token":"p","tokens":{"id_token":"x"}}"#,
            LoginKind::Token,
        ),
        (
            r#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk","tokens":{"id_token":"x"}}"#,
            LoginKind::Token,
        ),
        (
            r#"{"auth_mode":"agentIdentity","OPENAI_API_KEY":"sk"}"#,
            LoginKind::Unknown,
        ),
        (
            r#"{"auth_mode":7,"OPENAI_API_KEY":"sk"}"#,
            LoginKind::Unknown,
        ),
        (
            r#"{"bedrock_api_key":{"k":"v"},"OPENAI_API_KEY":"sk"}"#,
            LoginKind::Unknown,
        ),
        (
            r#"{"bedrock_access_keys":{"k":"v"},"tokens":{"id_token":"x"}}"#,
            LoginKind::Unknown,
        ),
        (r#"{"OPENAI_API_KEY":" "}"#, LoginKind::Token),
    ] {
        assert_eq!(login_kind(auth.as_bytes()), kind, "{auth}");
    }
    for unknown in [&b""[..], b"{}", b"[]", b"not json", br#"{"tokens":null}"#] {
        assert_eq!(login_kind(unknown), LoginKind::Unknown, "{unknown:?}");
    }
}

#[test]
fn a_personal_access_token_wins_over_chatgpt_tokens() {
    assert_eq!(login_kind(PAT_WITH_TOKENS.as_bytes()), LoginKind::Token);
    let mut patched: Value = serde_json::from_str(PAT_WITH_TOKENS).unwrap();
    patched["auth_mode"] = Value::Null;
    assert_eq!(
        login_kind(&serde_json::to_vec(&patched).unwrap()),
        LoginKind::Token
    );
    // And such a home gets the fold before a launch.
    let original = serde_json::to_vec(&broken_state()).unwrap();
    let h = home(Some(PAT_WITH_TOKENS), Some(&original));
    assert!(matches!(
        prepare(&h.home, &h.backups, Timestamp::from_millis(5)).unwrap(),
        Outcome::Flattened { stashes: 1, .. }
    ));
    let state: Value =
        serde_json::from_slice(&fs::read(h.home.global_state_json()).unwrap()).unwrap();
    assert!(state.get(ACCOUNT_STATE_KEY).is_none());
}

#[test]
fn flatten_restores_stashed_projects_and_keeps_the_current_ones() {
    let mut state = broken_state();
    assert_eq!(flatten(&mut state), Some(1));
    assert!(state.get(ACCOUNT_STATE_KEY).is_none());
    // Every other key keeps its place; restored keys are appended.
    assert_eq!(
        keys(&state),
        [
            "electron-persisted-atom-state",
            "electron-completed-local-data-migration-ids",
            "electron-main-window-bounds",
            "app-server-project-id-by-legacy-project-id-by-host",
            "local-projects",
            "project-order",
            "selected-project",
            "electron-saved-workspace-roots",
        ]
    );
    assert_eq!(
        keys(&state["local-projects"]),
        ["proj-c-id", "local-a", "local-b"]
    );
    assert_eq!(
        state["app-server-project-id-by-legacy-project-id-by-host"][HOST],
        json!({"proj-c-id": "srv-c", "local-a": "srv-a", "local-b": "srv-b"})
    );
    assert_eq!(
        state["electron-saved-workspace-roots"],
        json!(["/home/u/proj-a", "/home/u/proj-b"])
    );
    // A current scalar wins over the stashed one.
    assert_eq!(state["selected-project"]["projectId"], "proj-c-id");
    assert_eq!(state["project-order"], json!(["proj-c-id"]));
    // Stashed atoms fill the atom state; the global onboarding flag stays.
    assert_eq!(
        state["electron-persisted-atom-state"],
        json!({
            "electron:onboarding-projectless-completed": true,
            "flat-project-sidebar-preferences-v1": {"mode": "project", "initialized": true},
            "composer-auto-context-enabled": false
        })
    );
}

#[test]
fn flatten_keeps_the_signed_in_owner_state_and_folds_other_accounts_in() {
    // At rest after a ChatGPT session: the top level is the owner's state,
    // `inactive` holds another account.
    let mut state = json!({
        ACCOUNT_STATE_KEY: {
            "owner": "owner-hash",
            "inactive": {
                "other-hash": {
                    "atoms": {"composer-auto-context-enabled": true},
                    "globals": {
                        "electron-saved-workspace-roots": ["/w/other", "/w/mine"],
                        "pinned-thread-ids": ["t2"],
                        "selected-project": null
                    }
                }
            }
        },
        "electron-saved-workspace-roots": ["/w/mine"],
        "pinned-thread-ids": ["t1"],
        "electron-persisted-atom-state": {"composer-auto-context-enabled": false}
    });
    assert_eq!(flatten(&mut state), Some(1));
    assert_eq!(
        state,
        json!({
            "electron-saved-workspace-roots": ["/w/mine", "/w/other"],
            "pinned-thread-ids": ["t1", "t2"],
            "electron-persisted-atom-state": {"composer-auto-context-enabled": false}
        })
    );
}

#[test]
fn flatten_drops_an_empty_account_state() {
    let mut state = json!({ACCOUNT_STATE_KEY: {"owner": null, "inactive": {}}, "a": 1});
    assert_eq!(flatten(&mut state), Some(0));
    assert_eq!(state, json!({"a": 1}));
    let mut state = json!({ACCOUNT_STATE_KEY: {"owner": "x"}, "a": 1});
    assert_eq!(flatten(&mut state), Some(0));
    assert_eq!(state, json!({"a": 1}));
}

#[test]
fn flatten_leaves_unknown_shapes_alone() {
    for bad in [
        json!({ACCOUNT_STATE_KEY: "x"}),
        json!({ACCOUNT_STATE_KEY: {"inactive": []}}),
        json!({ACCOUNT_STATE_KEY: {"inactive": {"h": 1}}}),
        json!({ACCOUNT_STATE_KEY: {"inactive": {"h": {"atoms": [], "globals": {}}}}}),
        json!({ACCOUNT_STATE_KEY: {"inactive": {"h": {"atoms": {}, "globals": "x"}}}}),
        json!({"no-account-state": true}),
        json!([1]),
    ] {
        let mut state = bad.clone();
        assert_eq!(flatten(&mut state), None, "{bad}");
        assert_eq!(state, bad);
    }
}

struct Home {
    _tmp: tempfile::TempDir,
    home: CodexHome,
    backups: std::path::PathBuf,
}

fn home(auth: Option<&str>, state: Option<&[u8]>) -> Home {
    let tmp = tempfile::tempdir().unwrap();
    let home = CodexHome::new(tmp.path().join(".codex"));
    fs::create_dir_all(home.root()).unwrap();
    if let Some(auth) = auth {
        fs::write(home.auth_json(), auth).unwrap();
    }
    if let Some(state) = state {
        fs::write(home.global_state_json(), state).unwrap();
    }
    let backups = tmp.path().join("data/backups/app-state");
    Home {
        _tmp: tmp,
        home,
        backups,
    }
}

fn bak(home: &CodexHome) -> std::path::PathBuf {
    home.root().join(".codex-global-state.json.bak")
}

fn backups(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .map(|d| {
            d.map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

#[test]
fn prepare_folds_the_state_for_a_token_login_and_keeps_a_backup() {
    let original = serde_json::to_vec_pretty(&broken_state()).unwrap();
    for auth in [PAT, API_KEY] {
        let h = home(Some(auth), Some(&original));
        fs::write(bak(&h.home), b"app backup").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                h.home.global_state_json(),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        let outcome = prepare(&h.home, &h.backups, Timestamp::from_millis(1_000)).unwrap();
        let backup = h.backups.join("global-state-1000.json");
        assert_eq!(
            outcome,
            Outcome::Flattened {
                stashes: 1,
                backup: backup.clone()
            }
        );
        assert_eq!(fs::read(&backup).unwrap(), original);
        let state: Value =
            serde_json::from_slice(&fs::read(h.home.global_state_json()).unwrap()).unwrap();
        let mut expected = broken_state();
        flatten(&mut expected);
        // App 26.1106 (user bug 139): the folded atoms get the signed-out owner.
        assert!(name_signed_out_owner(&mut expected));
        assert_eq!(state, expected);
        assert_eq!(state[OWNER_KEY], SIGNED_OUT_OWNER);
        assert_eq!(fs::read(bak(&h.home)).unwrap(), b"app backup");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(h.home.global_state_json())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // A second launch has nothing left to do.
        assert_eq!(
            prepare(&h.home, &h.backups, Timestamp::from_millis(2_000)).unwrap(),
            Outcome::Unchanged
        );
        assert_eq!(backups(&h.backups), ["global-state-1000.json"]);
    }
}

#[test]
fn prepare_leaves_chatgpt_and_unknown_logins_alone() {
    let original = serde_json::to_vec(&broken_state()).unwrap();
    for auth in [Some(CHATGPT), Some("{}"), None] {
        let h = home(auth, Some(&original));
        assert_eq!(
            prepare(&h.home, &h.backups, Timestamp::now()).unwrap(),
            Outcome::Unchanged,
            "{auth:?}"
        );
        assert_eq!(fs::read(h.home.global_state_json()).unwrap(), original);
        assert!(!h.backups.exists());
    }
}

#[test]
fn prepare_without_account_state_or_file_changes_nothing() {
    let plain = br#"{"local-projects":{},"electron-saved-workspace-roots":["/w"]}"#;
    let h = home(Some(PAT), Some(plain));
    assert_eq!(
        prepare(&h.home, &h.backups, Timestamp::now()).unwrap(),
        Outcome::Unchanged
    );
    assert_eq!(fs::read(h.home.global_state_json()).unwrap(), plain);
    let h = home(Some(PAT), None);
    assert_eq!(
        prepare(&h.home, &h.backups, Timestamp::now()).unwrap(),
        Outcome::Unchanged
    );
    assert!(!h.home.global_state_json().exists());
    assert!(!h.backups.exists());
}

#[test]
fn prepare_refuses_files_the_app_cannot_parse_or_does_not_know() {
    let mut bom = b"\xEF\xBB\xBF".to_vec();
    bom.extend(serde_json::to_vec(&broken_state()).unwrap());
    let odd = serde_json::to_vec(&json!({ACCOUNT_STATE_KEY: {"inactive": {"h": 1}}})).unwrap();
    for bytes in [bom, b"{".to_vec(), odd] {
        let h = home(Some(PAT), Some(&bytes));
        assert!(prepare(&h.home, &h.backups, Timestamp::now()).is_err());
        assert_eq!(fs::read(h.home.global_state_json()).unwrap(), bytes);
        assert!(!h.backups.exists());
    }
}

#[test]
fn prepare_keeps_the_newest_ten_backups() {
    let original = serde_json::to_vec(&broken_state()).unwrap();
    let h = home(Some(PAT), None);
    fs::create_dir_all(&h.backups).unwrap();
    fs::write(h.backups.join("unrelated.txt"), b"x").unwrap();
    for ms in 1..=12 {
        fs::write(h.home.global_state_json(), &original).unwrap();
        prepare(&h.home, &h.backups, Timestamp::from_millis(ms)).unwrap();
    }
    // Same millisecond again: a new name, never an overwrite.
    fs::write(h.home.global_state_json(), &original).unwrap();
    let Outcome::Flattened { backup, .. } =
        prepare(&h.home, &h.backups, Timestamp::from_millis(12)).unwrap()
    else {
        panic!("not flattened");
    };
    assert!(backup.ends_with("global-state-13.json"));
    let mut expected: Vec<String> = (4..=13)
        .map(|ms| format!("global-state-{ms}.json"))
        .collect();
    expected.push("unrelated.txt".into());
    expected.sort();
    assert_eq!(backups(&h.backups), expected);
}

#[cfg(unix)]
#[test]
fn prepare_writes_through_a_symlinked_state_file() {
    let original = serde_json::to_vec(&broken_state()).unwrap();
    let h = home(Some(PAT), None);
    let target = h.home.root().join("synced-state.json");
    fs::write(&target, &original).unwrap();
    std::os::unix::fs::symlink(&target, h.home.global_state_json()).unwrap();
    assert!(matches!(
        prepare(&h.home, &h.backups, Timestamp::now()).unwrap(),
        Outcome::Flattened { stashes: 1, .. }
    ));
    assert!(
        fs::symlink_metadata(h.home.global_state_json())
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let state: Value = serde_json::from_slice(&fs::read(&target).unwrap()).unwrap();
    assert!(state.get(ACCOUNT_STATE_KEY).is_none());
    assert_eq!(state["local-projects"].as_object().unwrap().len(), 3);
}

/// Two ChatGPT accounts stashed (`hA` first, `hB` second) with the same
/// project id under different names, different selected projects and
/// different values for the same atom; the top level holds a PAT project.
fn two_account_state() -> Value {
    json!({
        ACCOUNT_STATE_KEY: {
            "owner": null,
            "inactive": {
                "hA": {
                    "atoms": {"x": "from-a"},
                    "globals": {
                        "electron-saved-workspace-roots": ["/a1", "/shared"],
                        "local-projects": {
                            "p1": {"id": "p1", "name": "alpha-name", "rootPaths": ["/shared"]},
                            "p2": {"id": "p2", "name": "only-a", "rootPaths": ["/a1"]}
                        },
                        "project-order": ["p1", "p2"],
                        "selected-project": {"type": "local", "projectId": "p1"}
                    }
                },
                "hB": {
                    "atoms": {"x": "from-b", "y": "only-b"},
                    "globals": {
                        "electron-saved-workspace-roots": ["/shared", "/b1"],
                        "local-projects": {
                            "p1": {"id": "p1", "name": "beta-name", "rootPaths": ["/shared"], "color": "blue"},
                            "p3": {"id": "p3", "name": "only-b", "rootPaths": ["/b1"]}
                        },
                        "project-order": ["p3", "p1"],
                        "selected-project": {"type": "local", "projectId": "p3"}
                    }
                }
            }
        },
        "local-projects": {"p9": {"id": "p9", "name": "pat", "rootPaths": ["/c1"]}},
        "electron-saved-workspace-roots": ["/c1"]
    })
}

#[test]
fn conflicting_stashes_keep_every_project_and_the_original_in_the_backup() {
    let original = serde_json::to_vec_pretty(&two_account_state()).unwrap();
    let h = home(Some(PAT), Some(&original));
    let Outcome::Flattened { stashes, backup } =
        prepare(&h.home, &h.backups, Timestamp::from_millis(7)).unwrap()
    else {
        panic!("not flattened");
    };
    assert_eq!(stashes, 2);
    // The backup holds every owner's stash byte for byte.
    assert_eq!(fs::read(&backup).unwrap(), original);
    let state: Value =
        serde_json::from_slice(&fs::read(h.home.global_state_json()).unwrap()).unwrap();
    // No project entry is lost: every id of both accounts and the PAT one.
    assert_eq!(keys(&state["local-projects"]), ["p9", "p1", "p2", "p3"]);
    assert_eq!(
        state["electron-saved-workspace-roots"],
        json!(["/c1", "/a1", "/shared", "/b1"])
    );
    assert_eq!(state["project-order"], json!(["p1", "p2", "p3"]));
    // Conflicting leaves: the first stash in the file wins, the second only
    // fills what is missing (p1 keeps alpha's name and gains beta's colour).
    assert_eq!(
        state["local-projects"]["p1"],
        json!({"id": "p1", "name": "alpha-name", "rootPaths": ["/shared"], "color": "blue"})
    );
    assert_eq!(state["selected-project"]["projectId"], "p1");
    assert_eq!(
        state["electron-persisted-atom-state"],
        json!({"x": "from-a", "y": "only-b"})
    );
    assert!(state.get(ACCOUNT_STATE_KEY).is_none());
}

/// The copy must exist before the state file is replaced: when the
/// replacement fails, the backup is already there and the state is intact.
#[cfg(unix)]
#[test]
fn the_backup_is_written_before_the_state_file_is_replaced() {
    use std::os::unix::fs::PermissionsExt;
    let original = serde_json::to_vec(&broken_state()).unwrap();
    let h = home(Some(PAT), Some(&original));
    // A read-only Codex home: the temporary file for the replacement cannot
    // be created there. (Root ignores the mode; nothing to test then.)
    fs::set_permissions(h.home.root(), fs::Permissions::from_mode(0o500)).unwrap();
    let writable = fs::write(h.home.root().join("probe"), b"").is_ok();
    let result = prepare(&h.home, &h.backups, Timestamp::from_millis(9));
    fs::set_permissions(h.home.root(), fs::Permissions::from_mode(0o700)).unwrap();
    if writable {
        eprintln!("skipped: running with permission overrides");
        return;
    }
    assert!(result.is_err());
    assert_eq!(fs::read(h.home.global_state_json()).unwrap(), original);
    assert_eq!(backups(&h.backups), ["global-state-9.json"]);
    assert_eq!(
        fs::read(h.backups.join("global-state-9.json")).unwrap(),
        original
    );
}

#[cfg(unix)]
#[test]
fn backups_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let original = serde_json::to_vec(&broken_state()).unwrap();
    let h = home(Some(PAT), Some(&original));
    // A permissive umask-independent state file mode must not leak into it.
    fs::set_permissions(
        h.home.global_state_json(),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let Outcome::Flattened { backup, .. } =
        prepare(&h.home, &h.backups, Timestamp::from_millis(3)).unwrap()
    else {
        panic!("not flattened");
    };
    let mode = fs::metadata(&backup).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    // The state file keeps its own mode.
    let mode = fs::metadata(h.home.global_state_json())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o644);
}

#[test]
fn a_token_launch_names_the_signed_out_owner_of_atoms_without_one() {
    // User bug 139: without an owner, app 26.1106's first API-key start
    // drops every persisted atom that is not global.
    let state = serde_json::json!({
        "electron-persisted-atom-state": {"last_completed_onboarding": 1, "sidebar": {"open": true}},
        "local-projects": {"p": {"name": "p"}},
    });
    let original = serde_json::to_vec(&state).unwrap();
    for auth in [PAT, API_KEY] {
        let h = home(Some(auth), Some(&original));
        let done = prepare_launch(&h.home, &h.backups, None, Timestamp::from_millis(5)).unwrap();
        assert!(done.owner_named);
        assert_eq!(done.stashes, None);
        assert_eq!(fs::read(done.backup.unwrap()).unwrap(), original);
        let now: Value =
            serde_json::from_slice(&fs::read(h.home.global_state_json()).unwrap()).unwrap();
        let mut expected = state.clone();
        expected[OWNER_KEY] = SIGNED_OUT_OWNER.into();
        assert_eq!(now, expected);
        // An owner is never replaced.
        let again = prepare_launch(&h.home, &h.backups, None, Timestamp::from_millis(6)).unwrap();
        assert!(!again.owner_named && again.backup.is_none());
    }
    // A ChatGPT login, a file without atoms, or one with an owner: untouched.
    let h = home(Some(CHATGPT), Some(&original));
    assert!(
        !prepare_launch(&h.home, &h.backups, None, Timestamp::now())
            .unwrap()
            .owner_named
    );
    assert_eq!(fs::read(h.home.global_state_json()).unwrap(), original);
    for other in [
        serde_json::json!({"local-projects": {}}),
        serde_json::json!({"electron-persisted-atom-state": {}, OWNER_KEY: "abc"}),
    ] {
        let mut v = other.clone();
        assert!(!name_signed_out_owner(&mut v));
        assert_eq!(v, other);
    }
}
