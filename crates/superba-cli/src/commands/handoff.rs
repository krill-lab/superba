//! `spb hand-off` (GH#5): let another tool (cc-switch, AI Toolbox, ...) manage
//! Codex's configuration. Superba puts back only what it still manages, keeps
//! what the other tool changed, keeps encrypted copies of both files and owns
//! nothing afterwards; a later `spb switch` takes over again.

use superba_core::{ChangeSource, SwitchIntent};

use super::{Result, execute, request};
use crate::{HandOffArgs, HandOffTo, Output, Runtime, exit, text::text};

pub(super) async fn run(args: &HandOffArgs, rt: &Runtime, out: &mut Output) -> Result {
    let overview = rt.service.overview().await?;
    if let Some(h) = overview.config.as_ref().and_then(|c| c.handed_off.as_ref()) {
        out.line(text("cli.handoff.already").arg_text("tool", h.to.text()))?;
        out.value(&serde_json::json!({ "status": "handed_off", "handed_off": h }))?;
        return Ok(exit::OK);
    }
    let seen = overview.external.as_ref().map(|x| x.source.clone());
    let to = match args.to {
        None => seen.unwrap_or_default(),
        Some(HandOffTo::CcSwitch) => match seen {
            Some(s @ ChangeSource::CcSwitch { .. }) => s,
            _ => ChangeSource::CcSwitch { provider: None },
        },
        Some(HandOffTo::AiToolbox) => match seen {
            Some(s @ ChangeSource::AiToolbox { .. }) => s,
            _ => ChangeSource::AiToolbox { provider: None },
        },
        Some(HandOffTo::Other) => ChangeSource::Unknown,
    };
    execute::run(
        request(SwitchIntent::HandOff { to }, &args.exec),
        &args.exec,
        rt,
        out,
    )
    .await
}
