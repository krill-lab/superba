//! The Codex the desktop app runs, compared as one program however it is named
//! (user bug 18): on macOS the install names the `codex-cli/bin/codex` launcher
//! while the app-server's process image is `CodexCLI.app/Contents/MacOS/codex`;
//! pins, links and the Windows WSL twin likewise. Layouts are simulated in
//! temporary directories, so every platform runs these.

#![allow(missing_docs)]

mod fakes;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fakes::*;
use pretty_assertions::assert_eq;
use superba_core::{
    AppCodexPin, AppInstall, CodexVersion, ExecuteOptions, Locale, LocalizedText, NoEvents,
    OutcomeStatus, SafeDefaults, SwitchIntent, SwitchRequest, SwitchService, VersionsView,
};
use superba_switch::{Switcher, messages};

const BUNDLED: &str = "0.162.0-alpha.17.2";

fn write(path: &Path, text: &str) -> PathBuf {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    path.to_owned()
}

/// `<root>/ChatGPT.app` with the launcher and its CodexCLI.app binary:
/// `(app, launcher, binary)`.
fn mac_app(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let app = root.join("ChatGPT.app");
    let cli = app.join("Contents/Resources/codex-cli");
    let launcher = write(
        &cli.join("bin/codex"),
        "#!/bin/sh\nset -eu\nSCRIPT_DIR=\"$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd -P)\"\n\
         exec \"$SCRIPT_DIR/../CodexCLI.app/Contents/MacOS/codex\" \"$@\"\n",
    );
    let binary = write(
        &cli.join("CodexCLI.app/Contents/MacOS/codex"),
        "native codex (fake)",
    );
    (app, launcher, binary)
}

/// Point the fake install at `app` with `bundled` as its Codex.
fn install_at(w: &World, app: &Path, bundled: &Path) {
    let mut installs = w.app.installs.lock().unwrap();
    installs[0] = AppInstall {
        path: app.to_owned(),
        executable: app.join("Contents/MacOS/ChatGPT"),
        bundled_codex: Some(bundled.to_owned()),
        bundled_codex_version: Some(CodexVersion::parse(BUNDLED).unwrap()),
        ..installs[0].clone()
    };
}

fn runs(w: &World, exe: &Path) {
    *w.app.app_server_exe.lock().unwrap() = Some(exe.to_owned());
}

/// Whether Superba opened the running app (and passed `CODEX_CLI_PATH` itself).
fn opened_by_superba(w: &World, yes: bool) {
    if let Some(s) = w.app.session.lock().unwrap().as_mut() {
        s.launched_by_us = yes;
    }
}

fn root(w: &World) -> PathBuf {
    superba_core::paths::canonical(w.tmp.path()).unwrap()
}

async fn view(w: &World) -> VersionsView {
    w.switcher().versions(false).await.unwrap()
}

fn en(t: &LocalizedText) -> String {
    messages::catalog(Locale::En).text(t)
}

fn zh(t: &LocalizedText) -> String {
    messages::catalog(Locale::ZhCn).text(t)
}

fn cause(t: &LocalizedText) -> &str {
    &t.nested.iter().find(|(n, _)| n == "cause").unwrap().1.key
}

