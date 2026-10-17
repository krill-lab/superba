//! [`JsonProfileStore`]: `profiles.json` (profiles, custom sites, settings).

use std::collections::BTreeSet;

use async_trait::async_trait;
use superba_core::{Error, LocalContext, ProfileSet, ProfileStore, Result, catalog};

use crate::fsx;

/// Newest `profiles.json` schema this build understands.
pub const PROFILES_SCHEMA: u32 = superba_core::PROFILE_SET_SCHEMA;

/// [`ProfileStore`] persisting `profiles.json` in the data dir (0600, atomic).
#[derive(Debug, Clone)]
pub struct JsonProfileStore {
    ctx: LocalContext,
}

impl JsonProfileStore {
    /// Create a store for `ctx.data_dir.profiles_json()`.
    pub fn new(ctx: LocalContext) -> Self {
        Self { ctx }
    }

    /// The context this store was created with.
    pub fn context(&self) -> &LocalContext {
        &self.ctx
    }

    fn load_sync(&self) -> Result<ProfileSet> {
        let path = self.ctx.data_dir.profiles_json();
        let Some(set) = fsx::read_json::<ProfileSet>(&path, "profiles.json")? else {
            return Ok(ProfileSet::default());
        };
        if set.schema > PROFILES_SCHEMA {
            return Err(Error::Unsupported {
                what: format!(
                    "profiles.json schema {} was written by a newer Superba (this build reads {PROFILES_SCHEMA})",
                    set.schema
                ),
            });
        }
        validate(&set)?;
        let mut set = set;
        normalize_builtins(&mut set);
        Ok(set)
    }

    fn save_sync(&self, set: &ProfileSet) -> Result<()> {
        validate(set)?;
        let mut set = set.clone();
        normalize_builtins(&mut set);
        fsx::write_json(&self.ctx.data_dir.profiles_json(), &set)
    }
}

/// Built-in profiles always come from the catalog (first, in catalog order);
/// stored copies of them are replaced, user profiles keep their order.
fn normalize_builtins(set: &mut ProfileSet) {
    let builtins = catalog::builtin_profiles();
    let builtin_ids: BTreeSet<_> = builtins.iter().map(|p| p.id.clone()).collect();
    let user: Vec<_> = set
        .profiles
        .drain(..)
        .filter(|p| p.builtin.is_none() && !builtin_ids.contains(&p.id))
        .collect();
    set.profiles = builtins.into_iter().chain(user).collect();
}

fn validate(set: &ProfileSet) -> Result<()> {
    let mut ids = BTreeSet::new();
    for p in &set.profiles {
        if p.id.as_str().trim().is_empty() {
            return Err(Error::invalid("a profile has an empty id"));
        }
        if !ids.insert(p.id.as_str()) {
            return Err(Error::invalid(format!("duplicate profile id {}", p.id)));
        }
    }
    let mut site_ids = BTreeSet::new();
    for s in &set.custom_sites {
        if catalog::builtin_sites().iter().any(|b| b.id == s.id) {
            return Err(Error::invalid(format!(
                "custom site {} uses a built-in site id",
                s.id
            )));
        }
        if !site_ids.insert(s.id.as_str()) {
            return Err(Error::invalid(format!("duplicate site id {}", s.id)));
        }
    }
    Ok(())
}

#[async_trait]
impl ProfileStore for JsonProfileStore {
    async fn load(&self) -> Result<ProfileSet> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.load_sync())
            .await
            .map_err(|e| Error::other(format!("profile store task failed: {e}")))?
    }

    async fn save(&self, set: &ProfileSet) -> Result<()> {
        let this = self.clone();
        let set = set.clone();
        tokio::task::spawn_blocking(move || this.save_sync(&set))
            .await
            .map_err(|e| Error::other(format!("profile store task failed: {e}")))?
    }
}
