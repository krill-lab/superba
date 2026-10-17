use std::fs;
use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use serde_json::json;
use superba_appctl::install::{
    ENV_SUPERBA_APP_DIR, OwlAppIni, appimage_install, explicit_install, inspect_install,
    parse_appx_application_id, parse_appx_packages_json, parse_codex_package_json,
    parse_desktop_exec, parse_linux_package_metadata, parse_owl_app_ini, parse_plist_string,
};
use superba_core::codex::ENV_SUPERBA_APP_PATH;
use superba_core::{AppVariant, CodexVersion, MapEnv};
use tempfile::tempdir;

fn write(path: impl AsRef<Path>, text: &str) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn linux_app(root: &Path, name: &str) -> PathBuf {
    write(root.join(name), "fake desktop executable");
    write(root.join("codex-launcher"), "fake launcher");
    write(root.join("version"), "42.3.0");
    write(root.join("resources/app.asar"), "fake archive");
    write(
        root.join("resources/codex"),
        "fake CLI; must not be executed by inspection",
    );
    write(
        root.join("resources/owl-app.ini"),
        "[Owl]\nUserDataDirectoryName=Codex\nAppVersion=26.1007.71326\n",
    );
    write(
        root.join("resources/linux-package-metadata.json"),
        r#"{"codexAppBrand":"chatgpt","version":"26.1007.70000"}"#,
    );
    fs::canonicalize(root).unwrap()
}

fn mac_app(root: &Path, id: &str, name: &str, framework: &str) -> PathBuf {
    write(
        root.join("Contents/Info.plist"),
        &format!(
            "<?xml version=\"1.0\"?><plist><dict>\n\
             <key>CFBundleIdentifier</key><string>{id}</string>\n\
             <key>CFBundleExecutable</key><string>{name}</string>\n\
             <key>CFBundleShortVersionString</key><string>26.1007.30912</string>\n\
             </dict></plist>"
        ),
    );
    write(
        root.join("Contents/MacOS").join(name),
        "fake desktop executable",
    );
    fs::create_dir_all(root.join("Contents/Frameworks").join(framework)).unwrap();
    write(root.join("Contents/Resources/app.asar"), "fake archive");
    write(
        root.join("Contents/Resources/owl-app.ini"),
        "[Owl]\nUserDataDirectoryName=Codex\nAppVersion=26.1007.71326\n",
    );
    write(
        root.join("Contents/Resources/codex-cli/bin/codex"),
        "fake CLI wrapper",
    );
    write(
        root.join("Contents/Resources/codex-cli/codex-package.json"),
        r#"{"layoutVersion":1,"version":"0.162.0-alpha.17.2","entrypoint":"bin/codex","target":"aarch64-apple-darwin"}"#,
    );
    fs::canonicalize(root).unwrap()
}

#[test]
fn owl_ini_tolerates_sections_comments_and_crlf() {
    let parsed = parse_owl_app_ini(
        "\u{feff}; header\r\n[Anything]\r\n # AppVersion=bad\r\n\
         UserDataDirectoryName = Codex Beta \r\n\
         AppVersion = 26.1007.71326 ; release\r\n\
         Unknown=unused\r\nnot an assignment\r\n",
    );
    assert_eq!(
        parsed,
        OwlAppIni {
            user_data_dir_name: Some("Codex Beta".into()),
            app_version: Some("26.1007.71326".into()),
        }
    );
    assert_eq!(
        parse_owl_app_ini("# ignored\n[Owl]\nAppVersion=\n"),
        OwlAppIni::default()
    );
    assert_eq!(
        parse_owl_app_ini("AppVersion=first\n[Another]\nAppVersion=second # final").app_version,
        Some("second".into())
    );
}

