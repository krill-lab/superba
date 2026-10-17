//! Where Superba keeps secrets: the OS keyring, or 0600 files under
//! `<data>/secrets/` when no keyring is available (headless Linux) or when
//! `SUPERBA_SECRET_BACKEND=file` is set (tests, e2e, CI).
//!
//! Keyring entries are namespaced by a random per-data-dir id
//! (`secrets/install.json`), so a second data dir (an e2e run, a portable copy)
//! never overwrites the entries of the real one.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use superba_core::{Error, LocalContext, Result, SecretBackend};

use crate::fsx;

/// Environment variable selecting the secret backend: `keyring`, `file` or
/// `auto` (default: keyring when available, else file).
pub const ENV_SUPERBA_SECRET_BACKEND: &str = "SUPERBA_SECRET_BACKEND";

/// Requested backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    Auto,
    Keyring,
    File,
}

impl Choice {
    pub(crate) fn from_ctx(ctx: &LocalContext) -> Self {
        match ctx
            .env
            .var_non_empty(ENV_SUPERBA_SECRET_BACKEND)
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("file" | "plaintext" | "plaintext_file") => Choice::File,
            Some("keyring" | "os" | "os_keyring") => Choice::Keyring,
            _ => Choice::Auto,
        }
    }
}

/// Thin wrapper over the `keyring` crate (v1 API: Keychain, Credential Manager,
/// Secret Service).
pub(crate) mod os {
    use super::*;

    fn entry(service: &str, account: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(service, account).map_err(|e| Error::Secret {
            message: format!("keyring unavailable: {e}"),
        })
    }

    /// True if the platform keyring could be initialised.
    pub(crate) fn available() -> bool {
        keyring::Entry::store_status().is_ok()
    }

    pub(crate) fn get(service: &str, account: &str) -> Result<Option<String>> {
        match entry(service, account)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(Error::Secret {
                message: format!("keyring read failed: {e}"),
            }),
        }
    }

    pub(crate) fn set(service: &str, account: &str, value: &str) -> Result<()> {
        entry(service, account)?
            .set_password(value)
            .map_err(|e| Error::Secret {
                message: format!("keyring write failed: {e}"),
            })
    }

    pub(crate) fn delete(service: &str, account: &str) -> Result<()> {
        match entry(service, account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(Error::Secret {
                message: format!("keyring delete failed: {e}"),
            }),
        }
    }
}

/// Per-data-dir identity used to namespace keyring entries.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Install {
    id: String,
}

/// The random id of this data dir (created on first use).
pub(crate) fn install_id(ctx: &LocalContext) -> Result<String> {
    let path = install_path(ctx);
    if let Some(i) = fsx::read_json::<Install>(&path, "secrets/install.json")? {
        return Ok(i.id);
    }
    let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_owned();
    fsx::ensure_private_dir(&ctx.data_dir.secrets_dir())?;
    fsx::write_json(&path, &Install { id: id.clone() })?;
    Ok(id)
}

fn install_path(ctx: &LocalContext) -> PathBuf {
    ctx.data_dir.secrets_dir().join("install.json")
}

/// Backend new secrets go to for `choice` (probing the keyring for `Auto`).
pub(crate) fn effective_backend(choice: Choice) -> SecretBackend {
    match choice {
        Choice::File => SecretBackend::PlaintextFile,
        Choice::Keyring => SecretBackend::OsKeyring,
        Choice::Auto => {
            if os::available() {
                SecretBackend::OsKeyring
            } else {
                SecretBackend::PlaintextFile
            }
        }
    }
}
