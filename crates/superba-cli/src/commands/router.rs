//! `spb router`: the opt-in local router (v2 decision 6). While router mode is
//! on, Codex on this computer sends its requests to a Superba process on
//! 127.0.0.1, which forwards them to the member profiles' sites with each
//! member's own key and fails over between lines and members. Remote hosts and
//! the official ChatGPT login always stay direct.

use std::time::Duration;

use superba_core::{
    ProfileId, RouterDoctorReport, RouterInstallRequest, RouterView, RouterWatchState, SwitchIntent,
};
use superba_switch::router_doctor::{
    Eligibility, RouterCandidate, has_eligible_member, router_candidates,
};

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
        RouterCommand::Uninstall {
            quiet: true,
            keep_service,
        } => installer_teardown(*keep_service, rt, out).await,
        RouterCommand::Uninstall { .. } => {
            let view = rt.service.router_uninstall().await?;
            out.line(text("cli.router.uninstalled"))?;
            out.value(&view)?;
            Ok(exit::OK)
        }
        RouterCommand::Doctor { foreground } => doctor(*foreground, rt, out).await,
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

/// `spb router uninstall --quiet [--keep-service]`, run by the Windows installer before it
/// removes or replaces the files: take the router out of Codex and out of login startup
/// (unless this is an upgrade), then stop the router processes started from this install
/// so their file can be replaced. Every step is best effort and the exit code is always OK:
/// an installer must never fail because of the router.
async fn installer_teardown(keep_service: bool, rt: &Runtime, out: &mut Output) -> Result {
    let dir = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(ToOwned::to_owned));
    if !keep_service {
        if let Ok(view) = rt.service.router_status().await
            && view.codex_routed
        {
            // Configuration only: the uninstaller does not restart Codex.
            let exec = ExecArgs {
                yes: true,
                write_only: true,
                ..ExecArgs::default()
            };
            let _ = execute::run(router_switch(false, &exec), &exec, rt, out).await;
        }
        let _ = rt.service.router_uninstall().await;
        // That is refused when the uninstaller runs as another account than
        // the signed-in user, but the program goes away all the same: its
        // login startup must not outlive it.
        if let Some(dir) = &dir {
            superba_switch::router::remove_login_startup(dir);
        }
    }
    if let Some(dir) = &dir {
        superba_switch::installer::stop_router_processes(dir);
    }
    Ok(exit::OK)
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
    // Say plainly when no member can carry traffic, and what to run next,
    // instead of failing later in the install or the plan (user bug 28). The
    // one rule of the Router page, the planner and `spb router doctor`, also
    // while the router is already running (user bug 39).
    let candidates = candidates_with(members, &view, rt).await?;
    if !has_eligible_member(&candidates) {
        no_eligible_member(&candidates, out)?;
        return Ok(exit::BLOCKED);
    }
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

/// The member candidates with `members` (the `--member` arguments, else the
/// stored members) as the router's members.
async fn candidates_with(
    members: &[String],
    view: &RouterView,
    rt: &Runtime,
) -> Result<Vec<RouterCandidate>> {
    let mut set = rt.service.profile_set().await?;
    let sites = rt.service.sites().await?;
    set.settings.router.members = if members.is_empty() {
        view.settings.members.clone()
    } else {
        members.iter().map(ProfileId::new).collect()
    };
    Ok(router_candidates(&set, &sites))
}

fn candidate_id(c: &RouterCandidate) -> String {
    c.profile_id
        .as_ref()
        .map_or_else(|| c.site_id.to_string(), ProfileId::to_string)
}

/// Why router mode cannot be turned on, every provider with its reason, and the
/// commands that fix it.
fn no_eligible_member(candidates: &[RouterCandidate], out: &mut Output) -> Result<()> {
    out.value(&serde_json::json!({
        "error": "no_eligible_member",
        "providers": candidates.iter().map(|c| serde_json::json!({
            "id": candidate_id(c),
            "member": c.member,
            "eligibility": c.eligibility.id(),
        })).collect::<Vec<_>>(),
    }))?;
    out.line(text("cli.router.no_eligible"))?;
    if !candidates.is_empty() {
        out.line(text("cli.router.candidates"))?;
        for c in candidates {
            let key = if c.member {
                "cli.router.candidate_member"
            } else {
                "cli.router.candidate"
            };
            out.line(
                text(key)
                    .arg_text("name", c.name.clone())
                    .arg("id", candidate_id(c))
                    .arg_text("reason", c.eligibility.text()),
            )?;
        }
    }
    let mut next = Vec::new();
    for c in candidates {
        let id = candidate_id(c);
        match (c.eligibility, &c.profile_id) {
            (Eligibility::Eligible, Some(_)) => {
                next.push(text("cli.router.next_add").arg("id", id));
            }
            (Eligibility::NoKey, Some(_)) => {
                next.push(text("cli.router.next_key").arg("id", id));
            }
            (Eligibility::NoKey, None) => {
                next.push(text("cli.router.next_profile").arg("site", id));
            }
            (Eligibility::PatOnly, Some(_)) => {
                next.push(text("cli.router.next_auth").arg("id", id));
            }
            _ => {}
        }
    }
    if !next.is_empty() {
        out.line(text("cli.router.next"))?;
        for line in next {
            out.line(line)?;
        }
    }
    out.line(text("cli.router.doctor_hint"))?;
    Ok(())
}

