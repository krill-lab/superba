//! Encryption of login stashes and file backups (plan 12.2): ChaCha20-Poly1305
//! with a random 256-bit data key kept in the OS keyring (or a 0600 file when no
//! keyring is available / `SUPERBA_SECRET_BACKEND=file`).
//!
//! Sealed format: `SPBV1\0` magic, 12-byte random nonce, ciphertext + tag.
//! `secrets/vault.json` records where the key lives plus a key check value, so a
//! keyring that is unavailable later (e.g. an ssh session without Secret
//! Service) produces a clear error instead of a silently regenerated key that
//! could not open old backups.

use std::path::{Path, PathBuf};

use chacha20poly1305::aead::{Aead, Generate, Key, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use serde::{Deserialize, Serialize};
use superba_core::secret::KEYRING_SERVICE;
use superba_core::{Error, LocalContext, Result, SecretBytes};
use zeroize::Zeroizing;

use crate::fsx::{self, Mode};
use crate::keystore::{self, Choice};

const MAGIC: &[u8] = b"SPBV1\0";
const NONCE_LEN: usize = 12;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum KeyLocation {
    Keyring { account: String },
    File { path: PathBuf },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct VaultMeta {
    schema: u32,
    location: KeyLocation,
    /// First 16 hex chars of sha256(key): detects a wrong key before decrypting.
    check: String,
}

/// Seals and opens Superba's encrypted files.
#[derive(Clone, Debug)]
pub(crate) struct Vault {
    ctx: LocalContext,
}

impl Vault {
    pub(crate) fn new(ctx: LocalContext) -> Self {
        Self { ctx }
    }

    fn meta_path(&self) -> PathBuf {
        self.ctx.data_dir.secrets_dir().join("vault.json")
    }

    /// Load the data key, creating it on first use.
    fn key(&self) -> Result<Zeroizing<Vec<u8>>> {
        let meta_path = self.meta_path();
        if let Some(meta) = fsx::read_json::<VaultMeta>(&meta_path, "secrets/vault.json")? {
            let key = self.load_key(&meta.location)?;
            if fsx::short_hash(&key) != meta.check {
                return Err(Error::Secret {
                    message: "the backup encryption key does not match secrets/vault.json".into(),
                });
            }
            return Ok(key);
        }
        self.create_key()
    }

    fn load_key(&self, loc: &KeyLocation) -> Result<Zeroizing<Vec<u8>>> {
        use base64::Engine as _;
        let encoded = match loc {
            KeyLocation::Keyring { account } => keystore::os::get(KEYRING_SERVICE, account)?
                .ok_or_else(|| Error::Secret {
                    message: "the backup encryption key is missing from the OS keyring".into(),
                })?,
            KeyLocation::File { path } => {
                let raw = fsx::read_optional(path)?.ok_or_else(|| Error::Secret {
                    message: format!("backup encryption key file {} is missing", path.display()),
                })?;
                String::from_utf8(raw).map_err(|_| Error::Secret {
                    message: "backup encryption key file is not text".into(),
                })?
            }
        };
        let encoded = Zeroizing::new(encoded);
        let key = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .map_err(|_| Error::Secret {
                message: "backup encryption key is corrupt".into(),
            })?;
        if key.len() != 32 {
            return Err(Error::Secret {
                message: "backup encryption key has the wrong length".into(),
            });
        }
        Ok(Zeroizing::new(key))
    }

    fn create_key(&self) -> Result<Zeroizing<Vec<u8>>> {
        use base64::Engine as _;
        let generated = Key::<ChaCha20Poly1305>::generate();
        let key = Zeroizing::new(generated.to_vec());
        let encoded = Zeroizing::new(base64::engine::general_purpose::STANDARD.encode(&*key));
        fsx::ensure_private_dir(&self.ctx.data_dir.secrets_dir())?;
        let choice = Choice::from_ctx(&self.ctx);
        let mut location = None;
        if choice != Choice::File && keystore::os::available() {
            let account = format!("vault:{}", keystore::install_id(&self.ctx)?);
            match keystore::os::set(KEYRING_SERVICE, &account, &encoded) {
                Ok(()) => location = Some(KeyLocation::Keyring { account }),
                Err(e) if choice == Choice::Keyring => return Err(e),
                Err(e) => tracing::warn!("keyring unusable, using a key file: {e}"),
            }
        } else if choice == Choice::Keyring {
            return Err(Error::Secret {
                message: "SUPERBA_SECRET_BACKEND=keyring but no OS keyring is available".into(),
            });
        }
        let location = match location {
            Some(l) => l,
            None => {
                let path = self.ctx.data_dir.secrets_dir().join("vault-key");
                fsx::write_atomic(&path, encoded.as_bytes(), Mode::Private)?;
                KeyLocation::File { path }
            }
        };
        let meta = VaultMeta {
            schema: 1,
            location,
            check: fsx::short_hash(&key),
        };
        fsx::write_json(&self.meta_path(), &meta)?;
        Ok(key)
    }

    /// Encrypt `plaintext`.
    pub(crate) fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let key = self.key()?;
        let cipher = ChaCha20Poly1305::new_from_slice(&key).map_err(|_| Error::Secret {
            message: "invalid backup encryption key".into(),
        })?;
        let nonce = Nonce::generate();
        let ct = cipher
            .encrypt(&nonce, plaintext)
            .map_err(|_| Error::Secret {
                message: "encryption failed".into(),
            })?;
        let mut out = Vec::with_capacity(MAGIC.len() + NONCE_LEN + ct.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Decrypt data produced by [`Vault::seal`].
    pub(crate) fn open(&self, sealed: &[u8]) -> Result<SecretBytes> {
        let body = sealed.strip_prefix(MAGIC).ok_or_else(|| Error::Secret {
            message: "not a Superba encrypted file".into(),
        })?;
        if body.len() < NONCE_LEN + 16 {
            return Err(Error::Secret {
                message: "encrypted file is truncated".into(),
            });
        }
        let (nonce, ct) = body.split_at(NONCE_LEN);
        let nonce = Nonce::try_from(nonce).map_err(|_| Error::Secret {
            message: "bad nonce".into(),
        })?;
        let key = self.key()?;
        let cipher = ChaCha20Poly1305::new_from_slice(&key).map_err(|_| Error::Secret {
            message: "invalid backup encryption key".into(),
        })?;
        cipher
            .decrypt(&nonce, ct)
            .map(SecretBytes::new)
            .map_err(|_| Error::Secret {
                message: "cannot decrypt (wrong key or corrupted file)".into(),
            })
    }

    /// Seal `plaintext` into `path` (0600, atomic).
    pub(crate) fn write_sealed(&self, path: &Path, plaintext: &[u8]) -> Result<()> {
        if let Some(dir) = path.parent() {
            fsx::ensure_private_dir(dir)?;
        }
        let sealed = self.seal(plaintext)?;
        fsx::write_atomic(path, &sealed, Mode::Private)
    }

    /// Open a sealed file; `None` if it does not exist.
    pub(crate) fn read_sealed(&self, path: &Path) -> Result<Option<SecretBytes>> {
        match fsx::read_optional(path)? {
            None => Ok(None),
            Some(b) => self.open(&b).map(Some),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use superba_core::{DataDir, MapEnv};

    use super::*;

    fn ctx(root: &Path) -> LocalContext {
        LocalContext::new(
            root.join("home"),
            DataDir::new(root.join("data")),
            Arc::new(MapEnv::new().with(keystore::ENV_SUPERBA_SECRET_BACKEND, "file")),
        )
    }

    #[test]
    fn seal_open_roundtrip_and_tamper() {
        let tmp = tempfile::tempdir().unwrap();
        let v = Vault::new(ctx(tmp.path()));
        let sealed = v.seal(b"secret login").unwrap();
        assert!(sealed.starts_with(MAGIC));
        assert!(!sealed.windows(6).any(|w| w == b"secret"));
        assert_eq!(v.open(&sealed).unwrap().expose(), b"secret login");
        // A second vault on the same data dir reuses the key.
        let v2 = Vault::new(ctx(tmp.path()));
        assert_eq!(v2.open(&sealed).unwrap().expose(), b"secret login");
        let mut bad = sealed.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(v.open(&bad).is_err());
        assert!(v.open(b"plain").is_err());
        // Another data dir cannot open it.
        let other = tempfile::tempdir().unwrap();
        assert!(Vault::new(ctx(other.path())).open(&sealed).is_err());
    }
}
