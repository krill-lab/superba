//! `spb remote`: the SSH hosts the ChatGPT app is connected to (hosts only in
//! `~/.ssh/config` are never listed). A switch changes a host only when it
//! follows this computer, and then only its connection; settings reach a host
//! through `spb remote sync`, filtered by the host's Codex version.

use superba_core::{
    CodexVersion, HostSource, HostSyncAction, HostSyncSelection, HostView, InstallRequest,
    InstallTarget, RemoteOverview, SwitchIntent, TargetId,
};

use super::{Result, execute, request, switch, usage};
use crate::text::text;
use crate::{OnOff, Output, RemoteCodexCommand, RemoteCommand, Runtime, SwitchAnswers, exit};

/// Host names as typed (`dev`) or as target ids (`ssh:dev`).
fn target(t: &str) -> TargetId {
    if t.contains(':') {
        TargetId::new(t)
    } else {
        TargetId::remote(t)
    }
}

pub(super) async fn run(command: &RemoteCommand, rt: &Runtime, out: &mut Output) -> Result {
    match command {
        RemoteCommand::List => {
            let hosts = rt.service.hosts().await?;
            render_hosts(&hosts, out)?;
            out.value(&hosts)?;
            Ok(exit::OK)
        }
        RemoteCommand::Probe { target: t } => {
            let view = rt.service.host_probe(&target(t)).await?;
            render_host(&view, out)?;
            out.value(&view)?;
            Ok(exit::OK)
        }
        RemoteCommand::Follow { target: t, state } => {
            let view = rt
                .service
                .set_host_follow(&target(t), state.is_on())
                .await?;
            out.line(
                text(match state {
                    OnOff::On => "cli.hosts.follow_on",
                    OnOff::Off => "cli.hosts.follow_off",
                })
                .arg("host", &view.host.label),
            )?;
            out.value(&view)?;
            Ok(exit::OK)
        }
        RemoteCommand::Sync {
            target: t,
            only,
            exec,
        } => {
            let id = target(t);
            let preview = rt.service.host_sync_plan(&id).await?;
            render_sync(&preview, only, out)?;
            if exec.dry_run || !preview.blocked.is_empty() {
                out.value(&preview)?;
                return Ok(if preview.blocked.is_empty() {
                    exit::OK
                } else {
                    exit::BLOCKED
                });
            }
            let selection = HostSyncSelection {
                paths: (!only.is_empty()).then(|| only.clone()),
            };
            let mut r = request(
                SwitchIntent::HostSync {
                    target_id: id,
                    selection,
                },
                exec,
            );
            r.targets.local = false;
            execute::run(r, exec, rt, out).await
        }
        RemoteCommand::Codex { target: t, action } => {
            let id = target(t);
            let rollback = matches!(action, RemoteCodexCommand::Rollback { to: None });
            let report = match action {
                RemoteCodexCommand::Rollback { to: None } => {
                    out.progress(text("cli.hosts.rolling_back"))?;
                    rt.service
                        .rollback_codex(InstallTarget::Remote { target_id: id })
                        .await?
                }
                RemoteCodexCommand::Install { version }
                | RemoteCodexCommand::Rollback { to: Some(version) } => {
                    let version = CodexVersion::parse(version).map_err(|_| {
                        usage(out, text("cli.versions.bad_version").arg("value", version))
                    })?;
                    out.progress(text("cli.hosts.installing").arg("version", &version))?;
                    rt.service
                        .install_codex(InstallRequest {
                            version,
                            target: InstallTarget::Remote { target_id: id },
                        })
                        .await?
                }
            };
            out.line(
                text(if rollback {
                    "cli.versions.rolled_back"
                } else {
                    "cli.versions.installed"
                })
                .arg("version", &report.version)
                .arg("path", out.optional(report.executable.as_ref())),
            )?;
            if let Some(p) = &report.previous {
                out.line(text("cli.versions.previous").arg("version", p))?;
            }
            for n in &report.notes {
                out.notice(n)?;
            }
            out.value(&report)?;
            Ok(exit::OK)
        }
        RemoteCommand::Apply {
            targets,
            profile,
            line,
            auth,
            exec,
        } => {
            let id = switch::resolve_profile(profile.as_deref(), None, rt, out).await?;
            let mut request = switch::build(
                id,
                &SwitchAnswers {
                    line: line.clone(),
                    auth: *auth,
                    ..Default::default()
                },
                exec,
                out,
            )?;
            request.targets.local = false;
            request.targets.remotes = targets
                .iter()
                .chain(&exec.remotes)
                .map(TargetId::new)
                .collect();
            execute::run(request, exec, rt, out).await
        }
        RemoteCommand::Rollback { target, exec } => {
            execute::run(
                request(
                    SwitchIntent::RollbackRemote {
                        target_id: TargetId::new(target),
                    },
                    exec,
                ),
                exec,
                rt,
                out,
            )
            .await
        }
    }
}