#[test]
fn package_metadata_requires_string_fields() {
    assert_eq!(
        parse_linux_package_metadata(r#"{"version":"26.1007.71326","codexAppBrand":"chatgpt"}"#),
        (Some("26.1007.71326".into()), Some("chatgpt".into()))
    );
    for text in [
        "",
        "{",
        "[]",
        "null",
        r#"{"version":42,"codexAppBrand":false}"#,
    ] {
        assert_eq!(parse_linux_package_metadata(text), (None, None));
    }
    assert_eq!(
        parse_linux_package_metadata(r#"{"version":"","codexAppBrand":"codex"}"#),
        (None, Some("codex".into()))
    );
    assert_eq!(
        parse_codex_package_json(r#"{"version":"0.162.0-alpha.17.2","entrypoint":"bin/codex"}"#),
        Some(("0.162.0-alpha.17.2".into(), "bin/codex".into()))
    );
    for text in [
        "bad",
        "{}",
        r#"{"version":"1.2.3"}"#,
        r#"{"version":1,"entrypoint":"bin/codex"}"#,
        r#"{"version":"1.2.3","entrypoint":" "}"#,
    ] {
        assert!(parse_codex_package_json(text).is_none());
    }
}

#[test]
fn plist_strings_decode_entities_once_and_keep_value_types_separate() {
    let xml = r#"<?xml version="1.0"?>
        <!DOCTYPE plist PUBLIC "identifier" "https://example.invalid/plist.dtd">
        <plist><dict>
        <!-- <key>Name</key><string>decoy</string> -->
        <key>Name</key> <!-- a comment -->
        <string>A &amp; B &lt;C&gt; &quot;D&quot; &apos;E&apos; &amp;lt; &#65; &#x42;</string>
        <key>Number</key><integer>12</integer><key>Other</key><string>other</string>
        <key>Empty</key><string/>
        <key>A&amp;B</key><string>matched key</string>
        </dict></plist>"#;
    assert_eq!(
        parse_plist_string(xml, "Name"),
        Some("A & B <C> \"D\" 'E' &lt; A B".into())
    );
    assert_eq!(parse_plist_string(xml, "A&B"), Some("matched key".into()));
    assert_eq!(parse_plist_string(xml, "Empty"), Some(String::new()));
    assert!(parse_plist_string(xml, "Number").is_none());
    assert!(parse_plist_string(xml, "Missing").is_none());
    assert!(parse_plist_string("<key>Name</key><string>unfinished", "Name").is_none());
}

#[test]
fn desktop_exec_uses_the_main_group_and_handles_two_levels_of_escaping() {
    let text = r#"Exec=outside
[Desktop Action Other]
Exec=wrong
[Desktop Entry]
# Exec=comment
Exec="/tmp/Program Files/ChatGPT" "--title=Say \"hi\"" "C:\\\\tools" "\\$HOME" "\\`uname\\`" --url=%U %% --literal=%%U "" %f %F %u %d %D %n %N %i %c %k %v %m
[Desktop Action Later]
Exec=also-wrong
"#;
    assert_eq!(
        parse_desktop_exec(text),
        Some(vec![
            "/tmp/Program Files/ChatGPT".into(),
            "--title=Say \"hi\"".into(),
            "C:\\tools".into(),
            "$HOME".into(),
            "`uname`".into(),
            "--url=".into(),
            "%".into(),
            "--literal=%U".into(),
            String::new(),
        ])
    );
    assert_eq!(
        parse_desktop_exec("[Desktop Entry]\r\n Exec = /tmp/Codex %U --flag\r\n"),
        Some(vec!["/tmp/Codex".into(), "--flag".into()])
    );
    assert_eq!(
        parse_desktop_exec("[Desktop Entry]\nExec=ChatGPT \"line\\nnext\""),
        Some(vec!["ChatGPT".into(), "line\nnext".into()])
    );
    for text in [
        "[Desktop Action Other]\nExec=wrong",
        "[Desktop Entry]\nName=Codex",
        "[Desktop Entry]\nExec=\"unclosed",
        "[Desktop Entry]\nExec=ChatGPT %Z",
        "[Desktop Entry]\nExec=ChatGPT \\",
    ] {
        assert!(parse_desktop_exec(text).is_none(), "{text}");
    }
}

#[test]
fn appx_manifest_reads_the_first_application_not_comments_or_other_attributes() {
    let xml = r#"<Package><Applications>
        <!-- <Application Id="Decoy"/> -->
        <Application Executable="a>b.exe" OtherId="wrong" Id = 'Desktop&amp;App'/>
        <Application Id="Second"/>
        </Applications></Package>"#;
    assert_eq!(parse_appx_application_id(xml), Some("Desktop&App".into()));
    assert_eq!(
        parse_appx_application_id("<x:Application Id=\"Codex\"/>"),
        Some("Codex".into())
    );
    assert!(parse_appx_application_id("<Applications/>").is_none());
    assert!(parse_appx_application_id("<Application Executable=\"ChatGPT.exe\"/>").is_none());
}

#[test]
fn appx_json_accepts_object_or_array_and_filters_exact_family_names() {
    let package = json!({
        "Name": "OpenAI.Codex", "PackageFamilyName": "OpenAI.Codex_2p2nqsd0c76g0",
        "InstallLocation": "C:\\Packages\\Codex", "Version": "26.1007.71326"
    });
    let one = parse_appx_packages_json(&package.to_string());
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].family_name, "OpenAI.Codex_2p2nqsd0c76g0");
    assert_eq!(
        one[0].install_location,
        PathBuf::from("C:\\Packages\\Codex")
    );
    assert_eq!(one[0].version.as_deref(), Some("26.1007.71326"));
    let array = json!([
        package,
        {"Name":"beta", "PackageFamilyName":"OpenAI.CodexBeta_publisher", "InstallLocation":"C:\\Beta"},
        {"Name":"chat", "PackageFamilyName":"OpenAI.ChatGPT-Desktop_publisher", "InstallLocation":"C:\\Chat", "Version":null},
        {"Name":"cli", "PackageFamilyName":"OpenAI.CodexCLI_publisher", "InstallLocation":"C:\\CLI"},
        {"Name":"lookalike", "PackageFamilyName":"OpenAI.CodexBetaExtra_publisher", "InstallLocation":"C:\\Extra"},
        {"Name":"missing", "PackageFamilyName":"OpenAI.Codex_publisher"},
        {"Name":"wrong type", "PackageFamilyName":"OpenAI.Codex_publisher", "InstallLocation":23},
        null
    ]);
    let parsed = parse_appx_packages_json(&array.to_string());
    assert_eq!(parsed.len(), 3);
    assert_eq!(parsed[0], one[0]);
    assert!(parsed[1].version.is_none());
    assert!(parsed[2].version.is_none());
    for text in ["", " \r\n", "null", "[]", "{}", "malformed"] {
        assert!(parse_appx_packages_json(text).is_empty());
    }
}

#[test]
fn inspect_linux_directory_main_and_launcher_uses_app_metadata() {
    let temp = tempdir().unwrap();
    let root = linux_app(&temp.path().join("ChatGPT app"), "ChatGPT");
    let install = inspect_install(&root).unwrap();
    assert_eq!(install.id, format!("path:{}", root.display()));
    assert_eq!(install.path, root);
    assert_eq!(install.executable, root.join("ChatGPT"));
    assert_eq!(install.variant, AppVariant::ChatGpt);
    assert_eq!(install.app_version.as_deref(), Some("26.1007.71326"));
    assert_eq!(install.bundled_codex, Some(root.join("resources/codex")));
    assert!(install.bundled_codex_version.is_none());
    assert_eq!(
        inspect_install(&root.join("ChatGPT")),
        Some(install.clone())
    );
    assert_eq!(inspect_install(&root.join("codex-launcher")), Some(install));
    assert!(inspect_install(&root.join("resources/codex")).is_none());
    assert!(inspect_install(&root.join("version")).is_none());

    fs::remove_file(root.join("resources/owl-app.ini")).unwrap();
    assert_eq!(
        inspect_install(&root).unwrap().app_version.as_deref(),
        Some("26.1007.70000")
    );
    fs::remove_file(root.join("resources/linux-package-metadata.json")).unwrap();
    assert!(inspect_install(&root).unwrap().app_version.is_none());
    fs::remove_file(root.join("resources/app.asar")).unwrap();
    assert!(inspect_install(&root).is_none());
}

#[test]
fn inspect_linux_codex_uses_brand_when_present() {
    let temp = tempdir().unwrap();
    let root = linux_app(&temp.path().join("codex.beta-dir"), "Codex");
    assert_eq!(inspect_install(&root).unwrap().variant, AppVariant::ChatGpt);
    write(
        root.join("resources/linux-package-metadata.json"),
        r#"{"codexAppBrand":"codex"}"#,
    );
    assert_eq!(inspect_install(&root).unwrap().variant, AppVariant::Codex);
}

#[test]
fn inspect_mac_bundle_accepts_owl_electron_and_other_framework_names() {
    let temp = tempdir().unwrap();
    for (index, framework) in [
        "Codex Framework.framework",
        "Electron Framework.framework",
        "OwlFramework.framework",
    ]
    .into_iter()
    .enumerate()
    {
        let root = mac_app(
            &temp.path().join(format!("ChatGPT {index}.app")),
            "com.openai.codex",
            "ChatGPT",
            framework,
        );
        let install = inspect_install(&root).unwrap();
        assert_eq!(install.id, "com.openai.codex");
        assert_eq!(install.variant, AppVariant::ChatGpt);
        assert_eq!(install.path, root);
        assert_eq!(install.executable, root.join("Contents/MacOS/ChatGPT"));
        assert_eq!(install.app_version.as_deref(), Some("26.1007.30912"));
        assert_eq!(
            install.bundled_codex,
            Some(root.join("Contents/Resources/codex-cli/bin/codex"))
        );
        assert_eq!(
            install.bundled_codex_version,
            Some(CodexVersion::parse("0.162.0-alpha.17.2").unwrap())
        );
        assert_eq!(inspect_install(&install.executable), Some(install.clone()));
        assert_eq!(
            inspect_install(&root.join("Contents/Resources/app.asar")),
            Some(install)
        );
    }
}

#[test]
fn inspect_mac_variants_follow_executable_and_bundle_id() {
    let temp = tempdir().unwrap();
    for (index, id, exe, expected) in [
        (0, "com.openai.codex", "Codex", AppVariant::Codex),
        (1, "com.openai.codex.beta", "ChatGPT", AppVariant::CodexBeta),
        (2, "com.openai.chat", "ChatGPT", AppVariant::ChatGpt),
        (3, "com.openai.chatgpt", "ChatGPT", AppVariant::ChatGpt),
        (
            4,
            "example.desktop",
            "Other.Shell",
            AppVariant::Other("Other.Shell".into()),
        ),
    ] {
        let root = mac_app(
            &temp.path().join(format!("App{index}.app")),
            id,
            exe,
            "Codex Framework.framework",
        );
        assert_eq!(inspect_install(&root).unwrap().variant, expected);
    }
}

#[test]
fn inspect_excludes_classic_cli_bundles_and_apps_without_frameworks() {
    let temp = tempdir().unwrap();
    let classic = mac_app(
        &temp.path().join("ChatGPT Classic.app"),
        "com.openai.chat",
        "ChatGPT",
        "Codex Framework.framework",
    );
    fs::remove_file(classic.join("Contents/Resources/app.asar")).unwrap();
    assert!(inspect_install(&classic).is_none());
    let parent = mac_app(
        &temp.path().join("ChatGPT.app"),
        "com.openai.codex",
        "ChatGPT",
        "Codex Framework.framework",
    );
    let cli = mac_app(
        &parent.join("Contents/Resources/codex-cli/CodexCLI.app"),
        "com.openai.codex.cli",
        "codex",
        "Codex Framework.framework",
    );
    assert!(inspect_install(&cli).is_none());
    assert!(inspect_install(&cli.join("Contents/MacOS/codex")).is_none());
    let renamed_cli = mac_app(
        &temp.path().join("Renamed.app"),
        "com.openai.codex.cli",
        "codex",
        "Codex Framework.framework",
    );
    assert!(inspect_install(&renamed_cli).is_none());
    let no_framework = mac_app(
        &temp.path().join("NoFramework.app"),
        "com.openai.codex",
        "Codex",
        "Unrelated.bundle",
    );
    assert!(inspect_install(&no_framework).is_none());
}

#[test]
fn bundled_metadata_selects_entrypoint_without_escaping_the_cli_directory() {
    let temp = tempdir().unwrap();
    let root = mac_app(
        &temp.path().join("ChatGPT.app"),
        "com.openai.codex",
        "ChatGPT",
        "Codex Framework.framework",
    );
    let cli = root.join("Contents/Resources/codex-cli");
    fs::remove_file(cli.join("bin/codex")).unwrap();
    write(cli.join("bin/custom"), "fake custom CLI");
    write(
        cli.join("codex-package.json"),
        r#"{"version":"0.162.0-alpha.17.2","entrypoint":"bin/custom"}"#,
    );
    assert_eq!(
        inspect_install(&root).unwrap().bundled_codex,
        Some(cli.join("bin/custom"))
    );
    write(
        root.join("Contents/Resources/outside"),
        "not a CLI entrypoint",
    );
    write(
        cli.join("codex-package.json"),
        r#"{"version":"0.162.0","entrypoint":"../outside"}"#,
    );
    let install = inspect_install(&root).unwrap();
    assert!(install.bundled_codex.is_none());
    assert!(install.bundled_codex_version.is_none());
}

#[test]
fn inspect_windows_layout_finds_nested_desktop_and_skips_resources_subtrees() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("Package");
    write(
        root.join("AppxManifest.xml"),
        "<Package><Applications><Application Id=\"Desktop\"/></Applications></Package>",
    );
    write(root.join("app/bin/ChatGPT.exe"), "fake desktop");
    write(root.join("resources/ChatGPT.exe"), "decoy");
    write(root.join("resources/codex.exe"), "fake CLI");
    write(root.join("resources/app.asar"), "fake archive");
    let root = root.canonicalize().unwrap();
    let install = inspect_install(&root).unwrap();
    assert_eq!(install.executable, root.join("app/bin/ChatGPT.exe"));
    assert_eq!(
        install.bundled_codex,
        Some(root.join("resources/codex.exe"))
    );
    assert_eq!(install.variant, AppVariant::ChatGpt);
    assert_eq!(inspect_install(&install.executable), Some(install));
    fs::remove_file(root.join("app/bin/ChatGPT.exe")).unwrap();
    write(root.join("too/deep/for/Codex.exe"), "too deep");
    assert!(inspect_install(&root).is_none());
}

