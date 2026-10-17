//! `spb codex`, `spb app` and `spb feeds`: Codex releases and the verified
//! store, which Codex the desktop app runs (`CODEX_CLI_PATH` pin, always a full
//! app restart), the local CLI binaries by install kind, the newest desktop app
//! build and its download (verified; the install command is shown, never run),
//! and the signed feeds Superba reads model lists, prices and versions from.

use std::path::PathBuf;

use superba_core::{
    AppCodexPin, AppUpdateAction, CliUpdateAction, CodexVersion, InstallRequest, InstallTarget,
    SwitchIntent, VersionsView,
};

use super::{Result, execute, request, usage};
use crate::{AppCommand, CodexCommand, FeedsCommand, Output, Runtime, exit, text::text};

fn version(out: &Output, value: &str) -> Result<CodexVersion> {
    CodexVersion::parse(value)
        .map_err(|_| usage(out, text("cli.versions.bad_version").arg("value", value)))
}

pub(super) async fn codex(command: &CodexCommand, rt: &Runtime, out: &mut Output) -> Result {
    match command {
        CodexCommand::List { all, refresh } => {
            let view = rt.service.versions(*refresh).await?;
            render_codex(&view, *all, out)?;
            out.value(&view)?;
            Ok(exit::OK)
        }
        CodexCommand::Install { version: v, local } => {
            let v = version(out, v)?;
            out.progress(text("cli.versions.installing").arg("version", &v))?;
            let report = rt
                .service
                .install_codex(InstallRequest {
                    version: v,
                    target: if *local {
                        InstallTarget::Local
                    } else {
                        InstallTarget::Store
                    },
                })
                .await?;
            out.line(
                text("cli.versions.installed")
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
        CodexCommand::Rollback => {
            out.progress(text("cli.versions.rolling_back"))?;
            let report = rt.service.rollback_codex(InstallTarget::Local).await?;
            out.line(
                text("cli.versions.rolled_back")
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
        CodexCommand::Pin {
            target,
            persist_env,
            exec,
        } => {
            let pin = if target == "bundled" {
                AppCodexPin::Bundled
            } else if let Ok(v) = CodexVersion::parse(target)
                && !target.contains('/')
                && !target.contains('\\')
            {
                AppCodexPin::Version { version: v }
            } else {
                let path = PathBuf::from(target);
                let path = if path.is_absolute() {
                    path
                } else {
                    std::env::current_dir()
                        .map(|d| d.join(&path))
                        .unwrap_or(path)
                };
                AppCodexPin::Path { path }
            };
            pin_to(pin, *persist_env, exec, rt, out).await
        }
        CodexCommand::Unpin { exec } => pin_to(AppCodexPin::Bundled, false, exec, rt, out).await,
    }
}

async fn pin_to(
    pin: AppCodexPin,
    persist_env: bool,
    exec: &crate::ExecArgs,
    rt: &Runtime,
    out: &mut Output,
) -> Result {
    if persist_env && !exec.dry_run {
        let mut settings = rt.service.profile_set().await?.settings;
        if !settings.app_codex.persist_env {
            settings.app_codex.persist_env = true;
            rt.service.save_settings(settings).await?;
        }
    }
    out.line(text("cli.versions.pin_restart"))?;
    execute::run(
        request(SwitchIntent::AppCodexPin { pin }, exec),
        exec,
        rt,
        out,
    )
    .await
}

fn pin_text(pin: &AppCodexPin) -> superba_core::LocalizedText {
    match pin {
        AppCodexPin::Version { version } => {
            text("cli.versions.pin.version").arg("version", version)
        }
        AppCodexPin::Path { path } => text("cli.versions.pin.path").arg("path", path.display()),
        _ => text("cli.versions.pin.bundled"),
    }
}

fn render_codex(view: &VersionsView, all: bool, out: &mut Output) -> superba_core::Result<()> {
    let ac = &view.app_codex;
    if ac.supported {
        out.field("cli.versions.app_pin", out.render(&pin_text(&ac.pin)))?;
    } else {
        out.line(text("cli.versions.pin_unsupported"))?;
    }
    out.field(
        "cli.versions.app_runs",
        match (&ac.effective_version, &ac.effective_exe) {
            (Some(v), _) => v.to_string(),
            (None, Some(p)) => p.display().to_string(),
            _ => out.none(),
        },
    )?;
    out.field("cli.versions.floor", out.optional(ac.floor.as_ref()))?;
    if let Some(m) = &ac.mismatch {
        // The sentence names both sides briefly; the full paths follow.
        out.line(m.clone())?;
        if let Some(p) = &ac.effective_exe {
            out.field("cli.versions.running_exe", p.display())?;
        }
        if let Some(p) = view.expected_codex() {
            out.field("cli.versions.expected_exe", p.display())?;
        }
    }
    let candidates = ac
        .candidates
        .iter()
        .filter(|c| all || !c.release.prerelease || c.stored)
        .take(if all { usize::MAX } else { 10 })
        .map(|c| {
            vec![
                c.release.version.to_string(),
                out.render(&c.tier.text()),
                out.boolean(c.release.prerelease),
                out.boolean(c.stored),
            ]
        })
        .collect::<Vec<_>>();
    out.section_table(
        "cli.versions.releases",
        &[
            "cli.versions.col.version",
            "cli.versions.col.fit",
            "cli.versions.col.prerelease",
            "cli.versions.col.stored",
        ],
        candidates,
    )?;
    let cli = view
        .cli
        .iter()
        .map(|r| {
            vec![
                r.binary.path.display().to_string(),
                out.optional(r.binary.version.as_ref()),
                out.render(&r.kind.text()),
                out.optional(r.latest.as_ref()),
                match &r.action {
                    CliUpdateAction::Install { version } => {
                        out.render(&text("cli.versions.action.install").arg("version", version))
                    }
                    CliUpdateAction::Command { command } => command.clone(),
                    _ => out.none(),
                },
            ]
        })
        .collect::<Vec<_>>();
    out.section_table(
        "cli.versions.cli",
        &[
            "cli.versions.col.path",
            "cli.versions.col.version",
            "cli.versions.col.kind",
            "cli.versions.col.latest",
            "cli.versions.col.action",
        ],
        cli,
    )?;
    let remote = view
        .remote
        .iter()
        .map(|r| {
            vec![
                r.label.clone(),
                out.optional(r.version.as_ref()),
                r.install_kind
                    .map(|k| out.render(&k.text()))
                    .unwrap_or_else(|| out.none()),
                r.tier
                    .map(|t| out.render(&t.text()))
                    .unwrap_or_else(|| out.none()),
            ]
        })
        .collect::<Vec<_>>();
    out.section_table(
        "cli.versions.remote",
        &[
            "cli.label.name",
            "cli.versions.col.version",
            "cli.versions.col.kind",
            "cli.versions.col.fit",
        ],
        remote,
    )?;
    for n in &view.notes {
        out.notice(n)?;
    }
    Ok(())
}

pub(super) async fn app(command: &AppCommand, rt: &Runtime, out: &mut Output) -> Result {
    match command {
        AppCommand::Check { refresh } => {
            let view = rt.service.versions(*refresh).await?;
            let app = &view.app;
            out.field(
                "cli.versions.installed_app",
                app.install
                    .as_ref()
                    .and_then(|i| i.app_version.clone())
                    .unwrap_or_else(|| out.none()),
            )?;
            out.field(
                "cli.versions.bundled_codex",
                out.optional(
                    app.install
                        .as_ref()
                        .and_then(|i| i.bundled_codex_version.as_ref()),
                ),
            )?;
            out.field(
                "cli.versions.latest_app",
                app.latest
                    .as_ref()
                    .map(|l| l.version.clone())
                    .unwrap_or_else(|| out.none()),
            )?;
            out.field(
                "cli.versions.latest_bundled",
                out.optional(app.latest.as_ref().and_then(|l| l.bundled_codex.as_ref())),
            )?;
            out.line(text(if app.update_available {
                "cli.versions.update_available"
            } else {
                "cli.versions.up_to_date"
            }))?;
            match &app.action {
                Some(AppUpdateAction::LinuxPackage { command, .. }) => {
                    out.line(text("cli.versions.linux_action"))?;
                    if !command.is_empty() {
                        out.data_line(command)?;
                    }
                }
                Some(AppUpdateAction::MacSparkle { .. }) => {
                    out.line(text("cli.versions.mac_action"))?;
                }
                Some(AppUpdateAction::WindowsStore { store_url, command }) => {
                    out.line(text("cli.versions.windows_action").arg("url", store_url))?;
                    out.data_line(command)?;
                }
                _ => {}
            }
            for n in &view.notes {
                out.notice(n)?;
            }
            out.value(&view.app)?;
            Ok(exit::OK)
        }
        AppCommand::Download { open } => {
            out.progress(text("cli.versions.downloading"))?;
            let report = rt.service.download_app_update().await?;
            out.line(
                text("cli.versions.downloaded")
                    .arg("path", report.path.display())
                    .arg("sha256", &report.sha256),
            )?;
            if let Some(c) = &report.command {
                out.line(text("cli.versions.install_with"))?;
                out.data_line(c)?;
            }
            if *open {
                open_installer(&report.path, out)?;
            }
            out.value(&report)?;
            Ok(exit::OK)
        }
    }
}

/// Open the package with the desktop's handler (the system installer); never
/// with elevated rights.
fn open_installer(path: &std::path::Path, out: &mut Output) -> superba_core::Result<()> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    match superba_core::process::command(program).arg(path).spawn() {
        Ok(_) => out.line(text("cli.versions.opened")),
        Err(_) => out.line(text("cli.versions.open_failed")),
    }
}

pub(super) async fn feeds(command: &FeedsCommand, rt: &Runtime, out: &mut Output) -> Result {
    let status = match command {
        FeedsCommand::Status => rt.service.feeds_status().await?,
        FeedsCommand::Refresh => {
            out.progress(text("cli.feeds.refreshing"))?;
            rt.service.refresh_feeds().await?
        }
    };
    out.field("cli.feeds.serial", status.serial)?;
    out.field("cli.feeds.builtin_serial", status.builtin_serial)?;
    out.field(
        "cli.feeds.origin",
        match &status.source {
            superba_core::FeedOrigin::Mirror { name } => {
                out.render(&text("cli.feeds.origin_mirror").arg("name", name))
            }
            superba_core::FeedOrigin::Cache => out.label("cli.feeds.origin_cache"),
            _ => out.label("cli.feeds.origin_builtin"),
        },
    )?;
    out.field(
        "cli.feeds.checked",
        status
            .checked_at
            .map(|t| t.to_rfc3339())
            .unwrap_or_else(|| out.none()),
    )?;
    out.field("cli.feeds.online", out.boolean(status.online_checks))?;
    if let Some(e) = &status.last_error {
        out.field("cli.feeds.error", out.render(&e.to_text()))?;
    }
    out.value(&status)?;
    Ok(exit::OK)
}
