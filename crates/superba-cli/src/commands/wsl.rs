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
                    vec![
                        format!("wsl:{}", d.name),
                        out.render(&d.version_text()),
                        out.boolean(d.default),
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
            out.value(&o)?;
            Ok(exit::OK)
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
