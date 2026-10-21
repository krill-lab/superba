//! Encrypted managed ChatGPT credentials and disposable Codex homes.

use crate::{authjson, fsx, vault::Vault};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use superba_core::{ChatGptAccount, Error, LocalContext, Result, SecretBytes, Timestamp};
use zeroize::Zeroizing;

#[derive(Serialize, Deserialize)]
struct Saved {
    account: ChatGptAccount,
    auth: String,
}

impl Drop for Saved {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.auth.zeroize();
    }
}

/// Managed logins; all persistent credentials use the existing Superba vault.
pub struct Accounts {
    ctx: LocalContext,
}

impl Accounts {
    /// Bind the store to an injected context.
    pub fn new(ctx: LocalContext) -> Self {
        Self { ctx }
    }

    fn path(&self) -> PathBuf {
        self.ctx.data_dir.secrets_dir().join("chatgpt-accounts.enc")
    }

    fn read(&self) -> Result<BTreeMap<String, Saved>> {
        let Some(bytes) = Vault::new(self.ctx.clone()).read_sealed(&self.path())? else {
            return Ok(BTreeMap::new());
        };
        serde_json::from_slice(bytes.expose())
            .map_err(|_| Error::invalid("invalid encrypted ChatGPT account store"))
    }

    /// Metadata only. The active identity is read from the live credential.
    pub fn list(&self, live: Option<&SecretBytes>) -> Result<Vec<ChatGptAccount>> {
        let active = live
            .and_then(|a| authjson::login_info(a.expose()))
            .map(|i| i.identity);
        Ok(self
            .read()?
            .values()
            .map(|s| {
                let mut account = s.account.clone();
                account.active = active.as_deref() == Some(&account.id);
                account
            })
            .collect())
    }

    /// Save or refresh a credential. Duplicate identities retain their first-save date.
    pub fn put(&self, auth: &SecretBytes, now: Timestamp) -> Result<ChatGptAccount> {
        fsx::ensure_private_dir(&self.ctx.data_dir.secrets_dir())?;
        let lock_path = self
            .ctx
            .data_dir
            .secrets_dir()
            .join("chatgpt-accounts.lock");
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| Error::io(&lock_path, &e))?;
        lock.lock().map_err(|e| Error::io(&lock_path, &e))?;
        let info = authjson::login_info(auth.expose())
            .filter(|i| i.identity != "unknown")
            .ok_or_else(|| Error::invalid("a complete ChatGPT login is required"))?;
        let mut all = self.read()?;
        let account = ChatGptAccount {
            id: info.identity.clone(),
            email: info
                .email
                .unwrap_or_else(|| info.account_id.unwrap_or_else(|| info.identity.clone())),
            added_at: all.get(&info.identity).map_or(now, |s| s.account.added_at),
            active: false,
        };
        let auth_text = String::from_utf8(auth.expose().to_vec())
            .map_err(|_| Error::invalid("invalid ChatGPT credential encoding"))?;
        all.insert(
            account.id.clone(),
            Saved {
                account: account.clone(),
                auth: auth_text,
            },
        );
        let plaintext = Zeroizing::new(
            serde_json::to_vec(&all)
                .map_err(|_| Error::invalid("cannot encode ChatGPT account store"))?,
        );
        Vault::new(self.ctx.clone()).write_sealed(&self.path(), &plaintext)?;
        Ok(account)
    }

    /// Load an exact identity; never fall back to another account.
    pub fn load(&self, id: &str) -> Result<SecretBytes> {
        let path = self.ctx.codex_home().auth_json();
        if let Some(live) = fsx::read_optional(&path)?
            && authjson::login_info(&live).is_some_and(|i| i.identity == id)
            && self.read()?.contains_key(id)
        {
            // Confirm the account is managed before accepting its live refresh.
            return Ok(SecretBytes::new(live));
        }
        self.read()?
            .get(id)
            .map(|s| SecretBytes::new(s.auth.as_bytes().to_vec()))
            .ok_or_else(|| Error::NotFound {
                what: "ChatGPT account".into(),
            })
    }

    /// Publish Codex's rotated tokens only if the same live login is unchanged.
    /// Another app/account switch always wins over an older quota helper.
    pub fn refresh_live(&self, before: &SecretBytes, refreshed: &SecretBytes) -> Result<()> {
        let path = self.ctx.codex_home().auth_json();
        if fsx::read_optional(&path)?.as_deref() == Some(before.expose()) {
            fsx::write_atomic(&path, refreshed.expose(), fsx::Mode::Private)?;
        }
        Ok(())
    }

    /// A private disposable home for Codex's own login/refresh implementation.
    pub fn session(&self, auth: Option<&SecretBytes>) -> Result<SessionHome> {
        let path = self
            .ctx
            .data_dir
            .secrets_dir()
            .join("chatgpt-sessions")
            .join(uuid::Uuid::new_v4().to_string());
        fsx::ensure_private_dir(&path)?;
        let session = SessionHome { path };
        if let Some(auth) = auth {
            fsx::write_atomic(
                &session.path.join("auth.json"),
                auth.expose(),
                fsx::Mode::Private,
            )?;
        }
        Ok(session)
    }
}

