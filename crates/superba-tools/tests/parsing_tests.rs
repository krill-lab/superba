//! Parsers of the vendors' answers and of OS data: Claude Code's manifest,
//! the ZCode release YAML, WorkBuddy's update JSON, opencode's update API,
//! pi's SHA256SUMS, PyPI's wheel list, Windows command lines and PATH lists,
//! property lists.

use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use serde_json::json;
use superba_core::{MapEnv, ShellKind};
use superba_tools::pathenv::{list_with, list_without, unix_hint};
use superba_tools::tools::{apps, claude, dsh, opencode, pi, workbuddy, zcode};
use superba_tools::winapps::{UninstallEntry, icon_path, split_command_line};

#[test]
fn claude_manifest_entry() {
    let m = json!({
        "version": "2.1.292",
        "platforms": {
            "linux-x64": {"binary": "claude", "checksum": "a967e7b1d8b4e47ee421d5433027880347952b0c0857abf880e2c942a4ec93b3", "size": 251456696},
            "win32-x64": {"binary": "claude.exe", "checksum": "EB95BB65955F8B1702E800815F9A2C0388A5DE0F196C354C7EE1DD5CB9A2BA23", "size": 254858400},
            "linux-arm64": {"binary": "claude", "checksum": "short", "size": 1}
        }
    });
    let e = claude::manifest_entry(&m, "2.1.292", "linux-x64").unwrap();
    assert_eq!(e.binary, "claude");
    assert_eq!(e.size, 251_456_696);
    let w = claude::manifest_entry(&m, "2.1.292", "win32-x64").unwrap();
    assert_eq!(w.binary, "claude.exe");
    assert!(
        w.checksum.chars().all(|c| !c.is_ascii_uppercase()),
        "lower-cased"
    );
    assert!(
        claude::manifest_entry(&m, "2.1.300", "linux-x64").is_err(),
        "wrong version"
    );
    assert!(
        claude::manifest_entry(&m, "2.1.292", "darwin-arm64").is_err(),
        "no platform"
    );
    assert!(
        claude::manifest_entry(&m, "2.1.292", "linux-arm64").is_err(),
        "no sha256"
    );
}

const ZCODE: &str = r#"version: 3.15.3
files:
    - url: https://cdn-zcode.z.ai/zcode/electron/releases/3.15.3/windows-x64/ZCode-3.15.3-win-x64.exe
      sha512: iZa7fC6e22v31afkO7L4pykVa5aJUfF0QpRV8h5ZhDZbNCbOTa6935b2dn/eN0iQxCvTW5z5/pjWDmELxbvlwg==
      size: 181367144
    - url: https://cdn-zcode.z.ai/x/ZCode-3.15.3-win-x64.zip
      sha512: abc==
      size: 5
path: https://cdn-zcode.z.ai/zcode/electron/releases/3.15.3/windows-x64/ZCode-3.15.3-win-x64.exe
sha512: iZa7fC6e22v31afkO7L4pykVa5aJUfF0QpRV8h5ZhDZbNCbOTa6935b2dn/eN0iQxCvTW5z5/pjWDmELxbvlwg==
releaseName: Release v3.15.3
releaseNotes: "notes: with colons"
releaseNotesByLocale:
    en-US:
        title: ""
releaseDate: "2026-10-13T08:54:16.134Z"
"#;

#[test]
fn zcode_manifest() {
    let r = zcode::parse_manifest(ZCODE).unwrap();
    assert_eq!(r.version, "3.15.3");
    assert_eq!(r.files.len(), 2);
    assert!(r.files[0].url.ends_with("ZCode-3.15.3-win-x64.exe"));
    assert!(r.files[0].url.starts_with("https://"));
    assert_eq!(r.files[0].size, 181_367_144);
    assert!(r.files[0].sha512.ends_with("=="));
    assert!(zcode::parse_manifest("garbage").is_none());
}

#[test]
fn workbuddy_update_answers() {
    let cn = json!({"version":"5.8.3.41334564","url":"https://download.codebuddy.cn/x/WorkBuddy-win32-x64-user-5.8.3.41334564-db0e9b51.exe","sha256hash":"824DE10F3196C501004F540EB84EE881292FEF2F5DF9FB3FC48AF9EA5366C03C"});
    let u = workbuddy::parse_update(&cn).unwrap();
    assert_eq!(u.version, "5.8.3");
    assert_eq!(u.full, "5.8.3.41334564");
    assert_eq!(u.sha256.as_deref().map(str::len), Some(64));
    let intl = json!({"version":"5.8.4.41366630","url":"https://download.codebuddy.ai/x.exe","sha256hash":""});
    let u = workbuddy::parse_update(&intl).unwrap();
    assert_eq!(
        u.sha256, None,
        "no published hash: the signature is the check"
    );
    assert!(workbuddy::parse_update(&json!({"code": 10001})).is_none());
    assert_eq!(
        workbuddy::build(superba_core::ToolId::WorkbuddyAi).exe,
        "WorkBuddyAI.exe"
    );
    assert_eq!(
        workbuddy::build(superba_core::ToolId::Workbuddy).data,
        ".workbuddy"
    );
}

