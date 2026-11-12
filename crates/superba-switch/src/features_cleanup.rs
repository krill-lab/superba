//! One-time clean-up of user bug 144 (an approved exception to the
//! no-migration rule, like user bugs 137 and 139). Remove this module, its
//! call in `analytics_reapply.rs`, [`superba_core::Settings::features_cleanup`]
//! and the profile store's `drop_retired_feature_keys` together, after
//! alpha.17+N.
//!
//! Superba up to alpha.16 preset `features.context_management`,
//! `features.token_budget.enabled` and
//! `features.token_budget.use_history_notes_extension` in new Krill profiles
//! and wrote them at every switch; a hand edit of them was taken over again.
//! The profile store dropped them from stored profiles
//! ([`superba_core::codex::RETIRED_PRESET_KEYS`]). When the applied profile was
//! one of those, the first start of Superba's window afterwards writes it once
//! more, write-only, so the values Superba wrote leave `config.toml` without a
//! manual switch:
//!
//! - only keys Superba still owns with the value it wrote go (back to their
//!   baseline: removed, or the user's value from before Superba); a key the
//!   user changed since stays as it is and is no longer Superba's (it is not
//!   reported as changed outside Superba again); a key the user wrote is not
//!   Superba's and stays;
//! - the plan must change nothing but those keys (the same rules as the bug
//!   137 re-apply: no other key, file, environment line, question, router
//!   mode or remote host); otherwise nothing is written and the next switch
//!   removes them;
//! - one attempt, silent (logged): whatever happens, the record is cleared.

use superba_core::codex::RETIRED_PRESET_KEYS;

use crate::Switcher;
use crate::analytics_reapply::Skip;

impl Switcher {
    /// The clean-up owed after the profile store's bug 144 migration, at most
    /// once (the window calls it once per start).
    pub(crate) async fn features_cleanup_window(&self) {
        let d = &self.deps;
        let set = match d.profiles.load().await {
            Ok(set) => set,
            Err(e) => {
                tracing::warn!(error = %e, "user bug 144 clean-up: cannot read the profiles");
                return;
            }
        };
        if set.settings.features_cleanup.is_empty() {
            return;
        }
        let profiles = set.settings.features_cleanup.clone();
        match self
            .release_owned_keys(&set, &profiles, RETIRED_PRESET_KEYS, true, |_| true)
            .await
        {
            Ok(None) => tracing::info!("user bug 144 clean-up: nothing Superba wrote is left"),
            Ok(Some((id, changed))) => {
                tracing::info!(profile = %id, changed, "user bug 144 clean-up: written again");
            }
            Err((id, Skip::MoreThanTheKey)) => tracing::info!(
                profile = %id,
                "user bug 144 clean-up: the write would change more; left to the next switch"
            ),
            Err((id, Skip::Failed(reason))) => tracing::warn!(
                profile = %id,
                reason = %reason.render_fallback(),
                "user bug 144 clean-up failed; left to the next switch"
            ),
        }
        let result = async {
            let mut now = d.profiles.load().await?;
            now.settings.features_cleanup.clear();
            d.profiles.save(&now).await
        }
        .await;
        if let Err(e) = result {
            tracing::warn!(error = %e, "cannot record the user bug 144 clean-up");
        }
    }
}
