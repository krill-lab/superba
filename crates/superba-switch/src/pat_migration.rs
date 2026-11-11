//! One-off alpha.16 PAT→API-key migration (user bug 139; an approved
//! exception to the no-migration rule, like user bug 137's). Remove this
//! module, its two calls in `analytics_reapply.rs`, its notices and
//! [`superba_core::Settings::pat_migration`] together.
//!
//! Superba up to alpha.15 wrote a Krill key as a personal access token
//! (`{"personal_access_token":K,"OPENAI_API_KEY":K}` with `chatgpt_base_url`
//! and `CODEX_AUTHAPI_BASE_URL`). Desktop app 26.1106 never confirms an account
//! for one, so queued messages fail with "Account is not confirmed". Profiles
//! saved that way already read as API-key profiles; this writes the applied
//! one again once, at the first start of Superba's window after the upgrade,
//! when `auth.json` still holds the token Superba wrote:
//!
//! - The same key is used as an API key (it is the key both fields held; Krill
//!   lines and the local router take it as one). The switch probes the line
//!   with it first: a line that refuses it blocks the write, and a notice asks
//!   for a key instead of guessing one.
//! - The applied profile is planned through the normal planner (its line in
//!   use, the user's changed keys kept, foreign shell lines left alone) and
//!   executed write-only: nothing is restarted and a running app is not
//!   interrupted; the app reads the files at its next start. The switch keeps
//!   its usual backups (generations, journal, environment ownership). The
//!   profile store and the last selection are not touched.
//! - A plan that needs an answer (a question, an unknown version) is not
//!   written; a notice asks for one switch. A failure is tried once more at the
//!   next start, then given up with a notice.
//! - Every outcome is settled once its notice was shown; nothing runs twice.

use superba_core::{
    AuthShape, BlockReason, Error, ExecuteOptions, KeyResolution, LocalizedText, NoEvents, Notice,
    OutcomeStatus, PatMigration, ProfileId, ProfileSet, RestartKind, SafeDefaults, Severity,
    SwitchIntent, SwitchRequest,
};

use crate::Switcher;
use crate::messages::text;

/// Why the write did not happen.
enum Skip {
    /// The Codex home could not be read: nothing is said or recorded, the
    /// next start checks again.
    Unchecked,
    /// The line does not take the key as an API key (or there is none).
    NeedsKey,
    /// The plan needs an answer from the user.
    SwitchOnce,
    /// Something failed: retried once at the next start.
    Failed(LocalizedText),
}