#[test]
fn opencode_update_api_files() {
    let doc = json!({"artifacts": [
        {"distribution": "npm", "version": "2.0.32"},
        {"distribution": "opencode", "version": "2.0.32", "metadata": {"files": {
            "opencode-linux-x64.tar.gz": {"url": "https://opencode.ai/files/bin/2.0.32/opencode-linux-x64.tar.gz", "sha256": "aa", "size": 98601332}
        }}}
    ]});
    let (v, url, sha, size) = opencode::api_file(&doc, "opencode-linux-x64.tar.gz").unwrap();
    assert_eq!(
        (v.as_str(), sha.as_str(), size),
        ("2.0.32", "aa", 98_601_332)
    );
    assert!(url.ends_with("/2.0.32/opencode-linux-x64.tar.gz"));
    assert!(opencode::api_file(&doc, "opencode-windows-x64.zip").is_none());
    assert!(opencode::service_running(true, "http://127.0.0.1:49374\n"));
    assert!(!opencode::service_running(true, "stopped\n"));
    assert!(!opencode::service_running(false, "http://127.0.0.1:49374"));
    assert_eq!(opencode::major("1.19.3"), Some(1));
    assert_eq!(opencode::major("v2.0.32"), Some(2));
}

#[test]
fn pi_sha256sums() {
    let sums = "24f5be328b66d4da36876296989501974f43ab8434fbb57c35c523a3ea505ded  pi-linux-x64.tar.gz\n\
                daf0ef45fd08f13dbb64c62d559e332c2d3d3087f8ae022ec5a1b9b655b59894 *pi-windows-x64.zip\n\
                bad  pi-darwin-arm64.tar.gz\n";
    assert_eq!(
        pi::sha_for(sums, "pi-linux-x64.tar.gz").as_deref(),
        Some("24f5be328b66d4da36876296989501974f43ab8434fbb57c35c523a3ea505ded")
    );
    assert!(pi::sha_for(sums, "pi-windows-x64.zip").is_some());
    assert_eq!(
        pi::sha_for(sums, "pi-darwin-arm64.tar.gz"),
        None,
        "not a sha256"
    );
    assert_eq!(pi::sha_for(sums, "pi-linux-arm64.tar.gz"), None);
}

#[test]
fn pypi_wheel_for_the_platform() {
    let doc = json!({"urls": [
        {"filename": "deepseek_harness_runtime_bin-0.2.1rc2-py3-none-manylinux_2_28_x86_64.whl", "url": "https://files/x.whl", "digests": {"sha256": "0a0a"}, "size": 94615763},
        {"filename": "deepseek_harness_runtime_bin-0.2.1rc2-py3-none-win_amd64.whl", "url": "https://files/w.whl", "digests": {"sha256": "d38c"}, "size": 1}
    ]});
    let (file, url, sha, size) = dsh::pick_wheel(&doc, "win_amd64").unwrap();
    assert!(file.ends_with("win_amd64.whl"));
    assert_eq!(
        (url.as_str(), sha.as_str(), size),
        ("https://files/w.whl", "d38c", 1)
    );
    assert!(dsh::pick_wheel(&doc, "macosx_14_0_arm64").is_none());
}