#[test]
fn bundled_codex_is_one_identity_with_its_launcher_target_and_wsl_twin() {
    use superba_appctl::codexid::{CodexIdentity, same_codex};

    let temp = tempdir().unwrap();
    // macOS: the package entrypoint is the `bin/codex` launcher of CodexCLI.app.
    let root = mac_app(
        &temp.path().join("ChatGPT.app"),
        "com.openai.codex",
        "ChatGPT",
        "Codex Framework.framework",
    );
    let cli = root.join("Contents/Resources/codex-cli");
    write(
        cli.join("bin/codex"),
        "#!/bin/sh\nexec \"$(dirname \"$0\")/../CodexCLI.app/Contents/MacOS/codex\" \"$@\"\n",
    );
    write(
        cli.join("CodexCLI.app/Contents/MacOS/codex"),
        "fake native CLI",
    );
    let install = inspect_install(&root).unwrap();
    let launcher = install.bundled_codex.clone().unwrap();
    assert_eq!(launcher, cli.join("bin/codex"));
    let process_image = cli.join("CodexCLI.app/Contents/MacOS/codex");
    assert!(CodexIdentity::of(&launcher).contains(&process_image));
    assert!(same_codex(&launcher, &process_image));

    // Windows: codex.exe, not the Linux build for WSL beside it.
    let package = temp.path().join("Package");
    write(package.join("app/ChatGPT.exe"), "fake desktop");
    write(package.join("app/resources/app.asar"), "fake archive");
    write(
        package.join("app/resources/codex"),
        "fake Linux CLI for WSL",
    );
    write(package.join("app/resources/codex.exe"), "fake CLI");
    let package = package.canonicalize().unwrap();
    let install = inspect_install(&package).unwrap();
    let exe = package.join("app/resources/codex.exe");
    assert_eq!(install.bundled_codex, Some(exe.clone()));
    assert!(same_codex(&exe, &package.join("app/resources/codex")));
}

