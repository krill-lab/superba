//! User login startup for the GUI. The router owns a different registration.
//!
//! The OS entry is the source of truth: removing it in System Settings or an
//! uninstaller must never cause a saved preference to recreate it at launch.

use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use superba_core::{Error, LocalContext, Result, autostart::AutostartStatus};

use crate::fsx;

#[cfg(windows)]
mod windows;

pub use superba_core::autostart::{RUN_KEY, RUN_VALUE};
/// Linux autostart desktop file.
pub const DESKTOP_FILE: &str = "superba.desktop";
/// macOS LaunchAgent label.
pub const AGENT_LABEL: &str = "com.krill-code.superba.autostart";
/// Bundle identifier used by the release packaging.
pub const BUNDLE_ID: &str = "com.krill-code.superba";

/// Injectable login startup operations. Ordinary tests never change HKCU.
pub trait LoginStartup: Debug + Send + Sync {
    /// Read the actual OS registration.
    fn status(&self) -> Result<AutostartStatus>;
    /// Enable with the current installation's GUI, or remove the registration.
    fn set_enabled(&self, enabled: bool) -> Result<AutostartStatus>;
    /// Refresh an existing enabled entry after a move/update; never opt in.
    fn refresh(&self) -> Result<()>;
}

/// Registration format; rendering all formats is testable on every platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    /// HKCU Run.
    Windows,
    /// User LaunchAgent.
    Mac,
    /// XDG autostart.
    Linux,
}

impl Platform {
    /// Current build target.
    pub fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Mac
        } else {
            Self::Linux
        }
    }
}

/// Storage for the single registration owned by the GUI.
pub trait EntryStore: Debug + Send + Sync {
    /// Description displayed by `spb autostart status`.
    fn location(&self) -> String;
    /// Read the registration, if present.
    fn read(&self) -> Result<Option<String>>;
    /// Replace or remove the registration.
    fn write(&self, entry: Option<&str>) -> Result<()>;
}

/// Desktop/plist file, rooted in the injected user context.
#[derive(Debug)]
pub struct FileEntry(pub PathBuf);
impl EntryStore for FileEntry {
    fn location(&self) -> String {
        self.0.display().to_string()
    }
    fn read(&self) -> Result<Option<String>> {
        match std::fs::read_to_string(&self.0) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::io(&self.0, &e)),
        }
    }
    fn write(&self, entry: Option<&str>) -> Result<()> {
        match entry {
            Some(s) => fsx::write_atomic(&self.0, s.as_bytes(), fsx::Mode::PreserveOrPublic),
            None => fsx::remove_if_exists(&self.0),
        }
    }
}

/// A LaunchAgent file plus removal from this login session. Injecting the
/// command keeps tests away from the user's launchd domain.
#[derive(Debug)]
pub struct LaunchAgentEntry {
    /// Plist in the current user's Library/LaunchAgents.
    pub file: FileEntry,
    /// Current login user's id.
    pub uid: u32,
    /// Native `/bin/launchctl`, or a test command.
    pub launchctl: PathBuf,
}
impl EntryStore for LaunchAgentEntry {
    fn location(&self) -> String {
        self.file.location()
    }
    fn read(&self) -> Result<Option<String>> {
        self.file.read()
    }
    fn write(&self, entry: Option<&str>) -> Result<()> {
        self.file.write(entry)?;
        if entry.is_none() {
            // An absent/not-loaded job is already disabled. Bootout errors
            // must not undo successful removal of the next-login entry.
            let _ = superba_core::process::command(&self.launchctl)
                .args(["bootout", &format!("gui/{}/{AGENT_LABEL}", self.uid)])
                .output();
        }
        Ok(())
    }
}

/// In-memory registration for service tests on every platform.
#[derive(Debug, Default)]
pub struct MemoryEntry(pub Mutex<Option<String>>);
impl EntryStore for MemoryEntry {
    fn location(&self) -> String {
        "memory:Superba".into()
    }
    fn read(&self) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap_or_else(|p| p.into_inner()).clone())
    }
    fn write(&self, entry: Option<&str>) -> Result<()> {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = entry.map(str::to_owned);
        Ok(())
    }
}

