//! User bugs 135/136 follow-up: folders the user picked come back into the
//! app's project list at the next launch through Superba, in the same read,
//! backup and atomic replacement as the account-state fold.

#![allow(missing_docs)]

use std::fs;
use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use superba_appctl::account_state::{ACCOUNT_STATE_KEY, Prepared, prepare_launch};
use superba_appctl::project_restore::{
    self, ANSWERED_FILE, PENDING_FILE, RestoreItem, answered, apply, clear_pending, known_roots,
    pending, queue, record_answered,
};
use superba_core::{CodexHome, Timestamp};

const PAT: &str = r#"{"personal_access_token":"nb_test","OPENAI_API_KEY":"nb_test"}"#;
const CHATGPT: &str = r#"{"auth_mode":"chatgpt","tokens":{"id_token":"a.b.c","access_token":"a.b.c","refresh_token":"r","account_id":"acc"}}"#;

fn item(path: &str, name: &str, app_id: Option<&str>) -> RestoreItem {
    RestoreItem {
        path: path.into(),
        extra_roots: Vec::new(),
        name: name.into(),
        app_id: app_id.map(Into::into),
    }
}

fn keys(v: &Value) -> Vec<&str> {
    v.as_object().unwrap().keys().map(String::as_str).collect()
}

#[test]
fn apply_adds_missing_folders_and_never_changes_existing_projects() {
    let mut state = json!({
        "local-projects": {
            "local-a": {"id": "local-a", "name": "proj-a", "rootPaths": ["/w/a"], "createdAt": 1, "updatedAt": 1}
        },
        "project-order": ["local-a"],
        "selected-project": {"type": "local", "projectId": "local-a"}
    });
    let items = vec![
        // Already a project (trailing separator ignored): skipped.
        item("/w/a/", "renamed", Some("x")),
        // The app's own id comes back.
        item("/w/c", "proj-c", Some("877476ef-uuid")),
        // Taken id: a path-derived one instead.
        item("/w/d", "proj-d", Some("local-a")),
        // No id at all.
        item("/w/e", "proj-e", None),
        // Twice in one request: once.
        item("/w/e", "proj-e", None),
    ];
    assert_eq!(
        apply(&mut state, &items, Timestamp::from_millis(42)),
        Some(3)
    );
    let projects = &state["local-projects"];
    let ids = keys(projects);
    assert_eq!(ids[0], "local-a");
    assert_eq!(ids[1], "877476ef-uuid");
    assert!(
        ids[2].starts_with("local-") && ids[2].len() == 6 + 32,
        "{}",
        ids[2]
    );
    assert!(ids[3].starts_with("local-") && ids[3] != ids[2]);
    assert_eq!(
        projects["local-a"],
        json!({"id": "local-a", "name": "proj-a", "rootPaths": ["/w/a"], "createdAt": 1, "updatedAt": 1})
    );
    assert_eq!(
        projects["877476ef-uuid"],
        json!({"id": "877476ef-uuid", "name": "proj-c", "rootPaths": ["/w/c"], "createdAt": 42, "updatedAt": 42})
    );
    assert_eq!(projects[ids[2]]["rootPaths"], json!(["/w/d"]));
    assert_eq!(
        state["project-order"],
        json!(["local-a", "877476ef-uuid", ids[2], ids[3]])
    );
    assert_eq!(state["selected-project"]["projectId"], "local-a");
    // Again: nothing left to add.
    let before = state.clone();
    assert_eq!(
        apply(&mut state, &items, Timestamp::from_millis(43)),
        Some(0)
    );
    assert_eq!(state, before);
}

#[test]
fn apply_creates_the_list_without_inventing_an_order_and_refuses_odd_shapes() {
    let mut state = json!({"electron-persisted-atom-state": {}});
    assert_eq!(
        apply(
            &mut state,
            &[item("/w/c", "c", Some("k"))],
            Timestamp::from_millis(1)
        ),
        Some(1)
    );
    assert_eq!(
        keys(&state),
        ["electron-persisted-atom-state", "local-projects"]
    );
    assert!(state.get("project-order").is_none());
    for odd in [
        json!([]),
        json!({"local-projects": []}),
        json!({"local-projects": {}, "project-order": {}}),
    ] {
        let mut s = odd.clone();
        assert_eq!(
            apply(&mut s, &[item("/w/c", "c", None)], Timestamp::EPOCH),
            None
        );
        assert_eq!(s, odd);
    }
}

