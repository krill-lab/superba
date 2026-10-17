//! # superba-import
//!
//! Imports provider configurations from other tools that manage Codex (and
//! other coding tools) providers:
//!
//! - **cc-switch** (`farion1231/cc-switch`): `~/.cc-switch/cc-switch.db`
//!   (SQLite, current) or `~/.cc-switch/config.json` (older JSON formats);
//! - **AI Toolbox** (`coulsontl/ai-toolbox`): its SQLite database in the app
//!   data directory.
//!
//! Reading is strictly read-only ([`sqlite`]: databases are opened `immutable`
//! or copied first) and defensive: an unknown version or an unreadable store is
//! skipped with a reason, never guessed at. [`mapping`] turns what was found into
//! import candidates (the Krill preset when the address is a Krill line, else a
//! custom OpenAI-compatible site), finds conflicts with existing providers and
//! plans the profiles and sites an import saves. Saving is the switching
//! service's job (`superba-switch`), through the normal profile and key storage;
//! nothing here writes Codex's files.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod aitoolbox;
pub mod ccs;
pub mod codex;
pub mod foreign;
pub mod mapping;
pub mod sqlite;

use std::path::{Path, PathBuf};

use superba_core::{
    EnvSource, ImportScanRequest, ImportSource, ImportStore, LocalizedText, StoreStatus,
};

pub use foreign::{ForeignKey, ForeignProvider, SourceRead};
pub use mapping::{MakeSite, Planned, PlannedAction, plan, preview};

/// Localizable text of this crate (keys under `import.` in the core catalog).
pub(crate) fn text(key: &str, fallback: &str) -> LocalizedText {
    LocalizedText::new(key, fallback)
}

/// Operating system whose path conventions apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    /// Linux and other Unix systems (XDG directories).
    Linux,
    /// macOS (`~/Library/Application Support`).
    MacOs,
    /// Windows (`%APPDATA%`, `%LOCALAPPDATA%`).
    Windows,
}

impl Os {
    /// The OS this build runs on.
    pub fn current() -> Self {
        if cfg!(target_os = "windows") {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else {
            Os::Linux
        }
    }
}

/// Where to look: the home directory, the environment (for `XDG_*`,
/// `APPDATA`, ... and the tools' own overrides), the OS conventions, and a
/// private scratch directory of Superba's for database copies.
#[derive(Clone, Copy, Debug)]
pub struct ScanEnv<'a> {
    /// The user's home directory.
    pub home: &'a Path,
    /// Environment view.
    pub env: &'a dyn EnvSource,
    /// Path conventions.
    pub os: Os,
    /// Superba's scratch directory (database copies; removed after reading).
    pub scratch: &'a Path,
}

impl ScanEnv<'_> {
    /// The per-user application data directory: `$XDG_DATA_HOME` or
    /// `~/.local/share` (Linux), `~/Library/Application Support` (macOS),
    /// `%APPDATA%` (Windows, roaming).
    pub fn app_data_dir(&self) -> PathBuf {
        match self.os {
            Os::Linux => self
                .env
                .var_non_empty("XDG_DATA_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| self.home.join(".local").join("share")),
            Os::MacOs => self.home.join("Library").join("Application Support"),
            Os::Windows => self
                .env
                .var_non_empty("APPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| self.home.join("AppData").join("Roaming")),
        }
    }

    /// The per-user local (non-roaming) data directory: as
    /// [`ScanEnv::app_data_dir`] except `%LOCALAPPDATA%` on Windows.
    pub fn local_data_dir(&self) -> PathBuf {
        match self.os {
            Os::Windows => self
                .env
                .var_non_empty("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| self.home.join("AppData").join("Local")),
            _ => self.app_data_dir(),
        }
    }

    /// The per-user configuration directory: `$XDG_CONFIG_HOME` or `~/.config`
    /// (Linux), `~/Library/Application Support` (macOS), `%APPDATA%` (Windows).
    pub fn config_dir(&self) -> PathBuf {
        match self.os {
            Os::Linux => self
                .env
                .var_non_empty("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| self.home.join(".config")),
            _ => self.app_data_dir(),
        }
    }

    /// `path` for display, with this system's separator throughout:
    /// `~/.cc-switch/cc-switch.db` under the home directory
    /// (`~\.cc-switch\cc-switch.db` on Windows), else the whole path. A path
    /// joined from a `/`-separated setting (a `~/Dropbox/cc-switch` override)
    /// shows no mixed separators on Windows.
    pub fn display(&self, path: &Path) -> String {
        // Re-joining the components writes the native separator between them.
        let native = |p: &Path| p.components().collect::<PathBuf>().display().to_string();
        match path.strip_prefix(self.home) {
            Ok(rest) if !rest.as_os_str().is_empty() => {
                format!("~{}{}", std::path::MAIN_SEPARATOR, native(rest))
            }
            _ => native(path),
        }
    }

    /// A store entry for `path`.
    pub(crate) fn store(
        &self,
        path: &Path,
        format: impl Into<String>,
        status: StoreStatus,
        note: Option<LocalizedText>,
    ) -> ImportStore {
        ImportStore {
            path: path.to_path_buf(),
            display_path: self.display(path),
            format: format.into(),
            status,
            note,
        }
    }
}

/// Read one source (blocking file I/O).
pub fn read_source(source: ImportSource, env: &ScanEnv<'_>) -> SourceRead {
    match source {
        ImportSource::Ccs => ccs::read(env),
        ImportSource::AiToolbox => aitoolbox::read(env),
        #[allow(unreachable_patterns)]
        other => SourceRead {
            source: other,
            stores: Vec::new(),
            providers: Vec::new(),
        },
    }
}

/// Read every source of `request` (blocking file I/O).
pub fn read_sources(request: &ImportScanRequest, env: &ScanEnv<'_>) -> Vec<SourceRead> {
    request
        .effective_sources()
        .into_iter()
        .map(|s| read_source(s, env))
        .collect()
}
