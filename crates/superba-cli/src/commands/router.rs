//! `spb router`: the opt-in local router (v2 decision 6). While router mode is
//! on, Codex on this computer sends its requests to a Superba process on
//! 127.0.0.1, which forwards them to the member profiles' sites with each
//! member's own key and fails over between lines and members. Remote hosts and
//! the official ChatGPT login always stay direct.

use std::time::Duration;

use superba_core::{ProfileId, RouterInstallRequest, RouterView, RouterWatchState, SwitchIntent};

use super::{Result, execute, request, usage};
use crate::{ExecArgs, Output, RouterCommand, Runtime, exit, text::text};

pub(super) async fn run(command: &RouterCommand, rt: &Runtime, out: &mut Output) -> Result {
    match command {
        RouterCommand::Status => {
            let view = rt.service.router_status().await?;
            render(&view, out)?;
            out.value(&view)?;
            Ok(exit::OK)
        }
        RouterCommand::Install { members } => {
            let view = install(members, rt, out).await?;
            render(&view, out)?;
            out.value(&view)?;
            Ok(exit::OK)
        }
        RouterCommand::Enable { members, exec } => enable(members, exec, rt, out).await,
        RouterCommand::Disable { uninstall, exec } => disable(*uninstall, exec, rt, out).await,
        RouterCommand::Uninstall => {
            let view = rt.service.router_uninstall().await?;
            out.line(text("cli.router.uninstalled"))?;
            out.value(&view)?;
            Ok(exit::OK)
        }
        RouterCommand::Watch { interval, once } => watch(*interval, *once, rt, out).await,
        RouterCommand::StatsLine {
            state,
            clients,
            template,
            italic,
        } => {
            super::requests::stats_line(*state, clients, template.as_deref(), *italic, rt, out)
                .await
        }
        RouterCommand::Record { state } => super::requests::record(*state, rt, out).await,
        RouterCommand::Clear { older_than, yes } => {
            if older_than.is_none() && !yes {
                return Err(usage(out, text("cli.requests.clear_all_needs_yes")));
            }
            super::requests::clear(*older_than, rt, out).await
        }
    }
}

async fn install(members: &[String], rt: &Runtime, out: &mut Output) -> Result<RouterView> {
    out.progress(text("cli.router.installing"))?;
    Ok(rt
        .service
        .router_install(RouterInstallRequest {
            members: members.iter().map(ProfileId::new).collect(),
        })
        .await?)
}

fn router_switch(enable: bool, exec: &ExecArgs) -> superba_core::SwitchRequest {
    let mut r = request(SwitchIntent::Router { enable }, exec);
    // Remote hosts are never routed; router mode changes only this computer.
    r.targets.remotes.clear();
    r
}

async fn enable(members: &[String], exec: &ExecArgs, rt: &Runtime, out: &mut Output) -> Result {
    if !exec.remotes.is_empty() || exec.no_local {
        return Err(usage(out, text("cli.router.local_only")));
    }
    let view = rt.service.router_status().await?;
    if !members.is_empty() || !view.running {
        if exec.dry_run {
            // A dry run writes nothing: no service, no copied keys.
            let names: Vec<String> = if members.is_empty() {
                view.settings
                    .members
                    .iter()
                    .map(|m| m.to_string())
                    .collect()
            } else {
                members.to_vec()
            };
            let names = if names.is_empty() {
                out.none()
            } else {
                names.join(", ")
            };
            out.line(text("cli.router.would_install").arg("members", names))?;
            if !view.running {
                return Ok(exit::OK);
            }
        } else {
            install(members, rt, out).await?;
        }
    }
    out.line(text("cli.router.traffic_note"))?;
    let code = execute::run(router_switch(true, exec), exec, rt, out).await?;
    if code == exit::OK && !exec.dry_run {
        let view = rt.service.router_status().await?;
        out.line(text("cli.router.enabled").arg("port", out.optional(view.port)))?;
    }
    Ok(code)
}

