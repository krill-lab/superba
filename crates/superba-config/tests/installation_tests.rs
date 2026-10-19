use std::path::{Path, PathBuf};
use std::sync::Arc;
use superba_config::installation::{Executable, macos_executable};
use superba_core::{LocalContext, MapEnv};

fn binary(path: PathBuf) -> PathBuf {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "binary").unwrap();
    path
}

#[test]
fn readme_cli_install_finds_installed_bundle_by_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = LocalContext::for_tests(tmp.path());
    let cli = binary(ctx.home_dir.join(".local/bin/spb"));
    let bundle = tmp.path().join("Moved Superba.app");
    let gui = binary(bundle.join("Contents/MacOS/superba"));
    assert_eq!(
        macos_executable(&cli, &ctx, Executable::Gui, || Some(bundle)),
        Some(gui)
    );
}

#[test]
fn override_then_sibling_then_bundle_are_preferred_in_order() {
    let tmp = tempfile::tempdir().unwrap();
    let mut ctx = LocalContext::for_tests(tmp.path());
    let cli = binary(tmp.path().join("bin/spb"));
    let sibling = binary(tmp.path().join("bin/superba"));
    let override_gui = binary(tmp.path().join("custom/gui"));
    ctx.env = Arc::new(MapEnv::new().with("SUPERBA_GUI_EXE", override_gui.as_os_str()));
    assert_eq!(
        macos_executable(&cli, &ctx, Executable::Gui, || panic!("override first")),
        Some(override_gui)
    );
    ctx.env = Arc::new(MapEnv::new());
    assert_eq!(
        macos_executable(&cli, &ctx, Executable::Gui, || panic!("sibling first")),
        Some(sibling)
    );
}

#[test]
fn invalid_explicit_override_is_not_silently_replaced() {
    let tmp = tempfile::tempdir().unwrap();
    let mut ctx = LocalContext::for_tests(tmp.path());
    let cli = binary(tmp.path().join("spb"));
    binary(tmp.path().join("superba"));
    for path in [Path::new("relative"), tmp.path()] {
        ctx.env = Arc::new(MapEnv::new().with("SUPERBA_GUI_EXE", path.as_os_str()));
        assert_eq!(
            macos_executable(&cli, &ctx, Executable::Gui, || panic!("explicit override")),
            None
        );
    }
}

#[test]
fn dmg_and_home_applications_work_without_launchservices_registration() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = LocalContext::for_tests(tmp.path());
    let cli = binary(tmp.path().join("dmg/spb"));
    let dmg_gui = binary(tmp.path().join("dmg/Superba.app/Contents/MacOS/superba"));
    assert_eq!(
        macos_executable(&cli, &ctx, Executable::Gui, || None),
        Some(dmg_gui.clone())
    );
    std::fs::remove_file(dmg_gui).unwrap();
    let installed = binary(
        ctx.home_dir
            .join("Applications/Superba.app/Contents/MacOS/superba"),
    );
    assert_eq!(
        macos_executable(&cli, &ctx, Executable::Gui, || None),
        Some(installed)
    );
}

#[test]
fn finder_gui_finds_readme_cli_without_shell_path() {
    let tmp = tempfile::tempdir().unwrap();
    let mut ctx = LocalContext::for_tests(tmp.path());
    ctx.env = Arc::new(MapEnv::new().with("PATH", "/usr/bin:/bin"));
    let gui = binary(tmp.path().join("Apps/Superba.app/Contents/MacOS/superba"));
    let cli = binary(ctx.home_dir.join(".local/bin/spb"));
    assert_eq!(
        macos_executable(&gui, &ctx, Executable::Spb, || None),
        Some(cli)
    );
    let explicit = binary(tmp.path().join("custom/spb"));
    ctx.env = Arc::new(MapEnv::new().with("SUPERBA_SPB_EXE", explicit.as_os_str()));
    assert_eq!(
        macos_executable(&gui, &ctx, Executable::Spb, || None),
        Some(explicit)
    );
}
