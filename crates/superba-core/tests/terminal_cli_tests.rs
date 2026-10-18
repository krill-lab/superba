//! The desktop app's own Codex copies are not "the Codex CLI" (user bug 55:
//! the wizard and the Overview showed the app's runtime copy in
//! `%LOCALAPPDATA%\OpenAI\Codex\bin\<hash>` as the CLI on a machine without
//! one).

#![allow(missing_docs)]

use superba_core::{CliOverview, CodexBinary, CodexVersion, InstallSource, terminal_cli};

fn bin(path: &str, source: InstallSource, rank: Option<usize>) -> CodexBinary {
    CodexBinary {
        path: path.into(),
        version: Some(CodexVersion::new(0, 163, 0)),
        source,
        path_rank: rank,
    }
}

const RUNTIME_COPY: &str =
    r"C:\Users\superba\AppData\Local\OpenAI\Codex\bin\896a0166b774037a\codex.exe";
const BUNDLED: &str = r"C:\Program Files\WindowsApps\OpenAI.Codex_26.1013.4894.0_x64__2p2nqsd0c76g0\app\resources\codex.exe";

#[test]
fn only_the_apps_copies_means_no_cli() {
    let binaries = vec![
        bin(RUNTIME_COPY, InstallSource::AppBundled, None),
        bin(BUNDLED, InstallSource::AppBundled, None),
    ];
    assert!(binaries.iter().all(CodexBinary::is_app_copy));
    assert_eq!(terminal_cli(&binaries), None);
    let overview = CliOverview {
        binaries,
        processes: Vec::new(),
    };
    assert_eq!(overview.terminal_cli(), None);
}

#[test]
fn the_cli_is_the_first_on_path_else_one_that_is_not_the_apps() {
    let path_cli = bin(
        r"C:\Users\superba\bin\codex.exe",
        InstallSource::Unknown,
        Some(5),
    );
    let earlier = bin(r"C:\npm\codex.cmd", InstallSource::Npm, Some(2));
    let binaries = vec![
        bin(RUNTIME_COPY, InstallSource::AppBundled, None),
        path_cli.clone(),
        earlier.clone(),
    ];
    assert_eq!(terminal_cli(&binaries), Some(&earlier));
    // An app's Codex the user put on PATH is a CLI.
    let on_path = bin("/usr/local/bin/codex", InstallSource::AppBundled, Some(0));
    assert!(!on_path.is_app_copy());
    assert_eq!(terminal_cli(std::slice::from_ref(&on_path)), Some(&on_path));
    // Not on PATH, but a real install (`~/.cargo/bin`): still the CLI.
    let cargo = bin("/home/me/.cargo/bin/codex", InstallSource::Cargo, None);
    let binaries = vec![bin(BUNDLED, InstallSource::AppBundled, None), cargo.clone()];
    assert_eq!(terminal_cli(&binaries), Some(&cargo));
}
