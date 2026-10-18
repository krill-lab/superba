//! What Superba knows about each tool on each OS, as data: the support level,
//! the official per-user install method it uses (and a fallback), where the
//! newest version comes from, the channels, the command and how its version
//! is read, the tool's own updater and how Superba turns it off. The tool
//! modules (`tools/*`) carry out what is described here; the research
//! reports in `harness-research/` are the source of every entry.

use superba_core::ToolId;

use crate::platform::{Arch, Os, Platform};

/// How Superba installs a tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// Claude Code's own native installer, driven by Superba: the channel
    /// head, `manifest.json` (sha256 per platform), the binary into
    /// `~/.local/share/claude/versions/<v>`, then `<binary> install <v>`.
    ClaudeNative,
    /// `npm install -g --prefix <superba>/tools/<tool>/npm <package>@<v>` with
    /// `--allow-scripts` for the packages whose install scripts are needed
    /// (npm 12 blocks them otherwise); needs Node.
    NpmPrefix {
        /// Package.
        package: &'static str,
        /// `--allow-scripts=` value.
        allow_scripts: &'static str,
        /// Oldest Node (major, minor) the package supports.
        min_node: (u64, u64),
    },
    /// dsh's Node-free runtime: the PyPI `deepseek-harness-runtime-bin`
    /// wheel (sha256 from PyPI) unpacked into `<superba>/tools/dsh/runtime`.
    DshRuntime,
    /// opencode's standalone archive into `~/.opencode/bin` (the official
    /// installer's place): OpenCode 2 from `opencode.ai/files` (sha256 from
    /// the update API; an older version from its npm platform package,
    /// sha512 from the registry), OpenCode 1 from GitHub (sha256 digest).
    OpencodeArchive,
    /// pi's standalone release archive (GitHub, `SHA256SUMS`) into
    /// `<superba>/tools/pi/app`.
    PiArchive,
    /// The vendor's per-user Windows installer run silently with `args`
    /// (sha256 / sha512 when published, and the signature is checked).
    WindowsInstaller {
        /// Silent-install arguments.
        args: &'static [&'static str],
        /// Who must have signed it (part of the certificate subject).
        signer: &'static str,
    },
    /// Only a link to the download page.
    Link,
}

/// Where the newest version comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LatestSource {
    /// `downloads.claude.ai/claude-code-releases/<channel>` (plain text).
    ClaudeChannel,
    /// npm dist-tags of `package` (the channel is the tag).
    NpmDistTags {
        /// Package.
        package: &'static str,
    },
    /// `opencode.ai/update/api/<channel>/cli` (OpenCode 2) and the npm
    /// `opencode-ai` `latest` tag (OpenCode 1, channel `v1`).
    OpencodeApi,
    /// `pi.dev/api/latest-version`.
    PiApi,
    /// `zcode.z.ai/api/v1/releases/electron/manifest` (channel 1 = stable,
    /// 3 = preview).
    ZcodeManifest,
    /// WorkBuddy's `/v2/update?platform=` on `host`.
    WorkbuddyUpdate {
        /// `www.workbuddy.cn` or `www.workbuddy.ai`.
        host: &'static str,
    },
}

/// What the tool does about updates by itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnUpdater {
    /// Nothing.
    None,
    /// Checks and shows a notice.
    Notifies,
    /// Installs updates by itself (Claude Code's native updater).
    Automatic,
    /// Installs updates by itself and cannot be stopped.
    Forced,
}

/// How Superba turns the tool's own updater off ("Superba manages updates").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdaterSwitch {
    /// `env.DISABLE_AUTOUPDATER = "1"` in Claude Code's user `settings.json`
    /// (never `DISABLE_UPDATES`: that blocks `claude install` too, exit 0).
    ClaudeEnv,
    /// `update: "disable"` (OpenCode 2) / `autoupdate: false` (OpenCode 1) in
    /// opencode's global config.
    OpencodeConfig,
}

/// The support level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// Install, update, remove.
    Full,
    /// Install and remove; updates are the tool's own (forced).
    InstallOnly,
    /// A download link.
    Link,
    /// No build for this OS / processor.
    Unavailable,
}

