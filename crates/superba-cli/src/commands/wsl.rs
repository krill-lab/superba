//! `spb wsl`: the WSL distributions of this Windows computer (user bug 45). They
//! are managed hosts (`spb remote … wsl:<distro>`): a switch here also switches
//! the distributions that follow this computer (by default), with their own
//! settings filter and, in router mode, the local router when they reach it.

use superba_core::wsl::WslAccessReport;
use superba_core::{RouterPath, WslNetworking};

use super::Result;
use crate::text::text;
use crate::{Output, Runtime, WslCommand, exit};

fn networking_key(n: WslNetworking) -> &'static str {
    match n {
        WslNetworking::Mirrored => "wsl.networking_mirrored",
        WslNetworking::None => "wsl.networking_none",
        WslNetworking::VirtioProxy => "wsl.networking_virtio_proxy",
        _ => "wsl.networking_nat",
    }
}

fn path_key(p: RouterPath) -> &'static str {
    match p {
        RouterPath::Loopback => "cli.wsl.path_loopback",
        RouterPath::Unreachable => "cli.wsl.path_none",
        _ => "cli.wsl.path_nat",
    }
}

pub(super) async fn run(command: &WslCommand, rt: &Runtime, out: &mut Output) -> Result {
    match command {
        WslCommand::List => {
            let o = rt.service.wsl_overview().await?;
            if let Some(e) = &o.error {
                out.line(text("cli.wsl.not_listed").arg("detail", e))?;
            }
            if o.distros.is_empty() {
                out.line(text("cli.wsl.none"))?;
                out.value(&o)?;
                return Ok(exit::OK);
            }
            out.field(
                "cli.wsl.networking",
                out.render(&superba_core::LocalizedText::new(
                    networking_key(o.networking),
                    "",
                )),
            )?;
            let rows = o
                .distros
                .iter()
                .map(|h| {
                    let d = h.host.wsl.clone().unwrap_or_else(|| {
                        superba_core::WslDistro::named(h.host.wsl_distro().unwrap_or_default())
                    });
                    let state = match d.running() {
                        Some(true) => out.label("cli.wsl.running"),
                        Some(false) if h.pending.is_some() => out.label("cli.wsl.stopped_pending"),
                        Some(false) => out.label("cli.wsl.stopped"),
                        None if h.pending.is_some() => out.label("cli.wsl.pending"),
                        None => out.none(),
                    };
                    vec![
                        format!("wsl:{}", d.name),
                        out.render(&d.version_text()),
                        out.boolean(d.default),
                        state,
                        out.boolean(h.settings.follows(h.host.kind)),
                        h.state
                            .as_ref()
                            .and_then(|s| s.codex_version.as_ref())
                            .map(ToString::to_string)
                            .unwrap_or_else(|| out.none()),
                        out.label(path_key(d.router_path())),
                        h.probe_error
                            .as_ref()
                            .map(|e| out.render(&e.to_text()))
                            .unwrap_or_else(|| out.none()),
                    ]
                })
                .collect::<Vec<_>>();
            out.table(
                &[
                    "cli.label.target",
                    "cli.wsl.col.version",
                    "cli.wsl.col.default",
                    "cli.wsl.col.state",
                    "cli.hosts.col.follow",
                    "cli.hosts.col.codex",
                    "cli.wsl.col.router",
                    "cli.label.probe_error",
                ],
                rows,
            )?;
            if !o.router_wsl_listen.is_empty() {
                out.field("cli.wsl.listening", o.router_wsl_listen.join(", "))?;
            }
            if let Some(distro) = &o.app_runs_codex_in {
                out.line(text("cli.wsl.app_runs_codex_in").arg("distro", distro))?;
            }
            for h in o.distros.iter().filter(|h| h.pending.is_some()) {
                let name = h.host.wsl_distro().unwrap_or_default();
                out.line(text("cli.wsl.pending_hint").arg("distro", &name))?;
            }
            out.value(&o)?;
            Ok(exit::OK)
        }
        WslCommand::Update { distro, exec } => {
            // What the last switch left out while the distribution was stopped
            // (user bug 54), for it alone; it is started for this.
            let name = distro.strip_prefix("wsl:").unwrap_or(distro);
            let id = superba_core::wsl::target_id(name);
            let o = rt.service.wsl_overview().await?;
            let Some(view) = o.distros.iter().find(|h| h.host.target_id == id) else {
                out.line(text("cli.wsl.unknown").arg("distro", name))?;
                return Ok(exit::BLOCKED);
            };
            let Some(pending) = &view.pending else {
                out.line(text("cli.wsl.nothing_pending").arg("distro", name))?;
                out.value(view)?;
                return Ok(exit::OK);
            };
            super::execute::run(pending.request(&id), exec, rt, out).await
        }
        WslCommand::RouterAccess { distro } => {
            let name = distro.strip_prefix("wsl:").unwrap_or(distro);
            let id = superba_core::wsl::target_id(name);
            out.progress(text("cli.wsl.checking").arg("distro", name))?;
            let report: WslAccessReport = rt.service.wsl_router_access(&id).await?;
            for n in &report.notes {
                out.notice(n)?;
            }
            out.value(&report)?;
            Ok(if report.reachable {
                exit::OK
            } else {
                exit::BLOCKED
            })
        }
    }
}
