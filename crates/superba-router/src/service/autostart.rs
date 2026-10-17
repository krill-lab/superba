//! XDG autostart entries and shared detached-supervisor lifecycle operations.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use superba_core::{Error, Result};

use super::system::{atomic_write, path_text, process_error, remove_file, validate};
use super::{AUTOSTART_FILE, ServiceHost, ServiceKind, ServiceManager, ServiceSpec, ServiceStatus};

#[derive(Debug)]
pub(super) struct XdgAutostart {
    host: ServiceHost,
}

impl XdgAutostart {
    pub(super) fn new(host: ServiceHost) -> Self {
        Self { host }
    }

    fn path(&self) -> PathBuf {
        self.host
            .config_home()
            .join("autostart")
            .join(AUTOSTART_FILE)
    }
}

fn quote(value: &str) -> String {
    let quoted = value.is_empty()
        || value.contains([
            ' ', '\t', '"', '\'', '\\', '>', '<', '~', '|', '&', ';', '$', '*', '?', '#', '(', ')',
            '`',
        ]);
    let mut output = String::new();
    if quoted {
        output.push('"');
    }
    for ch in value.chars() {
        if quoted && matches!(ch, '"' | '`' | '$' | '\\') {
            output.push('\\');
        }
        output.push(ch);
        if ch == '%' {
            output.push('%');
        }
    }
    if quoted {
        output.push('"');
    }
    output.replace('\\', "\\\\").replace('\t', "\\t")
}

impl ServiceManager for XdgAutostart {
    fn kind(&self) -> ServiceKind {
        ServiceKind::XdgAutostart
    }

    fn install(&self, spec: &ServiceSpec) -> Result<ServiceStatus> {
        validate(&self.host, spec)?;
        let mut args = Vec::new();
        if !spec.env.is_empty() {
            args.push("env".to_owned());
            args.extend(spec.env.iter().map(|(k, v)| quote(&format!("{k}={v}"))));
        }
        args.extend(
            [
                path_text(&spec.exe)?,
                "supervise",
                "--data-dir",
                path_text(&spec.data_dir)?,
            ]
            .map(quote),
        );
        let entry = format!(
            "[Desktop Entry]\nType=Application\nName=Superba local router\n\
            Comment=Managed by Superba: the local router service\nExec={}\nTerminal=false\n\
            NoDisplay=true\nX-GNOME-Autostart-enabled=true\n",
            args.join(" ")
        );
        atomic_write(&self.path(), &entry, 0o644)?;
        stop_supervisor(&self.host, spec)?;
        spawn_supervisor(&self.host, spec)?;
        self.status(spec)
    }

    fn uninstall(&self, spec: &ServiceSpec) -> Result<()> {
        validate(&self.host, spec)?;
        remove_file(&self.path())?;
        stop_supervisor(&self.host, spec)
    }

    fn status(&self, spec: &ServiceSpec) -> Result<ServiceStatus> {
        validate(&self.host, spec)?;
        supervisor_status(&self.host, spec, self.kind(), self.path().exists())
    }

    fn restart(&self, spec: &ServiceSpec) -> Result<()> {
        validate(&self.host, spec)?;
        stop_supervisor(&self.host, spec)?;
        spawn_supervisor(&self.host, spec)
    }
}

pub(super) fn read_pid(path: &Path) -> Result<Option<u32>> {
    match fs::read(path) {
        Ok(value) => Ok(std::str::from_utf8(&value)
            .ok()
            .and_then(|value| value.trim().parse().ok())
            .filter(|pid| *pid != 0)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(path, &e)),
    }
}

pub(super) fn stop_supervisor(host: &ServiceHost, spec: &ServiceSpec) -> Result<()> {
    if let Some(pid) = read_pid(&spec.pid_file)?
        && host.runner.is_router_process(pid)
    {
        // A concurrent exit may make termination fail; still discard the stale pid file.
        let _ = host.runner.terminate(pid);
        let deadline = Instant::now() + Duration::from_secs(5);
        while host.runner.is_router_process(pid) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100).min(remaining));
        }
    }
    remove_file(&spec.pid_file)
}

pub(super) fn spawn_supervisor(host: &ServiceHost, spec: &ServiceSpec) -> Result<()> {
    let pid = host
        .runner
        .spawn_detached(&spec.exe, &spec.supervise_args(), &spec.env, &spec.log_file)
        .map_err(|e| process_error(&spec.exe.to_string_lossy(), &e.to_string()))?;
    atomic_write(&spec.pid_file, &format!("{pid}\n"), 0o600)
}

pub(super) fn supervisor_status(
    host: &ServiceHost,
    spec: &ServiceSpec,
    kind: ServiceKind,
    installed: bool,
) -> Result<ServiceStatus> {
    let pid = read_pid(&spec.pid_file)?;
    Ok(ServiceStatus {
        kind,
        installed,
        running: Some(pid.is_some_and(|pid| host.runner.is_router_process(pid))),
        pid,
        detail: None,
    })
}
