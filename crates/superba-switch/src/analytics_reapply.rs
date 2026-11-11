//! User bug 137, after the one-time migration of the profile store (remove
//! after alpha.14+N together with it): when the migration took the old Krill
//! preset `analytics.enabled = false` out of the applied profile and that
//! value, as Superba wrote it, is still in `config.toml`, Superba's window
//! releases that one key at start ([`superba_core::SwitchService::analytics_reapply`];
//! `spb` never does). The applied profile is planned through the normal
//! planner and executed write-only (nothing is restarted; the app reads the
//! file at its next start), but only when that plan changes nothing except
//! `analytics.enabled`: a profile with saved but not yet applied edits, a new
//! catalog file, another auth.json or anything else is not written, and a
//! notice asks for one switch instead (saving never applies). The profile
//! store is not touched (no line order, auth choice or last selection). The
//! notice stays owed until the window showed it. The key goes back to its baseline:
//! removed, or the user's own value. A failure (with the switch's own reason)
//! is tried once more at the next start of the window, then given up;
//! nothing loops.

use superba_core::{
    CodexFileKind, Error, ExecuteOptions, FileChange, KeyResolution, LocalizedText, NoEvents,
    Notice, OutcomeStatus, ProfileSet, RestartKind, SafeDefaults, Severity, SwitchIntent,
    SwitchOutcome, SwitchPlan, SwitchRequest, codex::config_keys,
};

use crate::Switcher;
use crate::messages::text;

/// Why the owed write did not happen.
enum Skip {
    /// Something failed: retried once at the next start.
    Failed(LocalizedText),
    /// The plan would change more than the key: the user switches once.
    MoreThanTheKey,
}

impl Switcher {
    /// The write owed after the migration, at most one attempt per call (the
    /// window calls it once per start). A notice is kept as undelivered until
    /// [`Self::analytics_reapply_delivered_impl`]: a start whose window never
    /// showed it shows the same notice again (nothing is written again).
    pub(crate) async fn analytics_reapply_impl(&self) -> Vec<Notice> {
        // One-off alpha.16 PAT→API-key migration (user bug 139): it rides on
        // this once-per-start call; remove with `crate::pat_migration`.
        let mut notes = self.pat_migration_window().await;
        notes.extend(self.analytics_reapply_owed().await);
        notes
    }

