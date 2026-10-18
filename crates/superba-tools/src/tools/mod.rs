//! One module per tool: detection, the newest version, install / update /
//! removal and (where the tool has one) the updater switch.

pub mod apps;
pub mod claude;
pub mod common;
pub mod dsh;
pub mod opencode;
pub mod pi;
pub mod workbuddy;
pub mod zcode;

use superba_core::{Error, Result, ToolId, ToolReport};

use crate::descriptor::Descriptor;
use crate::state::ToolState;
use crate::{Ctx, Detected, Op};

/// What is installed.
pub async fn detect(cx: &Ctx, desc: &Descriptor, st: &ToolState) -> Detected {
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
