use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use superba_appctl::clibin::classify_source;
use superba_core::{AppInstall, AppVariant, InstallSource};

fn app_install(path: PathBuf) -> AppInstall {
    AppInstall {
        id: format!("path:{}", path.display()),
        variant: AppVariant::ChatGpt,
        executable: path.join("ChatGPT"),
        path,
        app_version: None,
        bundled_codex: None,
        bundled_codex_version: None,
    }
}

#[test]
fn sources_recognize_each_package_manager_and_both_path_separators() {
    for (path, source) in [
        (
            "/usr/local/lib/node_modules/@openai/codex/bin/codex.js",
            InstallSource::Npm,
        ),
        ("/test/home/.npm-global/bin/codex", InstallSource::Npm),
        (
            "/test/home/.nvm/versions/node/v22/bin/codex",
            InstallSource::Npm,
        ),
        ("/test/npm/bin/codex", InstallSource::Npm),
        (
            r"C:\Users\tester\AppData\Roaming\npm\codex.cmd",
            InstallSource::Npm,
        ),
        (
            r"C:\tools\node_modules\@openai\codex\codex.exe",
            InstallSource::Npm,
        ),
        (
            "/usr/local/Cellar/codex/0.162.0/bin/codex",
            InstallSource::Brew,
        ),
        ("/opt/homebrew/bin/codex", InstallSource::Brew),
        ("/home/linuxbrew/.linuxbrew/bin/codex", InstallSource::Brew),
        ("/test/home/.cargo/bin/codex", InstallSource::Cargo),
        (
            r"C:\Users\tester\.cargo\bin\codex.exe",
            InstallSource::Cargo,
        ),
        (
            r"C:\Users\tester\AppData\Local\OpenAI\Codex\bin\hash\codex.exe",
            InstallSource::AppBundled,
        ),
        ("/test/home/bin/codex", InstallSource::Unknown),
    ] {
        assert_eq!(
            classify_source(Path::new(path), Path::new(path), &[]),
            source,
            "{path}"
        );
    }
}

