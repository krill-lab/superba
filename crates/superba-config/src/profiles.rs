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
        if !set.settings.analytics_preset_dropped {
            let changed = drop_old_analytics_preset(&mut set);
            set.settings.analytics_preset_dropped = true;
            // The switch service writes the applied one again at start.
            if !changed.is_empty() {
                set.settings.analytics_reapply = Some(superba_core::AnalyticsReapply {
                    profiles: changed,
                    failed_once: false,
                });
            }
            if let Err(e) = fsx::write_json(&path, &set) {
                // Runs again at the next load; this one already has it.
                tracing::warn!("profiles.json: could not record the bug 137 migration: {e}");
            }
        }
        Ok(set)
    }

    fn save_sync(&self, set: &ProfileSet) -> Result<()> {
        validate(set)?;
        let mut set = set.clone();
        normalize_builtins(&mut set);
        fsx::write_json(&self.ctx.data_dir.profiles_json(), &set)
    }
}

/// One-time migration, user bug 137 (approved exception to the no-migration
/// rule; remove after alpha.14+N with [`superba_core::Settings::analytics_preset_dropped`]):
/// Superba up to alpha.13 put `analytics.enabled = false` into every new Krill
/// profile, which keeps the desktop app from caching the remote flag of its
/// Chinese UI. Drop exactly that value from profiles of the built-in Krill
/// site; `true`, removals and other sites' profiles are the user's. The next
/// switch then restores the key to its baseline (the user's own value, or
/// none). Returns the ids of the profiles it changed.
pub fn drop_old_analytics_preset(set: &mut ProfileSet) -> Vec<superba_core::ProfileId> {
    let mut changed = Vec::new();
    for p in &mut set.profiles {
        if p.site_id.as_str() == catalog::KRILL_SITE_ID && p.codex_settings.openai_telemetry_off() {
            p.codex_settings.set_openai_telemetry_off(false);
            changed.push(p.id.clone());
        }
    }
    changed
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
