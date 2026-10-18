use superba_core::{AppSession, EnvObservation, HotSwitchStatus, JournalAction};
use superba_switch::messages;

use super::{Result, remote};
use crate::{
    Output, Runtime, exit,
    text::{line_label, text},
};

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
    if let Some(x) = &overview.external {
        external(x, out)?;
    }
    out.value(&overview)?;
    Ok(exit::OK)
}

/// GH#5: what changed outside Superba in what it manages, who most likely made
/// the change, and the two ways forward.
pub(super) fn external(
    x: &superba_core::ExternalChange,
    out: &mut Output,
) -> superba_core::Result<()> {
    out.line(text("cli.external.title").arg_text("source", x.source.text()))?;
    for k in &x.keys {
        out.line(
            text("cli.format.change")
                .arg("path", &k.path)
                .arg("before", out.optional(k.before.as_ref()))
                .arg("after", out.optional(k.after.as_ref())),
        )?;
    }
    if let Some(a) = &x.auth {
        out.line(
            text("cli.format.change")
                .arg("path", superba_core::AUTH_JSON_PATH)
                .arg(
                    "before",
                    out.render(&superba_core::auth_shape_label(&a.written)),
                )
                .arg(
                    "after",
                    out.render(&superba_core::auth_shape_label(&a.live)),
                ),
        )?;
    }
    for e in &x.env {
        out.line(
            text("cli.format.change")
                .arg("path", format!("{} ({})", e.var, env_location(&e.location)))
                .arg("before", out.optional(e.written.as_ref()))
                .arg("after", out.optional(e.live.as_ref())),
        )?;
    }
    for ev in &x.evidence {
        out.line(text("cli.external.evidence").arg_text("text", ev.clone()))?;
    }
    out.line(text("cli.external.next").arg("binary", out.binary))
}

fn env_location(location: &superba_core::EnvLocation) -> String {
    match location {
        superba_core::EnvLocation::ShellRc { path, .. }
        | superba_core::EnvLocation::EnvironmentD { path }
        | superba_core::EnvLocation::LaunchAgent { path } => {
            superba_core::paths::display_path(path)
        }
        superba_core::EnvLocation::Launchctl => "launchctl".to_owned(),
        superba_core::EnvLocation::WindowsUserEnv => r"HKCU\Environment".to_owned(),
        other => format!("{other:?}"),
    }
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
