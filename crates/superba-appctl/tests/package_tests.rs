//! Windows app packages: package full names from the AppModel repository key
//! (framework dependencies already installed), and the platform functions
//! outside Windows.

use superba_appctl::package::parse_full_name;
use superba_core::InstalledPackage;

#[test]
fn package_full_names_split_into_name_version_arch_and_publisher() {
    assert_eq!(
        parse_full_name("Microsoft.VCLibs.140.00.UWPDesktop_14.0.33519.0_x64__8wekyb3d8bbwe"),
        Some(InstalledPackage {
            name: "Microsoft.VCLibs.140.00.UWPDesktop".into(),
            version: "14.0.33519.0".into(),
            arch: "x64".into(),
            publisher_id: "8wekyb3d8bbwe".into(),
        })
    );
    let bundle =
        parse_full_name("Microsoft.WindowsTerminal_1.21.2361.0_neutral_~_8wekyb3d8bbwe").unwrap();
    assert_eq!(bundle.arch, "neutral");
    let app = parse_full_name("OpenAI.Codex_26.1013.4894.0_X64__2p2nqsd0c76g0").unwrap();
    assert_eq!(
        (app.name.as_str(), app.arch.as_str()),
        ("OpenAI.Codex", "x64")
    );
    for bad in [
        "",
        "OpenAI.Codex",
        "OpenAI.Codex_26.1013.4894.0_x64__short",
        "OpenAI.Codex_v26_x64__2p2nqsd0c76g0",
        "a_b_c_d_e_f",
    ] {
        assert_eq!(parse_full_name(bad), None, "{bad}");
    }
}

#[cfg(not(windows))]
#[test]
fn outside_windows_nothing_is_verified_or_opened() {
    use superba_appctl::package;
    let p = std::path::Path::new("/nonexistent/does-not-matter.msix");
    assert!(matches!(
        package::verify_signature(p),
        Err(superba_core::Error::Unsupported { .. })
    ));
    assert!(matches!(
        package::open_with_default_handler(p),
        Err(superba_core::Error::Unsupported { .. })
    ));
    assert!(package::installed_packages().is_empty());
}

#[test]
fn machine_types_and_processor_names_map_to_msix_architectures() {
    use superba_appctl::package::{machine_arch, native_arch, processor_arch};
    // IMAGE_FILE_MACHINE_* as IsWow64Process2 reports the host.
    assert_eq!(machine_arch(0xAA64), Some("arm64"));
    assert_eq!(machine_arch(0x8664), Some("x64"));
    assert_eq!(machine_arch(0x014C), Some("x86"));
    assert_eq!(machine_arch(0), None);
    // The system-wide PROCESSOR_ARCHITECTURE (not the emulated process's).
    assert_eq!(processor_arch("ARM64"), Some("arm64"));
    assert_eq!(processor_arch("AMD64"), Some("x64"));
    assert_eq!(processor_arch("x86"), Some("x86"));
    assert_eq!(processor_arch("IA64"), None);
    // Outside Windows: the process's own architecture.
    #[cfg(not(windows))]
    assert_eq!(
        native_arch(),
        match std::env::consts::ARCH {
            "aarch64" => "arm64",
            "x86" => "x86",
            _ => "x64",
        }
    );
    #[cfg(windows)]
    assert!(["x64", "arm64", "x86"].contains(&native_arch()));
}
