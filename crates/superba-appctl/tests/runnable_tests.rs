//! The Codex Superba runs for an app install (user bug 143, GH#32): the MSIX
//! app's packaged `codex.exe` (which Windows does not let other programs start)
//! is replaced by the app's extracted copy only when its SHA-256 is identical,
//! else by Superba's own copy (found, or made the way the app makes its own:
//! 26.1111 with its app-contained core makes none); when that fails the error
//! says why and what to do; any other install is unchanged. The "packaged"
//! check is forced in the copy tests, so they run on every platform.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use superba_appctl::install::{AppxPackage, appx_install, inspect_install};
use superba_appctl::runnable::{
    PackagedCopies, RELOCATED_SIBLINGS, cached_sha256, copy_folder_name, copy_set, is_packaged,
    local_app_data, resolve_packaged, resolve_packaged_with, runnable_codex_with,
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
        reason: "no identical copy, and none is made here".into(),
    }
}

/// The production "packaged" check, and one that holds for every path.
fn always(_: &Path) -> bool {
    true
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
    assert!(
        text.contains("%LOCALAPPDATA%\\OpenAI\\Codex\\bin"),
        "{text}"
    );
    assert!(text.contains("about 1 GB free"), "{text}");
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
    let copies = PackagedCopies {
        local_app_data: local_app_data(&MapEnv::new().with("LOCALAPPDATA", &local)),
        own_dir: Some(temp.path().join("data").join("app-codex")),
    };
    let copy = extracted(&local, "896a0166b774037a", CODEX, 1_750_000_000);
    let got = runnable_codex_with(bundled(&install), &copies, &is_packaged).unwrap();
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
    let own = temp.path().join("data").join("app-codex");
    let copies = PackagedCopies {
        local_app_data: Some(local),
        own_dir: Some(own.clone()),
    };
    assert_eq!(
        runnable_codex_with(&binary, &copies, &is_packaged).unwrap(),
        binary
    );
    // A missing file is not checked here: running it reports what happens.
    let missing = temp.path().join("nowhere").join("codex");
    assert_eq!(
        runnable_codex_with(&missing, &copies, &is_packaged).unwrap(),
        missing
    );
    // Nothing is copied for a program outside a package.
    assert!(!own.exists());
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

// ---------------------------------------------------------------------------
// Superba's own copy (26.1111: the app's app-contained core makes no copy)
// ---------------------------------------------------------------------------

/// Two of the three helpers the app copies next to `codex.exe` (the sandbox
/// setup is left out: a helper the package lacks is skipped).
fn with_helpers(install: &AppInstall) -> PathBuf {
    let resources = bundled(install).parent().unwrap().to_path_buf();
    write(resources.join("codex-code-mode-host.exe"), "code mode host");
    write(resources.join("codex-command-runner.exe"), "command runner");
    resources
}

fn own_copies(temp: &Path) -> (PackagedCopies, PathBuf) {
    let own = temp.join("data").join("app-codex");
    let copies = PackagedCopies {
        local_app_data: Some(local(temp)),
        own_dir: Some(own.clone()),
    };
    (copies, own)
}

fn dir_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn the_copy_folder_is_named_like_the_apps_own() {
    let temp = tempdir().unwrap();
    let install = msix_install(temp.path());
    with_helpers(&install);
    let files = copy_set(bundled(&install)).unwrap();
    let names: Vec<_> = files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "codex.exe",
            "codex-code-mode-host.exe",
            "codex-command-runner.exe"
        ]
    );
    assert_eq!(
        RELOCATED_SIBLINGS,
        [
            "codex-code-mode-host.exe",
            "codex-windows-sandbox-setup.exe",
            "codex-command-runner.exe"
        ]
    );
    // sha256("codex.exe\0<hex>\0codex-code-mode-host.exe\0<hex>\0...")[..16],
    // computed independently (the app's `M(...).slice(0,16)`).
    assert_eq!(copy_folder_name(&files), "09624f131e266d46");
}

#[test]
fn without_any_copy_superba_copies_the_package_files_and_runs_its_copy() {
    let temp = tempdir().unwrap();
    let install = msix_install(temp.path());
    with_helpers(&install);
    let (copies, own) = own_copies(temp.path());
    // The app (26.1111, app-contained core) left nothing in %LOCALAPPDATA%.
    let got = runnable_codex_with(bundled(&install), &copies, &always).unwrap();
    let folder = own.join("09624f131e266d46");
    assert_eq!(got, folder.join("codex.exe"));
    assert_eq!(fs::read_to_string(&got).unwrap(), CODEX);
    assert_eq!(
        dir_names(&folder),
        [
            "codex-code-mode-host.exe",
            "codex-command-runner.exe",
            "codex.exe"
        ]
    );
    // No staging folder is left behind; the package is untouched.
    assert_eq!(dir_names(&own), ["09624f131e266d46"]);
    assert_eq!(fs::read_to_string(bundled(&install)).unwrap(), CODEX);

    // The next run reuses the verified copy (nothing is copied again).
    let stamp = fs::metadata(&got).unwrap().modified().unwrap();
    let again = runnable_codex_with(bundled(&install), &copies, &always).unwrap();
    assert_eq!(again, got);
    assert_eq!(fs::metadata(&got).unwrap().modified().unwrap(), stamp);
}

