//! The desktop app version shown to people: the release version, with the MSIX
//! package version labeled (user bug 43: 26.1013.40934 was shown next to a
//! folder named 26.1013.4894.0 without saying which is which).

#![allow(missing_docs)]

use std::path::PathBuf;

use pretty_assertions::assert_eq;
use superba_core::{AppInstall, AppVariant, Catalog, Locale, app_version_text};

fn install(path: &str, app_version: Option<&str>) -> AppInstall {
    AppInstall {
        id: "OpenAI.Codex_2p2nqsd0c76g0".into(),
        variant: AppVariant::Codex,
        path: PathBuf::from(path),
        executable: PathBuf::from(path).join("app").join("ChatGPT.exe"),
        app_version: app_version.map(str::to_owned),
        bundled_codex: None,
        bundled_codex_version: None,
    }
}

const MSIX: &str = r"C:\Program Files\WindowsApps\OpenAI.Codex_26.1013.4894.0_x64__2p2nqsd0c76g0";

fn shown(install: &AppInstall, locale: Locale) -> Option<String> {
    install
        .version_text()
        .map(|t| Catalog::new(locale).text(&t))
}

#[test]
fn the_package_version_comes_from_the_msix_folder_name() {
    // On Linux the Windows separators are part of the file name: use `/`.
    let msix = install(&MSIX.replace('\\', "/"), None);
    assert_eq!(msix.package_version(), Some("26.1013.4894.0"));
    for path in [
        "/Applications/ChatGPT.app",
        "/usr/lib/chatgpt",
        "C:/Users/me/AppData/Local/Programs/ChatGPT",
        // Not a package full name: three parts, a non-numeric version.
        "C:/apps/OpenAI.Codex_26.1013.4894.0_x64",
        "C:/apps/OpenAI.Codex_latest_x64__2p2nqsd0c76g0",
        "C:/apps/OpenAI.Codex_26.1013.4894_x64__2p2nqsd0c76g0",
    ] {
        assert_eq!(install(path, None).package_version(), None, "{path}");
    }
}

#[test]
fn the_release_version_is_shown_with_the_package_version_labeled() {
    let msix = MSIX.replace('\\', "/");
    let app = install(&msix, Some("26.1013.40934"));
    assert_eq!(
        shown(&app, Locale::En).as_deref(),
        Some("26.1013.40934 (package 26.1013.4894.0)")
    );
    assert_eq!(
        shown(&app, Locale::ZhCn).as_deref(),
        Some("26.1013.40934（包 26.1013.4894.0）")
    );
    // The app's own version could not be read: discovery fell back to the
    // package version, which is labeled as such.
    let fallback = install(&msix, Some("26.1013.4894.0"));
    assert_eq!(
        shown(&fallback, Locale::En).as_deref(),
        Some("package 26.1013.4894.0")
    );
    assert_eq!(
        shown(&install(&msix, None), Locale::ZhCn).as_deref(),
        Some("包 26.1013.4894.0")
    );
    // macOS / Linux: no package, the release version alone.
    let mac = install("/Applications/ChatGPT.app", Some("26.1013.40934"));
    assert_eq!(shown(&mac, Locale::ZhCn).as_deref(), Some("26.1013.40934"));
    assert_eq!(shown(&install("/usr/lib/chatgpt", None), Locale::En), None);
}

#[test]
fn a_downloaded_package_is_labeled_the_same_way() {
    let text = app_version_text(Some("26.1013.40934"), Some("26.1013.4894.0")).unwrap();
    assert_eq!(
        Catalog::new(Locale::En).text(&text),
        "26.1013.40934 (package 26.1013.4894.0)"
    );
    let unknown_release = app_version_text(None, Some("26.1013.4894.0")).unwrap();
    assert_eq!(
        Catalog::new(Locale::En).text(&unknown_release),
        "package 26.1013.4894.0"
    );
}