#[test]
fn known_roots_cover_every_account_and_the_legacy_lists() {
    let state = json!({
        "local-projects": {"p": {"rootPaths": ["/top/a", "/top/b"]}},
        "electron-saved-workspace-roots": ["/legacy"],
        ACCOUNT_STATE_KEY: {"owner": null, "inactive": {
            "h1": {"atoms": {}, "globals": {
                "local-projects": {"q": {"rootPaths": ["/stash/c"]}},
                "active-workspace-roots": ["/stash/d"]
            }}
        }}
    });
    let mut got = known_roots(&state);
    got.sort();
    let want: Vec<PathBuf> = ["/legacy", "/stash/c", "/stash/d", "/top/a", "/top/b"]
        .iter()
        .map(PathBuf::from)
        .collect();
    assert_eq!(got, want);
    assert!(known_roots(&json!([])).is_empty());
}

#[test]
fn the_queue_and_the_answers_are_kept_per_codex_home_and_owner_only() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("app-state");
    let h1 = Path::new("/u/.codex");
    let h2 = Path::new("/other/.codex");
    assert_eq!(pending(&dir, h1).unwrap(), None);
    queue(
        &dir,
        h1,
        vec![item("/w/c", "c", Some("k"))],
        Timestamp::from_millis(1),
    )
    .unwrap();
    queue(
        &dir,
        h1,
        vec![item("/w/c/", "dup", None), item("/w/d", "d", None)],
        Timestamp::from_millis(2),
    )
    .unwrap();
    queue(
        &dir,
        h2,
        vec![item("/x", "x", None)],
        Timestamp::from_millis(3),
    )
    .unwrap();
    let p = pending(&dir, Path::new("/u/.codex/")).unwrap().unwrap();
    assert_eq!(
        p.items,
        [item("/w/c", "c", Some("k")), item("/w/d", "d", None)]
    );
    assert_eq!(p.requested_at, Timestamp::from_millis(2));
    clear_pending(&dir, h1).unwrap();
    assert_eq!(pending(&dir, h1).unwrap(), None);
    assert!(pending(&dir, h2).unwrap().is_some());
    clear_pending(&dir, h2).unwrap();
    assert!(!dir.join(PENDING_FILE).exists());

    record_answered(&dir, h1, &["/w/c".into(), "/w/d".into()]).unwrap();
    record_answered(&dir, h1, &["/w/d/".into(), "/w/e".into()]).unwrap();
    assert_eq!(
        answered(&dir, h1).unwrap(),
        [PathBuf::from("/w/c"), "/w/d".into(), "/w/e".into()]
    );
    assert!(answered(&dir, h2).unwrap().is_empty());
    assert!(dir.join(ANSWERED_FILE).is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        queue(&dir, h1, vec![item("/w/f", "f", None)], Timestamp::EPOCH).unwrap();
        for f in [PENDING_FILE, ANSWERED_FILE] {
            let mode = fs::metadata(dir.join(f)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{f}");
        }
    }
}

struct World {
    _tmp: tempfile::TempDir,
    home: CodexHome,
    backups: PathBuf,
    app_state: PathBuf,
}

fn world(auth: &str, state: &Value) -> World {
    let tmp = tempfile::tempdir().unwrap();
    let home = CodexHome::new(tmp.path().join(".codex"));
    fs::create_dir_all(home.root()).unwrap();
    fs::write(home.auth_json(), auth).unwrap();
    fs::write(home.global_state_json(), serde_json::to_vec(state).unwrap()).unwrap();
    World {
        backups: tmp.path().join("data/backups/app-state"),
        app_state: tmp.path().join("data/app-state"),
        home,
        _tmp: tmp,
    }
}

fn read_state(home: &CodexHome) -> Value {
    serde_json::from_slice(&fs::read(home.global_state_json()).unwrap()).unwrap()
}

#[test]
fn a_launch_writes_the_queued_folders_once_with_a_backup_whatever_the_login() {
    let original = json!({"local-projects": {}, "project-order": []});
    for auth in [CHATGPT, PAT, "{}"] {
        let w = world(auth, &original);
        let before = fs::read(w.home.global_state_json()).unwrap();
        queue(
            &w.app_state,
            w.home.root(),
            vec![item("/w/c", "proj-c", Some("k-c"))],
            Timestamp::EPOCH,
        )
        .unwrap();
        let done = prepare_launch(
            &w.home,
            &w.backups,
            Some(&w.app_state),
            Timestamp::from_millis(9),
        )
        .unwrap();
        assert_eq!(done.stashes, None);
        assert_eq!(done.restored, 1);
        let backup = done.backup.clone().unwrap();
        assert_eq!(fs::read(&backup).unwrap(), before);
        let state = read_state(&w.home);
        assert_eq!(state["local-projects"]["k-c"]["name"], "proj-c");
        assert_eq!(state["project-order"], json!(["k-c"]));
        assert_eq!(pending(&w.app_state, w.home.root()).unwrap(), None);
        // The next launch has nothing to do.
        assert_eq!(
            prepare_launch(
                &w.home,
                &w.backups,
                Some(&w.app_state),
                Timestamp::from_millis(10)
            )
            .unwrap(),
            Prepared::default()
        );
    }
}