#[tokio::test]
async fn the_macos_launcher_and_its_codexcli_binary_are_the_bundled_codex() {
    let w = World::new();
    let (app, launcher, binary) = mac_app(&root(&w).join("Applications"));
    install_at(&w, &app, &launcher);
    runs(&w, &binary);
    let v = view(&w).await;
    assert_eq!(v.app_codex.mismatch, None);
    assert_eq!(
        v.app_codex.effective_version,
        Some(CodexVersion::parse(BUNDLED).unwrap())
    );
    // The process image may also be reported through the launcher's own name.
    runs(&w, &launcher);
    assert_eq!(view(&w).await.app_codex.mismatch, None);
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlinked_applications_folder_is_the_same_codex() {
    let w = World::new();
    let (_, _, binary) = mac_app(&root(&w).join("Volumes/Apps"));
    std::os::unix::fs::symlink(root(&w).join("Volumes/Apps"), root(&w).join("Applications"))
        .unwrap();
    let linked = root(&w).join("Applications/ChatGPT.app");
    install_at(
        &w,
        &linked,
        &linked.join("Contents/Resources/codex-cli/bin/codex"),
    );
    runs(&w, &binary);
    let v = view(&w).await;
    assert_eq!(v.app_codex.mismatch, None);
    assert_eq!(
        v.app_codex.effective_version,
        Some(CodexVersion::parse(BUNDLED).unwrap())
    );
}

#[tokio::test]
async fn windows_and_linux_layouts() {
    let w = World::new();
    // Windows: codex.exe; the Linux codex beside it (for WSL) is the same build.
    let resources = root(&w).join("Package/app/resources");
    write(&resources.join("app.asar"), "archive");
    let exe = write(&resources.join("codex.exe"), "MZ codex");
    let wsl = write(&resources.join("codex"), "ELF codex for WSL");
    install_at(&w, resources.parent().unwrap(), &exe);
    runs(&w, &wsl);
    let v = view(&w).await;
    assert_eq!(v.app_codex.mismatch, None);
    assert_eq!(
        v.app_codex.effective_version,
        Some(CodexVersion::parse(BUNDLED).unwrap())
    );
    // Linux: resources/codex; another codex is another program.
    let w = World::new();
    let lib = root(&w).join("usr/lib/chatgpt");
    write(&lib.join("resources/app.asar"), "archive");
    let bundled = write(&lib.join("resources/codex"), "ELF bundled");
    install_at(&w, &lib, &bundled);
    runs(&w, &bundled);
    assert_eq!(view(&w).await.app_codex.mismatch, None);
    let other = write(&root(&w).join("home/.local/bin/codex"), "ELF other");
    runs(&w, &other);
    let v = view(&w).await;
    if superba_switch::versions::PIN_SUPPORTED {
        let m = v
            .app_codex
            .mismatch
            .expect("a different binary is reported");
        assert_eq!(cause(&m), "switch.pin.cause_override");
        assert_eq!(
            en(&m),
            "The app runs the Codex at ~/.local/bin/codex instead of its bundled Codex \
             0.162.0-alpha.17.2. A CODEX_CLI_PATH set outside Superba (a shell profile, \
             launchctl setenv or a launcher entry) takes precedence."
        );
    }
}

#[tokio::test]
async fn a_pinned_codex_that_runs_matches_and_a_different_binary_is_named_precisely() {
    if !superba_switch::versions::PIN_SUPPORTED {
        return;
    }
    let w = World::new();
    let (app, launcher, binary) = mac_app(&root(&w).join("Applications"));
    install_at(&w, &app, &launcher);
    let v = CodexVersion::new(0, 163, 0);
    let pinned = w.v3.versions.add_stored(v.clone());
    w.profiles.set.lock().unwrap().settings.app_codex.pin =
        AppCodexPin::Version { version: v.clone() };
    runs(&w, &pinned);
    let view_ = view(&w).await;
    assert_eq!(view_.app_codex.mismatch, None);
    assert_eq!(view_.app_codex.effective_version, Some(v.clone()));
    assert!(view_.app_codex.stored.iter().any(|s| s.in_use && s.pinned));

    // The bundled Codex runs instead, in an app opened elsewhere without the pin.
    runs(&w, &binary);
    opened_by_superba(&w, false);
    let m = view(&w).await.app_codex.mismatch.unwrap();
    assert_eq!(cause(&m), "switch.pin.cause_outside");
    let text = en(&m);
    assert!(
        text.starts_with(
            "The app runs its bundled Codex 0.162.0-alpha.17.2 instead of Codex 0.163.0 \
             from Superba's store."
        ),
        "{text}"
    );
    // No full path in the sentence (the GUI shows them on hover).
    assert!(!text.contains(&root(&w).display().to_string()), "{text}");
    assert!(
        zh(&m).starts_with(
            "app 运行的是 app 自带的 Codex 0.162.0-alpha.17.2，而不是 Superba 下载的 Codex 0.163.0。"
        ),
        "{}",
        zh(&m)
    );
    // Opened by Superba (or with the pin exported): the pinned file failed.
    opened_by_superba(&w, true);
    let m = view(&w).await.app_codex.mismatch.unwrap();
    assert_eq!(cause(&m), "switch.pin.cause_fallback");
}

fn pin_request(pin: AppCodexPin) -> SwitchRequest {
    let mut r = SwitchRequest::profile(superba_core::ProfileId::new("work"));
    r.intent = SwitchIntent::AppCodexPin { pin };
    r
}

async fn execute(sw: &Switcher, plan: superba_core::SwitchPlan) -> superba_core::SwitchOutcome {
    sw.execute(
        plan,
        ExecuteOptions {
            allow_full_restart: true,
            turn_quiet_ms: 0,
            ..ExecuteOptions::default()
        },
        Arc::new(SafeDefaults),
        Arc::new(NoEvents),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn a_path_pin_on_a_launcher_is_verified_through_its_target() {
    if !superba_switch::versions::PIN_SUPPORTED {
        return;
    }
    let w = World::new();
    // Pinning another app's `codex-cli/bin/codex` launcher: the relaunched
    // app-server's image is the CodexCLI.app binary behind it.
    let (_, launcher, binary) = mac_app(&root(&w).join("Other"));
    runs(&w, &binary);
    let sw = w.switcher();
    let plan = sw
        .plan(pin_request(AppCodexPin::Path {
            path: launcher.clone(),
        }))
        .await
        .unwrap();
    let outcome = execute(&sw, plan).await;
    assert_eq!(
        outcome.status,
        OutcomeStatus::Switched,
        "{:?}",
        outcome.notices
    );
    assert!(
        !outcome
            .notices
            .iter()
            .any(|n| n.text.key == "switch.pin.mismatch"),
        "{:?}",
        outcome.notices
    );
    assert_eq!(
        w.profiles.set.lock().unwrap().settings.app_codex.pin,
        AppCodexPin::Path { path: launcher }
    );

    // A genuinely different program: reported, the pin not stored.
    let w = World::new();
    let (_, launcher, _) = mac_app(&root(&w).join("Other"));
    let elsewhere = write(&root(&w).join("home/bin/codex"), "ELF other");
    runs(&w, &elsewhere);
    let sw = w.switcher();
    let plan = sw
        .plan(pin_request(AppCodexPin::Path {
            path: launcher.clone(),
        }))
        .await
        .unwrap();
    let outcome = execute(&sw, plan).await;
    let m = outcome
        .notices
        .iter()
        .find(|n| n.text.key == "switch.pin.mismatch")
        .expect("mismatch reported");
    assert_eq!(cause(&m.text), "switch.pin.cause_override");
    let text = en(&m.text);
    assert!(
        text.starts_with("The app runs the Codex at ~/bin/codex instead of the Codex at /"),
        "{text}"
    );
    assert!(text.contains("/\u{2026}/codex-cli/bin/codex. "), "{text}");
    assert_eq!(
        w.profiles.set.lock().unwrap().settings.app_codex.pin,
        AppCodexPin::Bundled
    );
}
