//! Windows per-user login startup plus the detached self-supervisor.
//!
//! Run values cannot carry an environment. `ServiceSpec::env` is passed only to
//! supervisors started immediately by install or restart, not at the next login.

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
