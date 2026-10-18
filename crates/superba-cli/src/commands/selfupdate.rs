//! `spb self-update`: check for a newer Superba and install it, on the update
//! channel (Stable / Preview, `--channel` saves the choice like Preferences).

use superba_core::{
    CodexVersion, SelfInstallOutcome, SelfUpdateCheckRequest, SelfUpdateInstallRequest,
    SelfUpdateStatus, SelfUpdateView, UpdateChannel,
};

use super::{Result, usage};
use crate::{ChannelArg, Output, Runtime, SelfUpdateCommand, exit, text::text};

pub(super) async fn run(command: &SelfUpdateCommand, rt: &Runtime, out: &mut Output) -> Result {
    match command {
        SelfUpdateCommand::Check { channel } => {
            choose_channel(*channel, rt, out).await?;
            check(rt, out).await
        }
        SelfUpdateCommand::Install {
            yes,
            channel,
            version,
        } => {
            choose_channel(*channel, rt, out).await?;
            match version {
                Some(v) => install_version(v, *yes, rt, out).await,
                None => install(*yes, rt, out).await,
            }
        }
        SelfUpdateCommand::RestartRouter => {
            let restarted = rt.service.self_update_restart_router().await?;
            out.value(&serde_json::json!({ "restarted": restarted }))?;
            Ok(exit::OK)
        }
    }
}

/// `--channel`: saved as the user's choice (Preferences → Updates shows it).
async fn choose_channel(channel: Option<ChannelArg>, rt: &Runtime, out: &mut Output) -> Result<()> {
    let Some(channel) = channel.map(UpdateChannel::from) else {
        return Ok(());
    };
    let mut settings = rt.service.profile_set().await?.settings;
    if settings.update_channel != Some(channel) {
        settings.update_channel = Some(channel);
        rt.service.save_settings(settings).await?;
    }
    out.progress(text("cli.selfupdate.channel_saved").arg("channel", channel_name(channel, out)))?;
    Ok(())
}

fn channel_name(channel: UpdateChannel, out: &Output) -> String {
    out.label(match channel {
        UpdateChannel::Stable => "cli.selfupdate.channel_stable",
        UpdateChannel::Preview => "cli.selfupdate.channel_preview",
    })
}

/// "You're on preview 0.1.1-alpha.3 · latest preview 0.1.1-alpha.4 · latest
/// stable 0.1.0" (what is known).
fn channel_line(view: &SelfUpdateView, out: &Output) -> String {
    let on = if view.on_prerelease() {
        "cli.selfupdate.on_preview"
    } else {
        "cli.selfupdate.on_stable"
    };
    let mut parts = vec![out.render(&text(on).arg("version", &view.current))];
    if let Some(p) = &view.latest_preview {
        parts.push(out.render(&text("cli.selfupdate.latest_preview").arg("version", &p.version)));
    }
    if let Some(s) = &view.latest_stable {
        parts.push(out.render(&text("cli.selfupdate.latest_stable").arg("version", &s.version)));
    }
    parts.join(" · ")
}

fn describe(view: &SelfUpdateView, out: &mut Output) -> Result<()> {
    out.field("cli.selfupdate.current", &view.current)?;
    let why = if view.channel_chosen {
        "cli.selfupdate.channel_chosen"
    } else if view.on_prerelease() {
        "cli.selfupdate.channel_default_preview"
    } else {
        "cli.selfupdate.channel_default_stable"
    };
    let channel = out.render(
        &text("cli.selfupdate.channel_value")
            .arg("channel", channel_name(view.channel, out))
            .arg("why", out.label(why)),
    );
    out.field("cli.selfupdate.channel", channel)?;
    out.field(
        "cli.selfupdate.latest",
        out.optional(view.latest.as_ref().map(|l| l.version.clone())),
    )?;
    if let Some(t) = view.checked_at {
        out.field("cli.selfupdate.checked", out.time(t))?;
    }
    if view.latest_stable.is_some() || view.latest_preview.is_some() {
        let line = channel_line(view, out);
        out.data_line(&line)?;
    }
    if let Some(t) = view.limited_until {
        out.line(text("cli.selfupdate.rate_limited").arg("time", out.time(t)))?;
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
        SelfUpdateStatus::UpToDate
            if view.channel == UpdateChannel::Stable && view.on_prerelease() =>
        {
            match &view.latest_stable {
                Some(stable) => text("cli.selfupdate.no_downgrade_to")
                    .arg("version", &view.current)
                    .arg("stable", &stable.version),
                None => text("cli.selfupdate.no_downgrade").arg("version", &view.current),
            }
        }
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

/// Ask before installing (`--yes` skips it; required without a terminal).
/// `false` = the user said no.
fn confirm(yes: bool, version: &str, rt: &Runtime, out: &mut Output) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    if !out.interactive() {
        return Err(usage(out, text("cli.selfupdate.needs_yes")));
    }
    out.line(text("cli.selfupdate.confirm").arg("version", version))?;
    let answer = (rt.read_stdin_line)().unwrap_or_default();
    if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        out.line(text("cli.selfupdate.cancelled"))?;
        return Ok(false);
    }
    Ok(true)
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
    if !confirm(yes, &release.version, rt, out)? {
        return Ok(exit::OK);
    }
    run_install(&release.version, None, rt, out).await
}

/// `install --version X`: exactly that release, also an older one (the user
/// chose it by name; the update channel never offers one).
async fn install_version(version: &str, yes: bool, rt: &Runtime, out: &mut Output) -> Result {
    let wanted = version.trim().trim_start_matches('v').to_owned();
    let current = rt.service.self_update_view().await?.current;
    let older = match (CodexVersion::parse(&wanted), CodexVersion::parse(&current)) {
        (Ok(w), Ok(c)) if w == c => {
            return Err(usage(
                out,
                text("cli.selfupdate.same_version").arg("version", &wanted),
            ));
        }
        (Ok(w), Ok(c)) => w < c,
        _ => false,
    };
    if older {
        out.line(
            text("cli.selfupdate.downgrade")
                .arg("version", &wanted)
                .arg("current", &current),
        )?;
    }
    if !confirm(yes, &wanted, rt, out)? {
        return Ok(exit::OK);
    }
    run_install(&wanted, Some(wanted.clone()), rt, out).await
}

async fn run_install(
    shown: &str,
    version: Option<String>,
    rt: &Runtime,
    out: &mut Output,
) -> Result {
    out.progress(text("cli.selfupdate.downloading").arg("version", shown))?;
    let report = rt
        .service
        .self_update_install(SelfUpdateInstallRequest {
            relaunch: false,
            version,
        })
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