impl Switcher {
    /// The migration, at most one attempt per call (the window calls it once
    /// per start, with the bug 137 re-apply). Its notice stays undelivered
    /// until [`Self::pat_migration_delivered`].
    pub(crate) async fn pat_migration(&self) -> Vec<Notice> {
        let d = &self.deps;
        let mut set = match d.profiles.load().await {
            Ok(set) => set,
            Err(e) => {
                tracing::warn!(error = %e, "PAT migration: the profile store cannot be read");
                return Vec::new();
            }
        };
        let state = set.settings.pat_migration.clone().unwrap_or_default();
        if state.settled {
            return Vec::new();
        }
        if let Some(n) = &state.undelivered {
            return vec![n.clone()];
        }
        let label = |id: &ProfileId| {
            set.profile(id)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| id.to_string())
        };
        let note = match self.pat_reapply(&set).await {
            Err((_, Skip::Unchecked)) => return Vec::new(),
            Ok(None) => None,
            Ok(Some((id, running))) => Some(Notice::info(
                text(if running {
                    "switch.pat_migration.done_restart"
                } else {
                    "switch.pat_migration.done"
                })
                .arg("profile", label(&id)),
            )),
            Err((id, Skip::NeedsKey)) => Some(Notice::warning(
                text("switch.pat_migration.needs_key").arg("profile", label(&id)),
            )),
            Err((id, Skip::SwitchOnce)) => Some(Notice::warning(
                text("switch.pat_migration.switch_once").arg("profile", label(&id)),
            )),
            Err((id, Skip::Failed(reason))) => Some(Notice::danger(
                text(if state.failed_once {
                    "switch.pat_migration.given_up"
                } else {
                    "switch.pat_migration.retry"
                })
                .arg("profile", label(&id))
                .arg_text("reason", reason),
            )),
        };
        set.settings.pat_migration = Some(match &note {
            None => PatMigration {
                settled: true,
                ..PatMigration::default()
            },
            Some(n) => {
                let retry = n.severity == Severity::Danger && !state.failed_once;
                PatMigration {
                    settled: false,
                    failed_once: state.failed_once || retry,
                    undelivered: Some(n.clone()),
                    finished: !retry,
                }
            }
        });
        self.save_pat_migration(&set).await;
        let notes: Vec<Notice> = note.into_iter().collect();
        for n in &notes {
            tracing::info!(note = %n.text.render_fallback(), "PAT migration (user bug 139)");
        }
        notes
    }

    /// The window showed the notice: settle it (a first failure stays owed
    /// for one retry at the next start).
    pub(crate) async fn pat_migration_delivered(&self) -> superba_core::Result<()> {
        let d = &self.deps;
        let mut set = d.profiles.load().await?;
        let Some(state) = set.settings.pat_migration.as_mut() else {
            return Ok(());
        };
        if state.undelivered.is_none() {
            return Ok(());
        }
        state.undelivered = None;
        if state.finished {
            state.settled = true;
        }
        d.profiles.save(&set).await
    }

    /// `Ok(None)`: nothing to migrate. `Ok(Some((id, running)))`: the applied
    /// profile `id` was written with its key as an API key; `running` when the
    /// app runs (it reads the files at its next start).
    async fn pat_reapply(
        &self,
        set: &ProfileSet,
    ) -> std::result::Result<Option<(ProfileId, bool)>, (ProfileId, Skip)> {
        let d = &self.deps;
        let home = d.ctx.codex_home();
        let unknown = ProfileId::new("");
        let fail = |id: &ProfileId, e: Error| (id.clone(), Skip::Failed(e.to_text()));
        let state = d.config.read_state(&home).await.map_err(|e| {
            tracing::warn!(error = %e, "PAT migration: the Codex home cannot be read");
            (unknown.clone(), Skip::Unchecked)
        })?;
        // Only the token Superba wrote: not another tool's, not after a
        // hand-off, not a changed credential.
        if !matches!(state.auth, AuthShape::Pat { .. })
            || state.handed_off.is_some()
            || state.auth_drift.is_some()
        {
            return Ok(None);
        }
        let Some(active) = state.active.clone() else {
            return Ok(None);
        };
        let Some(id) = active.profile_id.clone().filter(|id| {
            set.profile(id)
                .is_some_and(|p| !p.is_builtin() && p.auth_mode.uses_key())
        }) else {
            return Ok(None);
        };
        let mut request = SwitchRequest::profile(id.clone());
        request.intent = SwitchIntent::Profile {
            profile_id: id.clone(),
            line_id: active.line_id,
            auth_mode: None,
        };
        request.take_over_foreign_env = Some(false);
        for k in &state.drift {
            request
                .key_resolutions
                .insert(k.path.clone(), KeyResolution::KeepCurrent);
        }
        let plan = self.plan_impl(request).await.map_err(|e| fail(&id, e))?;
        let pc = self.take_plan(&plan.id);
        if let RestartKind::Blocked { reasons } = &plan.primary {
            if reasons.iter().any(|r| {
                matches!(
                    r,
                    BlockReason::MissingKey
                        | BlockReason::KeyRejected { .. }
                        | BlockReason::AuthModeUnavailable { .. }
                )
            }) {
                return Err((id, Skip::NeedsKey));
            }
            let reason = reasons
                .first()
                .map(BlockReason::text)
                .unwrap_or_else(|| text("switch.outcome.failed"));
            return Err((id, Skip::Failed(reason)));
        }
        if !plan.choices.is_empty() {
            return Err((id, Skip::SwitchOnce));
        }
        let Some(mut pc) = pc else {
            return Err((id, Skip::Failed(text("switch.outcome.failed"))));
        };
        // The profile store stays as it is (line order, last selection).
        pc.profile_update = None;
        self.remember_plan(plan.id.clone(), pc);
        let running = d.app.running_session().await.ok().flatten().is_some();
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
            OutcomeStatus::WrittenOnly | OutcomeStatus::Switched => Ok(Some((id, running))),
            _ => Err((
                id,
                Skip::Failed(
                    [Severity::Danger, Severity::Warning]
                        .iter()
                        .find_map(|s| outcome.notices.iter().find(|n| n.severity == *s))
                        .map(|n| n.text.clone())
                        .unwrap_or_else(|| crate::messages::outcome(outcome.status)),
                ),
            )),
        }
    }

    /// Store the migration state (other settings as they are on disk now).
    async fn save_pat_migration(&self, set: &ProfileSet) {
        let d = &self.deps;
        let result = async {
            let mut now = d.profiles.load().await?;
            now.settings.pat_migration = set.settings.pat_migration.clone();
            d.profiles.save(&now).await
        }
        .await;
        if let Err(e) = result {
            tracing::warn!(error = %e, "cannot record the PAT migration");
        }
    }
}