#[test]
fn windows_command_lines_and_icons() {
    assert_eq!(
        split_command_line(
            r#""C:\Users\a b\AppData\Local\Programs\WorkBuddy\Uninstall WorkBuddy.exe" /currentuser /S"#
        ),
        vec![
            r"C:\Users\a b\AppData\Local\Programs\WorkBuddy\Uninstall WorkBuddy.exe".to_owned(),
            "/currentuser".into(),
            "/S".into()
        ]
    );
    assert_eq!(split_command_line("x.exe  /S"), vec!["x.exe", "/S"]);
    assert_eq!(
        icon_path(r#""C:\p\ZCode.exe",0"#),
        PathBuf::from(r"C:\p\ZCode.exe")
    );
    assert_eq!(
        icon_path(r"C:\p\ZCode.exe"),
        PathBuf::from(r"C:\p\ZCode.exe")
    );
}

#[test]
fn quiet_uninstall_commands() {
    let quiet = UninstallEntry {
        per_user: true,
        quiet_uninstall: Some(r#""C:\a\Uninstall WorkBuddy.exe" /currentuser /S"#.into()),
        uninstall: Some(r#""C:\a\Uninstall WorkBuddy.exe" /currentuser"#.into()),
        ..Default::default()
    };
    assert_eq!(
        apps::quiet_uninstall(&quiet).unwrap(),
        vec![r"C:\a\Uninstall WorkBuddy.exe", "/currentuser", "/S"]
    );
    let plain = UninstallEntry {
        per_user: true,
        uninstall: Some(r#""C:\a\Uninstall ZCode.exe""#.into()),
        ..Default::default()
    };
    assert_eq!(
        apps::quiet_uninstall(&plain).unwrap(),
        vec![r"C:\a\Uninstall ZCode.exe", "/currentuser", "/S"]
    );
    assert!(apps::quiet_uninstall(&UninstallEntry::default()).is_none());
}

#[test]
fn plist_versions() {
    let xml = "<dict>\n\t<key>CFBundleName</key>\n\t<string>ZCode</string>\n\t<key>CFBundleShortVersionString</key>\n\t<string>3.15.3</string>\n</dict>";
    assert_eq!(
        apps::plist_string(xml, "CFBundleShortVersionString").as_deref(),
        Some("3.15.3")
    );
    assert_eq!(apps::plist_string(xml, "Missing"), None);
}

#[test]
fn windows_path_lists() {
    let expand = |s: &str| s.replace("%USERPROFILE%", r"C:\Users\me");
    let list = r"%USERPROFILE%\AppData\Local\Microsoft\WindowsApps;C:\Tools";
    assert_eq!(
        list_with(list, r"C:\Users\me\.local\bin", &expand).as_deref(),
        Some(r"%USERPROFILE%\AppData\Local\Microsoft\WindowsApps;C:\Tools;C:\Users\me\.local\bin")
    );
    assert_eq!(
        list_with(list, r"c:\tools\", &expand),
        None,
        "already there (case, slash)"
    );
    assert_eq!(
        list_with(
            list,
            r"C:\Users\me\AppData\Local\Microsoft\WindowsApps",
            &expand
        ),
        None,
        "matches the %USERPROFILE% entry"
    );
    assert_eq!(list_with("", r"C:\x", &expand).as_deref(), Some(r"C:\x"));
    assert_eq!(
        list_without(list, r"C:\TOOLS", &expand).as_deref(),
        Some(r"%USERPROFILE%\AppData\Local\Microsoft\WindowsApps")
    );
    assert_eq!(list_without(list, r"C:\nope", &expand), None);
}

#[test]
fn unix_path_hints_per_shell() {
    let home = Path::new("/home/u");
    let dir = home.join(".local/bin");
    // PATH is parsed by the host OS even when the hint is for a Unix shell.
    let path = std::env::join_paths(["/usr/bin", "/bin"]).unwrap();
    for (shell, file, line) in [
        (
            "/bin/zsh",
            "~/.zshrc",
            "export PATH=\"$HOME/.local/bin:$PATH\"",
        ),
        (
            "/bin/bash",
            "~/.bashrc",
            "export PATH=\"$HOME/.local/bin:$PATH\"",
        ),
        (
            "/usr/bin/fish",
            "~/.config/fish/config.fish",
            "fish_add_path \"$HOME/.local/bin\"",
        ),
        (
            "/bin/dash",
            "~/.profile",
            "export PATH=\"$HOME/.local/bin:$PATH\"",
        ),
    ] {
        let env = MapEnv::new()
            .with("PATH", path.clone())
            .with("SHELL", shell);
        let h = unix_hint(&env, home, &dir, false).unwrap();
        assert_eq!((h.file.as_str(), h.line.as_str()), (file, line), "{shell}");
    }
    let env = MapEnv::new()
        .with(
            "PATH",
            std::env::join_paths([dir.as_path(), Path::new("/usr/bin")]).unwrap(),
        )
        .with("SHELL", "/bin/zsh");
    assert!(
        unix_hint(&env, home, &dir, false).is_none(),
        "already on PATH"
    );
    let env = MapEnv::new().with("SHELL", "/bin/bash");
    assert_eq!(
        unix_hint(&env, home, &dir, true).unwrap().file,
        "~/.bash_profile"
    );
    let _ = ShellKind::Zsh;
}
