use std::path::Path;
use std::sync::Arc;
use superba_config::autostart::{self, EntryStore, FileEntry, LoginStartup, Manager, Platform};
use superba_core::{LocalContext, MapEnv};

#[test]
fn enable_refresh_move_disable_and_missing_binary() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = LocalContext::for_tests(tmp.path());
    let path = autostart::entry_path(&ctx, Platform::Linux);
    let store = Arc::new(FileEntry(path.clone()));
    let first = tmp.path().join("old app");
    std::fs::write(&first, "binary").unwrap();
    let manager = Manager::with_store(Platform::Linux, store.clone(), Ok(first.clone()));
    assert!(!manager.status().unwrap().enabled);
    manager.refresh().unwrap();
    assert!(!path.exists(), "refresh never opts in");
    assert!(manager.set_enabled(true).unwrap().enabled);
    let original = std::fs::read_to_string(&path).unwrap();
    assert!(original.contains(" --autostart\n"));
    let moved = tmp.path().join("new app");
    std::fs::rename(&first, &moved).unwrap();
    let updated = Manager::with_store(Platform::Linux, store, Ok(moved.clone()));
    updated.refresh().unwrap();
    let refreshed = std::fs::read_to_string(&path).unwrap();
    assert!(!refreshed.contains("old app"));
    assert!(refreshed.contains("new app"));
    std::fs::remove_file(&moved).unwrap();
    assert!(
        !updated.set_enabled(false).unwrap().enabled,
        "off needs no binary"
    );
    updated.set_enabled(false).unwrap();
    updated.refresh().unwrap();
    assert!(!path.exists());
    assert!(updated.set_enabled(true).is_err());
}

#[test]
fn externally_hidden_desktop_entry_is_not_reenabled() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(FileEntry(tmp.path().join("superba.desktop")));
    store.write(Some("[Desktop Entry]\nHidden=true\n")).unwrap();
    let manager = Manager::with_store(
        Platform::Linux,
        store.clone(),
        Ok(tmp.path().join("missing")),
    );
    assert!(!manager.status().unwrap().enabled);
    manager.refresh().unwrap();
    assert!(store.read().unwrap().unwrap().contains("Hidden=true"));
    manager.set_enabled(false).unwrap();
    assert!(store.read().unwrap().is_none());
}

#[test]
fn xdg_paths_respect_absolute_config_home_only() {
    let tmp = tempfile::tempdir().unwrap();
    let mut ctx = LocalContext::for_tests(tmp.path());
    ctx.env = Arc::new(MapEnv::new().with("XDG_CONFIG_HOME", "relative"));
    assert_eq!(
        autostart::entry_path(&ctx, Platform::Linux),
        ctx.home_dir.join(".config/autostart/superba.desktop")
    );
    let config = tmp.path().join("config space");
    ctx.env = Arc::new(MapEnv::new().with("XDG_CONFIG_HOME", config.as_os_str()));
    assert_eq!(
        autostart::entry_path(&ctx, Platform::Linux),
        config.join("autostart/superba.desktop")
    );
    assert_eq!(
        autostart::entry_path(&ctx, Platform::Mac),
        ctx.home_dir
            .join("Library/LaunchAgents/com.krill-code.superba.autostart.plist")
    );
}

#[test]
fn desktop_exec_escapes_field_codes_and_shell_characters() {
    let entry =
        autostart::render(Platform::Linux, Path::new("/opt/my app/% f`$\"\\superba")).unwrap();
    assert_eq!(
        entry.lines().find(|l| l.starts_with("Exec=")).unwrap(),
        r#"Exec="/opt/my app/%% f\\`\\$\\"\\\\superba" --autostart"#
    );
    assert!(!entry.contains("sh -c"));
    for path in ["/opt/bad\nExec=x", "/opt/a=b", "/opt/bad\rpath"] {
        assert!(autostart::render(Platform::Linux, Path::new(path)).is_err());
    }
}

#[test]
fn windows_custom_install_directory_is_quoted_and_bounded() {
    assert_eq!(
        autostart::render(
            Platform::Windows,
            Path::new(r"\\?\D:\User apps\Superba\superba.exe")
        )
        .unwrap(),
        r#""D:\User apps\Superba\superba.exe" --autostart"#
    );
    assert!(
        autostart::render(
            Platform::Windows,
            Path::new(&format!("C:\\{}\\superba.exe", "x".repeat(260)))
        )
        .is_err()
    );
    assert!(autostart::render(Platform::Windows, Path::new("C:\\quote\"\\superba.exe")).is_err());
    assert_ne!(autostart::RUN_VALUE, "SuperbaRouter");
}

#[test]
fn mac_bundle_move_keeps_launchservices_identity() {
    let a = autostart::render(
        Platform::Mac,
        Path::new("/Applications/Superba.app/Contents/MacOS/superba"),
    )
    .unwrap();
    let b = autostart::render(
        Platform::Mac,
        Path::new("/Users/test/Moved/Superba.app/Contents/MacOS/superba"),
    )
    .unwrap();
    assert_eq!(a, b);
    assert!(a.contains("<string>/usr/bin/open</string><string>-g</string><string>-b</string>"));
    assert!(a.contains("<string>com.krill-code.superba</string>"));
    assert!(a.contains("<string>--autostart</string>"));
    assert!(!a.contains("KeepAlive"));
    let raw = autostart::render(Platform::Mac, Path::new("/opt/a & <b>/superba")).unwrap();
    assert!(raw.contains("/opt/a &amp; &lt;b&gt;/superba"));
}

#[test]
fn removing_gui_startup_leaves_router_entry_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let exe = tmp.path().join("superba");
    std::fs::write(&exe, "binary").unwrap();
    let router = tmp.path().join("superba-router.desktop");
    std::fs::write(&router, "router").unwrap();
    let manager = Manager::with_store(
        Platform::Linux,
        Arc::new(FileEntry(tmp.path().join("superba.desktop"))),
        Ok(exe),
    );
    manager.set_enabled(true).unwrap();
    manager.set_enabled(false).unwrap();
    assert_eq!(std::fs::read_to_string(router).unwrap(), "router");
}