#[test]
fn appimages_are_standalone_files_with_unknown_versions() {
    let temp = tempdir().unwrap();
    for (name, variant) in [
        ("cHaTgPt-26.AppImage", AppVariant::ChatGpt),
        ("Codex-x86_64.appimage", AppVariant::Codex),
    ] {
        let path = temp.path().join(name);
        write(&path, "fake AppImage");
        let install = appimage_install(&path).unwrap();
        assert_eq!(install.variant, variant);
        assert_eq!(install.executable, path.canonicalize().unwrap());
        assert_eq!(install.executable, install.path);
        assert!(install.app_version.is_none());
        assert!(install.bundled_codex.is_none());
        assert!(install.bundled_codex_version.is_none());
        assert_eq!(inspect_install(&path), Some(install));
    }
    let unrelated = temp.path().join("Other.AppImage");
    write(&unrelated, "unrelated");
    assert!(appimage_install(&unrelated).is_none());
    fs::create_dir(temp.path().join("ChatGPT-dir.AppImage")).unwrap();
    assert!(appimage_install(&temp.path().join("ChatGPT-dir.AppImage")).is_none());
}

// Desktop entries are a freedesktop (Linux) format: their Exec quoting treats a
// backslash as an escape, so Windows paths cannot be written into one, and
// Superba reads them only on Linux.
#[cfg(unix)]
#[test]
fn desktop_targets_filter_programs_and_superba_owned_entries() {
    let temp = tempdir().unwrap();
    let first = temp.path().join("entries-one");
    let second = temp.path().join("entries-two");
    let app = linux_app(&temp.path().join("ChatGPT with spaces"), "ChatGPT");
    let codex = linux_app(&temp.path().join("Codex"), "Codex");
    write(
        first.join("a.desktop"),
        &format!(
            "[Desktop Entry]\nExec=\"{}\" %U",
            app.join("ChatGPT").display()
        ),
    );
    write(
        first.join("b.desktop"),
        &format!(
            "[Desktop Entry]\nExec=\"{}\" --superba-owned",
            codex.join("Codex").display()
        ),
    );
    write(
        first.join("c.desktop"),
        &format!(
            "[Desktop Entry]\nExec=\"{}\" --owner=SuPeRbA",
            codex.join("Codex").display()
        ),
    );
    write(
        first.join("d.desktop"),
        "[Desktop Entry]\nExec=/irrelevant/bin/codex %U",
    );
    write(
        first.join("e.desktop"),
        "[Desktop Entry]\nExec=env /irrelevant/ChatGPT",
    );
    write(
        first.join("f.desktop"),
        "[Desktop Action Extra]\nExec=/irrelevant/ChatGPT",
    );
    write(
        first.join("ignored.txt"),
        "[Desktop Entry]\nExec=/irrelevant/ChatGPT",
    );
    write(
        second.join("a.desktop"),
        &format!(
            "[Desktop Entry]\nExec=\"{}\" %U",
            app.join("codex-launcher").display()
        ),
    );
    write(
        second.join("b.desktop"),
        &format!(
            "[Desktop Entry]\nExec=\"{}\"",
            codex.join("Codex").display()
        ),
    );
    assert_eq!(
        superba_appctl::install::desktop_entry_targets(&[
            first,
            second,
            temp.path().join("missing")
        ]),
        vec![app, codex]
    );
}

