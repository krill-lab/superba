//! One module per tool: detection, the newest version, install / update /
//! removal and (where the tool has one) the updater switch.

pub mod apps;
pub mod brew;
pub mod claude;
pub mod common;
pub mod dsh;
pub mod opencode;
pub mod pi;
pub mod workbuddy;
pub mod zcode;

use superba_core::{Error, Notice, Result, ToolId, ToolInstallKind, ToolOp, ToolReport, ToolUpdater};

use crate::descriptor::{Descriptor, Level};
use crate::platform::Os;
use crate::state::ToolState;
use crate::{Ctx, Detected, Op};

/// What is installed.
pub async fn detect(cx: &Ctx, desc: &Descriptor, st: &ToolState) -> Detected {
    let mut det = detect_tool(cx, desc, st).await;
    if let Some(cmd) = desc.command {
        det.command_path = crate::locate::Overrides::load(&cx.data).paths.get(cmd).cloned();
        let (found, searched) = cx.locate(cmd);
        if det.installs.is_empty() {
            det.searched = searched.summary();
        } else if let Some(l) = &found
            && l.source != crate::locate::Source::Path
            && l.source != crate::locate::Source::Override
            && det.installs.first().is_some_and(|i| common::same_file(&i.path, &l.path, cx.platform.os))
        {
            det.notes.push(Notice::info(
                common::text(
                    "tools.found_off_path",
                    "Found in {dir} ({source}), which is not on this app's PATH.",
                )
                .arg(
                    "dir",
                    l.path
                        .parent()
                        .map(superba_core::paths::display_path)
                        .unwrap_or_default(),
                )
                .arg("source", l.source.label()),
            ));
        }
    }
    if cx.platform.os == Os::Macos {
        with_homebrew(cx, desc, &mut det);
    }
    det
}

/// macOS: Homebrew is the default install method of a tool with a package
/// (when brew is there); a cask app Homebrew installed is updated and
/// removed with brew.
fn with_homebrew(cx: &Ctx, desc: &Descriptor, det: &mut Detected) {
    let pkg = brew::package(desc.tool);
    let found = brew::brew(cx);
    if det.installs.is_empty() {
        det.install_method = Some(match (pkg, &found) {
            (Some(p), Some(_)) => {
                det.brew = Some(p);
                det.blocked = None;
                common::text("tools.brew.default", "Installs with Homebrew: brew install {args}")
                    .arg("args", if p.cask { format!("--cask {}", p.name) } else { p.name.to_owned() })
            }
            (Some(_), None) => common::text(
                "tools.brew.missing",
                "Homebrew is not installed, so Superba uses the tool's own installer.",
            ),
            (None, _) if desc.tool == ToolId::Dsh => common::text(
                "tools.brew.none_dsh",
                "dsh has no Homebrew package (the `dsh` formula is a different program), so Superba installs it with npm.",
            ),
            (None, _) => return,
        });
        return;
    }
    if let (Some(p), Some(b)) = (pkg, &found)
        && p.cask
        && desc.level == Level::Link
        && brew::installed(b, p)
        && let Some(first) = det.installs.first_mut()
    {
        first.kind = ToolInstallKind::Homebrew;
        det.brew = Some(p);
        det.can_update = true;
        det.can_uninstall = true;
        det.updater = Some(ToolUpdater::PackageManager);
    }
}

/// The descriptor as this computer has it: a tool that is only a link on
/// macOS becomes installable when Homebrew can install it.
pub fn effective(mut desc: Descriptor, det: &Detected) -> Descriptor {
    if det.brew.is_some() && desc.level == Level::Link {
        desc.level = Level::Full;
    }
    desc
}

async fn detect_tool(cx: &Ctx, desc: &Descriptor, st: &ToolState) -> Detected {
    match desc.tool {
        ToolId::ClaudeCode => claude::detect(cx, st).await,
        ToolId::Dsh => dsh::detect(cx, desc, st).await,
        ToolId::Opencode => opencode::detect(cx, st).await,
        ToolId::Pi => pi::detect(cx, st).await,
        ToolId::Zcode => zcode::detect(cx, desc).await,
        ToolId::Workbuddy | ToolId::WorkbuddyAi => workbuddy::detect(cx, desc).await,
        _ => Detected::default(),
    }
}

/// The newest version on `channel`.
pub async fn latest(cx: &Ctx, desc: &Descriptor, channel: &str, det: &Detected) -> Result<String> {
    match desc.tool {
        ToolId::ClaudeCode => claude::latest(cx, channel).await,
        ToolId::Dsh => dsh::latest(cx, channel, det).await,
        ToolId::Opencode => opencode::latest(cx, channel).await,
        ToolId::Pi => pi::latest(cx).await,
        ToolId::Zcode => zcode::latest(cx, channel).await.map(|r| r.version),
        ToolId::Workbuddy | ToolId::WorkbuddyAi => {
            workbuddy::latest(cx, desc.tool).await.map(|r| r.version)
        }
        _ => Err(Error::not_implemented("latest version")),
    }
}

/// Install, update or remove.
pub async fn run(op: &Op<'_>) -> Result<ToolReport> {
    if op.cx.platform.os == Os::Macos
        && let Some(pkg) = op.det.brew
    {
        let primary = op.det.installs.first().map(|i| i.kind);
        let by_brew = match op.req.op {
            ToolOp::Install => primary.is_none(),
            _ => primary == Some(ToolInstallKind::Homebrew),
        };
        if by_brew {
            return brew::run(op, pkg).await;
        }
    }
    match op.tool() {
        ToolId::ClaudeCode => claude::run(op).await,
        ToolId::Dsh => dsh::run(op).await,
        ToolId::Opencode => opencode::run(op).await,
        ToolId::Pi => pi::run(op).await,
        ToolId::Zcode => zcode::run(op).await,
        ToolId::Workbuddy | ToolId::WorkbuddyAi => workbuddy::run(op).await,
        _ => Err(Error::not_implemented("this tool")),
    }
}

/// Turn the tool's own updater off (`on`: Superba manages updates) or back on.
pub async fn manage_updates(cx: &Ctx, desc: &Descriptor, on: bool) -> Result<()> {
    match desc.switch {
        Some(crate::descriptor::UpdaterSwitch::ClaudeEnv) => claude::manage_updates(cx, on),
        Some(crate::descriptor::UpdaterSwitch::OpencodeConfig) => {
            opencode::manage_updates(cx, on).await
        }
        None => Err(Error::Unsupported {
            what: format!(
                "turning off {}'s own updater (it has no setting for that)",
                desc.tool.display_name()
            ),
        }),
    }
}
