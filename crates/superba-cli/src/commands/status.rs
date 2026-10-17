use superba_core::{AppSession, EnvObservation, HotSwitchStatus, JournalAction};
use superba_switch::messages;

use super::{Result, remote};
use crate::{Output, Runtime, exit, text::line_label};

pub(super) async fn run(rt: &Runtime, out: &mut Output) -> Result {
    let overview = rt.service.overview().await?;
    if out.json {
        out.value(&overview)?;
        return Ok(exit::OK);
    }
    let sites = if overview
        .active
        .as_ref()
        .is_some_and(|active| active.line_id.is_some())
        || overview.app.authapi_line.is_some()
    {
        rt.service.sites().await?
    } else {
        Vec::new()
    };
    out.field("cli.label.codex_home", overview.codex_home.display())?;
    if let Some(active) = &overview.active {
        out.field(
            "cli.label.profile",
            out.optional(active.profile_id.as_ref()),
        )?;
        out.field("cli.label.site", out.optional(active.site_id.as_ref()))?;
        out.field(
            "cli.label.line",
            active
                .line_id
                .as_ref()
                .map(|id| out.render(&line_label(&sites, active.site_id.as_ref(), id)))
                .unwrap_or_else(|| out.none()),
        )?;
        out.field(
            "cli.label.auth",
            active
                .auth_mode
                .map(|a| out.render(&a.text()))
                .unwrap_or_else(|| out.none()),
        )?;
        out.field("cli.label.generation", active.generation)?;
    } else {
        out.field("cli.label.profile", out.none())?;
    }
    let rows = overview
        .app
        .installs
        .iter()
        .map(|install| {
            vec![
                install.id.clone(),
                superba_core::paths::display_path(&install.path),
                install
                    .version_text()
                    .map(|v| out.render(&v))
                    .unwrap_or_else(|| out.none()),
                out.optional(install.bundled_codex_version.as_ref()),
            ]
        })
        .collect::<Vec<_>>();
    out.section_table(
        "cli.label.installs",
        &[
            "cli.label.id",
            "cli.label.path",
            "cli.label.app_version",
            "cli.label.codex_version",
        ],
        rows,
    )?;
    if let Some(session) = &overview.app.session {
        app_session(session, out)?;
    }
    let hot = match &overview.app.hot_switch {
        HotSwitchStatus::Ready => out.label("cli.hot.ready"),
        HotSwitchStatus::Unavailable { reason } => out.render(&reason.text()),
        HotSwitchStatus::NotRunning => out.label("cli.hot.not_running"),
        HotSwitchStatus::NotInstalled => out.label("cli.hot.not_installed"),
        HotSwitchStatus::SafeMode => out.label("cli.hot.safe_mode"),
    };
    out.field("cli.label.hot_switch", hot)?;
    out.field(
        "cli.label.auth_line",
        overview
            .app
            .authapi_line
            .as_ref()
            .map(|id| out.render(&line_label(&sites, None, id)))
            .unwrap_or_else(|| out.none()),
    )?;
    let rows = overview
        .cli
        .binaries
        .iter()
        .map(|binary| {
            vec![
                binary.path.display().to_string(),
                out.optional(binary.version.as_ref()),
                out.optional(binary.path_rank),
            ]
        })
        .collect::<Vec<_>>();
    out.section_table(
        "cli.label.cli_binaries",
        &[
            "cli.label.path",
            "cli.label.codex_version",
            "cli.label.path_rank",
        ],
        rows,
    )?;
    out.field("cli.label.processes", overview.cli.processes.len())?;
    out.field("cli.label.stale", overview.cli.stale_count())?;
    remote::render(&overview.remotes, out)?;
    for notice in &overview.notices {
        out.notice(notice)?;
    }
    out.value(&overview)?;
    Ok(exit::OK)
}

pub(super) fn app_session(session: &AppSession, out: &mut Output) -> superba_core::Result<()> {
    out.field("cli.label.pid", session.main_pid)?;
    out.field(
        "cli.label.cdp_port",
        out.optional(session.cdp.as_ref().map(|c| c.port)),
    )?;
    out.field(
        "cli.label.launched_by_us",
        out.boolean(session.launched_by_us),
    )?;
    let env = match &session.authapi_env {
        EnvObservation::Present(value) => value.clone(),
        EnvObservation::Absent => out.label("cli.value.absent"),
        EnvObservation::Unknown => out.label("cli.value.unknown"),
    };
    out.field("cli.label.auth_env", env)
}

pub(super) async fn doctor(rt: &Runtime, out: &mut Output) -> Result {
    let findings = rt.service.doctor().await?;
    if findings.is_empty() {
        out.line(messages::text("switch.doctor.ok"))?;
    } else {
        let rows = findings
            .iter()
            .map(|f| {
                vec![
                    out.severity(f.severity),
                    out.render(&f.title),
                    f.detail
                        .as_ref()
                        .map(|d| out.render(d))
                        .unwrap_or_else(|| out.none()),
                    out.boolean(f.fixable),
                ]
            })
            .collect::<Vec<_>>();
        out.table(
            &[
                "cli.label.severity",
                "cli.label.title",
                "cli.label.detail",
                "cli.label.fixable",
            ],
            rows,
        )?;
    }
    out.value(&findings)?;
    Ok(exit::OK)
}

pub(super) async fn recover(rt: &Runtime, out: &mut Output) -> Result {
    let report = rt.service.recover().await?;
    let rows = report
        .entries
        .iter()
        .map(|entry| {
            vec![
                entry.journal_id.clone(),
                out.label(match entry.action {
                    JournalAction::Discarded => "cli.recovery.discarded",
                    JournalAction::Completed => "cli.recovery.completed",
                    JournalAction::RolledBack => "cli.recovery.rolled_back",
                    JournalAction::Drift => "cli.recovery.drift",
                }),
                entry
                    .files
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            ]
        })
        .collect::<Vec<_>>();
    out.table(
        &["cli.label.journal", "cli.label.action", "cli.label.files"],
        rows,
    )?;
    for n in &report.notices {
        out.notice(n)?;
    }
    out.value(&report)?;
    Ok(exit::OK)
}