#[test]
fn a_token_launch_folds_and_restores_in_one_replacement() {
    let state = json!({
        ACCOUNT_STATE_KEY: {"owner": null, "inactive": {"h": {"atoms": {}, "globals": {
            "local-projects": {"local-a": {"id": "local-a", "name": "a", "rootPaths": ["/w/a"]}}
        }}}}
    });
    let w = world(PAT, &state);
    queue(
        &w.app_state,
        w.home.root(),
        // /w/a comes back with the fold already: not twice.
        vec![
            item("/w/a", "a again", None),
            item("/w/c", "c", Some("k-c")),
        ],
        Timestamp::EPOCH,
    )
    .unwrap();
    let done = prepare_launch(
        &w.home,
        &w.backups,
        Some(&w.app_state),
        Timestamp::from_millis(5),
    )
    .unwrap();
    assert_eq!(done.stashes, Some(1));
    assert_eq!(done.restored, 1);
    let got = read_state(&w.home);
    assert!(got.get(ACCOUNT_STATE_KEY).is_none());
    assert_eq!(keys(&got["local-projects"]), ["local-a", "k-c"]);
    assert_eq!(fs::read_dir(&w.backups).unwrap().count(), 1);
}

#[test]
fn the_queue_waits_when_the_state_is_missing_or_odd_and_ignores_other_homes() {
    // No state file yet: nothing written, the folders keep waiting.
    let w = world(CHATGPT, &json!({}));
    fs::remove_file(w.home.global_state_json()).unwrap();
    queue(
        &w.app_state,
        w.home.root(),
        vec![item("/w/c", "c", None)],
        Timestamp::EPOCH,
    )
    .unwrap();
    assert_eq!(
        prepare_launch(&w.home, &w.backups, Some(&w.app_state), Timestamp::EPOCH).unwrap(),
        Prepared::default()
    );
    assert!(pending(&w.app_state, w.home.root()).unwrap().is_some());
    assert!(!w.home.global_state_json().exists());

    // A project list of an unknown shape: an error, the file and queue stay.
    let odd = json!({"local-projects": ["x"]});
    let w = world(CHATGPT, &odd);
    queue(
        &w.app_state,
        w.home.root(),
        vec![item("/w/c", "c", None)],
        Timestamp::EPOCH,
    )
    .unwrap();
    assert!(prepare_launch(&w.home, &w.backups, Some(&w.app_state), Timestamp::EPOCH).is_err());
    assert_eq!(read_state(&w.home), odd);
    assert!(pending(&w.app_state, w.home.root()).unwrap().is_some());

    // Queued for another Codex home: untouched.
    let w = world(CHATGPT, &json!({"local-projects": {}}));
    queue(
        &w.app_state,
        Path::new("/elsewhere/.codex"),
        vec![item("/w/c", "c", None)],
        Timestamp::EPOCH,
    )
    .unwrap();
    assert_eq!(
        prepare_launch(&w.home, &w.backups, Some(&w.app_state), Timestamp::EPOCH).unwrap(),
        Prepared::default()
    );
    assert!(
        pending(&w.app_state, Path::new("/elsewhere/.codex"))
            .unwrap()
            .is_some()
    );
    assert_eq!(read_state(&w.home), json!({"local-projects": {}}));
}

#[test]
fn folders_already_present_clear_the_queue_without_a_write() {
    let state = json!({"local-projects": {"p": {"id": "p", "name": "c", "rootPaths": ["/w/c"]}}});
    let w = world(CHATGPT, &state);
    let before = fs::read(w.home.global_state_json()).unwrap();
    queue(
        &w.app_state,
        w.home.root(),
        vec![item("/w/c", "c", None)],
        Timestamp::EPOCH,
    )
    .unwrap();
    let done = prepare_launch(&w.home, &w.backups, Some(&w.app_state), Timestamp::EPOCH).unwrap();
    assert_eq!(done, Prepared::default());
    assert_eq!(fs::read(w.home.global_state_json()).unwrap(), before);
    assert_eq!(pending(&w.app_state, w.home.root()).unwrap(), None);
    assert!(!w.backups.exists());
    let _ = project_restore::path_key(Path::new("/"));
}
