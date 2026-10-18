//! The per-tool, per-OS descriptors: support level, method, channels and
//! the vendor's file / platform names.

use pretty_assertions::assert_eq;
use superba_core::ToolId;
use superba_tools::descriptor::{
    Level, Method, OwnUpdater, UpdaterSwitch, claude_platform, descriptor, dsh_runtime_tag,
    opencode_target, pi_asset, workbuddy_platform, zcode_platform,
};
use superba_tools::platform::{Arch, Os, Platform};

fn p(os: Os, arch: Arch) -> Platform {
    Platform::new(os, arch)
}

const ALL_OS: [Os; 3] = [Os::Linux, Os::Macos, Os::Windows];

#[test]
fn command_line_tools_are_fully_supported_everywhere() {
    for os in ALL_OS {
        for tool in [
            ToolId::ClaudeCode,
            ToolId::Dsh,
            ToolId::Opencode,
            ToolId::Pi,
        ] {
            let d = descriptor(tool, &p(os, Arch::X64));
            assert_eq!(d.level, Level::Full, "{tool} on {os:?}");
            assert!(d.command.is_some(), "{tool}");
            assert!(!d.channels.is_empty(), "{tool}");
        }
    }
}

#[test]
fn claude_code_uses_its_native_installer_and_the_env_switch() {
    let d = descriptor(ToolId::ClaudeCode, &p(Os::Windows, Arch::X64));
    assert_eq!(d.method, Method::ClaudeNative);
    assert_eq!(d.channels, ["latest", "stable"]);
    assert_eq!(d.updater, OwnUpdater::Automatic);
    assert_eq!(d.switch, Some(UpdaterSwitch::ClaudeEnv));
}

#[test]
fn dsh_uses_npm_with_allowed_scripts_and_the_runtime_without_node() {
    let d = descriptor(ToolId::Dsh, &p(Os::Linux, Arch::X64));
    let Method::NpmPrefix {
        package,
        allow_scripts,
        min_node,
    } = d.method
    else {
        panic!("npm expected, got {:?}", d.method);
    };
    assert_eq!(package, "@deepseek-ai/dsh");
    assert_eq!(
        allow_scripts,
        "@deepseek-ai/dsh-subprocess-local,koffi,node-pty"
    );
    assert_eq!(min_node, (22, 19));
    assert_eq!(d.fallback, Some(Method::DshRuntime));
    assert_eq!(d.channels[0], "latest");
    assert_eq!(d.updater, OwnUpdater::None);
    // No runtime build for Windows on ARM or musl.
    assert_eq!(
        descriptor(ToolId::Dsh, &p(Os::Windows, Arch::Arm64)).fallback,
        None
    );
    let mut musl = p(Os::Linux, Arch::X64);
    musl.musl = true;
    assert_eq!(descriptor(ToolId::Dsh, &musl).fallback, None);
}

#[test]
fn opencode_defaults_to_opencode_2_and_offers_opencode_1() {
    let d = descriptor(ToolId::Opencode, &p(Os::Macos, Arch::Arm64));
    assert_eq!(d.method, Method::OpencodeArchive);
    assert_eq!(d.channels, ["latest", "beta", "v1"]);
    assert_eq!(d.switch, Some(UpdaterSwitch::OpencodeConfig));
}

