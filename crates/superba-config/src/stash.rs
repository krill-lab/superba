//! Encrypted stash of official ChatGPT logins (plan 9.4), per [`StashScope`] and
//! keyed by account identity: `backups/login-stash/<scope>/<identity>.enc` plus an
//! `index.json` naming the most recent one. The stash is refreshed whenever
//! Superba replaces a live login, so the newest refresh token is kept.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use superba_core::{
    AuthShape, Error, LocalContext, LoginStashInfo, Result, SecretBytes, StashScope, Timestamp,
};

use crate::authjson;
use crate::fsx;
use crate::vault::Vault;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Index {
    /// Identity of the most recently stashed login.
    #[serde(default)]
    latest: Option<String>,
    /// Scope description (for humans looking at the directory).
    #[serde(default)]
    scope: Option<StashScope>,
    #[serde(default)]
    entries: BTreeMap<String, LoginStashInfo>,
}

/// Login stash store.
#[derive(Clone, Debug)]
pub(crate) struct Stash {
    ctx: LocalContext,
    vault: Vault,
}

impl Stash {
    pub(crate) fn new(ctx: LocalContext) -> Self {
        let vault = Vault::new(ctx.clone());
        Self { ctx, vault }
    }

    fn dir(&self, scope: &StashScope) -> PathBuf {
        let key = match scope {
            StashScope::Local(p) => {
                format!("local-{}", fsx::short_hash(p.to_string_lossy().as_bytes()))
            }
            StashScope::Remote(t) => format!(
                "remote-{}-{}",
                fsx::sanitize_component(t.as_str()),
                fsx::short_hash(t.as_str().as_bytes())
            ),
        };
        self.ctx.data_dir.login_stash_dir().join(key)
    }

    fn index(&self, scope: &StashScope) -> Result<Index> {
        Ok(
            fsx::read_json(&self.dir(scope).join("index.json"), "login stash index")?
                .unwrap_or_default(),
        )
    }

    fn save_index(&self, scope: &StashScope, index: &Index) -> Result<()> {
        fsx::write_json(&self.dir(scope).join("index.json"), index)
    }

    /// Metadata of the latest stashed login.
    pub(crate) fn info(&self, scope: &StashScope) -> Result<Option<LoginStashInfo>> {
        let idx = self.index(scope)?;
        Ok(idx
            .latest
            .as_ref()
            .and_then(|id| idx.entries.get(id))
            .cloned())
    }

    /// Stash a ChatGPT login (must have a `tokens` object).
    pub(crate) fn put(
        &self,
        scope: &StashScope,
        auth_json: &SecretBytes,
        now: Timestamp,
    ) -> Result<LoginStashInfo> {
        let info = authjson::login_info(auth_json.expose()).ok_or_else(|| {
            Error::invalid(match authjson::shape(Some(auth_json.expose())) {
                AuthShape::Unknown { detail } => {
                    format!("only a ChatGPT login can be stashed ({detail})")
                }
                other => format!("only a ChatGPT login can be stashed, not {other:?}"),
            })
        })?;
        let dir = self.dir(scope);
        fsx::ensure_private_dir(&dir)?;
        let file = dir.join(format!("{}.enc", fsx::sanitize_component(&info.identity)));
        self.vault.write_sealed(&file, auth_json.expose())?;
        let meta = LoginStashInfo {
            stashed_at: now,
            last_refresh: info.last_refresh,
            id_token_exp: info.id_token_exp,
            email: info.email.clone(),
            account_id: info.account_id.clone(),
        };
        let mut idx = self.index(scope)?;
        idx.scope = Some(scope.clone());
        idx.latest = Some(info.identity.clone());
        idx.entries.insert(info.identity, meta.clone());
        self.save_index(scope, &idx)?;
        Ok(meta)
    }

    /// Decrypt the latest stashed login.
    pub(crate) fn load(&self, scope: &StashScope) -> Result<Option<SecretBytes>> {
        let idx = self.index(scope)?;
        let Some(id) = idx.latest else {
            return Ok(None);
        };
        let file = self
            .dir(scope)
            .join(format!("{}.enc", fsx::sanitize_component(&id)));
        self.vault.read_sealed(&file)
    }

    /// Forget a stashed identity (the user signed out of it in the app: never
    /// resurrect it). The latest pointer moves to the most recent remaining one.
    pub(crate) fn forget(&self, scope: &StashScope, identity: &str) -> Result<bool> {
        let mut idx = self.index(scope)?;
        if idx.entries.remove(identity).is_none() {
            return Ok(false);
        }
        let file = self
            .dir(scope)
            .join(format!("{}.enc", fsx::sanitize_component(identity)));
        fsx::remove_if_exists(&file)?;
        if idx.latest.as_deref() == Some(identity) {
            idx.latest = idx
                .entries
                .iter()
                .max_by_key(|(_, m)| m.stashed_at)
                .map(|(k, _)| k.clone());
        }
        self.save_index(scope, &idx)?;
        Ok(true)
    }
}
