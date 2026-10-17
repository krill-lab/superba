//! Secrets and references to where they are stored.
//!
//! [`Secret`] and [`SecretBytes`] zeroize on drop, never implement `Serialize`, and
//! print redacted in `Debug`, so they cannot leak through logs, journals or JSON.
//! Persisted data only ever holds a [`KeyRef`].

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Keyring service name used for every Superba keyring entry.
pub const KEYRING_SERVICE: &str = "superba";

/// An API key / personal access token held in memory.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct Secret(String);

impl Secret {
    /// Wrap a secret (surrounding whitespace is trimmed: keys are pasted).
    pub fn new(value: impl Into<String>) -> Self {
        let mut v: String = value.into();
        let trimmed = v.trim();
        if trimmed.len() != v.len() {
            let t = trimmed.to_owned();
            v.zeroize();
            v = t;
        }
        Secret(v)
    }

    /// The raw secret. Only call this at the point of use (HTTP header, auth.json).
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// True if empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Last four characters for display ("key ending in abcd"); empty for keys shorter
    /// than 8 characters so short secrets are never mostly revealed.
    pub fn last4(&self) -> String {
        let n = self.0.chars().count();
        if n < 8 {
            return String::new();
        }
        self.0.chars().skip(n - 4).collect()
    }

    /// `••••abcd` style mask for UI.
    pub fn masked(&self) -> String {
        format!("••••{}", self.last4())
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret(…{})", self.last4())
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Secret::new(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Secret::new(value)
    }
}

/// File contents that may contain secrets (auth.json, stashed logins).
#[derive(Clone, Default, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct SecretBytes(Vec<u8>);

impl SecretBytes {
    /// Wrap bytes.
    pub fn new(bytes: Vec<u8>) -> Self {
        SecretBytes(bytes)
    }

    /// Borrow the bytes.
    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    /// Length in bytes.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True if empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretBytes({} bytes)", self.0.len())
    }
}

impl From<Vec<u8>> for SecretBytes {
    fn from(value: Vec<u8>) -> Self {
        SecretBytes(value)
    }
}

/// Where a profile's key lives. Persisted in `profiles.json` instead of the key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum KeyRef {
    /// OS keyring entry (macOS Keychain, Windows Credential Manager, Secret Service).
    Keyring {
        /// Keyring service, normally [`KEYRING_SERVICE`].
        service: String,
        /// Keyring account / entry name, e.g. `profile:<profile-id>`.
        account: String,
    },
    /// Plain-text file with mode 0600 inside the data dir. Fallback for headless
    /// Linux without a Secret Service; the UI must warn when this is used.
    File {
        /// Absolute path of the key file.
        path: PathBuf,
    },
    /// Read from an environment variable at use time; never written by Superba.
    /// For `spb` in CI/e2e (`--key-env NAME`).
    Env {
        /// Variable name.
        var: String,
    },
}

/// Which backend a [`crate::SecretStore`] writes new secrets to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretBackend {
    /// The OS keyring.
    OsKeyring,
    /// 0600 files in the data dir (warn the user).
    PlaintextFile,
}