#[test]
fn the_apps_identical_copy_comes_before_superbas_own() {
    let temp = tempdir().unwrap();
    let install = msix_install(temp.path());
    let (copies, own) = own_copies(temp.path());
    let app = extracted(
        &local(temp.path()),
        "896a0166b774037a",
        CODEX,
        1_750_000_000,
    );
    assert_eq!(
        runnable_codex_with(bundled(&install), &copies, &always).unwrap(),
        app
    );
    assert!(!own.exists());
}

/// A directory opened so that its times can be set: Windows opens a
/// directory only with `FILE_FLAG_BACKUP_SEMANTICS` (and write access for
/// `set_modified`); elsewhere a plain open does.
fn open_dir(path: &Path) -> fs::File {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        fs::File::options()
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .unwrap()
    }
    #[cfg(not(windows))]
    {
        fs::File::open(path).unwrap()
    }
}

#[test]
fn an_app_update_makes_a_new_copy_and_prunes_the_old_ones() {
    let temp = tempdir().unwrap();
    let install = msix_install(temp.path());
    with_helpers(&install);
    let (copies, own) = own_copies(temp.path());
    let first = runnable_codex_with(bundled(&install), &copies, &always).unwrap();
    // Leftovers: an interrupted copy of long ago and a foreign folder.
    let staging = own.join(".staging-0123456789abcdef-x");
    write(staging.join("codex.exe"), "half");
    open_dir(&staging)
        .set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
        .unwrap();
    write(own.join("notes").join("keep.txt"), "not a copy");

    // The package updates: another codex.exe.
    write(bundled(&install), "the packaged codex.exe, 0.168.0");
    let second = runnable_codex_with(bundled(&install), &copies, &always).unwrap();
    assert_ne!(second, first);
    assert_eq!(
        fs::read_to_string(&second).unwrap(),
        "the packaged codex.exe, 0.168.0"
    );
    let folder = second
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy();
    assert_eq!(dir_names(&own), [folder.as_ref(), "notes"]);
}

#[test]
fn a_damaged_own_copy_is_never_run_and_is_made_again() {
    let temp = tempdir().unwrap();
    let install = msix_install(temp.path());
    with_helpers(&install);
    let (copies, _) = own_copies(temp.path());
    let copy = runnable_codex_with(bundled(&install), &copies, &always).unwrap();
    // Same size, other bytes, later stamp.
    write(&copy, &"x".repeat(CODEX.len()));
    fs::File::options()
        .write(true)
        .open(&copy)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(1_760_000_000))
        .unwrap();
    let again = runnable_codex_with(bundled(&install), &copies, &always).unwrap();
    assert_eq!(again, copy);
    assert_eq!(fs::read_to_string(&again).unwrap(), CODEX);
    // A missing helper of the copy is made again too.
    let helper = copy.parent().unwrap().join("codex-command-runner.exe");
    fs::remove_file(&helper).unwrap();
    runnable_codex_with(bundled(&install), &copies, &always).unwrap();
    assert_eq!(fs::read_to_string(&helper).unwrap(), "command runner");
}

#[test]
fn when_superba_cannot_copy_the_error_says_why() {
    let temp = tempdir().unwrap();
    let install = msix_install(temp.path());
    // The data folder cannot hold a folder: a file is in the way.
    let own = temp.path().join("data").join("app-codex");
    write(&own, "a file");
    let copies = PackagedCopies {
        local_app_data: Some(local(temp.path())),
        own_dir: Some(own.clone()),
    };
    let err = resolve_packaged_with(bundled(&install), &copies).unwrap_err();
    let Error::PackagedCodex { path, reason } = &err else {
        panic!("{err:?}");
    };
    assert_eq!(path, bundled(&install));
    assert!(reason.contains(&own.display().to_string()), "{reason}");
    let text = err.to_text();
    assert_eq!(text.key, "error.packaged_codex");
    assert!(text.render_fallback().contains(reason.as_str()));

    // A package file that cannot be read cannot be copied either.
    fs::remove_file(&own).unwrap();
    fs::remove_file(bundled(&install)).unwrap();
    let err = resolve_packaged_with(bundled(&install), &copies).unwrap_err();
    assert_eq!(err.code(), "packaged_codex");
    assert!(!own.join("09624f131e266d46").exists());
}
