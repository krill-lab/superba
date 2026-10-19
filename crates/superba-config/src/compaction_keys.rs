//! Persistent, rotating keys for router compaction envelopes, encrypted by the
//! same vault as Superba's login backups. Old keys remain readable until
//! explicitly retired; losing a vault must never silently recreate its keys.

use std::collections::BTreeMap;
use std::fs::OpenOptions;

use chacha20poly1305::ChaCha20Poly1305;
use chacha20poly1305::aead::{Generate, Key};

use serde::{Deserialize, Serialize};
use superba_core::{Error, LocalContext, Result, SecretBytes};
use zeroize::{Zeroize, Zeroizing};

use crate::{fsx, vault::Vault};

/// The keyring's encrypted file inside Superba's secrets directory.
pub const FILE: &str = "compaction-keys.enc";

#[derive(Serialize, Deserialize)]
struct Wire {
    format: u32,
    owner: String,
    active: String,
    keys: BTreeMap<String, Vec<u8>>,
}

impl Drop for Wire {
    fn drop(&mut self) {
        for key in self.keys.values_mut() {
            key.zeroize();
        }
    }
}

/// In-memory snapshot. Debug output never includes keys or owner identifiers.
#[derive(Clone)]
pub struct CompactionKeys {
    owner: String,
    active: String,
    keys: BTreeMap<String, SecretBytes>,
}

impl std::fmt::Debug for CompactionKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CompactionKeys([redacted])")
    }
}

impl CompactionKeys {
    /// Ephemeral keyring for embedded routers and tests. File-backed routers
    /// must use [`CompactionKeyStore::load`].
    pub fn ephemeral() -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        Self {
            owner: uuid::Uuid::new_v4().to_string(),
            active: id.clone(),
            keys: [(
                id,
                SecretBytes::new(Key::<ChaCha20Poly1305>::generate().to_vec()),
            )]
            .into(),
        }
    }

    /// Stable owner of the vault, preserved across router token/key rotation.
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// Key id and material used to seal new summaries.
    pub fn active(&self) -> (&str, &[u8]) {
        (&self.active, self.keys[&self.active].expose())
    }

    /// Read a currently accepted key, including retained old keys.
    pub fn key(&self, id: &str) -> Option<&[u8]> {
        self.keys.get(id).map(SecretBytes::expose)
    }
}

/// The vault-backed store. Operations serialize through an OS file lock and
/// publish atomically; concurrent starts and rotations cannot lose old keys.
#[derive(Clone, Debug)]
pub struct CompactionKeyStore {
    ctx: LocalContext,
}

impl CompactionKeyStore {
    /// Use only this injected data directory/environment.
    pub fn new(ctx: LocalContext) -> Self {
        Self { ctx }
    }

    /// Whether this installation already has sealed compaction keys.
    pub fn exists(&self) -> bool {
        self.ctx.data_dir.secrets_dir().join(FILE).exists()
    }

    /// Load or create the ring. Corrupt or unavailable vaults fail closed.
    pub fn load(&self) -> Result<CompactionKeys> {
        self.update(false, None)
    }

    /// Start sealing with a new key, retaining every old key for resume/fork.
    pub fn rotate(&self) -> Result<CompactionKeys> {
        self.update(true, None)
    }

    /// Explicitly revoke an old key. Affected conversations then report an
    /// unreadable envelope; the active key cannot be removed.
    pub fn retire(&self, id: &str) -> Result<CompactionKeys> {
        self.update(false, Some(id))
    }

    fn update(&self, rotate: bool, retire: Option<&str>) -> Result<CompactionKeys> {
        let dir = self.ctx.data_dir.secrets_dir();
        fsx::ensure_private_dir(&dir)?;
        let lock_path = dir.join("compaction-keys.lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(&lock_path)
            .map_err(|e| Error::io(&lock_path, &e))?;
        lock.lock().map_err(|e| Error::io(&lock_path, &e))?;
        let vault = Vault::new(self.ctx.clone());
        let path = dir.join(FILE);
        if path.exists() && !dir.join("vault.json").exists() {
            return Err(invalid());
        }
        let bytes = vault.read_sealed(&path)?;
        let mut ring = match bytes.as_ref() {
            None => CompactionKeys::ephemeral(),
            Some(b) => {
                let w: Wire = serde_json::from_slice(b.expose()).map_err(|_| invalid())?;
                if w.format != 1
                    || w.owner.is_empty()
                    || !w.keys.contains_key(&w.active)
                    || w.keys.values().any(|k| k.len() != 32)
                {
                    return Err(invalid());
                }
                CompactionKeys {
                    owner: w.owner.clone(),
                    active: w.active.clone(),
                    keys: w
                        .keys
                        .iter()
                        .map(|(k, v)| (k.clone(), SecretBytes::new(v.clone())))
                        .collect(),
                }
            }
        };
        if let Some(id) = retire {
            if id == ring.active {
                return Err(Error::invalid("cannot retire the active compaction key"));
            }
            ring.keys.remove(id);
        }
        if rotate {
            let id = uuid::Uuid::new_v4().to_string();
            ring.keys.insert(
                id.clone(),
                SecretBytes::new(Key::<ChaCha20Poly1305>::generate().to_vec()),
            );
            ring.active = id;
        }
        if bytes.is_none() || rotate || retire.is_some() {
            let w = Wire {
                format: 1,
                owner: ring.owner.clone(),
                active: ring.active.clone(),
                keys: ring
                    .keys
                    .iter()
                    .map(|(k, v)| (k.clone(), v.expose().to_vec()))
                    .collect(),
            };
            let bytes = Zeroizing::new(serde_json::to_vec(&w).map_err(|_| invalid())?);
            vault.write_sealed(&path, &bytes)?;
        }
        Ok(ring)
    }
}

fn invalid() -> Error {
    Error::Secret {
        message: "compaction keyring is invalid; restore its vault backup".into(),
    }
}
