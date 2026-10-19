//! The shared error type.
//!
//! Every trait in [`crate::traits`] returns [`Result`]. The error is `Clone` and
//! serializable so it can travel through UI channels, journals and `--json` CLI
//! output unchanged. Use [`Error::code`] (stable, snake_case) to map an error to an
//! i18n key (`error.<code>`), never the English `Display` text.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::i18n::LocalizedText;
use crate::plan::BlockReason;

/// Result alias used across all Superba crates.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Errors produced by Superba components.
///
/// The enum is `non_exhaustive`: match on [`Error::code`] or use a wildcard arm.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Error {
    /// A stub that has not been written yet. Placeholder implementations return this
    /// instead of panicking so the workspace always compiles and runs.
    #[error("not implemented yet: {what}")]
    NotImplemented {
        /// Which component and method, e.g. `superba-config: ConfigEngine::plan`.
        what: String,
    },
    /// The operation is not available on this platform or for this target.
    #[error("unsupported: {what}")]
    Unsupported {
        /// Human readable description of the unsupported operation.
        what: String,
    },
    /// File system error.
    #[error("I/O error on {}: {message}", crate::paths::display_path(path))]
    Io {
        /// Path involved in the failing operation.
        path: PathBuf,
        /// `std::io::ErrorKind` rendered with `Debug`, e.g. `PermissionDenied`
        /// ([`crate::fdlimit::IO_KIND_TOO_MANY_OPEN_FILES`] for "too many open
        /// files").
        kind: String,
        /// The OS error message, with the OS error code (`... (os error 24)`).
        message: String,
        /// For "too many open files" in this process: its soft open-file limit
        /// when the error happened. `None` for every other error or when unknown.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        open_files_limit: Option<u64>,
    },
    /// A file or response could not be parsed. Writers must refuse to write in this
    /// case rather than overwrite a file they do not understand.
    #[error("cannot parse {what}: {message}")]
    Parse {
        /// What was being parsed (`config.toml`, `auth.json`, `whoami response`, ...).
        what: String,
        /// Parser error message.
        message: String,
    },
    /// Invalid input supplied by the caller or the user.
    #[error("invalid input: {message}")]
    Invalid {
        /// Description of the problem.
        message: String,
    },
    /// Something that was referenced does not exist (profile, line, host, backup ...).
    #[error("not found: {what}")]
    NotFound {
        /// Description of the missing item.
        what: String,
    },
    /// A file changed under us and the bounded retry loop gave up (hash re-check
    /// before rename failed repeatedly), or two writers raced.
    #[error("conflict: {message}")]
    Conflict {
        /// Description of the conflict.
        message: String,
    },
    /// The planned switch must not run. Carries the reasons computed by planning.
    #[error("blocked: {reasons:?}")]
    Blocked {
        /// Why the switch is blocked (version gate, unusable line, rejected key, ...).
        reasons: Vec<BlockReason>,
    },
    /// Another Superba instance (GUI or `spb`) or another tool holds a lock.
    #[error("locked: {what}")]
    Locked {
        /// Which lock is held.
        what: String,
    },
    /// Transport level network failure (DNS, connect, TLS, timeout while connecting).
    #[error("network error for {url}: {message}")]
    Network {
        /// The URL that was requested. Never contains secrets.
        url: String,
        /// Error message.
        message: String,
    },
    /// The server answered with an unexpected HTTP status.
    #[error("HTTP {status} from {url}: {message}")]
    Http {
        /// The URL that was requested. Never contains secrets.
        url: String,
        /// HTTP status code.
        status: u16,
        /// Short, redacted excerpt of the body or a summary.
        message: String,
    },
    /// Chrome DevTools Protocol failure (connect, target lookup, evaluate).
    #[error("CDP error: {message}")]
    Cdp {
        /// Error message.
        message: String,
    },
    /// Process inspection, launch or termination failure.
    #[error("process error: {message}")]
    Process {
        /// Error message.
        message: String,
    },
    /// ssh invocation failure for a remote host.
    #[error("ssh error on {host}: {message}")]
    Ssh {
        /// Host label (alias or host name). Never contains credentials.
        host: String,
        /// Error message (stderr excerpt).
        message: String,
    },
    /// `wsl.exe` failure for a WSL distribution (it is not installed, the
    /// distribution does not exist or does not start, a script inside failed).
    #[error("WSL error on {distro}: {message}")]
    Wsl {
        /// Distribution name.
        distro: String,
        /// Error message (`wsl.exe` output excerpt).
        message: String,
    },
    /// OS keyring or secret file failure.
    #[error("secret store error: {message}")]
    Secret {
        /// Error message. Never contains the secret itself.
        message: String,
    },
    /// The user or a caller cancelled the operation.
    #[error("cancelled")]
    Cancelled,
    /// An operation did not finish in time.
    #[error("timed out: {what}")]
    Timeout {
        /// What timed out.
        what: String,
    },
    /// Anything else.
    #[error("{message}")]
    Other {
        /// Error message.
        message: String,
    },
}

