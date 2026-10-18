//! `spb self-update`: check for a newer Superba and install it.

use superba_core::{
    SelfInstallOutcome, SelfUpdateCheckRequest, SelfUpdateInstallRequest, SelfUpdateStatus,
    SelfUpdateView,
};

use super::{Result, usage};
use crate::{Output, Runtime, SelfUpdateCommand, exit, text::text};

pub(super) async fn run(command: &SelfUpdateCommand, rt: &Runtime, out: &mut Output) -> Result {
    match command {
        SelfUpdateCommand::Check => check(rt, out).await,
        SelfUpdateCommand::Install { yes } => install(*yes, rt, out).await,
        SelfUpdateCommand::RestartRouter => {
            let restarted = rt.service.self_update_restart_router().await?;
            out.value(&serde_json::json!({ "restarted": restarted }))?;
            Ok(exit::OK)
        }
    }
}

fn describe(view: &SelfUpdateView, out: &mut Output) -> Result<()> {
    out.field("cli.selfupdate.current", &view.current)?;
    out.field(
        "cli.selfupdate.latest",
        out.optional(view.latest.as_ref().map(|l| l.version.clone())),
    )?;
    if let Some(t) = view.checked_at {
        out.field("cli.selfupdate.checked", t.to_rfc3339())?;
    }
    let version = view
        .latest
        .as_ref()
        .map(|l| l.version.clone())
        .unwrap_or_default();
    out.line(match view.status {
        SelfUpdateStatus::Available if view.no_package => {
            text("cli.selfupdate.no_package").arg("version", &version)
        }
        SelfUpdateStatus::Available if view.is_skipped() => {
            text("cli.selfupdate.skipped").arg("version", &version)
        }
        SelfUpdateStatus::Available => text("cli.selfupdate.available").arg("version", &version),
        SelfUpdateStatus::UpToDate => text("cli.selfupdate.up_to_date"),
        SelfUpdateStatus::PageUnavailable => text("cli.selfupdate.unavailable"),
        SelfUpdateStatus::CheckFailed => {
            text("cli.selfupdate.failed").arg("reason", view.detail.clone().unwrap_or_default())
        }
        SelfUpdateStatus::OnlineChecksOff => text("cli.selfupdate.off"),
        _ => text("cli.selfupdate.not_checked"),
    })?;
    if let Some(p) = &view.problem {
        let key = if p.installed {
            "cli.selfupdate.problem_installed"
        } else {
            "cli.selfupdate.problem_failed"
        };
        out.line(
            text(key)
                .arg("version", &p.version)
                .arg_text("reason", p.reason.clone())
                .arg("path", superba_core::paths::display_path(&p.log)),
        )?;
    }
    Ok(())
}

async fn check(rt: &Runtime, out: &mut Output) -> Result {
    let view = rt
        .service
        .self_update_check(SelfUpdateCheckRequest { manual: true })
        .await?;
    describe(&view, out)?;
    out.value(&view)?;
    Ok(exit::OK)
}

async fn install(yes: bool, rt: &Runtime, out: &mut Output) -> Result {
    let view = rt
        .service
        .self_update_check(SelfUpdateCheckRequest { manual: true })
        .await?;
    let Some(release) = view
        .latest
        .clone()
        .filter(|_| view.status == SelfUpdateStatus::Available)
    else {
        describe(&view, out)?;
        out.value(&view)?;
        return Ok(exit::OK);
    };
    if view.no_package {
        describe(&view, out)?;
        return Err(usage(
            out,
            text("cli.selfupdate.no_package").arg("version", &release.version),
        ));
    }
    if !yes {
        if !out.interactive() {
            return Err(usage(out, text("cli.selfupdate.needs_yes")));
        }
        out.line(text("cli.selfupdate.confirm").arg("version", &release.version))?;
        let answer = (rt.read_stdin_line)().unwrap_or_default();
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            out.line(text("cli.selfupdate.cancelled"))?;
            return Ok(exit::OK);
        }
    }
    out.progress(text("cli.selfupdate.downloading").arg("version", &release.version))?;
    let report = rt
        .service
        .self_update_install(SelfUpdateInstallRequest { relaunch: false })
        .await?;
    out.finish_progress()?;
    match report.outcome {
        SelfInstallOutcome::Installed => {
            out.line(text("cli.selfupdate.installed").arg("version", &report.version))?;
        }
        SelfInstallOutcome::Handoff => {
            out.line(text("cli.selfupdate.handoff").arg("version", &report.version))?;
        }
        _ => {
            out.line(text("cli.selfupdate.manual").arg("version", &report.version))?;
            for step in &report.steps {
                out.line(step.clone())?;
            }
        }
    }
    if report.router_restarted {
        out.line(text("cli.selfupdate.router_restarted"))?;
    }
    out.value(&report)?;
    Ok(exit::OK)
}
