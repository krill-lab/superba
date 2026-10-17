//! Errors of the catalog crate.

use std::path::PathBuf;

/// Result alias.
pub type Result<T, E = CatalogError> = std::result::Result<T, E>;

/// What went wrong while loading, verifying or fetching a catalog.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CatalogError {
    /// File system error.
    #[error("{}: {message}", path.display())]
    Io {
        /// Path.
        path: PathBuf,
        /// `std::io::ErrorKind` rendered with `Debug`.
        kind: String,
        /// OS message.
        message: String,
    },
    /// A file does not parse.
    #[error("cannot parse {what}: {message}")]
    Parse {
        /// What was parsed (file path or description).
        what: String,
        /// Parser message.
        message: String,
    },
    /// The catalog uses a format this build does not understand.
    #[error("unsupported {what} format {found} (this build reads {supported})")]
    UnsupportedFormat {
        /// Which format.
        what: &'static str,
        /// Found.
        found: u32,
        /// Supported.
        supported: u32,
    },
    /// A file's digest or size does not match the index, or a listed file is missing.
    #[error("integrity check failed for {path}: {message}")]
    Integrity {
        /// Catalog-relative path.
        path: String,
        /// Details.
        message: String,
    },
    /// No valid signature from a trusted key.
    #[error("catalog signature rejected: {0}")]
    Signature(String),
    /// A remote index older than the one already in use (rollback protection).
    #[error("catalog index serial {found} is older than {current}")]
    Rollback {
        /// Serial of the offered index.
        found: u64,
        /// Serial already in use.
        current: u64,
    },
    /// Remote fetch is switched off.
    #[error("remote catalog updates are disabled")]
    RemoteDisabled,
    /// Remote fetch failed.
    #[error("fetching {url} failed: {message}")]
    Network {
        /// URL.
        url: String,
        /// Details (never contains credentials; the catalog is public data).
        message: String,
    },
    /// Invalid input (URL, configuration, version).
    #[error("{0}")]
    Invalid(String),
    /// The catalog has no version at all.
    #[error("the catalog lists no Codex versions")]
    Empty,
}

impl CatalogError {
    pub(crate) fn io(path: impl Into<PathBuf>, err: &std::io::Error) -> Self {
        CatalogError::Io {
            path: path.into(),
            kind: format!("{:?}", err.kind()),
            message: err.to_string(),
        }
    }

    pub(crate) fn parse(what: impl Into<String>, message: impl ToString) -> Self {
        CatalogError::Parse {
            what: what.into(),
            message: message.to_string(),
        }
    }

    pub(crate) fn integrity(path: impl Into<String>, message: impl Into<String>) -> Self {
        CatalogError::Integrity {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl From<CatalogError> for superba_core::Error {
    fn from(e: CatalogError) -> Self {
        match e {
            CatalogError::Io {
                path,
                kind,
                message,
            } => superba_core::Error::Io {
                path,
                kind,
                message,
            },
            CatalogError::Parse { what, message } => superba_core::Error::parse(what, message),
            CatalogError::Network { url, message } => superba_core::Error::Network { url, message },
            CatalogError::Invalid(m) => superba_core::Error::invalid(m),
            other => superba_core::Error::other(other.to_string()),
        }
    }
}
