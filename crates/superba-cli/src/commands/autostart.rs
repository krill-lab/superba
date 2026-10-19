//! Current-user login startup for the Superba GUI.
use super::Result;
use crate::{AutostartCommand, Output, Runtime, exit, text::text};

pub(super) async fn run(command: &AutostartCommand, rt: &Runtime, out: &mut Output) -> Result {
    let status = match command {
        AutostartCommand::Status => rt.service.autostart_status().await?,
        AutostartCommand::On => rt.service.set_autostart(true).await?,
        AutostartCommand::Off => rt.service.set_autostart(false).await?,
    };
    out.line(text(if status.enabled {
        "cli.autostart.enabled"
    } else {
        "cli.autostart.disabled"
    }))?;
    out.line(text("cli.autostart.entry").arg("entry", &status.entry))?;
    out.value(&status)?;
    Ok(exit::OK)
}
