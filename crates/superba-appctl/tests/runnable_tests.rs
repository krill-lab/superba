//! The Codex Superba runs for an app install (user bug 143, GH#32): the MSIX
//! app's packaged `codex.exe` (which Windows does not let other programs start)
//! is replaced by the app's extracted copy only when its SHA-256 is identical;
//! without one the error says what to do; any other install is unchanged.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use superba_appctl::install::{AppxPackage, appx_install, inspect_install};
use superba_appctl::runnable::{
    cached_sha256, is_packaged, local_app_data, resolve_packaged, runnable_codex,
};
use superba_core::{AppInstall, Error, MapEnv};
use tempfile::tempdir;

const FULL: &str = "OpenAI.Codex_26.1106.51428.0_x64__2p2nqsd0c76g0";
const FAMILY: &str = "OpenAI.Codex_2p2nqsd0c76g0";
const CODEX: &str = "the packaged codex.exe, 0.167.0-alpha.4.1";

fn write(path: impl AsRef<Path>, text: &str) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// A registered MSIX install under `<temp>\Program Files\WindowsApps`.
fn msix_install(temp: &Path) -> AppInstall {
    let root = temp.join("Program Files").join("WindowsApps").join(FULL);
    write(root.join("AppxManifest.xml"), "<Package/>");
    write(root.join("app/ChatGPT.exe"), "desktop");
    write(root.join("app/resources/app.asar"), "archive");
    write(root.join("app/resources/codex.exe"), CODEX);
    let install = appx_install(&AppxPackage {
        name: "OpenAI.Codex".into(),
        family_name: FAMILY.into(),
        install_location: root,
        version: Some("26.1106.51428.0".into()),
    })
    .unwrap();
    assert_eq!(install.id, FAMILY);
    install
}

fn bundled(install: &AppInstall) -> &Path {
    install.bundled_codex.as_deref().expect("bundled codex.exe")
}

/// `%LOCALAPPDATA%` of the fake user.
fn local(temp: &Path) -> PathBuf {
    temp.join("Users").join("me").join("AppData").join("Local")
}

/// The app's extracted copy `OpenAI\Codex\bin\<hash>\codex.exe`, written at `secs`.
fn extracted(local: &Path, hash: &str, text: &str, secs: u64) -> PathBuf {
    let path = local
        .join("OpenAI")
        .join("Codex")
        .join("bin")
        .join(hash)
        .join("codex.exe");
    write(&path, text);
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(secs))
        .unwrap();
    path
}

fn packaged_error(binary: &Path) -> Error {
    Error::PackagedCodex {
        path: binary.to_path_buf(),
    }
}

#[test]
fn a_packaged_codex_runs_as_the_extracted_copy_with_the_same_sha256() {
    let temp = tempdir().unwrap();
    let install = msix_install(temp.path());
    let local = local(temp.path());
    // An older extraction of another version, and the current one.
    extracted(
        &local,
        "0123456789abcdef",
        "an older codex.exe",
        1_700_000_000,
    );
    let copy = extracted(&local, "896a0166b774037a", CODEX, 1_750_000_000);
    assert_eq!(
        resolve_packaged(bundled(&install), Some(&local)).unwrap(),
        copy
    );
    assert_eq!(cached_sha256(&copy), cached_sha256(bundled(&install)));
}

#[test]
fn a_copy_of_the_same_size_but_another_sha256_is_never_run() {
    let temp = tempdir().unwrap();
    let install = msix_install(temp.path());
    let local = local(temp.path());
    let other = "x".repeat(CODEX.len());
    let copy = extracted(&local, "896a0166b774037a", &other, 1_750_000_000);
    let err = resolve_packaged(bundled(&install), Some(&local)).unwrap_err();
    assert_eq!(err, packaged_error(bundled(&install)));

    // The app extracts the current version again: the new stamp is hashed anew.
    extracted(&local, "896a0166b774037a", CODEX, 1_760_000_000);
    assert_eq!(
        resolve_packaged(bundled(&install), Some(&local)).unwrap(),
        copy
    );
}

#[test]
fn without_an_extracted_copy_the_error_says_what_to_do() {
    let temp = tempdir().unwrap();
    let install = msix_install(temp.path());
    let local = local(temp.path());
    let err = resolve_packaged(bundled(&install), Some(&local)).unwrap_err();
    assert_eq!(err, packaged_error(bundled(&install)));
    assert_eq!(err.code(), "packaged_codex");
    let text = err.to_string();
    assert!(text.contains("Open the ChatGPT app once"), "{text}");
    assert!(!text.contains("os error"), "{text}");
    assert_eq!(err.to_text().key, "error.packaged_codex");

    // No %LOCALAPPDATA% at all.
    let err = resolve_packaged(bundled(&install), None).unwrap_err();
    assert_eq!(err, packaged_error(bundled(&install)));

    // A copy whose package file is gone cannot be verified either.
    let copy = extracted(&local, "896a0166b774037a", CODEX, 1_750_000_000);
    fs::remove_file(bundled(&install)).unwrap();
    assert!(resolve_packaged(bundled(&install), Some(&local)).is_err());
    assert!(copy.is_file());
}

#[test]
fn only_windows_treats_windowsapps_paths_as_packaged() {
    let temp = tempdir().unwrap();
    let install = msix_install(temp.path());
    let local = local(temp.path());
    let env = MapEnv::new().with("LOCALAPPDATA", &local);
    let copy = extracted(&local, "896a0166b774037a", CODEX, 1_750_000_000);
    let got = runnable_codex(bundled(&install), &env).unwrap();
    if cfg!(windows) {
        assert!(is_packaged(bundled(&install)));
        assert_eq!(got, copy);
    } else {
        // Linux and macOS: unchanged, whatever the path looks like.
        assert!(!is_packaged(bundled(&install)));
        assert_eq!(got, bundled(&install));
    }
}

#[test]
fn a_codex_outside_a_package_is_run_where_it_is() {
    let temp = tempdir().unwrap();
    // A plain (Squirrel / deb / dmg-like) install with a bundled codex.exe.
    let root = temp.path().join("Programs").join("ChatGPT");
    write(root.join("ChatGPT.exe"), "desktop");
    write(root.join("resources/app.asar"), "archive");
    write(root.join("resources/codex.exe"), CODEX);
    let install = inspect_install(&root.join("ChatGPT.exe")).unwrap();
    let binary = bundled(&install).to_path_buf();
    assert!(!is_packaged(&binary));
    // Even with an identical extracted copy around, the bundled path is kept.
    let local = local(temp.path());
    extracted(&local, "896a0166b774037a", CODEX, 1_750_000_000);
    let env = MapEnv::new().with("LOCALAPPDATA", &local);
    assert_eq!(runnable_codex(&binary, &env).unwrap(), binary);
    // A missing file is not checked here: running it reports what happens.
    let missing = temp.path().join("nowhere").join("codex");
    assert_eq!(runnable_codex(&missing, &env).unwrap(), missing);
}

#[test]
fn local_app_data_falls_back_to_the_user_profile() {
    let env = MapEnv::new().with("LOCALAPPDATA", "/l");
    assert_eq!(local_app_data(&env), Some(PathBuf::from("/l")));
    let env = MapEnv::new()
        .with("LOCALAPPDATA", "")
        .with("USERPROFILE", "/u");
    assert_eq!(
        local_app_data(&env),
        Some(Path::new("/u").join("AppData").join("Local"))
    );
    assert_eq!(local_app_data(&MapEnv::new()), None);
}