#[test]
fn desktop_apps_install_on_windows_and_link_elsewhere() {
    for tool in [ToolId::Zcode, ToolId::Workbuddy, ToolId::WorkbuddyAi] {
        for os in [Os::Linux, Os::Macos] {
            let d = descriptor(tool, &p(os, Arch::X64));
            assert_eq!(d.level, Level::Link, "{tool} on {os:?}");
            assert_eq!(d.method, Method::Link);
            assert!(d.page.starts_with("https://"));
        }
        assert!(d_win(tool).command.is_none());
    }
    assert_eq!(d_win(ToolId::Zcode).level, Level::Full);
    assert_eq!(
        d_win(ToolId::Zcode).method,
        Method::WindowsInstaller {
            args: &["/S", "/currentuser"],
            signer: "北京智谱华章科技股份有限公司"
        }
    );
    for tool in [ToolId::Workbuddy, ToolId::WorkbuddyAi] {
        let d = d_win(tool);
        assert_eq!(d.level, Level::InstallOnly, "WorkBuddy updates itself");
        assert_eq!(d.updater, OwnUpdater::Forced);
        assert_eq!(
            d.method,
            Method::WindowsInstaller {
                args: &["/S"],
                signer: "Tencent"
            }
        );
        assert_eq!(
            descriptor(tool, &p(Os::Windows, Arch::Arm64)).level,
            Level::Unavailable
        );
    }
}

fn d_win(tool: ToolId) -> superba_tools::descriptor::Descriptor {
    descriptor(tool, &p(Os::Windows, Arch::X64))
}

#[test]
fn vendor_platform_names() {
    let mut musl = p(Os::Linux, Arch::X64);
    musl.musl = true;
    assert_eq!(claude_platform(&p(Os::Linux, Arch::X64)), "linux-x64");
    assert_eq!(claude_platform(&musl), "linux-x64-musl");
    assert_eq!(claude_platform(&p(Os::Macos, Arch::Arm64)), "darwin-arm64");
    assert_eq!(claude_platform(&p(Os::Windows, Arch::X64)), "win32-x64");

    let mut base = p(Os::Linux, Arch::X64);
    base.baseline = true;
    base.musl = true;
    assert_eq!(
        opencode_target(&base),
        ("linux-x64-baseline-musl".into(), "tar.gz")
    );
    assert_eq!(
        opencode_target(&p(Os::Windows, Arch::X64)),
        ("windows-x64".into(), "zip")
    );
    assert_eq!(
        opencode_target(&p(Os::Macos, Arch::Arm64)),
        ("darwin-arm64".into(), "zip")
    );

    assert_eq!(
        pi_asset(&p(Os::Linux, Arch::Arm64)),
        "pi-linux-arm64.tar.gz"
    );
    assert_eq!(pi_asset(&p(Os::Windows, Arch::X64)), "pi-windows-x64.zip");
    assert_eq!(pi_asset(&p(Os::Macos, Arch::X64)), "pi-darwin-x64.tar.gz");

    assert_eq!(
        dsh_runtime_tag(&p(Os::Windows, Arch::X64)),
        Some(("win-x64".into(), "win_amd64"))
    );
    assert_eq!(
        dsh_runtime_tag(&p(Os::Linux, Arch::X64)),
        Some(("linux-x64".into(), "manylinux_2_28_x86_64"))
    );
    assert_eq!(dsh_runtime_tag(&p(Os::Windows, Arch::Arm64)), None);

    assert_eq!(zcode_platform(&p(Os::Macos, Arch::Arm64)), "darwin-aarch64");
    assert_eq!(zcode_platform(&p(Os::Windows, Arch::X64)), "windows-x86_64");
    assert_eq!(
        workbuddy_platform(&p(Os::Windows, Arch::X64)),
        "workbuddy-win32-x64-user"
    );
    assert_eq!(
        workbuddy_platform(&p(Os::Linux, Arch::X64)),
        "workbuddy-linux-x64-deb"
    );
    assert_eq!(
        workbuddy_platform(&p(Os::Macos, Arch::Arm64)),
        "workbuddy-darwin-arm64"
    );
}

#[test]
fn tool_ids_round_trip() {
    for id in ToolId::ALL {
        assert_eq!(ToolId::parse(id.as_str()), Some(id));
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{}\"", id.as_str()));
    }
    assert_eq!(ToolId::parse("Claude-Code"), Some(ToolId::ClaudeCode));
    assert_eq!(ToolId::parse("workbuddy_intl"), Some(ToolId::WorkbuddyAi));
    assert_eq!(ToolId::parse("trae"), None, "Trae SOLO was dropped");
}
