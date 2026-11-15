//! The app location of the settings, installs found through the uninstall
//! entries of the Windows registry (parsed from given values, so these run on
//! every platform), the detection order and the location check.

use std::fs;
use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use superba_appctl::appx::canonical;
use superba_appctl::install::{
    UninstallEntry, check_location, uninstall_candidates, uninstall_entry_dirs,
    uninstall_entry_matches,
};
use superba_core::{AppLocationCheck, CodexVersion, MapEnv};
use tempfile::tempdir;

fn write(path: impl AsRef<Path>, text: &str) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// A Windows-style install: `ChatGPT.exe`, `resources/app.asar`, the app
/// version in `owl-app.ini` and, with `codex`, `resources/codex.exe` whose
/// version the package JSON names (nothing is run).
fn windows_app(root: &Path, codex: bool) -> PathBuf {
    write(root.join("ChatGPT.exe"), "fake desktop executable");
    write(root.join("resources/app.asar"), "fake archive");
    write(
        root.join("resources/owl-app.ini"),
        "[Owl]\nAppVersion=26.1013.40934\n",
    );
    if codex {
        write(
            root.join("resources/codex.exe"),
            "fake CLI; must not be executed by inspection",
        );
        write(
            root.join("resources/codex-cli/codex-package.json"),
            r#"{"version":"0.130.0","entrypoint":"bin/codex.exe"}"#,
        );
    }
    canonical(root).unwrap()
}

fn entry(name: &str, publisher: &str) -> UninstallEntry {
    UninstallEntry {
        display_name: Some(name.into()),
        publisher: Some(publisher.into()),
        ..Default::default()
    }
}