    async fn analytics_reapply_owed(&self) -> Vec<Notice> {
        let d = &self.deps;
        let mut set = match d.profiles.load().await {
            Ok(set) => set,
            // The debt (on disk) stays for the next start.
            Err(e) => {
                return vec![Notice::danger(
                    text("switch.analytics.store_unreadable").arg_text("reason", e.to_text()),
                )];
            }
        };
        let Some(owed) = set.settings.analytics_reapply.clone() else {
            return Vec::new();
        };
        if let Some(n) = &owed.undelivered {
            return vec![n.clone()];
        }
        let result = self.reapply_owed(&set, &owed).await;
        let label = |id: &superba_core::ProfileId| {
            set.profile(id)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| id.to_string())
        };
        let note = match result {
            Ok(None) | Ok(Some((_, false))) => None,
            Ok(Some((id, true))) => Some(Notice::info(
                text("switch.analytics.reapplied").arg("profile", label(&id)),
            )),
            Err((id, Skip::MoreThanTheKey)) => Some(Notice::warning(
                text("switch.analytics.switch_once").arg("profile", label(&id)),
            )),
            Err((id, Skip::Failed(reason))) => {
                let key = if owed.failed_once {
                    "switch.analytics.reapply_given_up"
                } else {
                    "switch.analytics.reapply_retry"
                };
                Some(Notice::danger(
                    text(key)
                        .arg("profile", label(&id))
                        .arg_text("reason", reason),
                ))
            }
        };
        // Nothing to tell: settled now. Otherwise settled (or, after a first
        // failure, left for one retry) once the window showed the notice.
        set.settings.analytics_reapply = note.as_ref().map(|n| {
            let retry = n.severity == Severity::Danger && !owed.failed_once;
            superba_core::AnalyticsReapply {
                profiles: owed.profiles.clone(),
                failed_once: owed.failed_once || retry,
                undelivered: Some(n.clone()),
                finished: !retry,
            }
        });
        self.save_reapply(&set).await;
        let notes: Vec<Notice> = note.into_iter().collect();
        for n in &notes {
            tracing::info!(note = %n.text.render_fallback(), "user bug 137 re-apply");
        }
        notes
    }

    /// The window showed the notice: settle it (a first failure stays owed
    /// for one retry at the next start).
    pub(crate) async fn analytics_reapply_delivered_impl(&self) -> superba_core::Result<()> {
        // One-off alpha.16 PAT→API-key migration (user bug 139).
        self.pat_migration_settle().await?;
        let d = &self.deps;
        let mut set = d.profiles.load().await?;
        let Some(owed) = set.settings.analytics_reapply.as_mut() else {
            return Ok(());
        };
        if owed.undelivered.is_none() {
            return Ok(());
        }
        if owed.finished {
            set.settings.analytics_reapply = None;
        } else {
            owed.undelivered = None;
        }
        d.profiles.save(&set).await
    }

    /// `Ok(None)`: nothing to do. `Ok(Some((id, changed)))`: `analytics.enabled`
    /// of the applied profile `id` was released; `changed` when that changed
    /// the file (not when the user's own `false` is its baseline).
    async fn reapply_owed(
        &self,
        set: &ProfileSet,
        owed: &superba_core::AnalyticsReapply,
    ) -> std::result::Result<Option<(superba_core::ProfileId, bool)>, (superba_core::ProfileId, Skip)>
    {
        let d = &self.deps;
        let home = d.ctx.codex_home();
        // Unknown which profile is applied: try again at the next start.
        let first = owed
            .profiles
            .first()
            .cloned()
            .unwrap_or_else(|| superba_core::ProfileId::new(""));
        let fail = |id: &superba_core::ProfileId, e: Error| (id.clone(), Skip::Failed(e.to_text()));
        let state = d
            .config
            .read_state(&home)
            .await
            .map_err(|e| fail(&first, e))?;
        let Some(active) = state.active.clone() else {
            return Ok(None);
        };
        let Some(id) = active
            .profile_id
            .clone()
            .filter(|id| owed.profiles.contains(id))
        else {
            return Ok(None);
        };
        // Superba's own `false` is still in the file (owned, not changed since).
        let owned = d.config.owned_keys(&home).await.map_err(|e| fail(&id, e))?;
        let drifted = state
            .drift
            .iter()
            .any(|k| k.path == config_keys::ANALYTICS_ENABLED);
        let written = owned.iter().any(|k| {
            k.path == config_keys::ANALYTICS_ENABLED && k.value.as_deref() == Some("false")
        });
        if drifted || !written || set.profile(&id).is_none() {
            return Ok(None);
        }
        // Router mode plans the router's members and its base URL: more than
        // the key (and the planner branches there).
        if set.settings.router.enabled {
            return Err((id, Skip::MoreThanTheKey));
        }
        // The line in use stays; the auth is the profile's own choice
        // (Automatic stays Automatic: an auth.json that would change stops the
        // write below); the user's own edits of owned keys stay too.
        let mut request = SwitchRequest::profile(id.clone());
        request.intent = SwitchIntent::Profile {
            profile_id: id.clone(),
            line_id: active.line_id,
            auth_mode: None,
        };
        for k in &state.drift {
            request
                .key_resolutions
                .insert(k.path.clone(), KeyResolution::KeepCurrent);
        }
        let plan = self.plan_impl(request).await.map_err(|e| fail(&id, e))?;
        let mut pc = self.take_plan(&plan.id);
        if let RestartKind::Blocked { reasons } = &plan.primary {
            let reason = reasons
                .first()
                .map(superba_core::BlockReason::text)
                .unwrap_or_else(|| text("switch.outcome.failed"));
            return Err((id, Skip::Failed(reason)));
        }
        if !only_the_key(&plan) || !pc.as_ref().is_some_and(only_the_key_context) {
            return Err((id, Skip::MoreThanTheKey));
        }
        let changed = plan
            .config
            .as_ref()
            .is_some_and(|c| !c.key_changes.is_empty());
        let Some(mut pc) = pc.take() else {
            return Err((id, Skip::Failed(text("switch.outcome.failed"))));
        };
        // The profile store stays as it is: no line order, auth choice or
        // last selection is recorded for this write. Nothing is deleted
        // either: no catalog record (whose commit prunes unreferenced
        // `superba/models-*.json` files), no pruning of the switch journal or
        // of older switch backups.
        pc.profile_update = None;
        pc.catalog_record = None;
        pc.keep_history = true;
        let mut plan = plan;
        if let Some(c) = plan.config.as_mut() {
            c.keep_history = true;
        }
        self.remember_plan(plan.id.clone(), pc);
        let outcome = self
            .execute_impl(
                plan,
                ExecuteOptions {
                    write_only: true,
                    force_turns: true,
                    ..Default::default()
                },
                std::sync::Arc::new(SafeDefaults),
                std::sync::Arc::new(NoEvents),
            )
            .await
            .map_err(|e| fail(&id, e))?;
        match outcome.status {
            OutcomeStatus::WrittenOnly | OutcomeStatus::Switched => Ok(Some((id, changed))),
            _ => Err((id, Skip::Failed(outcome_reason(&outcome)))),
        }
    }

    /// Store the re-apply state (other settings as they are on disk now).
    async fn save_reapply(&self, set: &ProfileSet) {
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
}