/// `spb router doctor`: the report as plain text (or JSON), then optionally
/// the router in the foreground.
async fn doctor(foreground: bool, rt: &Runtime, out: &mut Output) -> Result {
    let report = match rt.service.router_doctor().await {
        Ok(report) => report,
        // A service without router diagnostics: the member part still helps.
        Err(e) if e.is_not_implemented() => {
            let set = rt.service.profile_set().await?;
            let sites = rt.service.sites().await?;
            superba_switch::router_doctor::report(Err(&e), &router_candidates(&set, &sites))
        }
        Err(e) => return Err(e.into()),
    };
    let rendered = report.render_text(&|t| out.render(t));
    for line in rendered.lines() {
        out.data_line(line)?;
    }
    out.value(&report)?;
    if foreground {
        return run_foreground(&report, rt, out).await;
    }
    Ok(exit::OK)
}

/// Run the router program exactly as the service would, with its log in this
/// terminal, until it exits or the user presses Ctrl-C (which the terminal
/// also delivers to the router, so it stops cleanly).
async fn run_foreground(report: &RouterDoctorReport, rt: &Runtime, out: &mut Output) -> Result {
    let Some((program, args)) = report.foreground.split_first() else {
        return Err(usage(out, text("cli.router.foreground_unavailable")));
    };
    if rt.service.router_status().await?.running {
        out.line(text("cli.router.foreground_running"))?;
        return Ok(exit::OK);
    }
    let shown: Vec<std::ffi::OsString> = args.iter().map(Into::into).collect();
    out.line(
        text("cli.router.foreground_start")
            .arg("command", superba_router_command_line(program, &shown)),
    )?;
    // Through the process helper (no console window of its own on Windows; the
    // router attaches to this terminal's console itself).
    let mut command = tokio::process::Command::from(superba_core::process::command(program));
    command.args(args).stdin(std::process::Stdio::null());
    if rt.env.var_non_empty("RUST_LOG").is_none() {
        command.env("RUST_LOG", "info");
    }
    let mut child = command
        .spawn()
        .map_err(|e| superba_core::Error::io(program, &e))?;
    let status = loop {
        tokio::select! {
            status = child.wait() => break status.map_err(|e| superba_core::Error::io(program, &e))?,
            // The router got the same Ctrl-C and shuts down; wait for it.
            _ = tokio::signal::ctrl_c() => {}
        }
    };
    let code = status.code();
    out.line(text("cli.router.foreground_exit").arg(
        "status",
        code.map_or_else(|| "signal".to_owned(), |c| format!("exit {c}")),
    ))?;
    Ok(if status.success() || code.is_none() {
        exit::OK
    } else {
        exit::ERROR
    })
}

/// The router's local capacity, and what happened when it ran short (user
/// bug 35: those requests got a local 503; no line was blamed).
fn render_local(local: &superba_core::RouterLocalView, out: &mut Output) -> Result<()> {
    let limit = local
        .open_files_limit
        .map_or_else(|| out.label("cli.router.unlimited"), |n| n.to_string());
    let capacity = out.render(
        &text("cli.router.capacity_value")
            .arg("in_flight", local.in_flight)
            .arg("max", local.max_concurrent)
            .arg("limit", &limit),
    );
    out.field("cli.router.capacity", capacity)?;
    if local.ran_short() {
        out.line(
            text("cli.router.ran_short")
                .arg("files", local.file_limit_errors)
                .arg("busy", local.busy)
                .arg("limit", &limit),
        )?;
        if let Some(last) = &local.last_file_limit {
            out.field("cli.router.last_file_limit", last)?;
        }
    }
    Ok(())
}

/// `program args` as a POSIX shell would read it.
fn superba_router_command_line(program: &str, args: &[std::ffi::OsString]) -> String {
    superba_switch::router_doctor::command_line(program, args)
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
                RouterWatchState::Moved { from, to } => text("cli.router.watch_moved")
                    .arg("from", *from)
                    .arg("to", *to),
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
        if let Some(local) = &view.local {
            render_local(local, out)?;
        }
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