/// Platform startup manager; an executable is resolved lazily, so status/off
/// also work after the GUI binary was deleted.
#[derive(Debug)]
pub struct Manager {
    platform: Platform,
    store: Arc<dyn EntryStore>,
    executable: Result<PathBuf>,
}
impl Manager {
    /// Inject storage, platform and the GUI executable (tests/packagers).
    pub fn with_store(
        platform: Platform,
        store: Arc<dyn EntryStore>,
        executable: Result<PathBuf>,
    ) -> Self {
        Self {
            platform,
            store,
            executable,
        }
    }
    /// Native registration for this context. Uses the current installation,
    /// never a hard-coded Windows install directory or a PATH lookup.
    pub fn new(ctx: &LocalContext) -> Self {
        let platform = Platform::current();
        #[cfg(windows)]
        let store: Arc<dyn EntryStore> = Arc::new(windows::RunEntry);
        #[cfg(all(not(windows), not(target_os = "macos")))]
        let store: Arc<dyn EntryStore> = Arc::new(FileEntry(entry_path(ctx, platform)));
        #[cfg(target_os = "macos")]
        let store: Arc<dyn EntryStore> = Arc::new(LaunchAgentEntry {
            file: FileEntry(entry_path(ctx, platform)),
            // SAFETY: getuid has no pointer arguments or preconditions.
            uid: unsafe { libc::getuid() },
            launchctl: PathBuf::from("/bin/launchctl"),
        });
        let executable = std::env::current_exe()
            .map_err(|e| Error::other(e.to_string()))
            .and_then(|current| gui_executable(&current, ctx));
        Self::with_store(platform, store, executable)
    }
    fn rendered(&self) -> Result<String> {
        let exe = self
            .executable
            .as_ref()
            .map_err(|e| Error::other(e.to_string()))?;
        if !exe.is_absolute() || !exe.is_file() {
            return Err(Error::invalid(format!(
                "Superba GUI executable not found: {}",
                exe.display()
            )));
        }
        render(self.platform, exe)
    }
}
impl LoginStartup for Manager {
    fn status(&self) -> Result<AutostartStatus> {
        let command = self.store.read()?;
        let enabled = command.as_ref().is_some_and(|s| {
            !s.trim().is_empty()
                && (self.platform != Platform::Linux
                    || !s.lines().any(|l| {
                        matches!(l.trim(), "Hidden=true" | "X-GNOME-Autostart-enabled=false")
                    }))
        });
        Ok(AutostartStatus {
            enabled,
            entry: self.store.location(),
            command,
        })
    }
    fn set_enabled(&self, enabled: bool) -> Result<AutostartStatus> {
        let entry = enabled.then(|| self.rendered()).transpose()?;
        self.store.write(entry.as_deref())?;
        self.status()
    }
    fn refresh(&self) -> Result<()> {
        let status = self.status()?;
        if status.enabled {
            let rendered = self.rendered()?;
            if status.command.as_deref() != Some(&rendered) {
                self.store.write(Some(&rendered))?;
            }
        }
        Ok(())
    }
}

/// Current-user desktop/plist path. Relative XDG_CONFIG_HOME is ignored, as
/// required by the XDG base directory specification.
pub fn entry_path(ctx: &LocalContext, platform: Platform) -> PathBuf {
    match platform {
        Platform::Mac => ctx
            .home_dir
            .join("Library/LaunchAgents")
            .join(format!("{AGENT_LABEL}.plist")),
        _ => ctx
            .env
            .var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| ctx.home_dir.join(".config"))
            .join("autostart")
            .join(DESKTOP_FILE),
    }
}

fn gui_executable(current: &Path, ctx: &LocalContext) -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        crate::installation::macos_executable(
            current,
            ctx,
            crate::installation::Executable::Gui,
            crate::installation::installed_macos_bundle,
        )
        .ok_or_else(|| {
            Error::invalid(
                "Superba GUI executable not found; install Superba.app or set SUPERBA_GUI_EXE",
            )
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        if cfg!(target_os = "linux")
            && let Some(appimage) = ctx.env.var_os("APPIMAGE").map(PathBuf::from)
            && appimage.is_absolute()
            && appimage.is_file()
        {
            return Ok(appimage);
        }
        let dir = current
            .parent()
            .ok_or_else(|| Error::invalid("executable has no parent"))?;
        let sibling = dir.join(if cfg!(windows) {
            "superba.exe"
        } else {
            "superba"
        });
        if sibling.is_file() {
            return Ok(sibling);
        }
        #[cfg(windows)]
        if let Some(installed) = windows::installed_gui()? {
            return Ok(installed);
        }
        Ok(sibling)
    }
}

/// Render an entry without executing a shell. Paths with control characters
/// are rejected rather than producing extra desktop/plist/registry fields.
pub fn render(platform: Platform, executable: &Path) -> Result<String> {
    let text = executable
        .to_str()
        .ok_or_else(|| Error::invalid("login executable path is not UTF-8"))?;
    if text.chars().any(char::is_control) {
        return Err(Error::invalid(
            "login executable path contains a control character",
        ));
    }
    match platform {
        Platform::Windows => superba_core::autostart::windows_command(executable),
        Platform::Linux => {
            if text.contains('=') {
                return Err(Error::invalid(
                    "XDG Exec executable paths cannot contain '='",
                ));
            }
            let mut quoted = String::from("\"");
            for c in text.chars() {
                if matches!(c, '"' | '`' | '$' | '\\') {
                    quoted.push('\\');
                }
                quoted.push(c);
                if c == '%' {
                    quoted.push('%');
                }
            }
            quoted.push('"');
            let exec = quoted.replace('\\', "\\\\");
            Ok(format!(
                "[Desktop Entry]\nType=Application\nName=Superba\nExec={exec} --autostart\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
            ))
        }
        Platform::Mac => {
            // Direct launch attributes the login item to Superba and delivers
            // --autostart through silent IPC without an `open -n` application.
            // A manual launch refreshes the path after a bundle move.
            let args = [text, "--autostart"]
                .into_iter()
                .map(|s| format!("<string>{}</string>", xml(s)))
                .collect::<String>();
            Ok(format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>{AGENT_LABEL}</string><key>AssociatedBundleIdentifiers</key><array><string>{BUNDLE_ID}</string></array><key>ProgramArguments</key><array>{args}</array><key>RunAtLoad</key><true/><key>LimitLoadToSessionType</key><string>Aqua</string></dict></plist>\n"
            ))
        }
    }
}
fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
