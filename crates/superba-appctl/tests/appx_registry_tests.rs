//! Registry-based MSIX discovery (no PowerShell) and the `--version` probe cache.

use superba_appctl::install::{AppxPackage, newest_appx_per_family, parse_appx_full_name};

#[test]
fn full_name_gives_family_version_and_root() {
    let package = parse_appx_full_name(
        "OpenAI.Codex_26.415.1234.0_x64__2p2nqsd0c76g0",
        r"C:\Program Files\WindowsApps\OpenAI.Codex_26.415.1234.0_x64__2p2nqsd0c76g0",
    )
    .unwrap();
    assert_eq!(package.name, "OpenAI.Codex");
    assert_eq!(package.family_name, "OpenAI.Codex_2p2nqsd0c76g0");
    assert_eq!(package.version.as_deref(), Some("26.415.1234.0"));
}

#[test]
fn resource_packages_and_foreign_families_are_skipped() {
    let root = r"C:\x";
    assert!(
        parse_appx_full_name(
            "OpenAI.Codex_1.0.0.0_neutral_split.language-de_2p2nqsd0c76g0",
            root
        )
        .is_none()
    );
    assert!(parse_appx_full_name("Microsoft.Paint_1.0.0.0_x64__8wekyb3d8bbwe", root).is_none());
    assert!(parse_appx_full_name("OpenAI.Codex_1.0.0.0_x64__2p2nqsd0c76g0", "").is_none());
    assert!(parse_appx_full_name("garbage", root).is_none());
}

#[test]
fn newest_version_of_a_family_wins() {
    let package = |version: &str, family: &str| AppxPackage {
        name: "n".into(),
        family_name: family.into(),
        install_location: format!("C:\\{version}").into(),
        version: Some(version.into()),
    };
    let kept = newest_appx_per_family(vec![
        package("26.9.0.0", "A_x"),
        package("26.10.0.0", "A_x"),
        package("1.0.0.0", "B_x"),
    ]);
    let versions: Vec<_> = kept.iter().filter_map(|p| p.version.as_deref()).collect();
    assert_eq!(kept.len(), 2);
    assert!(versions.contains(&"26.10.0.0") && versions.contains(&"1.0.0.0"));
}

#[cfg(unix)]
#[tokio::test]
async fn version_probe_runs_once_per_binary_file() {
    use std::os::unix::fs::PermissionsExt;

    use superba_appctl::install::codex_version;

    let dir = tempfile::tempdir().unwrap();
    let counter = dir.path().join("runs");
    let binary = dir.path().join("codex");
    std::fs::write(
        &binary,
        format!(
            "#!/bin/sh\necho x >> '{}'\necho 'codex-cli 0.77.0'\n",
            counter.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let first = codex_version(&binary).await;
    let second = codex_version(&binary).await;
    assert!(first.is_some());
    assert_eq!(first, second);
    let runs = std::fs::read_to_string(&counter).unwrap().lines().count();
    assert_eq!(runs, 1);
}