impl Error {
    /// Stable snake_case code of the variant; the i18n key is `error.<code>`.
    pub fn code(&self) -> &'static str {
        match self {
            Error::NotImplemented { .. } => "not_implemented",
            Error::Unsupported { .. } => "unsupported",
            Error::Io { .. } => "io",
            Error::Parse { .. } => "parse",
            Error::Invalid { .. } => "invalid",
            Error::NotFound { .. } => "not_found",
            Error::Conflict { .. } => "conflict",
            Error::Blocked { .. } => "blocked",
            Error::Locked { .. } => "locked",
            Error::Network { .. } => "network",
            Error::Http { .. } => "http",
            Error::Cdp { .. } => "cdp",
            Error::Process { .. } => "process",
            Error::Ssh { .. } => "ssh",
            Error::Wsl { .. } => "wsl",
            Error::Secret { .. } => "secret",
            Error::Cancelled => "cancelled",
            Error::Timeout { .. } => "timeout",
            Error::Other { .. } => "other",
        }
    }

    /// Localizable summary: key `error.<code>` with the English `Display` text as the
    /// fallback and [`Error::detail`] as the only argument (the catalog template adds
    /// its own localized category prefix, so the detail must not repeat it).
    ///
    /// I/O errors whose cause the user can act on get their own message
    /// (`error_io.*`: too many open files with the limit, permission denied, file
    /// not found) with `path`, `message` (the OS text and code) and `detail`.
    pub fn to_text(&self) -> LocalizedText {
        if let Some(text) = self.io_text() {
            return text;
        }
        if let Some(text) = crate::data::error_text(&self.detail()) {
            return text;
        }
        LocalizedText::new(format!("error.{}", self.code()), self.to_string())
            .arg("detail", self.detail())
    }

    fn io_text(&self) -> Option<LocalizedText> {
        let Error::Io {
            path,
            kind,
            message,
            open_files_limit,
        } = self
        else {
            return None;
        };
        let (key, fallback) = match (kind.as_str(), open_files_limit) {
            (crate::fdlimit::IO_KIND_TOO_MANY_OPEN_FILES, Some(_)) => (
                "error_io.too_many_open_files",
                "Too many open files (system limit {limit}): cannot open {path}. Quit programs \
                 you do not need and try again; if it keeps happening, restart Superba. ({message})",
            ),
            (crate::fdlimit::IO_KIND_TOO_MANY_OPEN_FILES, None) => (
                "error_io.too_many_open_files_no_limit",
                "Too many open files: cannot open {path}. Quit programs you do not need and \
                 try again; if it keeps happening, restart Superba. ({message})",
            ),
            ("PermissionDenied", _) => (
                "error_io.permission_denied",
                "Permission denied: {path} ({message})",
            ),
            ("NotFound", _) => ("error_io.not_found", "File not found: {path} ({message})"),
            _ => return None,
        };
        let mut text = LocalizedText::new(key, fallback)
            .arg("path", crate::paths::display_path(path))
            .arg("message", message)
            .arg("detail", self.detail());
        if let Some(limit) = open_files_limit {
            text = text.arg("limit", limit);
        }
        Some(text)
    }

    /// The variant's payload without the English category prefix of `Display`
    /// (`"profile nope"` for `NotFound`, not `"not found: profile nope"`).
    pub fn detail(&self) -> String {
        match self {
            Error::NotImplemented { what }
            | Error::Unsupported { what }
            | Error::NotFound { what }
            | Error::Locked { what }
            | Error::Timeout { what } => what.clone(),
            Error::Io { path, message, .. } => {
                format!("{}: {message}", crate::paths::display_path(path))
            }
            Error::Parse { what, message } => format!("{what}: {message}"),
            Error::Invalid { message }
            | Error::Conflict { message }
            | Error::Cdp { message }
            | Error::Process { message }
            | Error::Secret { message }
            | Error::Other { message } => message.clone(),
            Error::Blocked { reasons } => format!("{reasons:?}"),
            Error::Network { url, message } => format!("{url}: {message}"),
            Error::Http {
                url,
                status,
                message,
            } => format!("HTTP {status} from {url}: {message}"),
            Error::Ssh { host, message } => format!("{host}: {message}"),
            Error::Wsl { distro, message } => format!("{distro}: {message}"),
            Error::Cancelled => String::new(),
        }
    }

    /// `NotImplemented` for the given component/method.
    pub fn not_implemented(what: impl Into<String>) -> Self {
        Error::NotImplemented { what: what.into() }
    }

    /// Wrap a `std::io::Error` together with the path it concerns. The kind and
    /// the OS message (with the OS error code) are kept; "too many open files"
    /// gets the kind [`crate::fdlimit::IO_KIND_TOO_MANY_OPEN_FILES`] and, when the
    /// process limit was hit, that limit.
    pub fn io(path: impl AsRef<Path>, err: &std::io::Error) -> Self {
        use crate::fdlimit::{FileLimitHit, IO_KIND_TOO_MANY_OPEN_FILES, file_limit_hit};
        let hit = file_limit_hit(err);
        Error::Io {
            path: path.as_ref().to_path_buf(),
            kind: match hit {
                Some(_) => IO_KIND_TOO_MANY_OPEN_FILES.to_owned(),
                None => format!("{:?}", err.kind()),
            },
            message: err.to_string(),
            open_files_limit: match hit {
                Some(FileLimitHit::Process) => crate::fdlimit::open_files_limit(),
                _ => None,
            },
        }
    }

    /// `Io` from an already rendered kind and message (errors that crossed a crate
    /// boundary as text). A "too many open files" kind gets the current limit.
    pub fn io_parts(path: impl Into<PathBuf>, kind: String, message: String) -> Self {
        let open_files_limit = (kind == crate::fdlimit::IO_KIND_TOO_MANY_OPEN_FILES)
            .then(crate::fdlimit::open_files_limit)
            .flatten();
        Error::Io {
            path: path.into(),
            kind,
            message,
            open_files_limit,
        }
    }

    /// `Parse` error helper.
    pub fn parse(what: impl Into<String>, message: impl ToString) -> Self {
        Error::Parse {
            what: what.into(),
            message: message.to_string(),
        }
    }

    /// `Invalid` error helper.
    pub fn invalid(message: impl Into<String>) -> Self {
        Error::Invalid {
            message: message.into(),
        }
    }

    /// `Other` error helper.
    pub fn other(message: impl Into<String>) -> Self {
        Error::Other {
            message: message.into(),
        }
    }

    /// True for [`Error::NotImplemented`]; lets callers degrade gracefully while the
    /// workspace is still being filled in.
    pub fn is_not_implemented(&self) -> bool {
        matches!(self, Error::NotImplemented { .. })
    }
}