#[test]
fn explicit_install_prefers_path_and_falls_back_to_the_directory_alias() {
    let temp = tempdir().unwrap();
    let first = linux_app(&temp.path().join("one"), "ChatGPT");
    let second = linux_app(&temp.path().join("two"), "Codex");
    let env = MapEnv::new()
        .with(ENV_SUPERBA_APP_PATH, first.join("codex-launcher"))
        .with(ENV_SUPERBA_APP_DIR, &second);
    assert_eq!(explicit_install(&env).unwrap().path, first);
    let fallback = env.with(ENV_SUPERBA_APP_PATH, temp.path().join("missing"));
    assert_eq!(explicit_install(&fallback).unwrap().path, second);
    assert!(explicit_install(&MapEnv::new()).is_none());
    assert!(explicit_install(&MapEnv::new().with(ENV_SUPERBA_APP_PATH, "")).is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn user_data_directory_uses_the_injected_home_and_xdg_config_root() {
    use superba_appctl::install::user_data_dir;

    let temp = tempdir().unwrap();
    let root = linux_app(&temp.path().join("app"), "ChatGPT");
    let install = inspect_install(&root).unwrap();
    let home = temp.path().join("app-home");
    let config = temp.path().join("xdg-config");
    let env = MapEnv::new()
        .with("XDG_CONFIG_HOME", &config)
        .with("HOME", temp.path().join("ignored-home"));
    assert_eq!(
        user_data_dir(&install, &env, &home),
        Some(config.join("Codex"))
    );
    write(
        root.join("resources/owl-app.ini"),
        "[Owl]\nUserDataDirectoryName=Codex Beta\n",
    );
    assert_eq!(
        user_data_dir(&install, &env, &home),
        Some(config.join("Codex Beta"))
    );
    assert_eq!(
        user_data_dir(&install, &MapEnv::new(), &home),
        Some(home.join(".config/Codex Beta"))
    );
    fs::remove_file(root.join("resources/owl-app.ini")).unwrap();
    assert_eq!(
        user_data_dir(&install, &MapEnv::new().with("XDG_CONFIG_HOME", ""), &home),
        Some(home.join(".config/Codex"))
    );
}

#[cfg(unix)]
fn script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    write(path, &format!("#!/bin/sh\n{body}\n"));
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn codex_version_runs_only_the_supplied_script_with_version_and_null_stdin() {
    use superba_appctl::install::codex_version;

    let temp = tempdir().unwrap();
    let binary = temp.path().join("bin with spaces/codex");
    script(
        &binary,
        "[ \"$#\" -eq 1 ] && [ \"$1\" = --version ] || exit 9\nif read -r line; then exit 8; fi\necho 'codex-cli 0.162.0-alpha.17.2'",
    );
    assert_eq!(
        codex_version(&binary).await,
        Some(CodexVersion::parse("0.162.0-alpha.17.2").unwrap())
    );
    script(&binary, "echo 'codex-cli 0.162.0-alpha.17.2' >&2");
    assert_eq!(
        codex_version(&binary).await,
        Some(CodexVersion::parse("0.162.0-alpha.17.2").unwrap())
    );
    script(&binary, "echo 'codex-cli 0.162.0-alpha.17.2'; exit 2");
    assert!(codex_version(&binary).await.is_none());
    script(&binary, "echo 'invalid version'");
    assert!(codex_version(&binary).await.is_none());
    assert!(
        codex_version(&temp.path().join("missing/codex"))
            .await
            .is_none()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn codex_version_times_out_a_script_that_does_not_exit() {
    use std::time::{Duration, Instant};
    use superba_appctl::install::codex_version;

    let temp = tempdir().unwrap();
    let binary = temp.path().join("codex");
    // exec replaces the shell, so timeout cleanup owns the only child process.
    script(&binary, "exec /bin/sleep 30");
    let start = Instant::now();
    assert!(codex_version(&binary).await.is_none());
    assert!(start.elapsed() >= Duration::from_secs(5));
    assert!(start.elapsed() < Duration::from_secs(15));
}
