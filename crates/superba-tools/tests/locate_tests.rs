//! Finding tools off the process PATH (user bugs 109, 110) and Homebrew as
//! the macOS install method, against fake filesystems, PATHs and a fake
//! `brew`.

#![cfg(unix)]

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::json;
use superba_core::{
    MapEnv, NetworkSettings, NoToolProgress, ToolId, ToolInstallKind, ToolOp, ToolRequest,
    ToolSettingsChange,
};
use superba_tools::locate::{self, Env, Overrides, Source};
use superba_tools::platform::{Arch, Os, Platform};
use superba_tools::{NoHost, ToolManager};
use support::{Mock, Route, script, sources};

fn env(root: &Path, path: &[&Path], extra: &[(&str, String)]) -> MapEnv {
    let mut m = MapEnv::new()
        .with(
            "PATH",
            std::env::join_paths(path)
                .unwrap()
                .to_string_lossy()
                .as_ref(),
        )
        .with(
            "SUPERBA_SEARCH_ROOT",
            root.join("sysroot").to_string_lossy().as_ref(),
        );
    for (k, v) in extra {
        m = m.with(k, v.as_str());
    }
    m
}

fn find(root: &Path, os: Os, e: &MapEnv, name: &str) -> Option<locate::Located> {
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    locate::locate(
        &Env {
            env: e,
            home: &home,
            os,
        },
        Some(&superba_core::DataDir::new(root.join("data"))),
        name,
    )
    .0
}

fn version_script(p: &Path, v: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    script(p, &format!("echo {v}"));
}

#[test]
fn a_custom_folder_on_path_is_found() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("D/tools/dsh-here");
    version_script(&dir.join("dsh"), "0.2.1");
    let e = env(t.path(), &[&dir], &[]);
    let l = find(t.path(), Os::Linux, &e, "dsh").unwrap();
    assert_eq!(l.path, dir.join("dsh"));
    assert_eq!(l.source, Source::Path);
}

#[test]
fn a_moved_npm_prefix_is_found_from_npm_itself_and_from_npmrc() {
    let t = tempfile::tempdir().unwrap();
    let npm_dir = t.path().join("node/bin");
    let prefix = t.path().join("my-npm-global");
    std::fs::create_dir_all(&npm_dir).unwrap();
    script(
        &npm_dir.join("npm"),
        &format!(
            "[ \"$1 $2\" = \"prefix -g\" ] && echo {} && exit 0; exit 1",
            prefix.display()
        ),
    );
    version_script(&prefix.join("bin/dsh"), "0.2.1");
    let e = env(t.path(), &[&npm_dir], &[]);
    let l = find(t.path(), Os::Linux, &e, "dsh").unwrap();
    assert_eq!(l.path, prefix.join("bin/dsh"));
    assert_eq!(l.source, Source::PackageManager);

    // Without npm on PATH: `prefix=` in ~/.npmrc.
    let t = tempfile::tempdir().unwrap();
    let prefix = t.path().join("elsewhere");
    version_script(&prefix.join("bin/dsh"), "0.2.1");
    std::fs::create_dir_all(t.path().join("home")).unwrap();
    std::fs::write(
        t.path().join("home/.npmrc"),
        format!("prefix={}\n", prefix.display()),
    )
    .unwrap();
    let e = env(t.path(), &[], &[]);
    assert_eq!(
        find(t.path(), Os::Linux, &e, "dsh").unwrap().path,
        prefix.join("bin/dsh")
    );
}

#[test]
fn pnpm_and_bun_homes_are_searched() {
    let t = tempfile::tempdir().unwrap();
    let pnpm = t.path().join("pnpm-home");
    version_script(&pnpm.join("pi"), "1.3.0");
    let bun = t.path().join("bun");
    version_script(&bun.join("bin/opencode"), "2.1.0");
    let e = env(
        t.path(),
        &[],
        &[
            ("PNPM_HOME", pnpm.display().to_string()),
            ("BUN_INSTALL", bun.display().to_string()),
        ],
    );
    assert_eq!(
        find(t.path(), Os::Linux, &e, "pi").unwrap().path,
        pnpm.join("pi")
    );
    assert_eq!(
        find(t.path(), Os::Linux, &e, "opencode").unwrap().path,
        bun.join("bin/opencode")
    );
}