pub(super) fn render(remotes: &[RemoteOverview], out: &mut Output) -> superba_core::Result<()> {
    let rows = remotes
        .iter()
        .map(|remote| {
            vec![
                remote.host.target_id.to_string(),
                remote.host.label.clone(),
                out.optional(remote.host.host_id.as_ref()),
                remote
                    .host
                    .connected
                    .map(|b| out.boolean(b))
                    .unwrap_or_else(|| out.label("cli.value.unknown")),
                remote
                    .host
                    .sources
                    .iter()
                    .map(|source| {
                        out.label(match source {
                            HostSource::GlobalState => "cli.source.global_state",
                            HostSource::Cdp => "cli.source.cdp",
                            HostSource::ProcessScan => "cli.source.process_scan",
                            HostSource::SshConfig => "cli.source.ssh_config",
                            HostSource::Manual => "cli.source.manual",
                        })
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
                remote
                    .error
                    .as_ref()
                    .map(|error| out.render(&error.to_text()))
                    .unwrap_or_else(|| out.none()),
            ]
        })
        .collect::<Vec<_>>();
    out.section_table(
        "cli.label.remote_hosts",
        &[
            "cli.label.target",
            "cli.label.name",
            "cli.label.host_id",
            "cli.label.connected",
            "cli.label.sources",
            "cli.label.probe_error",
        ],
        rows,
    )
}

fn render_hosts(hosts: &[HostView], out: &mut Output) -> superba_core::Result<()> {
    if hosts.is_empty() {
        return out.line(text("cli.hosts.none"));
    }
    let rows = hosts
        .iter()
        .map(|h| {
            vec![
                h.host.target_id.to_string(),
                h.host.label.clone(),
                out.optional(h.host.host_id.as_ref()),
                h.host
                    .connected
                    .map(|b| out.boolean(b))
                    .unwrap_or_else(|| out.label("cli.value.unknown")),
                out.boolean(h.settings.follow_switches),
                h.state
                    .as_ref()
                    .and_then(|s| s.codex_version.as_ref())
                    .map(ToString::to_string)
                    .unwrap_or_else(|| out.none()),
                h.state
                    .as_ref()
                    .map(|s| s.probed_at.to_rfc3339())
                    .unwrap_or_else(|| out.none()),
                h.probe_error
                    .as_ref()
                    .map(|e| out.render(&e.to_text()))
                    .unwrap_or_else(|| out.none()),
            ]
        })
        .collect::<Vec<_>>();
    out.section_table(
        "cli.label.remote_hosts",
        &[
            "cli.label.target",
            "cli.label.name",
            "cli.label.host_id",
            "cli.label.connected",
            "cli.hosts.col.follow",
            "cli.hosts.col.codex",
            "cli.hosts.col.probed",
            "cli.label.probe_error",
        ],
        rows,
    )
}

fn render_host(h: &HostView, out: &mut Output) -> superba_core::Result<()> {
    out.field("cli.label.target", &h.host.target_id)?;
    out.field("cli.label.name", &h.host.label)?;
    if let Some(s) = &h.state {
        out.field(
            "cli.hosts.os",
            format!("{:?} {}", s.os, s.arch.clone().unwrap_or_default()),
        )?;
        out.field(
            "cli.hosts.col.codex",
            out.optional(s.codex_version.as_ref()),
        )?;
        out.field(
            "cli.hosts.codex_path",
            out.optional(s.codex_resolved_path.as_ref().or(s.codex_path.as_ref())),
        )?;
        if let Some(k) = s.codex_install {
            out.field("cli.hosts.install_kind", out.render(&k.text()))?;
        }
        if let Some(kib) = s.disk_free_kib {
            out.field("cli.hosts.disk_free", format!("{} MiB", kib / 1024))?;
        }
    }
    if let Some(e) = &h.probe_error {
        out.field("cli.label.probe_error", out.render(&e.to_text()))?;
    }
    out.field(
        "cli.hosts.col.follow",
        out.boolean(h.settings.follow_switches),
    )?;
    out.field("cli.hosts.hot_restart", out.boolean(h.hot_restart))?;
    if let Some(n) = h.pending_sync {
        out.field("cli.hosts.pending", n)?;
    }
    Ok(())
}

fn render_sync(
    plan: &superba_core::HostSyncPlan,
    only: &[String],
    out: &mut Output,
) -> superba_core::Result<()> {
    out.field(
        "cli.hosts.col.codex",
        out.optional(plan.host_version.as_ref()),
    )?;
    let rows = plan
        .rows
        .iter()
        .map(|r| {
            let applies = match (&r.action, only.is_empty()) {
                (HostSyncAction::Skip { .. }, _) => false,
                (_, true) => r.selected,
                (_, false) => only.contains(&r.path),
            };
            vec![
                r.path.clone(),
                r.local.clone().unwrap_or_else(|| out.none()),
                r.remote.clone().unwrap_or_else(|| out.none()),
                out.render(&r.action.text()),
                out.boolean(applies),
            ]
        })
        .collect::<Vec<_>>();
    out.section_table(
        "cli.hosts.sync_title",
        &[
            "cli.hosts.col.path",
            "cli.hosts.col.local",
            "cli.hosts.col.remote",
            "cli.hosts.col.action",
            "cli.hosts.col.apply",
        ],
        rows,
    )?;
    for b in &plan.blocked {
        out.line(b.text())?;
    }
    for n in &plan.notes {
        out.notice(n)?;
    }
    Ok(())
}
