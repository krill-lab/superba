//! Use the Updates service's refreshed inventory and self-update channel.

use super::{check, record, text};
use crate::Switcher;
use superba_core::*;

fn version_result(
    current: Option<&str>,
    latest: Option<&str>,
    available: bool,
) -> (TroubleshootingStatus, LocalizedText) {
    match (current, latest) {
        (Some(current), Some(latest)) => (
            if available {
                TroubleshootingStatus::Warning
            } else {
                TroubleshootingStatus::Passed
            },
            text(
                if available {
                    "version_available"
                } else {
                    "version_current"
                },
                if available {
                    "Installed {current}; update {latest} is available on the Updates page."
                } else {
                    "Installed {current}; up to date with the update source ({latest})."
                },
            )
            .arg("current", current)
            .arg("latest", latest),
        ),
        _ => (
            TroubleshootingStatus::Unknown,
            text(
                "version_unknown",
                "The Updates service could not determine the installed or latest version.",
            ),
        ),
    }
}

impl Switcher {
    pub(super) async fn troubleshooting_versions(
        &self,
        report: &mut TroubleshootingReport,
        progress: &TroubleshootingProgressSink,
    ) -> Result<()> {
        let app_title = text("check.app_version", "Codex desktop version");
        let cli_title = text("check.cli_version", "Codex CLI version");
        match self.versions_impl(true).await {
            Ok(view) => {
                let vendor_cli_only = view.app.install.is_some()
                    && !view.cli.is_empty()
                    && view.cli.iter().all(|row| !row.managed_by_superba());
                let (status, detail) = version_result(
                    view.app
                        .install
                        .as_ref()
                        .and_then(|i| i.app_version.as_deref()),
                    view.app.latest.as_ref().map(|r| r.version.as_str()),
                    view.app.update_available,
                );
                record(
                    report,
                    progress,
                    check("update:app", app_title, status, detail),
                )?;
                if view.cli.is_empty() {
                    record(
                        report,
                        progress,
                        check(
                            "update:cli",
                            cli_title.clone(),
                            TroubleshootingStatus::Unknown,
                            text(
                                "cli_not_installed",
                                "No Codex CLI installation was found by the Updates service.",
                            ),
                        ),
                    )?;
                }
                for (index, row) in view.cli.iter().enumerate() {
                    if view.app.install.is_some() && !row.managed_by_superba() {
                        continue;
                    }
                    let current = row.binary.version.as_ref().map(ToString::to_string);
                    let latest = row.latest.as_ref().map(ToString::to_string);
                    let (status, detail) = version_result(
                        current.as_deref(),
                        latest.as_deref(),
                        row.binary
                            .version
                            .as_ref()
                            .zip(row.latest.as_ref())
                            .is_some_and(|(current, latest)| current < latest),
                    );
                    record(
                        report,
                        progress,
                        check(
                            format!("update:cli:{index}"),
                            cli_title.clone(),
                            status,
                            text("cli_version_detail", "{path} · {result}")
                                .arg("path", paths::display_path(&row.binary.path))
                                .arg_text("result", detail),
                        ),
                    )?;
                }
                for (index, note) in view.notes.into_iter().enumerate() {
                    if vendor_cli_only
                        && note.text.key == "switch.versions.check_failed"
                        && note
                            .text
                            .args
                            .iter()
                            .any(|(key, value)| key == "what" && value == "codex")
                    {
                        continue;
                    }
                    record(
                        report,
                        progress,
                        check(
                            format!("update:notice:{index}"),
                            text("check.version_source", "Version update source"),
                            if note.text.key == "switch.versions.check_failed" {
                                TroubleshootingStatus::Unknown
                            } else {
                                TroubleshootingStatus::Warning
                            },
                            note.text,
                        ),
                    )?;
                }
            }
            Err(error) => {
                for (id, title) in [("update:app", app_title), ("update:cli", cli_title)] {
                    record(
                        report,
                        progress,
                        check(id, title, TroubleshootingStatus::Unknown, error.to_text()),
                    )?;
                }
            }
        }
        self.troubleshooting_self_version(report, progress).await
    }

    async fn troubleshooting_self_version(
        &self,
        report: &mut TroubleshootingReport,
        progress: &TroubleshootingProgressSink,
    ) -> Result<()> {
        use TroubleshootingStatus as Status;
        let manual = self
            .deps
            .profiles
            .load()
            .await?
            .settings
            .network
            .online_checks;
        let (status, detail) = match self
            .self_update_check_impl(SelfUpdateCheckRequest { manual })
            .await
        {
            Ok(view) => match view.status {
                SelfUpdateStatus::Available | SelfUpdateStatus::UpToDate => version_result(
                    Some(&view.current),
                    view.latest.as_ref().map(|r| r.version.as_str()),
                    view.status == SelfUpdateStatus::Available,
                ),
                SelfUpdateStatus::CheckFailed => (
                    Status::Unknown,
                    text("version_check_failed", "Version check failed: {reason}")
                        .arg("reason", view.detail.unwrap_or_default()),
                ),
                SelfUpdateStatus::OnlineChecksOff => (
                    Status::Unknown,
                    text(
                        "version_checks_off",
                        "Online version checks are disabled in network settings.",
                    ),
                ),
                SelfUpdateStatus::PageUnavailable => (
                    Status::Unknown,
                    text(
                        "version_page_unavailable",
                        "The release source is currently unavailable.",
                    ),
                ),
                _ => version_result(None, None, false),
            },
            Err(error) => (Status::Unknown, error.to_text()),
        };
        record(
            report,
            progress,
            check(
                "update:superba",
                text("check.superba_version", "Superba version"),
                status,
                detail,
            ),
        )
    }
}