/// The plan changes no key of `config.toml` but `analytics.enabled` (none
/// when the baseline is the same value) and nothing else: no other file,
/// environment variable, session migration, remote host, or question
/// (unknown versions included).
fn only_the_key(plan: &SwitchPlan) -> bool {
    let Some(config) = &plan.config else {
        return false;
    };
    let keys_ok = config
        .key_changes
        .iter()
        .all(|k| k.path == config_keys::ANALYTICS_ENABLED);
    let files_ok = config
        .files
        .iter()
        .all(|f| f.kind == CodexFileKind::ConfigToml || matches!(f.change, FileChange::Unchanged));
    let env_ok = [&plan.env, &plan.certificate_env]
        .iter()
        .all(|e| e.as_ref().is_none_or(|e| e.is_noop()));
    keys_ok
        && files_ok
        && env_ok
        && plan.choices.is_empty()
        && plan.migration.is_none()
        && plan.remotes.is_empty()
}

/// Nothing the executor writes outside the plan: no router, pin, host sync,
/// uninstall or rollback, and a model catalog file only when the very same
/// bytes are already there (`write_catalog_file` then writes nothing).
fn only_the_key_context(pc: &crate::planner::PlanContext) -> bool {
    let catalog_ok = pc
        .catalog
        .as_ref()
        .is_none_or(|w| std::fs::read(&w.path).is_ok_and(|b| b == w.bytes));
    catalog_ok
        && pc.router_update.is_none()
        && pc.router_enabled_after.is_none()
        && pc.official_client.is_none()
        && pc.host_sync.is_none()
        && pc.pin.is_none()
        && pc.pin_config.is_none()
        && pc.uninstall.is_none()
        && pc.rollback_migration.is_none()
        && pc.rollback_remote.is_none()
        && pc.wsl_pending.is_empty()
}

/// The reason a write-only execution did not finish: its first error notice,
/// else its first warning, else the outcome itself (rolled back, failed, …).
fn outcome_reason(outcome: &SwitchOutcome) -> LocalizedText {
    [Severity::Danger, Severity::Warning]
        .iter()
        .find_map(|s| outcome.notices.iter().find(|n| n.severity == *s))
        .map(|n| n.text.clone())
        .unwrap_or_else(|| crate::messages::outcome(outcome.status))
}