/// Temporary private credentials exist only for the lifetime of a helper child.
pub struct SessionHome {
    path: PathBuf,
}

impl SessionHome {
    /// Directory passed as CODEX_HOME to the helper.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
    /// Read the credential after Codex has saved/refreshed it.
    pub fn auth(&self) -> Result<SecretBytes> {
        fsx::read_optional(&self.path.join("auth.json"))?
            .map(SecretBytes::new)
            .ok_or_else(|| Error::invalid("Codex did not save a ChatGPT login"))
    }
}

impl Drop for SessionHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use superba_core::{DataDir, MapEnv};

    fn credential(id: &str, token: &str) -> SecretBytes {
        SecretBytes::new(
            serde_json::to_vec(&serde_json::json!({
                "auth_mode":"chatgpt",
                "tokens":{"id_token":{"email":format!("{id}@example.test"),"sub":id},
                    "account_id":id,"access_token":token,"refresh_token":format!("refresh-{token}")}
            }))
            .unwrap(),
        )
    }

    fn context(root: &std::path::Path) -> LocalContext {
        LocalContext::new(
            root.join("home"),
            DataDir::new(root.join("data")),
            Arc::new(MapEnv::new().with(crate::ENV_SUPERBA_SECRET_BACKEND, "file")),
        )
    }

    #[test]
    fn chatgpt_accounts_are_encrypted_deduplicated_and_selected_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let store = Accounts::new(context(dir.path()));
        let a = credential("a", "private-a-12345");
        let b = credential("b", "private-b-12345");
        let account_a = store.put(&a, Timestamp(1)).unwrap();
        let account_b = store.put(&b, Timestamp(2)).unwrap();
        let refreshed = credential("a", "private-refreshed-a");
        store.put(&refreshed, Timestamp(3)).unwrap();
        let list = store.list(Some(&b)).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(
            list.iter().find(|a| a.id == account_a.id).unwrap().added_at,
            Timestamp(1)
        );
        assert_eq!(list.iter().filter(|a| a.active).count(), 1);
        assert_eq!(store.load(&account_a.id).unwrap(), refreshed);
        assert_eq!(store.load(&account_b.id).unwrap(), b);
        assert!(store.load("not-a-managed-account").is_err());
        let sealed = std::fs::read(store.path()).unwrap();
        assert!(!sealed.windows(7).any(|w| w == b"private"));
        let public = serde_json::to_string(&list).unwrap();
        assert!(!public.contains("private") && !public.contains("refresh_token"));
        assert!(
            store
                .put(
                    &SecretBytes::new(br#"{"tokens":{}}"#.to_vec()),
                    Timestamp(4)
                )
                .is_err()
        );
    }

    #[test]
    fn chatgpt_live_refresh_never_replaces_a_new_account_and_sessions_are_disposable() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let store = Accounts::new(ctx.clone());
        let a = credential("a", "old");
        let b = credential("b", "new-current-account");
        let refreshed = credential("a", "refreshed");
        let account = store.put(&a, Timestamp(1)).unwrap();
        std::fs::create_dir_all(ctx.codex_home().root()).unwrap();
        let live = ctx.codex_home().auth_json();
        std::fs::write(&live, refreshed.expose()).unwrap();
        assert_eq!(
            store.load(&account.id).unwrap(),
            refreshed,
            "a live token refresh wins over the saved copy"
        );
        std::fs::write(&live, b.expose()).unwrap();
        store.refresh_live(&a, &refreshed).unwrap();
        assert_eq!(std::fs::read(&live).unwrap(), b.expose());
        let session = store.session(Some(&a)).unwrap();
        let path = session.path().to_path_buf();
        assert_eq!(session.auth().unwrap(), a);
        drop(session);
        assert!(!path.exists());
    }
}
