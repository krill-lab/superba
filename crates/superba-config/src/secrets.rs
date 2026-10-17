//! [`KeyringSecretStore`]: profile keys in the OS keyring, with 0600 files under
//! `<data>/secrets/` as the fallback (plan 12.2, 15.2 #8).

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use superba_core::secret::KEYRING_SERVICE;
use superba_core::{
    Error, KeyRef, LocalContext, ProfileId, Result, Secret, SecretBackend, SecretStore,
};
use zeroize::Zeroizing;

use crate::fsx::{self, Mode};
use crate::keystore::{self, Choice};

/// [`SecretStore`] using the OS keyring with a 0600 file fallback in `secrets/`.
///
/// The backend for new secrets follows `SUPERBA_SECRET_BACKEND` from the
/// context's environment (`keyring`, `file`, default `auto`). Keyring accounts are
/// `profile:<id>:<data-dir id>` so separate data dirs never share entries.
#[derive(Debug, Clone)]
pub struct KeyringSecretStore {
    ctx: LocalContext,
    choice: Choice,
}

impl KeyringSecretStore {
    /// Create a store (file fallback under `ctx.data_dir.secrets_dir()`).
    pub fn new(ctx: LocalContext) -> Self {
        let choice = Choice::from_ctx(&ctx);
        Self { ctx, choice }
    }

    /// Create a store that always uses `backend` for new secrets.
    pub fn with_backend(ctx: LocalContext, backend: SecretBackend) -> Self {
        let choice = match backend {
            SecretBackend::OsKeyring => Choice::Keyring,
            SecretBackend::PlaintextFile => Choice::File,
        };
        Self { ctx, choice }
    }

    /// The context this store was created with.
    pub fn context(&self) -> &LocalContext {
        &self.ctx
    }

    fn file_for(&self, profile_id: &ProfileId) -> PathBuf {
        self.ctx.data_dir.secrets_dir().join(format!(
            "profile-{}.secret",
            fsx::sanitize_component(profile_id.as_str())
        ))
    }

    fn put_file(&self, profile_id: &ProfileId, secret: &Secret) -> Result<KeyRef> {
        let path = self.file_for(profile_id);
        fsx::ensure_private_dir(&self.ctx.data_dir.secrets_dir())?;
        fsx::write_atomic(&path, secret.expose().as_bytes(), Mode::Private)?;
        Ok(KeyRef::File { path })
    }

    fn put_sync(&self, profile_id: &ProfileId, secret: &Secret) -> Result<KeyRef> {
        if secret.is_empty() {
            return Err(Error::invalid("the key is empty"));
        }
        match self.choice {
            Choice::File => self.put_file(profile_id, secret),
            Choice::Keyring | Choice::Auto => {
                if self.choice == Choice::Auto && !keystore::os::available() {
                    return self.put_file(profile_id, secret);
                }
                let account = format!(
                    "profile:{}:{}",
                    profile_id.as_str(),
                    keystore::install_id(&self.ctx)?
                );
                match keystore::os::set(KEYRING_SERVICE, &account, secret.expose()) {
                    Ok(()) => {
                        // A previous plaintext copy is now stale.
                        fsx::remove_if_exists(&self.file_for(profile_id))?;
                        Ok(KeyRef::Keyring {
                            service: KEYRING_SERVICE.to_owned(),
                            account,
                        })
                    }
                    Err(e) if self.choice == Choice::Keyring => Err(e),
                    Err(e) => {
                        tracing::warn!("keyring unusable, storing the key in a 0600 file: {e}");
                        self.put_file(profile_id, secret)
                    }
                }
            }
        }
    }

    fn get_sync(&self, key_ref: &KeyRef) -> Result<Secret> {
        match key_ref {
            KeyRef::Keyring { service, account } => {
                let v = keystore::os::get(service, account)?.ok_or_else(|| Error::NotFound {
                    what: format!("keyring entry {service}/{account}"),
                })?;
                let v = Zeroizing::new(v);
                non_empty(Secret::new(v.as_str()), "keyring entry")
            }
            KeyRef::File { path } => {
                let raw = fsx::read_optional(path)?.ok_or_else(|| Error::NotFound {
                    what: format!("key file {}", path.display()),
                })?;
                let raw = Zeroizing::new(raw);
                let text = std::str::from_utf8(&raw).map_err(|_| Error::Secret {
                    message: format!("key file {} is not UTF-8", path.display()),
                })?;
                non_empty(Secret::new(text), "key file")
            }
            KeyRef::Env { var } => {
                let v = self
                    .ctx
                    .env
                    .var_non_empty(var)
                    .ok_or_else(|| Error::NotFound {
                        what: format!("environment variable {var}"),
                    })?;
                let v = Zeroizing::new(v);
                non_empty(Secret::new(v.as_str()), "environment variable")
            }
            other => Err(Error::Unsupported {
                what: format!("key reference {other:?}"),
            }),
        }
    }

    fn delete_sync(&self, key_ref: &KeyRef) -> Result<()> {
        match key_ref {
            KeyRef::Keyring { service, account } => keystore::os::delete(service, account),
            KeyRef::File { path } => {
                // Only delete files Superba created.
                if is_inside(path, &self.ctx.data_dir.secrets_dir()) {
                    fsx::remove_if_exists(path)
                } else {
                    Ok(())
                }
            }
            KeyRef::Env { .. } => Ok(()),
            _ => Ok(()),
        }
    }
}

fn non_empty(s: Secret, what: &str) -> Result<Secret> {
    if s.is_empty() {
        Err(Error::Secret {
            message: format!("{what} is empty"),
        })
    } else {
        Ok(s)
    }
}

fn is_inside(path: &Path, dir: &Path) -> bool {
    match (std::fs::canonicalize(path), std::fs::canonicalize(dir)) {
        (Ok(p), Ok(d)) => p.starts_with(d),
        _ => path.starts_with(dir),
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::other(format!("secret store task failed: {e}")))?
}

#[async_trait]
impl SecretStore for KeyringSecretStore {
    async fn get(&self, key_ref: &KeyRef) -> Result<Secret> {
        let this = self.clone();
        let key_ref = key_ref.clone();
        blocking(move || this.get_sync(&key_ref)).await
    }

    async fn put(&self, profile_id: &ProfileId, secret: &Secret) -> Result<KeyRef> {
        let this = self.clone();
        let id = profile_id.clone();
        let secret = secret.clone();
        blocking(move || this.put_sync(&id, &secret)).await
    }

    async fn delete(&self, key_ref: &KeyRef) -> Result<()> {
        let this = self.clone();
        let key_ref = key_ref.clone();
        blocking(move || this.delete_sync(&key_ref)).await
    }

    fn backend(&self) -> SecretBackend {
        keystore::effective_backend(self.choice)
    }
}
