//! User bug 137, after the one-time migration of the profile store (remove
//! after alpha.14+N together with it): when the migration took the old Krill
//! preset `analytics.enabled = false` out of the applied profile and that
//! value, as Superba wrote it, is still in `config.toml`, the applied profile
//! is written once more at start through the normal plan and execute path,
//! write-only (like the start-up catalog repair: nothing is restarted; the
//! app reads the file at its next start). The key is released to its
//! baseline: removed, or the user's own value. A failure is reported with
//! the switch's own error and tried once more at the next start, then given
//! up; nothing loops.

use superba_core::{
    ExecuteOptions, KeyResolution, NoEvents, Notice, OutcomeStatus, RestartKind, SafeDefaults,
    SwitchIntent, SwitchRequest, codex::config_keys,
};

use crate::Switcher;
use crate::messages::text;

impl Switcher {
    /// The re-apply owed after the migration, at most one attempt per call.
    pub(crate) async fn analytics_reapply(&self) -> Vec<Notice> {
        let d = &self.deps;
        let Ok(mut set) = d.profiles.load().await else {
            return Vec::new();
        };
        let Some(owed) = set.settings.analytics_reapply.clone() else {
            return Vec::new();
        };
        let home = d.ctx.codex_home();
        let state = d.config.read_state(&home).await.ok();
        let active = state.as_ref().and_then(|s| s.active.clone());
        let applied = active
            .as_ref()
            .and_then(|a| a.profile_id.clone())
            .filter(|id| owed.profiles.contains(id));
        // Superba's own `false` is still in the file (owned, not changed since).
        let written = match &applied {
            Some(_) => {
                let owned = d.config.owned_keys(&home).await.unwrap_or_default();
                let drifted = state.as_ref().is_some_and(|s| {
                    s.drift
                        .iter()
                        .any(|k| k.path == config_keys::ANALYTICS_ENABLED)
                });
                !drifted
                    && owned.iter().any(|k| {
                        k.path == config_keys::ANALYTICS_ENABLED
                            && k.value.as_deref() == Some("false")
                    })
            }
            None => false,
        };
        let (Some(id), true, Some(active)) = (applied, written, active) else {
            set.settings.analytics_reapply = None;
            self.save_reapply(&set).await;
            return Vec::new();
        };
        let label = set
            .profile(&id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| id.to_string());
        // The line and auth in use stay.
        let mut request = SwitchRequest::profile(id.clone());
        request.intent = SwitchIntent::Profile {
            profile_id: id,
            line_id: active.line_id,
            auth_mode: active.auth_mode,
        };
        let result = self.rewrite_profile(request).await;
        let note = match result {
            Ok(()) => {
                set.settings.analytics_reapply = None;
                Notice::info(text("switch.analytics.reapplied").arg("profile", &label))
            }
            Err(reason) => {
                let last = owed.failed_once;
                set.settings.analytics_reapply =
                    (!last).then_some(superba_core::AnalyticsReapply {
                        failed_once: true,
                        ..owed
                    });
                let key = if last {
                    "switch.analytics.reapply_given_up"
                } else {
                    "switch.analytics.reapply_retry"
                };
                Notice::danger(text(key).arg("profile", &label).arg_text("reason", reason))
            }
        };
        self.save_reapply(&set).await;
        tracing::info!(note = %note.text.render_fallback(), "user bug 137 re-apply");
        vec![note]
    }

    /// Store the re-apply state (other settings as they are on disk now).
    async fn save_reapply(&self, set: &superba_core::ProfileSet) {
        let d = &self.deps;
        let result = async {
            let mut now = d.profiles.load().await?;
            now.settings.analytics_reapply = set.settings.analytics_reapply.clone();
            d.profiles.save(&now).await
        }
        .await;
        if let Err(e) = result {
            tracing::warn!(error = %e, "cannot record the user bug 137 re-apply");
        }
    }

    /// Plan `request` and execute it write-only, keeping the user's own edits
    /// of owned keys (like the start-up catalog repair). Any question or
    /// block is a failure: nothing here asks the user.
    async fn rewrite_profile(
        &self,
        mut request: SwitchRequest,
    ) -> std::result::Result<(), superba_core::LocalizedText> {
        let d = &self.deps;
        request.accept_unknown_versions = true;
        if let Ok(state) = d.config.read_state(&d.ctx.codex_home()).await {
            for k in &state.drift {
                request
                    .key_resolutions
                    .insert(k.path.clone(), KeyResolution::KeepCurrent);
            }
        }
        let plan = self.plan_impl(request).await.map_err(|e| e.to_text())?;
        let pc = self.take_plan(&plan.id);
        if let RestartKind::Blocked { reasons } = &plan.primary {
            return Err(reasons
                .first()
                .map(superba_core::BlockReason::text)
                .unwrap_or_else(|| text("switch.analytics.question")));
        }
        if !plan.choices.is_empty() {
            return Err(text("switch.analytics.question"));
        }
        let pc = pc.ok_or_else(|| text("switch.analytics.question"))?;
        self.remember_plan(plan.id.clone(), pc);
        let outcome = self
            .execute_impl(
                plan,
                ExecuteOptions {
                    write_only: true,
                    force_turns: true,
                    allow_full_restart: true,
                    ..Default::default()
                },
                std::sync::Arc::new(SafeDefaults),
                std::sync::Arc::new(NoEvents),
            )
            .await
            .map_err(|e| e.to_text())?;
        match outcome.status {
            OutcomeStatus::WrittenOnly | OutcomeStatus::Switched => Ok(()),
            _ => Err(text("switch.analytics.question")),
        }
    }
}
