//! systemd user units, with all commands routed through the injected runner.

use std::ffi::OsString;
use std::path::PathBuf;

use superba_core::Result;

use super::system::{atomic_write, checked_command, path_text, remove_file, validate};
use super::{SYSTEMD_UNIT, ServiceHost, ServiceKind, ServiceManager, ServiceSpec, ServiceStatus};

#[derive(Debug)]
pub(super) struct SystemdUser {
    host: ServiceHost,
}

impl SystemdUser {
    pub(super) fn new(host: ServiceHost) -> Self {
        Self { host }
    }

    fn path(&self) -> PathBuf {
        self.host
            .config_home()
            .join("systemd/user")
            .join(SYSTEMD_UNIT)
    }

    fn command(&self, args: &[&str]) -> Result<()> {
        checked_command(&self.host, "systemctl", &argv(args)).map(|_| ())
    }
}

fn argv(args: &[&str]) -> Vec<OsString> {
    std::iter::once("--user")
        .chain(args.iter().copied())
        .map(Into::into)
        .collect()
}

fn quote(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
    )
}

impl ServiceManager for SystemdUser {
    fn kind(&self) -> ServiceKind {
        ServiceKind::SystemdUser
    }

    fn install(&self, spec: &ServiceSpec) -> Result<ServiceStatus> {
        validate(&self.host, spec)?;
        let command = [
            path_text(&spec.exe)?,
            "run",
            "--data-dir",
            path_text(&spec.data_dir)?,
        ]
        .map(quote)
        .join(" ");
        let mut unit = format!(
            "# Managed by Superba: the local router service. Removed when the router is disabled.\n\
            [Unit]\nDescription=Superba local router\nAfter=network.target\nStartLimitIntervalSec=0\n\n\
            [Service]\nType=simple\nExecStart={command}\nRestart=always\nRestartSec=2\nRestartPreventExitStatus=78\n"
        );
        for (key, value) in &spec.env {
            unit.push_str(&format!(
                "Environment={}\n",
                quote(&format!("{key}={value}"))
            ));
        }
        unit.push_str("\n[Install]\nWantedBy=default.target\n");
        atomic_write(&self.path(), &unit, 0o644)?;
        self.command(&["daemon-reload"])?;
        self.command(&["enable", SYSTEMD_UNIT])?;
        self.command(&["restart", SYSTEMD_UNIT])?;
        self.status(spec)
    }

    fn uninstall(&self, spec: &ServiceSpec) -> Result<()> {
        validate(&self.host, spec)?;
        let _ = self.command(&["disable", "--now", SYSTEMD_UNIT]);
        remove_file(&self.path())?;
        let _ = self.command(&["daemon-reload"]);
        let _ = self.command(&["reset-failed", SYSTEMD_UNIT]);
        Ok(())
    }

    fn status(&self, spec: &ServiceSpec) -> Result<ServiceStatus> {
        validate(&self.host, spec)?;
        let mut status = ServiceStatus {
            kind: self.kind(),
            installed: self.path().exists(),
            running: None,
            pid: None,
            detail: None,
        };
        if let Ok(output) = self.host.runner.run(
            "systemctl",
            &argv(&[
                "show",
                SYSTEMD_UNIT,
                "--property=LoadState,ActiveState,SubState,MainPID",
            ]),
        ) && output.success()
        {
            let mut active = "";
            let mut sub = "";
            for line in output.stdout.lines() {
                if let Some((key, value)) = line.split_once('=') {
                    match key {
                        "ActiveState" => active = value,
                        "SubState" => sub = value,
                        "MainPID" => status.pid = value.parse().ok().filter(|p| *p != 0),
                        _ => {}
                    }
                }
            }
            status.running = Some(active == "active");
            status.detail = Some(format!("{active}/{sub}"));
        }
        Ok(status)
    }

    fn restart(&self, spec: &ServiceSpec) -> Result<()> {
        validate(&self.host, spec)?;
        self.command(&["restart", SYSTEMD_UNIT])
    }
}

pub(super) fn user_manager_available(host: &ServiceHost) -> bool {
    host.runner
        .run("systemctl", &argv(&["show-environment"]))
        .is_ok_and(|output| output.success())
}
