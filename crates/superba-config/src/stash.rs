//! Encrypted stash of official ChatGPT logins (plan 9.4), per [`StashScope`] and
//! keyed by account identity: `backups/login-stash/<scope>/<identity>.enc` plus an
//! `index.json` naming the most recent one. The stash is refreshed whenever
//! Superba replaces a live login, so the newest refresh token is kept.
//!
//! The entries of a local home are also its saved ChatGPT accounts (PR #20):
//! the user can add one through a browser sign-in, save the current login,
//! switch to one ([`superba_core::ResolvedMode::ChatGptAccount`]) or forget
//! one. An index written before accounts existed reads as is: every entry is
//! an account, the one `latest` names is the one a plain official restore
//! picks, and a missing first-save time is the entry's `stashed_at`.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use superba_core::{
    AuthShape, ChatGptAccount, Error, LocalContext, LoginStashInfo, Result, SecretBytes,
    StashScope, Timestamp,
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

/// Which login a plain official restore picks after a save.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Latest {
    /// This one (a switch replaced it: it is the one to come back to).
    Set,
    /// Only when none is named yet (the user added or saved an account).
    IfNone,
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

    fn file(&self, scope: &StashScope, identity: &str) -> PathBuf {
        self.dir(scope)
            .join(format!("{}.enc", fsx::sanitize_component(identity)))
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

    /// The lock every credential write of the stash and of a live `auth.json`
    /// takes: applies, rollbacks and recovery (whole), account saves and the
    /// short publish of rotated tokens. Never held across a network request.
    /// Taken once per thread of work (an OS file lock does not nest).
    pub(crate) fn lock(&self) -> Result<File> {
        let dir = self.ctx.data_dir.login_stash_dir();
        fsx::ensure_private_dir(&dir)?;
        let path = dir.join("credentials.lock");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| Error::io(&path, &e))?;
        file.lock().map_err(|e| Error::io(&path, &e))?;
        Ok(file)
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

    /// Stash a ChatGPT login (must have a `tokens` object); it becomes the
    /// latest one. A saved copy of the same identity whose tokens are newer
    /// (`last_refresh`; rotated by a limits read after this login was read)
    /// is kept: the older copy's refresh token no longer works.
    pub(crate) fn put(
        &self,
        scope: &StashScope,
        auth_json: &SecretBytes,
        now: Timestamp,
    ) -> Result<LoginStashInfo> {
        self.save(scope, auth_json, now, Latest::Set)
    }

    /// [`Stash::put`] with a choice of the latest pointer.
    pub(crate) fn save(
        &self,
        scope: &StashScope,
        auth_json: &SecretBytes,
        now: Timestamp,
        latest: Latest,
    ) -> Result<LoginStashInfo> {
        let info = login_of(auth_json)?;
        let dir = self.dir(scope);
        fsx::ensure_private_dir(&dir)?;
        let mut idx = self.index(scope)?;
        let previous = idx.entries.get(&info.identity).cloned();
        let newer_saved = previous
            .as_ref()
            .and_then(|p| p.last_refresh)
            .zip(info.last_refresh)
            .is_some_and(|(saved, incoming)| saved > incoming)
            && self.file(scope, &info.identity).is_file();
        let meta = if newer_saved {
            let mut kept = previous.clone().unwrap_or_else(|| meta_of(&info, now));
            kept.added_at = Some(kept.added_at.unwrap_or(kept.stashed_at));
            kept.stashed_at = now;
            kept
        } else {
            self.vault
                .write_sealed(&self.file(scope, &info.identity), auth_json.expose())?;
            let mut meta = meta_of(&info, now);
            meta.added_at = Some(
                previous
                    .as_ref()
                    .map_or(now, |p| p.added_at.unwrap_or(p.stashed_at)),
            );
            meta
        };
        idx.scope = Some(scope.clone());
        if latest == Latest::Set || idx.latest.is_none() {
            idx.latest = Some(info.identity.clone());
        }
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
        self.vault.read_sealed(&self.file(scope, &id))
    }

    /// Decrypt the stashed login with exactly this identity.
    pub(crate) fn load_identity(
        &self,
        scope: &StashScope,
        identity: &str,
    ) -> Result<Option<SecretBytes>> {
        if !self.index(scope)?.entries.contains_key(identity) {
            return Ok(None);
        }
        self.vault.read_sealed(&self.file(scope, identity))
    }

    /// The stashed logins as accounts, oldest first; `live` is the live
    /// `auth.json` of the scope (marks the active one).
    pub(crate) fn accounts(
        &self,
        scope: &StashScope,
        live: Option<&[u8]>,
    ) -> Result<Vec<ChatGptAccount>> {
        let idx = self.index(scope)?;
        let live = live.and_then(authjson::login_info);
        let mut out: Vec<ChatGptAccount> = idx
            .entries
            .iter()
            .map(|(id, m)| {
                let active = live.as_ref().filter(|l| &l.identity == id);
                ChatGptAccount {
                    id: id.clone(),
                    email: m
                        .email
                        .clone()
                        .or_else(|| m.account_id.clone())
                        .unwrap_or_else(|| id.clone()),
                    plan_type: active
                        .and_then(|l| l.plan_type.clone())
                        .or_else(|| m.plan_type.clone()),
                    added_at: m.added_at.unwrap_or(m.stashed_at),
                    last_refresh: active.and_then(|l| l.last_refresh).or(m.last_refresh),
                    active: active.is_some(),
                    latest: idx.latest.as_deref() == Some(id.as_str()),
                }
            })
            .collect();
        out.sort_by(|a, b| a.added_at.cmp(&b.added_at).then_with(|| a.id.cmp(&b.id)));
        Ok(out)
    }

    /// One account's view (after a save).
    pub(crate) fn account(
        &self,
        scope: &StashScope,
        identity: &str,
        live: Option<&[u8]>,
    ) -> Result<ChatGptAccount> {
        self.accounts(scope, live)?
            .into_iter()
            .find(|a| a.id == identity)
            .ok_or_else(|| Error::NotFound {
                what: "saved ChatGPT account".into(),
            })
    }

    /// Replace the saved copy of `before`'s identity with `after` only while
    /// it still holds `before` (a newer save wins). Returns whether it did.
    pub(crate) fn replace_if_unchanged(
        &self,
        scope: &StashScope,
        before: &SecretBytes,
        after: &SecretBytes,
        now: Timestamp,
    ) -> Result<bool> {
        let info = login_of(after)?;
        let Some(saved) = self.load_identity(scope, &info.identity)? else {
            return Ok(false);
        };
        if saved != *before {
            return Ok(false);
        }
        let mut idx = self.index(scope)?;
        self.vault
            .write_sealed(&self.file(scope, &info.identity), after.expose())?;
        let previous = idx.entries.get(&info.identity).cloned();
        let mut meta = meta_of(&info, now);
        if let Some(p) = previous {
            meta.stashed_at = p.stashed_at;
            meta.added_at = Some(p.added_at.unwrap_or(p.stashed_at));
        }
        idx.entries.insert(info.identity, meta);
        self.save_index(scope, &idx)?;
        Ok(true)
    }

    /// Forget a stashed identity (the user signed out of it in the app: never
    /// resurrect it). The latest pointer moves to the most recent remaining one.
    pub(crate) fn forget(&self, scope: &StashScope, identity: &str) -> Result<bool> {
        let mut idx = self.index(scope)?;
        if idx.entries.remove(identity).is_none() {
            return Ok(false);
        }
        fsx::remove_if_exists(&self.file(scope, identity))?;
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

/// The login details of a ChatGPT `auth.json`, or why it cannot be stashed.
fn login_of(auth_json: &SecretBytes) -> Result<authjson::LoginInfo> {
    let info = authjson::login_info(auth_json.expose()).ok_or_else(|| {
        Error::invalid(match authjson::shape(Some(auth_json.expose())) {
            AuthShape::Unknown { detail } => {
                format!("only a ChatGPT login can be stashed ({detail})")
            }
            other => format!("only a ChatGPT login can be stashed, not {other:?}"),
        })
    })?;
    Ok(info)
}

fn meta_of(info: &authjson::LoginInfo, now: Timestamp) -> LoginStashInfo {
    LoginStashInfo {
        stashed_at: now,
        last_refresh: info.last_refresh,
        id_token_exp: info.id_token_exp,
        email: info.email.clone(),
        account_id: info.account_id.clone(),
        plan_type: info.plan_type.clone(),
        added_at: None,
    }
}

/// The live `auth.json` of a local scope (`None` for remote scopes).
pub(crate) fn live_auth(scope: &StashScope) -> Result<Option<Vec<u8>>> {
    match scope {
        StashScope::Local(root) => fsx::read_optional(&live_path(root)),
        StashScope::Remote(_) => Ok(None),
    }
}

/// `auth.json` of a local Codex home.
pub(crate) fn live_path(root: &Path) -> PathBuf {
    root.join("auth.json")
}
