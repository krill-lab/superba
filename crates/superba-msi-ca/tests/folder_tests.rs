//! `can_install_into`: the folder page's writability check (user bug 64).

use std::fs;
use std::path::Path;

use superba_msi_ca::can_install_into;

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn an_existing_writable_folder_passes_and_keeps_no_probe() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("keep.txt"), "x").unwrap();
    assert!(can_install_into(tmp.path()));
    assert_eq!(entries(tmp.path()), ["keep.txt"]);
}

#[test]
fn a_missing_folder_is_checked_in_the_nearest_existing_one() {
    let tmp = tempfile::tempdir().unwrap();
    // Spaces and CJK characters, as in "D:\我的 软件\Superba".
    let dir = tmp
        .path()
        .join("\u{6211}\u{7684} \u{8F6F}\u{4EF6}")
        .join("Superba");
    assert!(can_install_into(&dir));
    assert!(!dir.exists(), "the check creates no install folder");
    assert!(entries(tmp.path()).is_empty(), "nothing left behind");
}

#[test]
fn a_file_in_place_of_the_folder_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("Superba");
    fs::write(&file, "x").unwrap();
    assert!(!can_install_into(&file));
    assert!(!can_install_into(&file.join("below")));
}

#[test]
fn empty_and_relative_paths_are_refused() {
    assert!(!can_install_into(Path::new("")));
    assert!(!can_install_into(Path::new("Superba")));
    assert!(!can_install_into(Path::new("Apps/Superba")));
}

#[cfg(unix)]
#[test]
fn a_read_only_folder_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let locked = tmp.path().join("locked");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
    // Root writes anywhere: only meaningful for an ordinary user.
    let root_can_write = fs::write(locked.join("probe"), "x").is_ok();
    let _ = fs::remove_file(locked.join("probe"));
    if !root_can_write {
        assert!(!can_install_into(&locked));
        assert!(!can_install_into(&locked.join("Superba")));
        assert!(entries(&locked).is_empty());
    }
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
}
