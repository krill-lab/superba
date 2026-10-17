//! Per-user macOS LaunchAgents, independent of the executing platform.

use std::path::PathBuf;

use superba_core::{Error, Result};

use super::system::{atomic_write, checked_command, path_text, remove_file, validate};
use super::{LAUNCHD_LABEL, ServiceHost, ServiceKind, ServiceManager, ServiceSpec, ServiceStatus};

/// Extra `launchctl bootstrap` attempts after an EIO failure.
const BOOTSTRAP_RETRIES: u32 = 2;

#[derive(Debug)]
pub(super) struct LaunchAgent {
    host: ServiceHost,
}

impl LaunchAgent {
    pub(super) fn new(host: ServiceHost) -> Self {
        Self { host }
    }

    fn path(&self) -> PathBuf {
        self.host
            .home
            .join("Library/LaunchAgents")
            .join(format!("{LAUNCHD_LABEL}.plist"))
    }

    fn domain(&self) -> Result<String> {
        self.host
            .uid
            .map(|uid| format!("gui/{uid}"))
            .ok_or_else(|| Error::Unsupported {
                what: "LaunchAgent requires the current user's uid".into(),
            })
    }

    fn target(&self) -> Result<String> {
        Ok(format!("{}/{LAUNCHD_LABEL}", self.domain()?))
    }
}

fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

impl ServiceManager for LaunchAgent {
    fn kind(&self) -> ServiceKind {
        ServiceKind::LaunchAgent
    }

    fn install(&self, spec: &ServiceSpec) -> Result<ServiceStatus> {
        let domain = self.domain()?;
        let target = self.target()?;
        validate(&self.host, spec)?;
        let mut plist = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n<dict>\n",
        );
        plist.push_str(&format!(
            "  <key>Label</key>\n  <string>{LAUNCHD_LABEL}</string>\n"
        ));
        plist.push_str("  <key>ProgramArguments</key>\n  <array>\n");
        for arg in [
            path_text(&spec.exe)?,
            "run",
            "--data-dir",
            path_text(&spec.data_dir)?,
        ] {
            plist.push_str(&format!("    <string>{}</string>\n", xml(arg)));
        }
        let log = xml(path_text(&spec.log_file)?);
        for line in [
            "  </array>".to_owned(),
            "  <key>RunAtLoad</key>".to_owned(),
            "  <true/>".to_owned(),
            "  <key>KeepAlive</key>".to_owned(),
            "  <true/>".to_owned(),
            // Interactive, not Background: the router is on the latency path of
            // every Codex request the user makes (launchd.plist(5) throttles
            // Background jobs' CPU, I/O and network).
            "  <key>ProcessType</key>".to_owned(),
            "  <string>Interactive</string>".to_owned(),
            "  <key>ThrottleInterval</key>".to_owned(),
            "  <integer>5</integer>".to_owned(),
            "  <key>StandardOutPath</key>".to_owned(),
            format!("  <string>{log}</string>"),
            "  <key>StandardErrorPath</key>".to_owned(),
            format!("  <string>{log}</string>"),
        ] {
            plist.push_str(&line);
            plist.push('\n');
        }
        if !spec.env.is_empty() {
            plist.push_str("  <key>EnvironmentVariables</key>\n  <dict>\n");
            for (key, value) in &spec.env {
                plist.push_str(&format!(
                    "    <key>{}</key>\n    <string>{}</string>\n",
                    xml(key),
                    xml(value)
                ));
            }
            plist.push_str("  </dict>\n");
        }
        plist.push_str("</dict>\n</plist>\n");
        atomic_write(&self.path(), &plist, 0o644)?;
        if let Some(parent) = spec.log_file.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, &e))?;
        }
        let _ = self
            .host
            .runner
            .run("launchctl", &["bootout".into(), target.clone().into()]);
        let _ = self
            .host
            .runner
            .run("launchctl", &["enable".into(), target.into()]);
        // Right after a bootout, launchd may still be tearing the old instance down
        // and bootstrap fails with EIO ("5: Input/output error"); retry briefly.
        let args = [
            "bootstrap".into(),
            domain.into(),
            self.path().into_os_string(),
        ];
        let mut attempt = 0;
        loop {
            match checked_command(&self.host, "launchctl", &args) {
                Ok(_) => break,
                Err(Error::Process { message })
                    if attempt < BOOTSTRAP_RETRIES && message.contains("Input/output error") =>
                {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(300));
                }
                Err(e) => return Err(e),
            }
        }
        self.status(spec)
    }

    fn uninstall(&self, spec: &ServiceSpec) -> Result<()> {
        let target = self.target()?;
        validate(&self.host, spec)?;
        let _ = self
            .host
            .runner
            .run("launchctl", &["bootout".into(), target.into()]);
        remove_file(&self.path())
    }

    fn status(&self, spec: &ServiceSpec) -> Result<ServiceStatus> {
        let target = self.target()?;
        validate(&self.host, spec)?;
        let mut status = ServiceStatus {
            kind: self.kind(),
            installed: self.path().exists(),
            running: None,
            pid: None,
            detail: None,
        };
        if let Ok(output) = self
            .host
            .runner
            .run("launchctl", &["print".into(), target.into()])
        {
            status.running = Some(false);
            if output.success() {
                for line in output.stdout.lines() {
                    if let Some((key, value)) = line.trim().split_once('=') {
                        match key.trim() {
                            "state" => {
                                status.running = Some(value.trim() == "running");
                                status.detail = Some(value.trim().into());
                            }
                            "pid" => status.pid = value.trim().parse().ok().filter(|pid| *pid != 0),
                            _ => {}
                        }
                    }
                }
            }
        }
        Ok(status)
    }

    fn restart(&self, spec: &ServiceSpec) -> Result<()> {
        let target = self.target()?;
        validate(&self.host, spec)?;
        checked_command(
            &self.host,
            "launchctl",
            &["kickstart".into(), "-k".into(), target.into()],
        )?;
        Ok(())
    }
}