#[test]
fn source_classification_respects_component_boundaries_and_canonical_paths() {
    let installs = [
        app_install(PathBuf::from("/test/apps/ChatGPT")),
        app_install(PathBuf::from(r"C:\Apps\ChatGPT")),
    ];
    for path in [
        "/test/apps/ChatGPT/resources/codex",
        r"c:\apps\chatgpt\resources\codex.exe",
    ] {
        assert_eq!(
            classify_source(Path::new(path), Path::new(path), &installs),
            InstallSource::AppBundled
        );
    }
    for path in [
        "/test/apps/ChatGPT-other/resources/codex",
        "/test/apps/ChatGPT/../other/codex",
        "/test/node_modules/@openai/codex-other/bin/codex",
        "/test/notnpm/bin/codex",
        "/test/.npm-global-other/bin/codex",
        "/test/.cargo/bin-other/codex",
        "/opt/homebrew-other/bin/codex",
        "/home/linuxbrew-other/bin/codex",
        r"C:\Apps\ChatGPT-other\resources\codex.exe",
        r"C:\OpenAI\Codex\bin-other\codex.exe",
    ] {
        assert_eq!(
            classify_source(Path::new(path), Path::new(path), &installs),
            InstallSource::Unknown,
            "{path}"
        );
    }
    assert_eq!(
        classify_source(
            Path::new("/test/.npm-global/bin/codex"),
            Path::new("/usr/local/Cellar/codex/bin/codex"),
            &[]
        ),
        InstallSource::Brew
    );
    assert_eq!(
        classify_source(
            Path::new("/test/.npm-global/bin/codex"),
            Path::new("/test/elsewhere/codex"),
            &[]
        ),
        InstallSource::Npm
    );
}

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::{InstallSource, Path, app_install};
    use pretty_assertions::assert_eq;
    use superba_appctl::clibin::find_codex_binaries;
    use superba_core::{CodexVersion, MapEnv};
    use tempfile::tempdir;

    fn script(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "#!/bin/sh\necho 'codex-cli 0.162.0-alpha.17.2'\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn link(target: &Path, path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        symlink(target, path).unwrap();
    }

    #[test]
    fn discovery_keeps_path_ranks_then_extras_and_deduplicates_symlinks() {
        let temp = tempdir().unwrap();
        let root = superba_core::paths::canonical(temp.path()).unwrap();
        let home = root.join("home");
        let first = root.join("bin-one");
        let non_executable = root.join("bin-not-executable");
        let second = root.join("bin-two");
        let alias = root.join("bin-alias");
        let nvm_first = home.join(".nvm/versions/node/v22/bin/codex");
        script(&nvm_first);
        link(&nvm_first, &first.join("codex"));
        script(&non_executable.join("codex"));
        fs::set_permissions(
            non_executable.join("codex"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        script(&second.join("codex"));
        link(&nvm_first, &alias.join("codex"));
        let npm = home.join(".npm-global/bin/codex");
        let cargo = home.join(".cargo/bin/codex");
        let nvm_extra = home.join(".nvm/versions/node/v20/bin/codex");
        script(&npm);
        script(&cargo);
        script(&nvm_extra);
        fs::create_dir_all(home.join(".local/bin/codex")).unwrap();
        let mut install = app_install(root.join("app"));
        let bundled = install.path.join("resources/codex");
        script(&bundled);
        install.bundled_codex = Some(bundled.clone());
        install.bundled_codex_version = Some(CodexVersion::parse("0.162.0-alpha.17.2").unwrap());
        let path_value =
            std::env::join_paths([&first, &non_executable, &second, &alias, &first]).unwrap();
        let env = MapEnv::new().with("PATH", path_value);
        // Global extra directories may exist on the test machine. Only temp fixtures are asserted,
        // and discovery does not run any binary, including those outside this directory.
        let found: Vec<_> = find_codex_binaries(&env, &home, &[install.clone()])
            .into_iter()
            .filter(|binary| binary.path.starts_with(&root))
            .collect();
        assert_eq!(
            found
                .iter()
                .map(|binary| binary.path.clone())
                .collect::<Vec<_>>(),
            vec![
                first.join("codex"),
                second.join("codex"),
                npm,
                cargo,
                nvm_extra,
                bundled
            ]
        );
        assert_eq!(
            found
                .iter()
                .map(|binary| binary.path_rank)
                .collect::<Vec<_>>(),
            vec![Some(0), Some(2), None, None, None, None]
        );
        assert_eq!(
            found.iter().map(|binary| binary.source).collect::<Vec<_>>(),
            vec![
                InstallSource::Npm,
                InstallSource::Unknown,
                InstallSource::Npm,
                InstallSource::Cargo,
                InstallSource::Npm,
                InstallSource::AppBundled
            ]
        );
        assert!(found[..5].iter().all(|binary| binary.version.is_none()));
        assert_eq!(found[5].version, install.bundled_codex_version);
    }

    #[test]
    fn a_bundled_binary_on_path_keeps_its_rank_and_inherits_metadata() {
        let temp = tempdir().unwrap();
        let root = superba_core::paths::canonical(temp.path()).unwrap();
        let home = root.join("home");
        let mut install = app_install(root.join("app"));
        let bundled = install.path.join("resources/codex");
        script(&bundled);
        install.bundled_codex = Some(bundled.clone());
        install.bundled_codex_version = Some(CodexVersion::new(0, 162, 0));
        let on_path = root.join("bin/codex");
        link(&bundled, &on_path);
        let env = MapEnv::new().with("PATH", on_path.parent().unwrap());
        let found: Vec<_> = find_codex_binaries(&env, &home, &[install])
            .into_iter()
            .filter(|binary| binary.path.starts_with(&root))
            .collect();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, on_path);
        assert_eq!(found[0].path_rank, Some(0));
        assert_eq!(found[0].source, InstallSource::AppBundled);
        assert_eq!(found[0].version, Some(CodexVersion::new(0, 162, 0)));
    }

    #[test]
    fn absent_path_does_not_fall_back_to_the_process_environment() {
        let temp = tempdir().unwrap();
        let root = superba_core::paths::canonical(temp.path()).unwrap();
        let binary = root.join(".local/bin/codex");
        script(&binary);
        let found = find_codex_binaries(&MapEnv::new(), &root, &[]);
        assert!(found.iter().all(|binary| binary.path_rank.is_none()));
        assert!(found.iter().any(|entry| entry.path == binary));
    }

    #[test]
    fn relative_and_empty_path_entries_use_the_supplied_working_directory() {
        let temp = tempdir().unwrap();
        let root = superba_core::paths::canonical(temp.path()).unwrap();
        let cwd = root.join("cwd");
        let home = root.join("home");
        script(&cwd.join("bin/codex"));
        script(&cwd.join("codex"));
        let env = MapEnv::new().with("PATH", "bin:").with("PWD", &cwd);
        let found: Vec<_> = find_codex_binaries(&env, &home, &[])
            .into_iter()
            .filter(|binary| binary.path_rank.is_some())
            .collect();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].path, cwd.join("bin/codex"));
        assert_eq!(found[0].path_rank, Some(0));
        assert_eq!(found[1].path, cwd.join("codex"));
        assert_eq!(found[1].path_rank, Some(1));
        script(&home.join("bin/codex"));
        let env = MapEnv::new().with("PATH", "bin");
        let found = find_codex_binaries(&env, &home, &[]);
        assert_eq!(found[0].path, home.join("bin/codex"));
    }
}

#[test]
fn the_apps_codex_bin_is_built_per_component() {
    // User bug 55: a literal "OpenAI/Codex/bin" showed as
    // `…\Local\OpenAI/Codex/bin\…` on Windows.
    let dir = superba_appctl::clibin::app_codex_bin(std::path::Path::new("local"));
    let parts: Vec<_> = dir
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    assert_eq!(parts, ["local", "OpenAI", "Codex", "bin"]);
}