async fn disable(uninstall: bool, exec: &ExecArgs, rt: &Runtime, out: &mut Output) -> Result {
    if !exec.remotes.is_empty() || exec.no_local {
        return Err(usage(out, text("cli.router.local_only")));
    }
    let view = rt.service.router_status().await?;
    let code = if view.codex_routed {
        execute::run(router_switch(false, exec), exec, rt, out).await?
    } else {
        if !exec.dry_run {
            rt.service.router_set_enabled(false).await?;
        }
        exit::OK
    };
    if code == exit::OK && !exec.dry_run {
        out.line(text("cli.router.disabled"))?;
        if uninstall {
            rt.service.router_uninstall().await?;
            out.line(text("cli.router.uninstalled"))?;
        }
    }
    Ok(code)
}

async fn watch(interval: u64, once: bool, rt: &Runtime, out: &mut Output) -> Result {
    let mut last: Option<RouterWatchState> = None;
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    loop {
        let tick = rt.service.router_watch().await?;
        if last.as_ref() != Some(&tick.state) {
            let line = match &tick.state {
                RouterWatchState::Inactive => text("cli.router.watch_inactive"),
                RouterWatchState::Healthy => text("cli.router.watch_healthy"),
                RouterWatchState::Suspect { detail } => {
                    text("cli.router.watch_suspect").arg("detail", detail)
                }
                RouterWatchState::FellBack { detail } => {
                    text("cli.router.watch_fell_back").arg("detail", detail)
                }
                _ => text("cli.router.watch_inactive"),
            };
            out.progress(line)?;
            last = Some(tick.state.clone());
        }
        if let Some(e) = &tick.error {
            out.progress(text("cli.error.prefix").arg_text("error", e.to_text()))?;
        }
        if once {
            out.value(&tick)?;
            return Ok(exit::OK);
        }
        tokio::select! {
            _ = &mut stop => return Ok(exit::OK),
            () = tokio::time::sleep(Duration::from_secs(interval)) => {}
        }
    }
}

fn render(view: &RouterView, out: &mut Output) -> Result<()> {
    out.field("cli.router.mode", out.boolean(view.settings.enabled))?;
    out.field("cli.router.installed", out.boolean(view.installed))?;
    out.field("cli.router.running", out.boolean(view.running))?;
    out.field("cli.router.port", out.optional(view.port))?;
    out.field("cli.router.codex_routed", out.boolean(view.codex_routed))?;
    if view.running {
        out.field("cli.router.requests", view.requests)?;
        out.field("cli.router.failovers", view.failovers)?;
    }
    if let Some(d) = &view.detail
        && !view.running
    {
        out.field("cli.label.detail", d)?;
    }
    let rows = view
        .members
        .iter()
        .flat_map(|m| {
            if m.lines.is_empty() {
                vec![vec![
                    format!("{} ({})", m.name, m.profile_id),
                    out.none(),
                    out.none(),
                    out.none(),
                    out.none(),
                ]]
            } else {
                m.lines
                    .iter()
                    .map(|l| {
                        vec![
                            format!("{} ({})", m.name, m.profile_id),
                            line_name(&l.id, &l.name, out),
                            out.render(&l.health.text()),
                            out.optional(l.ttfb_ms),
                            l.requests.to_string(),
                        ]
                    })
                    .collect()
            }
        })
        .collect::<Vec<_>>();
    out.section_table(
        "cli.router.members",
        &[
            "cli.router.member",
            "cli.label.line",
            "cli.router.health",
            "cli.router.ttfb",
            "cli.router.requests",
        ],
        rows,
    )?;
    if view.settings.enabled {
        out.line(text("cli.router.traffic_note"))?;
    }
    Ok(())
}

/// A router line's name: Krill's built-in lines in the output language (the
/// router reports English names), any other line as the router names it.
fn line_name(id: &str, name: &str, out: &Output) -> String {
    superba_core::catalog::krill_site()
        .lines
        .iter()
        .find(|l| l.id.as_str() == id && l.name.fallback == name)
        .map_or_else(|| name.to_owned(), |l| out.render(&l.name))
}