/// One tool on one platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Descriptor {
    /// The tool.
    pub tool: ToolId,
    /// Support level.
    pub level: Level,
    /// The method Superba uses.
    pub method: Method,
    /// The method used when the first cannot run (dsh without Node).
    pub fallback: Option<Method>,
    /// Newest version.
    pub latest: LatestSource,
    /// Channels offered (the first is the default for a new install).
    pub channels: &'static [&'static str],
    /// The command (`None` for desktop apps).
    pub command: Option<&'static str>,
    /// The tool's own updater.
    pub updater: OwnUpdater,
    /// How Superba turns it off, when it can.
    pub switch: Option<UpdaterSwitch>,
    /// The vendor's download page.
    pub page: &'static str,
}

/// The descriptor of `tool` on `platform`.
pub fn descriptor(tool: ToolId, p: &Platform) -> Descriptor {
    match tool {
        ToolId::ClaudeCode => Descriptor {
            tool,
            level: Level::Full,
            method: Method::ClaudeNative,
            fallback: None,
            latest: LatestSource::ClaudeChannel,
            channels: &["latest", "stable"],
            command: Some("claude"),
            updater: OwnUpdater::Automatic,
            switch: Some(UpdaterSwitch::ClaudeEnv),
            page: "https://code.claude.com/docs/en/setup",
        },
        ToolId::Dsh => Descriptor {
            tool,
            // The Node-free runtime has no Windows arm64 build; npm works there.
            level: Level::Full,
            method: Method::NpmPrefix {
                package: "@deepseek-ai/dsh",
                allow_scripts: "@deepseek-ai/dsh-subprocess-local,koffi,node-pty",
                min_node: (22, 19),
            },
            fallback: (!(p.os == Os::Windows && p.arch == Arch::Arm64) && !p.musl)
                .then_some(Method::DshRuntime),
            latest: LatestSource::NpmDistTags {
                package: "@deepseek-ai/dsh",
            },
            channels: &["latest", "next", "alpha"],
            command: Some("dsh"),
            updater: OwnUpdater::None,
            switch: None,
            page: "https://github.com/deepseek-ai/deepseek-harness",
        },
        ToolId::Opencode => Descriptor {
            tool,
            level: Level::Full,
            method: Method::OpencodeArchive,
            fallback: None,
            latest: LatestSource::OpencodeApi,
            // `latest` = OpenCode 2 (decisions.md); `v1` keeps OpenCode 1.
            channels: &["latest", "beta", "v1"],
            command: Some("opencode"),
            updater: OwnUpdater::Notifies,
            switch: Some(UpdaterSwitch::OpencodeConfig),
            page: "https://opencode.ai/download",
        },
        ToolId::Pi => Descriptor {
            tool,
            level: if p.musl {
                Level::Unavailable
            } else {
                Level::Full
            },
            method: Method::PiArchive,
            fallback: None,
            latest: LatestSource::PiApi,
            channels: &["latest"],
            command: Some("pi"),
            updater: OwnUpdater::Notifies,
            switch: None,
            page: "https://pi.dev",
        },
        ToolId::Zcode => Descriptor {
            tool,
            level: match p.os {
                Os::Windows => Level::Full,
                _ => Level::Link,
            },
            method: match p.os {
                // electron-builder NSIS, per user without elevation.
                Os::Windows => Method::WindowsInstaller {
                    args: &["/S", "/currentuser"],
                    // Its EV certificate (seen on 3.15.3).
                    signer: "北京智谱华章科技股份有限公司",
                },
                _ => Method::Link,
            },
            fallback: None,
            latest: LatestSource::ZcodeManifest,
            channels: &["stable", "preview"],
            command: None,
            updater: OwnUpdater::Notifies,
            switch: None,
            page: "https://zcode.z.ai",
        },
        ToolId::Workbuddy | ToolId::WorkbuddyAi => {
            let cn = tool == ToolId::Workbuddy;
            Descriptor {
                tool,
                level: match (p.os, p.arch) {
                    (Os::Windows, Arch::X64) => Level::InstallOnly,
                    (Os::Windows, Arch::Arm64) => Level::Unavailable,
                    _ => Level::Link,
                },
                method: match p.os {
                    // NSIS, manifest asInvoker: per user, no elevation.
                    Os::Windows => Method::WindowsInstaller {
                        args: &["/S"],
                        signer: "Tencent",
                    },
                    _ => Method::Link,
                },
                fallback: None,
                latest: LatestSource::WorkbuddyUpdate {
                    host: if cn {
                        "www.workbuddy.cn"
                    } else {
                        "www.workbuddy.ai"
                    },
                },
                channels: &["latest"],
                command: None,
                updater: OwnUpdater::Forced,
                switch: None,
                page: if cn {
                    "https://www.workbuddy.cn"
                } else {
                    "https://www.workbuddy.ai"
                },
            }
        }
        _ => Descriptor {
            tool,
            level: Level::Unavailable,
            method: Method::Link,
            fallback: None,
            latest: LatestSource::PiApi,
            channels: &[],
            command: None,
            updater: OwnUpdater::None,
            switch: None,
            page: "",
        },
    }
}