#[test]
fn macos_homebrew_is_searched_without_it_on_path() {
    let t = tempfile::tempdir().unwrap();
    let cellar = t
        .path()
        .join("sysroot/opt/homebrew/Caskroom/claude-code/2.1.304/claude");
    version_script(&cellar, "2.1.304 (Claude Code)");
    let bin = t.path().join("sysroot/opt/homebrew/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(&cellar, bin.join("claude")).unwrap();
    // launchd's PATH of a Finder-started app.
    let e = env(t.path(), &[Path::new("/nonexistent/usr/bin")], &[]);
    let l = find(t.path(), Os::Macos, &e, "claude").unwrap();
    assert_eq!(l.path, bin.join("claude"));
    assert_eq!(l.source, Source::Homebrew);
    let (cask, is_cask, v) =
        superba_tools::tools::common::homebrew_of(&l.path).expect("Caskroom target");
    assert_eq!(
        (cask.as_str(), is_cask, v.as_str()),
        ("claude-code", true, "2.1.304")
    );
}

#[test]
fn windows_cmd_shims_are_found_with_pathext() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("AppData/Roaming/npm");
    version_script(&dir.join("dsh.cmd"), "0.2.1");
    let e = env(
        t.path(),
        &[],
        &[
            (
                "APPDATA",
                t.path().join("AppData/Roaming").display().to_string(),
            ),
            ("PATHEXT", ".COM;.EXE;.BAT;.CMD".into()),
        ],
    );
    let l = find(t.path(), Os::Windows, &e, "dsh").unwrap();
    assert_eq!(l.path, dir.join("dsh.cmd"));
    assert_eq!(l.source, Source::Default);
}

#[test]
fn the_chosen_path_wins_and_is_checked_with_version() {
    let t = tempfile::tempdir().unwrap();
    let data = superba_core::DataDir::new(t.path().join("data"));
    let on_path = t.path().join("bin");
    version_script(&on_path.join("dsh"), "0.1.0");
    let chosen = t.path().join("D/custom/dsh");
    version_script(&chosen, "0.2.1");
    let e = env(t.path(), &[&on_path], &[]);
    assert!(locate::validate(&chosen).is_ok());
    let broken = t.path().join("broken");
    script(&broken, "exit 3");
    assert!(locate::validate(&broken).is_err());
    assert!(locate::validate(&t.path().join("missing")).is_err());
    Overrides::set(&data, "dsh", Some(chosen.clone())).unwrap();
    let l = find(t.path(), Os::Linux, &e, "dsh").unwrap();
    assert_eq!((l.path, l.source), (chosen, Source::Override));
    Overrides::set(&data, "dsh", None).unwrap();
    assert_eq!(
        find(t.path(), Os::Linux, &e, "dsh").unwrap().source,
        Source::Path
    );
}

#[test]
fn the_search_summary_names_the_places() {
    let t = tempfile::tempdir().unwrap();
    let e = env(t.path(), &[Path::new("/a"), Path::new("/b")], &[]);
    let home = t.path().join("home");
    let dirs = Env {
        env: &e,
        home: &home,
        os: Os::Macos,
    }
    .dirs();
    let s = dirs.summary().join("\n");
    assert!(s.contains("PATH: /a, /b"), "{s}");
    assert!(s.contains("Homebrew:"), "{s}");
    assert!(s.contains(".local/bin"), "{s}");
}

// --- the engine -------------------------------------------------------------

fn manager(root: &Path, mock: &Mock, os: Os, path: &[&Path]) -> ToolManager {
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let ctx = superba_core::LocalContext::new(
        &home,
        superba_core::DataDir::new(root.join("data")),
        Arc::new(env(
            root,
            path,
            &[("SUPERBA_BREW_LOCK_WAIT_MS", "10".into())],
        )),
    );
    ToolManager::with(
        &ctx,
        NetworkSettings::default(),
        Arc::new(NoHost),
        Platform::new(os, Arch::Arm64),
        sources(&mock.base),
    )
}