fn strings(paths: Vec<PathBuf>) -> Vec<String> {
    paths
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn uninstall_entries_match_chatgpt_codex_and_openai_but_not_superba() {
    assert!(uninstall_entry_matches(&entry("ChatGPT", "")));
    assert!(uninstall_entry_matches(&entry("Codex 26.1013.40934", "")));
    assert!(uninstall_entry_matches(&entry("Desktop", "OpenAI, Inc.")));
    assert!(uninstall_entry_matches(&entry("chatgpt (beta)", "Someone")));
    assert!(!uninstall_entry_matches(&entry(
        "Superba for Codex",
        "OpenAI"
    )));
    assert!(!uninstall_entry_matches(&entry("7-Zip", "Igor Pavlov")));
    assert!(!uninstall_entry_matches(&UninstallEntry::default()));
}

#[test]
fn install_location_comes_first_then_icon_and_uninstaller_folders() {
    let env = MapEnv::new();
    let full = UninstallEntry {
        install_location: Some("\"D:\\Apps\\ChatGPT\\\"".into()),
        display_icon: Some("\"D:\\Apps\\ChatGPT\\ChatGPT.exe\",0".into()),
        uninstall_string: Some("\"E:\\Other\\Uninstall ChatGPT.exe\" /currentuser".into()),
        ..entry("ChatGPT", "OpenAI")
    };
    // The icon's folder is the install location again (case and the trailing
    // separator aside): listed once.
    assert_eq!(
        strings(uninstall_entry_dirs(&full, &env)),
        ["D:\\Apps\\ChatGPT", "E:\\Other"]
    );
}

#[test]
fn icon_and_uninstall_string_give_the_folder_without_install_location() {
    let env = MapEnv::new();
    // An unquoted icon with an index; an unquoted uninstaller path with spaces
    // and arguments.
    let e = UninstallEntry {
        display_icon: Some("D:\\Program Files\\ChatGPT\\ChatGPT.exe,-101".into()),
        uninstall_string: Some(
            "D:\\Program Files\\ChatGPT\\Uninstall ChatGPT.exe /S /currentuser".into(),
        ),
        ..entry("ChatGPT", "OpenAI")
    };
    assert_eq!(
        strings(uninstall_entry_dirs(&e, &env)),
        ["D:\\Program Files\\ChatGPT"]
    );
    // An installer's uninstaller only.
    let e = UninstallEntry {
        uninstall_string: Some(
            "\"C:\\Users\\me\\AppData\\Local\\Programs\\ChatGPT\\Uninstall ChatGPT.exe\" /currentuser"
                .into(),
        ),
        ..entry("ChatGPT", "OpenAI")
    };
    assert_eq!(
        strings(uninstall_entry_dirs(&e, &env)),
        ["C:\\Users\\me\\AppData\\Local\\Programs\\ChatGPT"]
    );
    // A drive root keeps its separator.
    let e = UninstallEntry {
        display_icon: Some("\"F:\\ChatGPT.exe\"".into()),
        ..entry("ChatGPT", "OpenAI")
    };
    assert_eq!(strings(uninstall_entry_dirs(&e, &env)), ["F:\\"]);
}

#[test]
fn msiexec_relative_and_empty_values_are_dropped() {
    let env = MapEnv::new();
    let e = UninstallEntry {
        install_location: Some("   ".into()),
        display_icon: Some("ChatGPT.exe,0".into()),
        uninstall_string: Some("MsiExec.exe /X{01234567-89AB-CDEF-0123-456789ABCDEF}".into()),
        ..entry("ChatGPT", "OpenAI")
    };
    assert!(uninstall_entry_dirs(&e, &env).is_empty());
    let e = UninstallEntry {
        uninstall_string: Some("C:\\Windows\\System32\\msiexec.exe /X{GUID}".into()),
        ..entry("ChatGPT", "OpenAI")
    };
    assert!(uninstall_entry_dirs(&e, &env).is_empty());
}

#[test]
fn expandable_values_use_the_supplied_environment() {
    let env = MapEnv::new().with("LOCALAPPDATA", "C:\\Users\\me\\AppData\\Local");
    let e = UninstallEntry {
        install_location: Some("%LOCALAPPDATA%\\Programs\\ChatGPT".into()),
        display_icon: Some("%UNKNOWN%\\ChatGPT.exe,0".into()),
        ..entry("ChatGPT", "OpenAI")
    };
    // An unknown variable stays as written, which is not an absolute path.
    assert_eq!(
        strings(uninstall_entry_dirs(&e, &env)),
        ["C:\\Users\\me\\AppData\\Local\\Programs\\ChatGPT"]
    );
}

#[test]
fn candidates_keep_entry_order_skip_other_programs_and_dedupe() {
    let env = MapEnv::new();
    let entries = [
        UninstallEntry {
            install_location: Some("D:\\Tools\\Notepad++".into()),
            ..entry("Notepad++", "Notepad++ Team")
        },
        UninstallEntry {
            install_location: Some("D:\\Apps\\ChatGPT".into()),
            ..entry("ChatGPT", "OpenAI")
        },
        UninstallEntry {
            install_location: Some("E:\\Codex".into()),
            ..entry("Codex", "OpenAI")
        },
        // The same install registered for the machine and the user.
        UninstallEntry {
            display_icon: Some("D:\\Apps\\ChatGPT\\ChatGPT.exe,0".into()),
            ..entry("ChatGPT", "OpenAI")
        },
    ];
    assert_eq!(
        strings(uninstall_candidates(&entries, &env)),
        ["D:\\Apps\\ChatGPT", "E:\\Codex"]
    );
}

#[test]
fn a_registry_entry_for_a_custom_folder_is_an_install() {
    let dir = tempdir().unwrap();
    let root = windows_app(&dir.path().join("Custom Apps/ChatGPT"), true);
    let e = UninstallEntry {
        display_icon: Some(format!("\"{}\",0", root.join("ChatGPT.exe").display())),
        uninstall_string: Some(format!(
            "\"{}\" /currentuser",
            root.join("Uninstall ChatGPT.exe").display()
        )),
        ..entry("ChatGPT", "OpenAI")
    };
    let dirs = uninstall_candidates(&[e], &MapEnv::new());
    assert_eq!(dirs, std::slice::from_ref(&root));
    let install = superba_appctl::install::inspect_install(&dirs[0]).unwrap();
    assert_eq!(install.path, root);
    assert_eq!(install.app_version.as_deref(), Some("26.1013.40934"));
    assert_eq!(
        install.bundled_codex,
        Some(root.join("resources").join("codex.exe"))
    );
    assert_eq!(
        install.bundled_codex_version,
        Some(CodexVersion::parse("0.130.0").unwrap())
    );
}

#[test]
fn location_check_tells_missing_not_an_app_and_no_bundled_codex_apart() {
    let dir = tempdir().unwrap();
    assert_eq!(
        check_location(&dir.path().join("nowhere")),
        AppLocationCheck::Missing
    );
    // A folder without resources/app.asar (here: only the executable).
    write(dir.path().join("empty/ChatGPT.exe"), "exe");
    assert_eq!(
        check_location(&dir.path().join("empty")),
        AppLocationCheck::NotAnApp
    );
    // A file that is not the app.
    write(dir.path().join("notes.txt"), "text");
    assert_eq!(
        check_location(&dir.path().join("notes.txt")),
        AppLocationCheck::NotAnApp
    );
    let bare = windows_app(&dir.path().join("bare"), false);
    let check = check_location(&bare);
    assert!(matches!(check, AppLocationCheck::NoBundledCodex { .. }));
    assert!(check.usable());
    assert_eq!(check.install().unwrap().path, bare);
    let full = windows_app(&dir.path().join("full"), true);
    // The folder and the executable in it are the same install.
    for path in [full.clone(), full.join("ChatGPT.exe")] {
        let AppLocationCheck::Install { install } = check_location(&path) else {
            panic!("{} is an install", path.display());
        };
        assert_eq!(install.path, full);
        assert_eq!(install.executable, full.join("ChatGPT.exe"));
        assert_eq!(
            install.bundled_codex_version,
            Some(CodexVersion::parse("0.130.0").unwrap())
        );
    }
}

#[test]
fn location_check_texts_name_the_path_and_versions() {
    let dir = tempdir().unwrap();
    let full = windows_app(&dir.path().join("full"), true);
    let text = check_location(&full).text(&full).render_fallback();
    assert!(text.contains("26.1013.40934"), "{text}");
    assert!(text.contains("0.130.0"), "{text}");
    let missing = dir.path().join("nowhere");
    let text = check_location(&missing).text(&missing);
    assert_eq!(text.key, "app_location.missing");
    let empty = dir.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    assert_eq!(
        check_location(&empty).text(&empty).key,
        "app_location.not_app"
    );
    let bare = windows_app(&dir.path().join("bare"), false);
    assert_eq!(
        check_location(&bare).text(&bare).key,
        "app_location.no_codex"
    );
}

/// The detection order: `SUPERBA_APP_PATH`, the setting, the default
/// locations, then discovered installs; a discovered install already found
/// earlier is listed once, at its earlier place. Off Windows only: on Windows
/// detection also reads the registered MSIX packages from the real registry.
#[cfg(not(windows))]
#[tokio::test]
async fn env_comes_before_the_setting_before_defaults_before_discovered() {
    use superba_appctl::install::detect_installs_from;
    use superba_core::codex::ENV_SUPERBA_APP_PATH;

    let dir = tempdir().unwrap();
    let home = dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let env_app = windows_app(&dir.path().join("env"), true);
    let setting_app = windows_app(&dir.path().join("setting"), true);
    let found_app = windows_app(&dir.path().join("found"), true);
    let env = MapEnv::new()
        .with(ENV_SUPERBA_APP_PATH, env_app.to_str().unwrap())
        .with("XDG_DATA_HOME", dir.path().join("xdg").to_str().unwrap());
    let ours = |installs: Vec<superba_core::AppInstall>| -> Vec<PathBuf> {
        installs
            .into_iter()
            .map(|i| i.path)
            .filter(|p| p.starts_with(canonical(dir.path()).unwrap()))
            .collect()
    };

    let all = detect_installs_from(
        &env,
        &home,
        Some(&setting_app),
        &[found_app.clone(), setting_app.join("ChatGPT.exe")],
    )
    .await;
    assert_eq!(
        ours(all),
        [env_app.clone(), setting_app.clone(), found_app.clone()]
    );

    // Without the variable the setting is first.
    let no_env = MapEnv::new().with("XDG_DATA_HOME", dir.path().join("xdg").to_str().unwrap());
    let all = detect_installs_from(
        &no_env,
        &home,
        Some(&setting_app),
        std::slice::from_ref(&found_app),
    )
    .await;
    assert_eq!(ours(all), [setting_app.clone(), found_app.clone()]);

    // A setting that holds no app is skipped; the variable naming the same
    // install as the setting lists it once.
    let same = MapEnv::new()
        .with(ENV_SUPERBA_APP_PATH, setting_app.to_str().unwrap())
        .with("XDG_DATA_HOME", dir.path().join("xdg").to_str().unwrap());
    let all = detect_installs_from(&same, &home, Some(&dir.path().join("nowhere")), &[]).await;
    assert_eq!(ours(all), std::slice::from_ref(&setting_app));
    let all = detect_installs_from(&same, &home, Some(&setting_app), &[]).await;
    assert_eq!(ours(all), [setting_app]);
}

/// The controller asks its location source at every detection: a changed
/// setting applies to the next detection, and the stamp watches the location
/// even before an app is there. Off Windows only (see above).
#[cfg(not(windows))]
#[tokio::test]
async fn the_controller_detects_the_location_its_source_gives_now() {
    use std::sync::{Arc, Mutex};

    use superba_appctl::{AppLocationSource, DesktopAppController, FixedAppLocation};
    use superba_core::{AppController, DataDir, LocalContext};

    struct Shared(Mutex<Option<PathBuf>>);
    #[async_trait::async_trait]
    impl AppLocationSource for Shared {
        async fn app_location(&self) -> Option<PathBuf> {
            self.0.lock().unwrap().clone()
        }
    }

    let dir = tempdir().unwrap();
    let home = dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let env = MapEnv::new().with("XDG_DATA_HOME", dir.path().join("xdg").to_str().unwrap());
    let ctx = LocalContext::new(&home, DataDir::new(dir.path().join("data")), Arc::new(env));
    let target = dir.path().join("D/ChatGPT");
    let source = Arc::new(Shared(Mutex::new(Some(target.clone()))));
    let app = DesktopAppController::new(ctx.clone()).with_app_location(source.clone());
    let ours = |installs: Vec<superba_core::AppInstall>| -> Vec<PathBuf> {
        installs
            .into_iter()
            .map(|i| i.path)
            .filter(|p| p.starts_with(canonical(dir.path()).unwrap()))
            .collect()
    };

    // Set, but nothing there yet.
    assert!(ours(app.detect_installs().await.unwrap()).is_empty());
    let before = app.environment_stamp(&[], &[]);
    let root = windows_app(&target, true);
    assert_ne!(app.environment_stamp(&[], &[]), before);
    assert_eq!(
        ours(app.detect_installs().await.unwrap()),
        std::slice::from_ref(&root)
    );
    let check = app.check_app_location(&root).await.unwrap();
    assert!(matches!(check, AppLocationCheck::Install { .. }));

    // Cleared: the default locations only.
    *source.0.lock().unwrap() = None;
    assert!(ours(app.detect_installs().await.unwrap()).is_empty());

    // A fixed location and none at all.
    let fixed = DesktopAppController::new(ctx.clone())
        .with_app_location(Arc::new(FixedAppLocation(Some(root.join("ChatGPT.exe")))));
    assert_eq!(ours(fixed.detect_installs().await.unwrap()), [root]);
    let plain = DesktopAppController::new(ctx);
    assert!(ours(plain.detect_installs().await.unwrap()).is_empty());
}

// ---------------------------------------------------------------------------
// Locations inside the Microsoft Store app's package (WindowsApps)
// ---------------------------------------------------------------------------

const OLD_FULL: &str = "OpenAI.Codex_26.1103.4239.0_x64__2p2nqsd0c76g0";
const NEW_FULL: &str = "OpenAI.Codex_26.1111.2228.0_x64__2p2nqsd0c76g0";
const STORE_FAMILY: &str = "OpenAI.Codex_2p2nqsd0c76g0";

#[test]
fn a_path_in_a_store_package_folder_names_its_family() {
    use superba_appctl::install::windows_apps_package;

    // What a user set SUPERBA_APP_PATH to before the app updated.
    let given = format!("C:\\Program Files\\WindowsApps\\{OLD_FULL}\\app\\ChatGPT.exe");
    let package = windows_apps_package(Path::new(&given)).unwrap();
    assert_eq!(package.family_name, STORE_FAMILY);
    assert_eq!(package.version.as_deref(), Some("26.1103.4239.0"));
    assert_eq!(
        package.install_location,
        PathBuf::from(format!("C:\\Program Files\\WindowsApps\\{OLD_FULL}"))
    );
    // Other separators and case, the package folder itself.
    let package = windows_apps_package(Path::new(&format!("d:/windowsapps/{NEW_FULL}"))).unwrap();
    assert_eq!(package.family_name, STORE_FAMILY);
    assert_eq!(package.version.as_deref(), Some("26.1111.2228.0"));
    // Not a package of the desktop app, a resource package, no WindowsApps.
    for other in [
        "C:\\Program Files\\WindowsApps\\Microsoft.WindowsTerminal_1.21.0.0_x64__8wekyb3d8bbwe\\wt.exe",
        "C:\\Program Files\\WindowsApps\\OpenAI.Codex_26.1111.2228.0_neutral_split.scale-200_2p2nqsd0c76g0",
        "D:\\Apps\\OpenAI.Codex_26.1111.2228.0_x64__2p2nqsd0c76g0\\app\\ChatGPT.exe",
        "C:\\Program Files\\WindowsApps",
    ] {
        assert_eq!(windows_apps_package(Path::new(other)), None, "{other}");
    }
}

#[test]
fn a_store_package_folder_is_not_saved_as_the_app_location() {
    // Gone after an update, or still there: refused with the explanation.
    let dir = tempdir().unwrap();
    let stale = dir.path().join("WindowsApps").join(OLD_FULL).join("app");
    let current = registered_store_app(dir.path());
    for path in [stale, current.path.clone(), current.executable.clone()] {
        let check = check_location(&path);
        assert_eq!(
            check,
            AppLocationCheck::StorePackage {
                family: STORE_FAMILY.into()
            }
        );
        assert!(!check.usable());
        let text = check.text(&path).render_fallback();
        assert!(text.contains("no location is needed"), "{text}");
    }
}

/// The install of the registered Store package (26.1111) in a fake
/// `WindowsApps`, as registry discovery builds it.
fn registered_store_app(temp: &Path) -> superba_core::AppInstall {
    use superba_appctl::install::{AppxPackage, appx_install};

    let root = temp.join("WindowsApps").join(NEW_FULL);
    write(root.join("AppxManifest.xml"), "<Package/>");
    write(root.join("app/ChatGPT.exe"), "desktop");
    write(root.join("app/resources/app.asar"), "archive");
    write(root.join("app/resources/codex.exe"), "packaged codex");
    write(
        root.join("app/resources/codex-cli/codex-package.json"),
        r#"{"version":"0.168.0","entrypoint":"bin/codex.exe"}"#,
    );
    appx_install(&AppxPackage {
        name: "OpenAI.Codex".into(),
        family_name: STORE_FAMILY.into(),
        install_location: canonical(&root).unwrap(),
        version: Some("26.1111.2228.0".into()),
    })
    .unwrap()
}

/// `SUPERBA_APP_PATH` (or the setting) naming a version of the Store package
/// that an update replaced is the installed version of that package, listed
/// once and first; without the package installed it is ignored.
#[cfg(not(windows))]
#[tokio::test]
async fn a_stale_store_package_path_follows_the_installed_version() {
    use superba_appctl::install::detect_installs_among;
    use superba_core::codex::ENV_SUPERBA_APP_PATH;

    let dir = tempdir().unwrap();
    let home = dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let current = registered_store_app(dir.path());
    let stale = dir
        .path()
        .join("WindowsApps")
        .join(OLD_FULL)
        .join("app")
        .join("ChatGPT.exe");
    let xdg = dir.path().join("xdg");
    let env = MapEnv::new()
        .with(ENV_SUPERBA_APP_PATH, stale.to_str().unwrap())
        .with("XDG_DATA_HOME", xdg.to_str().unwrap());
    let other = windows_app(&dir.path().join("other"), true);
    let ours = |installs: Vec<superba_core::AppInstall>| -> Vec<String> {
        installs
            .into_iter()
            .filter(|i| i.path.starts_with(canonical(dir.path()).unwrap()))
            .map(|i| i.id)
            .collect()
    };
    let other_id = format!("path:{}", other.display());

    // The variable's folder is gone: the installed version comes first,
    // before an install found elsewhere, and only once.
    let all = detect_installs_among(
        &env,
        &home,
        None,
        std::slice::from_ref(&other),
        vec![current.clone()],
    )
    .await;
    assert_eq!(
        ours(all.clone()),
        [STORE_FAMILY.to_owned(), other_id.clone()]
    );
    assert_eq!(all[0].path, current.path);
    assert_eq!(all[0].app_version, current.app_version);

    // The old folder is still there (an update in progress): still the
    // installed version, never the old one.
    let old_root = dir.path().join("WindowsApps").join(OLD_FULL);
    write(old_root.join("AppxManifest.xml"), "<Package/>");
    write(old_root.join("app/ChatGPT.exe"), "old desktop");
    write(old_root.join("app/resources/app.asar"), "old archive");
    let all = detect_installs_among(&env, &home, None, &[], vec![current.clone()]).await;
    assert_eq!(ours(all.clone()), [STORE_FAMILY.to_owned()]);
    assert_eq!(all[0].path, current.path);
    fs::remove_dir_all(&old_root).unwrap();

    // The same for the app location of the settings.
    let no_env = MapEnv::new().with("XDG_DATA_HOME", xdg.to_str().unwrap());
    let all = detect_installs_among(
        &no_env,
        &home,
        Some(&stale),
        std::slice::from_ref(&other),
        vec![current.clone()],
    )
    .await;
    assert_eq!(ours(all), [STORE_FAMILY.to_owned(), other_id.clone()]);

    // The package is not installed and its folder is gone: ignored, the
    // other installs are still found.
    let all = detect_installs_among(
        &env,
        &home,
        Some(&stale),
        std::slice::from_ref(&other),
        vec![],
    )
    .await;
    assert_eq!(ours(all), [other_id]);
}
