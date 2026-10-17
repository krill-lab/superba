//! The Windows installer's fallback for the router's login startup
//! (`remove_run_entry_of`): only the Run entry that starts the router of the
//! install being removed goes, whatever way its folder is written.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use superba_router::service::{RUN_VALUE_NAME, RunKeyStore, remove_run_entry_of};

#[derive(Debug, Default)]
struct Store(Mutex<BTreeMap<String, String>>);

impl RunKeyStore for Store {
    fn get(&self, name: &str) -> io::Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(name).cloned())
    }
    fn set(&self, name: &str, value: &str) -> io::Result<()> {
        self.0.lock().unwrap().insert(name.into(), value.into());
        Ok(())
    }
    fn delete(&self, name: &str) -> io::Result<()> {
        self.0.lock().unwrap().remove(name);
        Ok(())
    }
}

fn store_with(command: &str) -> Store {
    let store = Store::default();
    store.set(RUN_VALUE_NAME, command).unwrap();
    store
}

fn router_in(dir: &Path) -> PathBuf {
    dir.join(format!("superba-router{}", std::env::consts::EXE_SUFFIX))
}

/// A Run command the way the service writes it: quoted paths with spaces.
fn command(exe: &Path, data: &Path) -> String {
    format!(
        "\"{}\" supervise --data-dir \"{}\"",
        exe.display(),
        data.display()
    )
}

#[test]
fn only_the_entry_that_starts_this_installs_router_is_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("Programs").join("Superba Beta");
    let other = tmp.path().join("Downloads").join("superba-0.1.0");
    let data = tmp.path().join("data dir");
    for dir in [&install, &other] {
        std::fs::create_dir_all(dir).unwrap();
    }

    let store = store_with(&command(&router_in(&install), &data));
    assert!(remove_run_entry_of(&store, &install).unwrap());
    assert_eq!(store.get(RUN_VALUE_NAME).unwrap(), None);
    assert!(
        !remove_run_entry_of(&store, &install).unwrap(),
        "nothing left to remove"
    );

    // The router of another copy keeps its login startup.
    let store = store_with(&command(&router_in(&other), &data));
    assert!(!remove_run_entry_of(&store, &install).unwrap());
    assert!(store.get(RUN_VALUE_NAME).unwrap().is_some());

    // A value that does not start superba-router is not the router's.
    let store = store_with(&command(&install.join("superba"), &data));
    assert!(!remove_run_entry_of(&store, &install).unwrap());
    assert!(store.get(RUN_VALUE_NAME).unwrap().is_some());
}

#[test]
fn a_bare_program_path_and_another_spelling_of_the_folder_still_match() {
    let tmp = tempfile::tempdir().unwrap();
    let install = tmp.path().join("Superba");
    std::fs::create_dir_all(&install).unwrap();
    let bare = format!("{} supervise --data-dir x", router_in(&install).display());

    let store = store_with(&bare);
    let roundabout = install.join("..").join("Superba");
    assert!(remove_run_entry_of(&store, &roundabout).unwrap());

    #[cfg(windows)]
    {
        // Windows paths are case-insensitive.
        let store = store_with(&command(&router_in(&install), &install));
        let upper = PathBuf::from(install.display().to_string().to_uppercase());
        assert!(remove_run_entry_of(&store, &upper).unwrap());
    }
}