#[tokio::test]
async fn dsh_in_a_moved_npm_prefix_is_detected_and_a_chosen_path_is_used() {
    let mock = Mock::start().await;
    mock.route(
        "/npm/-/package/@deepseek-ai/dsh/dist-tags",
        Route::ok(json!({"latest": "0.2.1"}).to_string()),
    );
    let t = tempfile::tempdir().unwrap();
    let prefix = t.path().join("D/npm-global");
    let pkg = prefix.join("lib/node_modules/@deepseek-ai/dsh");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(pkg.join("package.json"), r#"{"version":"0.2.1"}"#).unwrap();
    version_script(&pkg.join("lib/bin.js"), "0.2.1");
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    std::os::unix::fs::symlink(pkg.join("lib/bin.js"), prefix.join("bin/dsh")).unwrap();
    std::fs::create_dir_all(t.path().join("home")).unwrap();
    std::fs::write(
        t.path().join("home/.npmrc"),
        format!("prefix={}\n", prefix.display()),
    )
    .unwrap();
    let m = manager(t.path(), &mock, Os::Linux, &[]);
    let row = m.row(ToolId::Dsh, false).await.unwrap();
    let i = row.primary().expect("dsh found");
    assert_eq!(i.kind, ToolInstallKind::NpmGlobal);
    assert_eq!(i.version.as_deref(), Some("0.2.1"));
    assert!(row.searched.is_empty());

    // A program somewhere else, chosen by the user.
    let chosen = t.path().join("E/dsh/dsh");
    version_script(&chosen, "0.3.0");
    let row = m
        .settings(
            ToolId::Dsh,
            ToolSettingsChange {
                command_path: Some(chosen.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(row.command_path.as_deref(), Some(chosen.as_path()));
    assert_eq!(row.primary().unwrap().path, chosen);
    assert_eq!(row.primary().unwrap().version.as_deref(), Some("0.3.0"));
    // Something that is not a program is refused.
    let bad = t.path().join("E/readme.txt");
    std::fs::write(&bad, "hi").unwrap();
    assert!(
        m.settings(
            ToolId::Dsh,
            ToolSettingsChange {
                command_path: Some(bad),
                ..Default::default()
            },
        )
        .await
        .is_err()
    );
    // Cleared: back to the search.
    let row = m
        .settings(
            ToolId::Dsh,
            ToolSettingsChange {
                command_path: Some(PathBuf::new()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(row.command_path, None);
    assert_eq!(row.primary().unwrap().kind, ToolInstallKind::NpmGlobal);
}

#[tokio::test]
async fn nothing_found_says_where_superba_looked() {
    let mock = Mock::start().await;
    mock.route(
        "/npm/-/package/@deepseek-ai/dsh/dist-tags",
        Route::ok(json!({"latest": "0.2.1"}).to_string()),
    );
    let t = tempfile::tempdir().unwrap();
    let m = manager(t.path(), &mock, Os::Linux, &[Path::new("/x/bin")]);
    let row = m.row(ToolId::Dsh, false).await.unwrap();
    assert!(row.installs.is_empty());
    assert!(
        row.searched.iter().any(|s| s.starts_with("PATH: /x/bin")),
        "{:?}",
        row.searched
    );
}

#[tokio::test]
async fn homebrews_dsh_formula_is_not_deepseeks_dsh() {
    let mock = Mock::start().await;
    mock.route(
        "/npm/-/package/@deepseek-ai/dsh/dist-tags",
        Route::ok(json!({"latest": "0.2.1"}).to_string()),
    );
    let t = tempfile::tempdir().unwrap();
    let cellar = t
        .path()
        .join("sysroot/opt/homebrew/Cellar/dsh/0.25.10/bin/dsh");
    version_script(&cellar, "dsh 0.25.10");
    let bin = t.path().join("sysroot/opt/homebrew/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(&cellar, bin.join("dsh")).unwrap();
    let m = manager(t.path(), &mock, Os::Macos, &[]);
    let row = m.row(ToolId::Dsh, false).await.unwrap();
    assert!(row.installs.is_empty(), "{:?}", row.installs);
    assert_eq!(
        row.install_method.as_ref().map(|t| t.key.as_str()),
        Some("tools.brew.none_dsh")
    );
}

/// A fake Homebrew under the sysroot: `install` / `upgrade` / `uninstall` of
/// formula `opencode` (args logged; a lock and an unknown formula once each
/// when asked).
fn fake_brew(root: &Path, quirks: &str) -> PathBuf {
    let prefix = root.join("sysroot/opt/homebrew");
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    let log = root.join("brew.log");
    let body = format!(
        r#"PATH=/bin:/usr/bin:$PATH
P={prefix}
[ "$1" = --prefix ] || echo "$* NO_AUTO_UPDATE=$HOMEBREW_NO_AUTO_UPDATE" >> {log}
[ "$1" = --prefix ] && exit 0
case "{quirks}" in *lock*) if [ "$1" = install ] && [ ! -f {root}/locked ]; then touch {root}/locked; echo "Error: Another active Homebrew process is already in progress." >&2; exit 1; fi;; esac
case "{quirks}" in *unknown*) if [ "$1" = install ] && [ ! -f {root}/updated ]; then echo "Error: No available formula with the name \"opencode\"." >&2; exit 1; fi;; esac
case "$1" in
  update) touch {root}/updated; echo "Already up-to-date."; exit 0;;
  install|upgrade)
    v=2.1.0; [ "$1" = upgrade ] && v=2.2.0
    echo "==> Downloading opencode $v"; echo "==> Pouring opencode--$v"
    rm -rf "$P/Cellar/opencode"; mkdir -p "$P/Cellar/opencode/$v/bin"
    printf '#!/bin/sh\necho %s\n' "$v" > "$P/Cellar/opencode/$v/bin/opencode"
    chmod 755 "$P/Cellar/opencode/$v/bin/opencode"
    ln -sf "$P/Cellar/opencode/$v/bin/opencode" "$P/bin/opencode"; exit 0;;
  uninstall) rm -rf "$P/Cellar/opencode" "$P/bin/opencode"; exit 0;;
esac
exit 1"#,
        prefix = prefix.display(),
        log = log.display(),
        root = root.display(),
    );
    script(&prefix.join("bin/brew"), &body);
    log
}

fn opencode_latest(mock: &Mock, v: &str) {
    for ch in ["latest", "beta"] {
        mock.route(
            &format!("/opencode/update/api/{ch}/cli/npm"),
            Route::ok(json!({"version": v}).to_string()),
        );
    }
}

#[tokio::test]
async fn macos_installs_updates_and_removes_with_homebrew() {
    let mock = Mock::start().await;
    opencode_latest(&mock, "2.1.0");
    let t = tempfile::tempdir().unwrap();
    let log = fake_brew(t.path(), "lock unknown");
    let m = manager(t.path(), &mock, Os::Macos, &[]);
    let row = m.row(ToolId::Opencode, false).await.unwrap();
    assert!(row.actions.install);
    assert_eq!(
        row.install_method.as_ref().map(|t| t.key.as_str()),
        Some("tools.brew.default")
    );
    let report = m
        .run(
            ToolRequest {
                confirmed: true,
                ..ToolRequest::new(ToolId::Opencode, ToolOp::Install)
            },
            Arc::new(NoToolProgress),
        )
        .await
        .unwrap();
    assert_eq!(report.version.as_deref(), Some("2.1.0"));
    let row = m.row(ToolId::Opencode, false).await.unwrap();
    assert_eq!(row.primary().unwrap().kind, ToolInstallKind::Homebrew);

    opencode_latest(&mock, "2.2.0");
    let row = m.row(ToolId::Opencode, true).await.unwrap();
    assert!(row.actions.update, "{row:?}");
    m.run(
        ToolRequest {
            confirmed: true,
            ..ToolRequest::new(ToolId::Opencode, ToolOp::Update)
        },
        Arc::new(NoToolProgress),
    )
    .await
    .unwrap();
    assert_eq!(
        m.row(ToolId::Opencode, false)
            .await
            .unwrap()
            .installed_version(),
        Some("2.2.0")
    );
    m.run(
        ToolRequest {
            confirmed: true,
            ..ToolRequest::new(ToolId::Opencode, ToolOp::Uninstall)
        },
        Arc::new(NoToolProgress),
    )
    .await
    .unwrap();
    let calls = std::fs::read_to_string(log).unwrap();
    let lines: Vec<&str> = calls.lines().collect();
    assert_eq!(lines[0], "install opencode NO_AUTO_UPDATE=1", "{calls}"); // locked
    assert_eq!(lines[1], "install opencode NO_AUTO_UPDATE=1", "{calls}"); // unknown
    assert!(lines[2].starts_with("update"), "{calls}");
    assert_eq!(lines[3], "install opencode NO_AUTO_UPDATE=1", "{calls}");
    assert!(
        lines.iter().any(|l| l.starts_with("upgrade opencode")),
        "{calls}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("uninstall opencode")),
        "{calls}"
    );
}

#[tokio::test]
async fn without_brew_macos_says_why_it_uses_the_tools_installer() {
    let mock = Mock::start().await;
    opencode_latest(&mock, "2.1.0");
    let t = tempfile::tempdir().unwrap();
    let m = manager(t.path(), &mock, Os::Macos, &[]);
    let row = m.row(ToolId::Opencode, false).await.unwrap();
    assert_eq!(
        row.install_method.as_ref().map(|t| t.key.as_str()),
        Some("tools.brew.missing")
    );
}
