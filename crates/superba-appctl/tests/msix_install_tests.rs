//! MSIX installs (user bug 40): the install built from the package manifest
//! without listing the package folder, the family id for a running app found
//! by its executable, matching of older `path:` ids, and the bundled Codex
//! version read from the binary when it cannot be run.

use std::fs;
use std::path::{Path, PathBuf};

use superba_appctl::appx::canonical;
use superba_appctl::codexbin::version_in_bytes;
use superba_appctl::install::{
    AppxPackage, appx_install, inspect_install, install_matches, parse_appx_applications,
};
use superba_core::{AppVariant, CodexVersion};
use tempfile::tempdir;

const FULL: &str = "OpenAI.Codex_26.1013.4894.0_x64__2p2nqsd0c76g0";
const FAMILY: &str = "OpenAI.Codex_2p2nqsd0c76g0";

/// The manifest of the real package (26.1013.4894.0), reduced to what is read.
const MANIFEST: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10">
  <Identity Name="OpenAI.Codex" ProcessorArchitecture="x64" Version="26.1013.4894.0" Publisher="CN=50BDFD77-8903-4850-9FFE-6E8522F64D5B" />
  <Applications>
    <Application Id="App" Executable="app/ChatGPT.exe" EntryPoint="Windows.FullTrustApplication">
      <uap:VisualElements DisplayName="ChatGPT" Description="ChatGPT" />
    </Application>
    <Application Id="CodexCoreCommandRunner" Executable="app/resources/codex-command-runner.exe" EntryPoint="Windows.FullTrustApplication" />
  </Applications>
</Package>"#;

fn write(path: impl AsRef<Path>, text: &str) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// A package folder laid out like `WindowsApps\<full name>`.
fn package(parent: &Path) -> PathBuf {
    let root = parent.join(FULL);
    write(root.join("AppxManifest.xml"), MANIFEST);
    write(root.join("app/ChatGPT.exe"), "desktop");
    write(root.join("app/Codex.exe"), "stub");
    write(root.join("app/resources/app.asar"), "archive");
    write(root.join("app/resources/codex.exe"), "cli");
    write(root.join("app/resources/codex"), "linux cli for WSL");
    write(
        root.join("app/resources/owl-app.ini"),
        "[Owl]\nUserDataDirectoryName=Codex\nAppVersion=26.1013.40934\n",
    );
    // Canonical in the plain form installs use (no `\\?\` prefix on Windows).
    canonical(&root).unwrap()
}

fn registered(root: &Path) -> AppxPackage {
    AppxPackage {
        name: "OpenAI.Codex".into(),
        family_name: FAMILY.into(),
        install_location: root.to_path_buf(),
        version: Some("26.1013.4894.0".into()),
    }
}

#[test]
fn manifest_applications_keep_order_ids_and_executables() {
    let apps = parse_appx_applications(MANIFEST);
    assert_eq!(
        apps,
        vec![
            ("App".to_owned(), Some("app/ChatGPT.exe".to_owned())),
            (
                "CodexCoreCommandRunner".to_owned(),
                Some("app/resources/codex-command-runner.exe".to_owned())
            ),
        ]
    );
    assert!(parse_appx_applications("<Package/>").is_empty());
}

#[test]
fn a_registered_package_is_built_from_its_manifest() {
    let temp = tempdir().unwrap();
    let root = package(temp.path());
    let install = appx_install(&registered(&root)).unwrap();
    assert_eq!(install.id, FAMILY);
    assert_eq!(install.variant, AppVariant::ChatGpt);
    assert_eq!(install.path, root);
    assert_eq!(install.executable, root.join("app").join("ChatGPT.exe"));
    assert_eq!(
        install.bundled_codex,
        Some(root.join("app").join("resources").join("codex.exe"))
    );
    // The desktop app version (owl-app.ini), not the package version.
    assert_eq!(install.app_version.as_deref(), Some("26.1013.40934"));

    // Without a readable manifest the known layout is assumed.
    fs::remove_file(root.join("AppxManifest.xml")).unwrap();
    let install = appx_install(&registered(&root)).unwrap();
    assert_eq!(install.executable, root.join("app").join("ChatGPT.exe"));
}

#[test]
fn a_running_packaged_app_has_the_family_id_of_its_install() {
    let temp = tempdir().unwrap();
    let root = package(temp.path());
    let from_exe = inspect_install(&root.join("app/ChatGPT.exe")).unwrap();
    let registered = appx_install(&registered(&root)).unwrap();
    assert_eq!(from_exe.id, FAMILY);
    assert_eq!(from_exe.id, registered.id);
    assert_eq!(from_exe.executable, registered.executable);
    assert_eq!(from_exe.app_version.as_deref(), Some("26.1013.40934"));

    // A folder that is not named like a package keeps its path id.
    let plain = temp.path().join("Unpacked");
    fs::rename(&root, &plain).unwrap();
    let inspected = inspect_install(&plain.join("app/ChatGPT.exe")).unwrap();
    assert!(inspected.id.starts_with("path:"), "{}", inspected.id);
}

#[test]
fn older_path_ids_still_name_the_install() {
    let temp = tempdir().unwrap();
    let root = package(temp.path());
    let install = appx_install(&registered(&root)).unwrap();
    assert!(install_matches(&install, FAMILY));
    assert!(install_matches(
        &install,
        &format!("path:{}", root.display())
    ));
    assert!(install_matches(
        &install,
        &format!("path:{}", root.join("app").join("ChatGPT.exe").display())
    ));
    assert!(!install_matches(&install, "OpenAI.CodexBeta_2p2nqsd0c76g0"));
    assert!(!install_matches(&install, "path:/somewhere/else"));
    // The verbatim form `fs::canonicalize` gives on Windows (user bug 40's id).
    let windows = superba_core::AppInstall {
        id: FAMILY.into(),
        path: PathBuf::from(format!(r"C:\Program Files\WindowsApps\{FULL}")),
        executable: PathBuf::from(format!(
            r"C:\Program Files\WindowsApps\{FULL}\app\ChatGPT.exe"
        )),
        ..install
    };
    assert!(install_matches(
        &windows,
        &format!(r"path:\\?\C:\Program Files\WindowsApps\{FULL}")
    ));
}

#[test]
fn the_codex_version_is_read_from_the_binary_without_running_it() {
    let v = |s: &str| CodexVersion::parse(s).unwrap();
    // Excerpts of the 0.162.0-alpha.17.2 binary: the version runs into the
    // next string constant.
    let bytes = b"\0\0eadableLinux sandbox helper path does not existcodex-doctor/0.162.0-alpha.17.2version response exceeds size limit";
    assert_eq!(version_in_bytes(bytes), Some(v("0.162.0-alpha.17.2")));
    let bytes = b"versionCodex App Server Daemon0.163.0failed to send initialize request";
    assert_eq!(version_in_bytes(bytes), Some(v("0.163.0")));
    // A marker without a version (a format piece) is skipped for a later one.
    let bytes = b"codex-doctor/{}\0codex-doctor/0.160.1-beta.2x";
    assert_eq!(version_in_bytes(bytes), Some(v("0.160.1-beta.2")));
    assert_eq!(version_in_bytes(b"codex-cli 0.1.0"), None);
    assert_eq!(version_in_bytes(b"codex-doctor/1.2"), None);
}
