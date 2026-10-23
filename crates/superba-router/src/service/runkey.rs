//! Windows per-user login startup plus the detached self-supervisor.
//!
//! Run values cannot carry an environment. `ServiceSpec::env` is passed only to
//! supervisors started immediately by install or restart, not at the next login.

use std::path::Path;

use superba_core::{Error, Result};

use super::autostart::{spawn_supervisor, stop_supervisor, supervisor_status};
use super::system::{path_text, validate};
use super::{
    RUN_VALUE_NAME, RunKeyStore, ServiceHost, ServiceKind, ServiceManager, ServiceSpec,
    ServiceStatus,
};

#[derive(Debug)]
pub(super) struct WindowsRunKeyService {
    host: ServiceHost,
}

impl WindowsRunKeyService {
    pub(super) fn new(host: ServiceHost) -> Self {
        Self { host }
    }

    fn store(&self) -> Result<&dyn RunKeyStore> {
        self.host
            .run_key
            .as_deref()
            .ok_or_else(|| Error::Unsupported {
                what: "Windows login startup requires a per-user Run key store".into(),
            })
    }
}

/// Remove the login-startup entry when it starts the `superba-router` in `dir`
/// (the Windows installer removing that install); an entry that starts another
/// copy is left alone. Registry only, no file is written: it runs even where
/// the full router removal is refused (an uninstaller running as another
/// account than the signed-in user).
/// Returns whether an entry was removed.
pub fn remove_run_entry_of(store: &dyn RunKeyStore, dir: &Path) -> std::io::Result<bool> {
    let Some(command) = store.get(RUN_VALUE_NAME)? else {
        return Ok(false);
    };
    if !starts_router_in(&command, dir) {
        return Ok(false);
    }
    store.delete(RUN_VALUE_NAME)?;
    Ok(true)
}

/// The program of a Run command (quoted by [`quote`], or bare) is
/// `superba-router[.exe]` in `dir`.
fn starts_router_in(command: &str, dir: &Path) -> bool {
    let command = command.trim_start();
    let program = match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next(),
        None => command.split_whitespace().next(),
    };
    let program = Path::new(program.unwrap_or_default());
    let is_router = program
        .file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("superba-router"));
    let canonical = |p: &Path| superba_core::paths::canonical(p).unwrap_or_else(|_| p.to_owned());
    is_router
        && program.parent().is_some_and(|parent| {
            let (a, b) = (canonical(parent), canonical(dir));
            if cfg!(windows) {
                a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
            } else {
                a == b
            }
        })
}

// CommandLineToArgvW: double each run of backslashes before a quote or the end.
fn quote(value: &str) -> String {
    if !value.is_empty() && !value.contains([' ', '\t', '"']) {
        return value.to_owned();
    }
    let mut output = String::from("\"");
    let mut slashes = 0;
    for ch in value.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        output.extend(std::iter::repeat_n(
            '\\',
            if ch == '"' { slashes * 2 + 1 } else { slashes },
        ));
        output.push(ch);
        slashes = 0;
    }
    output.extend(std::iter::repeat_n('\\', slashes * 2));
    output.push('"');
    output
}

fn registry_error(error: std::io::Error) -> Error {
    Error::io(
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run\SuperbaRouter",
        &error,
    )
}

impl ServiceManager for WindowsRunKeyService {
    fn kind(&self) -> ServiceKind {
        ServiceKind::WindowsRunKey
    }

    fn install(&self, spec: &ServiceSpec) -> Result<ServiceStatus> {
        let store = self.store()?;
        validate(&self.host, spec)?;
        let command = [
            path_text(&spec.exe)?,
            "supervise",
            "--data-dir",
            path_text(&spec.data_dir)?,
        ]
        .map(quote)
        .join(" ");
        store
            .set(RUN_VALUE_NAME, &command)
            .map_err(registry_error)?;
        stop_supervisor(&self.host, spec)?;
        spawn_supervisor(&self.host, spec)?;
        self.status(spec)
    }

    fn uninstall(&self, spec: &ServiceSpec) -> Result<()> {
        let store = self.store()?;
        validate(&self.host, spec)?;
        store.delete(RUN_VALUE_NAME).map_err(registry_error)?;
        stop_supervisor(&self.host, spec)
    }

    fn status(&self, spec: &ServiceSpec) -> Result<ServiceStatus> {
        let store = self.store()?;
        validate(&self.host, spec)?;
        let installed = store.get(RUN_VALUE_NAME).map_err(registry_error)?.is_some();
        supervisor_status(&self.host, spec, self.kind(), installed)
    }

    fn restart(&self, spec: &ServiceSpec) -> Result<()> {
        self.store()?;
        validate(&self.host, spec)?;
        stop_supervisor(&self.host, spec)?;
        spawn_supervisor(&self.host, spec)
    }
}