/// Claude Code's platform key in `manifest.json` (`linux-x64-musl`,
/// `darwin-arm64`, `win32-x64`).
pub fn claude_platform(p: &Platform) -> String {
    let os = match p.os {
        Os::Macos => "darwin",
        Os::Linux => "linux",
        Os::Windows => "win32",
    };
    let musl = if p.musl { "-musl" } else { "" };
    format!("{os}-{}{musl}", p.arch_name())
}

/// opencode's archive target (`linux-x64-baseline-musl`, `windows-x64`,
/// `darwin-arm64`) and extension.
pub fn opencode_target(p: &Platform) -> (String, &'static str) {
    let os = match p.os {
        Os::Macos => "darwin",
        Os::Linux => "linux",
        Os::Windows => "windows",
    };
    let mut t = format!("{os}-{}", p.arch_name());
    if p.baseline && p.arch == Arch::X64 {
        t.push_str("-baseline");
    }
    if p.musl && p.os == Os::Linux {
        t.push_str("-musl");
    }
    (t, if p.os == Os::Linux { "tar.gz" } else { "zip" })
}

/// pi's release asset (`pi-linux-x64.tar.gz`, `pi-windows-x64.zip`).
pub fn pi_asset(p: &Platform) -> String {
    match p.os {
        Os::Windows => format!("pi-windows-{}.zip", p.arch_name()),
        Os::Macos => format!("pi-darwin-{}.tar.gz", p.arch_name()),
        Os::Linux => format!("pi-linux-{}.tar.gz", p.arch_name()),
    }
}

/// dsh runtime's platform tag (`linux-x64`, `macos-arm64`, `win-x64`) and the
/// PyPI wheel tag that carries it.
pub fn dsh_runtime_tag(p: &Platform) -> Option<(String, &'static str)> {
    Some(match (p.os, p.arch) {
        (Os::Linux, Arch::X64) => ("linux-x64".into(), "manylinux_2_28_x86_64"),
        (Os::Linux, Arch::Arm64) => ("linux-arm64".into(), "manylinux_2_28_aarch64"),
        (Os::Macos, Arch::X64) => ("macos-x64".into(), "macosx_14_0_x86_64"),
        (Os::Macos, Arch::Arm64) => ("macos-arm64".into(), "macosx_14_0_arm64"),
        (Os::Windows, Arch::X64) => ("win-x64".into(), "win_amd64"),
        (Os::Windows, Arch::Arm64) => return None,
    })
}

/// ZCode's manifest platform (`linux-x86_64`, `darwin-aarch64`).
pub fn zcode_platform(p: &Platform) -> String {
    let os = match p.os {
        Os::Macos => "darwin",
        Os::Linux => "linux",
        Os::Windows => "windows",
    };
    let arch = match p.arch {
        Arch::X64 => "x86_64",
        Arch::Arm64 => "aarch64",
    };
    format!("{os}-{arch}")
}

/// WorkBuddy's update platform (`workbuddy-win32-x64-user`,
/// `workbuddy-darwin-arm64`, `workbuddy-linux-x64-deb`).
pub fn workbuddy_platform(p: &Platform) -> String {
    match p.os {
        Os::Windows => format!("workbuddy-win32-{}-user", p.arch_name()),
        Os::Macos => format!("workbuddy-darwin-{}", p.arch_name()),
        Os::Linux => format!("workbuddy-linux-{}-deb", p.arch_name()),
    }
}
