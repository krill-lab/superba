//! The read side and housekeeping of the service: overview, doctor, profiles,
//! settings, sites, key checks, opening the app and start-up recovery.

use std::collections::BTreeMap;

use superba_core::{
    AppSession, AuthMode, BuiltinProfile, CodexProcessKind, DoctorFinding, EnvObservation, Error,
    JournalAction, KeyCheck, KeyInput, KeyRef, LaunchOptions, LineId, LineSource, LocalizedText,
    Notice, Overview, Profile, ProfileId, RecoveredJournal, RecoveryReport, RemoteOverview, Result,
    Secret, SecretBackend, Settings, Severity, Site, SiteId, SiteKind, catalog,
    codex::{ENV_AUTHAPI_BASE_URL, PROVIDER_OPENAI},
    plan::{AppOverview, CliOverview},
};

use crate::Switcher;
use crate::journal::{JournalEntry, JournalStatus, StateLock, SwitchJournal};
use crate::messages::text;
use crate::planner::{account_block, all_sites, effective_root, launch_cdp_mode};
use crate::rules;

/// A change of what the router copies from a member profile (its site, lines,
/// base URL or name); keys are compared by the caller.
fn router_member_changed(old: &Profile, new: &Profile) -> bool {
    old.site_id != new.site_id
        || old.line_priority() != new.line_priority()
        || old.base_url_override != new.base_url_override
        || old.name != new.name
        || old.key_ref != new.key_ref
}

/// The existing profile a wizard save under a preset id (`krill`, `custom`)
/// edits: one on the same site, preferring the applied, then the last selected.
fn preset_match<'a>(
    set: &'a superba_core::ProfileSet,
    profile: &Profile,
    applied: Option<ProfileId>,
) -> Option<&'a Profile> {
    let is_preset = catalog::presets()
        .iter()
        .any(|p| p.id == profile.id.as_str() && p.id != catalog::PRESET_OFFICIAL);
    if !is_preset {
        return None;
    }
    let same_site = |id: Option<&ProfileId>| {
        id.and_then(|id| set.profile(id))
            .filter(|p| !p.is_builtin() && p.site_id == profile.site_id)
    };
    same_site(applied.as_ref())
        .or_else(|| same_site(set.last_selected.as_ref()))
        .or_else(|| {
            set.profiles
                .iter()
                .find(|p| !p.is_builtin() && p.site_id == profile.site_id)
        })
}

/// A wizard re-run applied to an existing profile: the connection (lines, auth,
/// base URL, key reference) and any model choice come from the wizard; the
/// name, Codex settings and model overrides the user set stay.
fn merge_rerun(existing: &Profile, wizard: Profile) -> Profile {
    let mut p = existing.clone();
    p.set_line_priority(wizard.line_priority());
    p.auth_mode = wizard.auth_mode;
    p.set_auth(wizard.auth);
    p.base_url_override = wizard.base_url_override;
    if wizard.key_ref.is_some() {
        p.key_ref = wizard.key_ref;
    }
    if wizard.model.model.is_some() {
        p.model.model = wizard.model.model;
    }
    if wizard.model.reasoning_effort.is_some() {
        p.model.reasoning_effort = wizard.model.reasoning_effort;
    }
    if p.codex_settings.is_empty() {
        p.codex_settings = wizard.codex_settings;
    }
    p
}

/// Profile ids: lowercase ASCII, digits, `-` and `_`, 1..=64 characters.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        && id.as_bytes()[0].is_ascii_alphanumeric()
}

fn finding(id: &str, severity: Severity, title: LocalizedText) -> DoctorFinding {
    DoctorFinding {
        id: id.to_owned(),
        severity,
        title,
        detail: None,
        fixable: false,
    }
}

impl Switcher {
    /// The profile applied to the local home, if any.
    pub(crate) async fn applied_profile(&self) -> Option<ProfileId> {
        self.deps
            .config
            .read_state(&self.deps.ctx.codex_home())
            .await
            .ok()
            .and_then(|s| s.active)
            .and_then(|a| a.profile_id)
    }

    /// Writes into the user's data directory refuse to run through `sudo`
    /// (files would end up owned by root) or, on Windows, as another account
    /// than the signed-in user (they would land in that account's profile),
    /// like switching does. An elevated token of the signed-in user is fine.
    pub(crate) fn refuse_elevated(&self) -> Result<()> {
        if let Some(reason) = account_block(self.deps.ctx.env.as_ref()) {
            return Err(Error::Blocked {
                reasons: vec![reason],
            });
        }
        Ok(())
    }

    pub(crate) async fn sites_impl(&self) -> Result<Vec<Site>> {
        match self.deps.profiles.load().await {
            Ok(set) => Ok(all_sites(&set, self.deps.ctx.env.as_ref())),
            // Without a profile store the built-in catalog is still useful.
            Err(e) if e.is_not_implemented() => Ok(all_sites(
                &superba_core::ProfileSet::default(),
                self.deps.ctx.env.as_ref(),
            )),
            Err(e) => Err(e),
        }
    }

    pub(crate) async fn save_profile_impl(
        &self,
        mut profile: Profile,
        key: Option<Secret>,
    ) -> Result<Profile> {
        let d = &self.deps;
        if !valid_id(profile.id.as_str()) {
            return Err(Error::invalid(format!(
                "invalid profile id {:?}",
                profile.id.as_str()
            )));
        }
        let mut set = d.profiles.load().await?;
        let sites = all_sites(&set, self.deps.ctx.env.as_ref());
        // The setup wizard saves under the preset's id; when a profile of that
        // provider exists already, a re-run edits it instead of adding another.
        if set.profile(&profile.id).is_none()
            && let Some(existing) = preset_match(&set, &profile, self.applied_profile().await)
        {
            profile = merge_rerun(existing, profile);
        }
        if let Some(existing) = set.profile(&profile.id)
            && existing.is_builtin()
        {
            return Err(Error::invalid(format!(
                "built-in profile {} cannot be changed",
                profile.id
            )));
        }
        if profile.builtin.is_some() {
            return Err(Error::invalid("profiles cannot be marked built-in"));
        }
        let site = sites
            .iter()
            .find(|s| s.id == profile.site_id)
            .ok_or_else(|| Error::NotFound {
                what: format!("site {}", profile.site_id),
            })?;
        if let Some(l) = &profile.line_id
            && site.line(l).is_none()
        {
            return Err(Error::NotFound {
                what: format!("line {l} of site {}", site.id),
            });
        }
        // v2: a manual choice must be offered by the site; Auto needs any key mode.
        let usable = match profile.auth_choice().manual_mode() {
            Some(mode) => site.supports(mode),
            None if profile.auth_mode.uses_key() => {
                site.capabilities.auth_modes.iter().any(|m| m.uses_key())
            }
            None => site.supports(profile.auth_mode),
        };
        if !usable {
            return Err(Error::invalid(format!(
                "site {} does not offer {:?}",
                site.id,
                profile.auth_choice()
            )));
        }
        if site.kind == SiteKind::Compatible
            && profile.base_url_override.is_none()
            && site.lines.is_empty()
        {
            return Err(Error::invalid("a compatible site profile needs a base URL"));
        }
        let new_key = key.is_some();
        if let Some(secret) = key {
            if secret.is_empty() {
                return Err(Error::invalid("empty key"));
            }
            profile.key_ref = Some(d.secrets.put(&profile.id, &secret).await?);
        }
        let previous = set.profile(&profile.id).cloned();
        match set.profiles.iter_mut().find(|p| p.id == profile.id) {
            Some(slot) => *slot = profile.clone(),
            None => set.profiles.push(profile.clone()),
        }
        d.profiles.save(&set).await?;
        // The router holds its own copy of member keys and lines: a member with a
        // new key (say, after a leak) or connection must not keep the old one.
        if set.settings.router.members.contains(&profile.id)
            && (new_key
                || previous
                    .as_ref()
                    .is_none_or(|old| router_member_changed(old, &profile)))
        {
            self.resync_router_members(&set).await?;
        }
        Ok(profile)
    }

    pub(crate) async fn delete_profile_impl(&self, id: &ProfileId) -> Result<()> {
        let d = &self.deps;
        let mut set = d.profiles.load().await?;
        let Some(profile) = set.profile(id).cloned() else {
            return Err(Error::NotFound {
                what: format!("profile {id}"),
            });
        };
        if profile.is_builtin() {
            return Err(Error::invalid(format!(
                "built-in profile {id} cannot be deleted"
            )));
        }
        let member = set.settings.router.members.contains(id);
        if member {
            set.settings.router.members.retain(|m| m != id);
            if set.settings.router.members.is_empty() {
                let state = d.config.read_state(&d.ctx.codex_home()).await?;
                if self.codex_routed(&state) {
                    return Err(Error::Conflict {
                        message: format!(
                            "profile {id} is the local router's only member and Codex goes \
                             through the router; turn router mode off first (spb router disable)"
                        ),
                    });
                }
                set.settings.router.enabled = false;
            }
        }
        set.profiles.retain(|p| &p.id != id);
        if set.last_selected.as_ref() == Some(id) {
            set.last_selected = None;
        }
        d.profiles.save(&set).await?;
        if member {
            // Drop the member's key from the router's key file (or remove the
            // router when no member is left) before deleting the stored key.
            self.resync_router_members(&set).await?;
        }
        if let Some(r) = &profile.key_ref
            && !matches!(r, KeyRef::Env { .. })
            && !set.profiles.iter().any(|p| p.key_ref.as_ref() == Some(r))
            && let Err(e) = d.secrets.delete(r).await
        {
            tracing::warn!(error = %e, "cannot delete the stored key");
        }
        Ok(())
    }

    pub(crate) async fn save_settings_impl(&self, mut settings: Settings) -> Result<()> {
        settings.countdown_secs = settings
            .countdown_secs
            .min(crate::machine::MAX_COUNTDOWN_SECS);
        let mut set = self.deps.profiles.load().await?;
        let was_persisted = set.settings.app_codex.persist_env;
        set.settings = settings;
        let persist = set.settings.app_codex.persist_env;
        if was_persisted == persist {
            return self.deps.profiles.save(&set).await;
        }
        // "Also outside Superba" changed: the persisted CODEX_CLI_PATH follows it
        // now (removed when turned off, the pin's executable when turned on), so a
        // value never outlives the choice. Not while a switch writes.
        let _lock = StateLock::acquire(&self.deps.ctx.data_dir)?;
        let op = match persist {
            true => self
                .pin_exe(&set, &set.settings.app_codex.pin)
                .await
                .map(|p| superba_core::EnvOp::Set {
                    value: p.display().to_string(),
                    take_over_foreign: false,
                }),
            false => Some(superba_core::EnvOp::Clear),
        };
        if let Some(op) = op {
            match self
                .deps
                .env
                .plan(superba_core::codex::ENV_CODEX_CLI_PATH, op)
                .await
            {
                Ok(p) if p.is_noop() => {}
                Ok(p) => {
                    self.deps.env.apply(&p).await?;
                }
                Err(e) if e.is_not_implemented() => {}
                Err(e) => return Err(e),
            }
        }
        self.deps.profiles.save(&set).await
    }

    pub(crate) async fn save_site_impl(&self, site: Site) -> Result<Site> {
        if catalog::builtin_sites().iter().any(|b| b.id == site.id) {
            return Err(Error::invalid(format!(
                "built-in site {} cannot be changed",
                site.id
            )));
        }
        let site = validated_custom_site(site)?;
        let mut set = self.deps.profiles.load().await?;
        match set.custom_sites.iter_mut().find(|s| s.id == site.id) {
            Some(slot) => *slot = site.clone(),
            None => set.custom_sites.push(site.clone()),
        }
        self.deps.profiles.save(&set).await?;
        // Router members on this site carry its lines and model map.
        if set
            .settings
            .router
            .members
            .iter()
            .any(|id| set.profile(id).is_some_and(|p| p.site_id == site.id))
        {
            self.resync_router_members(&set).await?;
        }
        Ok(site)
    }

    pub(crate) async fn delete_site_impl(&self, id: &SiteId) -> Result<()> {
        let mut set = self.deps.profiles.load().await?;
        if !set.custom_sites.iter().any(|s| &s.id == id) {
            return Err(Error::NotFound {
                what: format!("custom site {id}"),
            });
        }
        if let Some(p) = set.profiles.iter().find(|p| &p.site_id == id) {
            return Err(Error::Conflict {
                message: format!("profile {} still uses site {id}", p.id),
            });
        }
        set.custom_sites.retain(|s| &s.id != id);
        self.deps.profiles.save(&set).await
    }

    pub(crate) async fn resolve_key(&self, key: KeyInput) -> Result<Secret> {
        let d = &self.deps;
        let secret = match key {
            KeyInput::Secret(s) => s,
            KeyInput::Ref(r) => d.secrets.get(&r).await?,
            KeyInput::Profile(id) => {
                let set = d.profiles.load().await?;
                let profile = set.profile(&id).ok_or_else(|| Error::NotFound {
                    what: format!("profile {id}"),
                })?;
                let r = profile.key_ref.as_ref().ok_or_else(|| Error::Blocked {
                    reasons: vec![superba_core::BlockReason::MissingKey],
                })?;
                d.secrets.get(r).await?
            }
        };
        if secret.is_empty() {
            return Err(Error::Blocked {
                reasons: vec![superba_core::BlockReason::MissingKey],
            });
        }
        Ok(secret)
    }

    pub(crate) async fn check_key_impl(
        &self,
        site_id: &SiteId,
        line_id: &LineId,
        mode: AuthMode,
        key: KeyInput,
    ) -> Result<KeyCheck> {
        let site = self.site(site_id).await?;
        let line = site.line(line_id).cloned().ok_or_else(|| Error::NotFound {
            what: format!("line {line_id} of site {site_id}"),
        })?;
        if !site.supports(mode) {
            return Err(Error::invalid(format!(
                "site {site_id} does not offer {mode:?}"
            )));
        }
        let secret = self.resolve_key(key).await?;
        self.deps
            .prober
            .check_key(&site, &line, mode, &secret)
            .await
    }

    pub(crate) async fn open_app_impl(&self) -> Result<AppSession> {
        let d = &self.deps;
        if let Some(reason) = account_block(d.ctx.env.as_ref()) {
            return Err(Error::Blocked {
                reasons: vec![reason],
            });
        }
        if let Some(s) = d.app.running_session().await? {
            if s.launched_by_us && s.cdp.is_some() {
                // Already open through Superba: bring it to the front, as its
                // own icon would (GH#7: the "ChatGPT (Superba)" shortcut).
                if let Err(e) = d.app.activate(&s).await {
                    tracing::debug!(error = %e, "the running app was not brought to the front");
                }
                return Ok(s);
            }
            return Err(Error::Conflict {
                message: "ChatGPT is already running without Superba; restart it through Superba \
                          (spb launch --restart)"
                    .into(),
            });
        }
        let set = d.profiles.load().await?;
        let home = d.ctx.codex_home();
        let state = d.config.read_state(&home).await.ok();
        // The applied profile's line when its site is Krill-kind (built-in or an e2e
        // test site); otherwise the Krill default line (plan 7.1 launch note).
        let active = state.as_ref().and_then(|s| s.active.as_ref());
        let (krill, active_line) = rules::active_krill_site(
            &all_sites(&set, self.deps.ctx.env.as_ref()),
            active.and_then(|a| a.site_id.as_ref()),
            active.and_then(|a| a.line_id.as_ref()),
        );
        let origin = rules::launch_authapi_origin(&krill, None, false, active_line.as_ref());
        let mut env = BTreeMap::new();
        if let Some(o) = &origin {
            env.insert(ENV_AUTHAPI_BASE_URL.to_owned(), o.as_str());
        }
        self.prepare_launch_env(origin.as_ref()).await;
        let options = self
            .with_pin(LaunchOptions {
                install_id: set.settings.preferred_app_id.clone(),
                cdp: launch_cdp_mode(set.settings.safe_mode, effective_root()),
                env,
                ..LaunchOptions::default()
            })
            .await;
        d.app.launch(&options).await
    }

    /// Before Superba opens the desktop app: the auth address a PAT profile
    /// needs, where the app takes it from at start-up (user bug 69a). On
    /// Windows the packaged app reads `HKCU\Environment` when it is activated
    /// (no per-launch environment), and with "Run Codex in WSL" its Codex sees
    /// the variable only when `WSLENV` forwards it; both are written here like
    /// a switch writes them (same ownership records, so a restore removes them),
    /// so the first PAT switch after the mode was turned on can be hot. The
    /// value is the one Superba persisted for the applied profile, else
    /// `origin`. Elsewhere nothing is written (the launch passes the value).
    /// Failures are logged: the app opens anyway.
    pub(crate) async fn prepare_launch_env(&self, origin: Option<&superba_core::Origin>) {
        let d = &self.deps;
        let Some(origin) = origin else {
            return;
        };
        let pat = d
            .config
            .read_state(&d.ctx.codex_home())
            .await
            .ok()
            .and_then(|s| s.active)
            .and_then(|a| a.auth_mode)
            .is_some_and(AuthMode::needs_authapi_env);
        if !pat {
            return;
        }
        let value = match d.env.inspect(ENV_AUTHAPI_BASE_URL).await {
            Ok(r) => r.superba_value.unwrap_or_else(|| origin.as_str()),
            Err(e) if e.is_not_implemented() => return,
            Err(_) => origin.as_str(),
        };
        let plan = match d.env.plan_launch(ENV_AUTHAPI_BASE_URL, &value).await {
            Ok(p) if p.is_noop() => return,
            Ok(p) => p,
            Err(e) if e.is_not_implemented() => return,
            Err(e) => {
                tracing::warn!(error = %e, "cannot plan the auth address for the app's start");
                return;
            }
        };
        match d.env.apply(&plan).await {
            Ok(applied) => tracing::info!(
                locations = ?applied.edits.iter().map(|e| &e.location).collect::<Vec<_>>(),
                "auth address persisted for the app's start"
            ),
            Err(e) => {
                tracing::warn!(error = %e, "cannot persist the auth address for the app's start")
            }
        }
    }

    pub(crate) async fn overview_impl(&self) -> Result<Overview> {
        let d = &self.deps;
        let home = d.ctx.codex_home();
        let set = d.profiles.load().await?;
        let sites = all_sites(&set, self.deps.ctx.env.as_ref());
        let mut notices = Vec::new();
        let config = match d.config.read_state(&home).await {
            Ok(s) => Some(s),
            Err(e) => {
                notices.push(Notice::warning(e.to_text()));
                None
            }
        };
        let baseline_at = match d.config.baseline(&home).await {
            Ok(Some(b)) => Some(b.captured_at),
            _ => None,
        };
        let baseline_captured = baseline_at.is_some();
        let login_stash = d
            .config
            .login_stash(&superba_core::StashScope::Local(home.root().to_path_buf()))
            .await
            .ok()
            .flatten();
        // Slow facts from the environment monitor's snapshot (no detection on
        // this polled path); fast facts from one look at the process table.
        let facts = self.monitor.facts().await;
        let installs = facts.installs.clone();
        let (session, processes) = d.app.session_and_processes(&home).await;
        let session = session.ok().flatten();
        let hot_switch = self
            .hot_switch_status(
                installs.is_empty(),
                session.as_ref(),
                set.settings.safe_mode,
            )
            .await;
        let authapi_line = session
            .as_ref()
            .and_then(|s| s.authapi_env.value())
            .and_then(|o| {
                sites
                    .iter()
                    .find_map(|site| rules::line_for_origin(site, o))
            })
            .map(|l| l.id.clone());
        let active = config.as_ref().and_then(|c| c.active.clone());
        let mut processes = processes.unwrap_or_default();
        rules::mark_stale(&mut processes, active.as_ref().map(|a| a.applied_at));
        let binaries = facts.binaries.clone();
        let remotes = {
            let live: Vec<_> = processes
                .iter()
                .filter(|p| matches!(p.kind, CodexProcessKind::RemoteLink { .. }))
                .cloned()
                .collect();
            // Only hosts the desktop app knows (v0.3), with their last probe.
            let cache = self.cached_hosts();
            d.remote
                .discover(&home, &[], &live)
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|h| h.is_app_known() && h.is_listed())
                .map(|host| {
                    let cached = cache.get(&host.target_id).filter(|_| host.is_managed());
                    RemoteOverview {
                        state: cached.and_then(|c| c.state.clone()),
                        error: cached.and_then(|c| c.error.clone()),
                        host,
                    }
                })
                .collect()
        };

        let external = match &config {
            Some(c) => self.external_change(c).await,
            None => None,
        };
        if let Some(c) = &config {
            if c.journal_pending {
                notices.push(Notice::warning(text("switch.overview.journal_pending")));
            }
            // GH#5: the changes and who made them are in `external`; the
            // notice is the one-line version for `spb status` and the log.
            if let Some(x) = &external {
                notices.push(Notice::info(
                    text("switch.overview.external")
                        .arg("count", x.count())
                        .arg_text("source", x.source.text()),
                ));
            }
            if let Some(h) = &c.handed_off {
                notices.push(Notice::info(
                    text("switch.overview.handed_off").arg_text("tool", h.to.text()),
                ));
            }
        }
        if installs.is_empty() && session.is_none() {
            notices.push(Notice::info(text("switch.overview.no_app")));
        }
        // The app's own Codex moved past the version it is pinned to.
        if let superba_core::AppCodexPin::Version { version } = &set.settings.app_codex.pin
            && let Some(bundled) = installs
                .iter()
                .find(|i| Some(&i.id) == set.settings.preferred_app_id.as_ref())
                .or_else(|| installs.first())
                .and_then(|i| i.bundled_codex_version.as_ref())
            && bundled.triple() > version.triple()
        {
            notices.push(Notice::info(
                text("switch.pin.bundled_ahead")
                    .arg("bundled", bundled)
                    .arg("pinned", version),
            ));
        }
        // Written versus in effect, without the running-turn check (this path
        // is polled; `runtime()` adds it).
        let runtime = self
            .runtime_view(
                &processes,
                session.as_ref(),
                Some(&hot_switch),
                active.as_ref(),
                false,
            )
            .await;
        let cli = CliOverview {
            binaries,
            processes,
        };
        if cli.stale_count() > 0 {
            notices.push(Notice::info(
                text("switch.overview.stale_processes").arg("count", cli.stale_count()),
            ));
        }
        if let (Some(line), Some(a)) = (&authapi_line, &active)
            && a.auth_mode == Some(AuthMode::Pat)
            && a.line_id.as_ref() != Some(line)
            && let Some(l) = sites.iter().find_map(|s| s.line(line))
        {
            notices.push(Notice::info(
                text("switch.overview.authapi_line").arg_text("line", l.name.clone()),
            ));
        }
        let account = match (&active, &set) {
            (Some(a), set) => self.account_summary(a, set).await,
            _ => None,
        };
        Ok(Overview {
            generated_at: d.clock.now(),
            codex_home: home.root().to_path_buf(),
            active,
            config,
            app: AppOverview {
                installs,
                session,
                hot_switch,
                authapi_line,
            },
            cli,
            remotes,
            account,
            baseline_captured,
            baseline_at,
            login_stash,
            runtime: Some(runtime),
            notices,
            external,
        })
    }

    /// Account card for the applied profile, when its site has a summary endpoint.
    async fn account_summary(
        &self,
        active: &superba_core::ActiveState,
        set: &superba_core::ProfileSet,
    ) -> Option<superba_core::AccountSummary> {
        let profile = set.profile(active.profile_id.as_ref()?)?;
        let site = all_sites(set, self.deps.ctx.env.as_ref())
            .into_iter()
            .find(|s| s.id == profile.site_id)?;
        if !site.capabilities.account_summary {
            return None;
        }
        let line = active
            .line_id
            .as_ref()
            .and_then(|l| site.line(l))
            .or_else(|| site.default_line())?
            .clone();
        let key = self
            .deps
            .secrets
            .get(profile.key_ref.as_ref()?)
            .await
            .ok()?;
        self.deps
            .prober
            .account_summary(&site, &line, &key)
            .await
            .ok()
            .flatten()
    }

    pub(crate) async fn doctor_impl(&self) -> Result<Vec<DoctorFinding>> {
        let d = &self.deps;
        let home = d.ctx.codex_home();
        let mut out = Vec::new();
        let state = match d.config.read_state(&home).await {
            Ok(s) => Some(s),
            Err(e) => {
                out.push(finding(
                    "config_unreadable",
                    Severity::Warning,
                    text("switch.doctor.check_failed")
                        .arg("what", "config.toml")
                        .arg("detail", e.to_string()),
                ));
                None
            }
        };
        if let Some(s) = &state {
            if s.journal_pending {
                out.push(finding(
                    "journal_pending",
                    Severity::Warning,
                    text("switch.doctor.journal_pending"),
                ));
            }
            if let Some(m) = &s.parse_error {
                out.push(finding(
                    "unparseable",
                    Severity::Danger,
                    text("switch.doctor.unparseable").arg("message", m),
                ));
            }
            if let Some(p) = &s.profile_override {
                out.push(finding(
                    "profile_override",
                    Severity::Warning,
                    text("switch.doctor.profile_override").arg("profile", p),
                ));
            }
            if let Some(v) = &s.cli_auth_credentials_store {
                out.push(finding(
                    "cli_auth_store",
                    Severity::Info,
                    text("switch.doctor.cli_auth_store").arg("value", v),
                ));
            }
            if let Some(p) = s
                .model_provider
                .as_ref()
                .filter(|p| p.as_str() != PROVIDER_OPENAI)
            {
                out.push(finding(
                    "legacy_provider",
                    Severity::Warning,
                    text("switch.doctor.legacy_provider").arg("provider", p),
                ));
                if s.provider_table_present {
                    out.push(finding(
                        "provider_table",
                        Severity::Info,
                        text("switch.doctor.provider_table").arg("provider", p),
                    ));
                }
            }
            if let Some(x) = self.external_change(s).await {
                out.push(finding(
                    "external_change",
                    Severity::Warning,
                    text("switch.doctor.external")
                        .arg("count", x.count())
                        .arg_text("source", x.source.text()),
                ));
            }
        }
        if let Ok(unfinished) = SwitchJournal::new(&d.ctx.data_dir).unfinished()
            && !unfinished.is_empty()
            && !out.iter().any(|f| f.id == "journal_pending")
        {
            out.push(finding(
                "journal_pending",
                Severity::Warning,
                text("switch.doctor.journal_pending"),
            ));
        }
        if let Ok(report) = d.sessions.scan(&home).await {
            let current = state
                .as_ref()
                .and_then(|s| s.model_provider.clone())
                .unwrap_or_else(|| PROVIDER_OPENAI.to_owned());
            let hidden = report.hidden_under(&current);
            if hidden > 0 {
                out.push(finding(
                    "hidden_sessions",
                    Severity::Info,
                    text("switch.doctor.hidden_sessions").arg("count", hidden),
                ));
            }
        }
        let session = d.app.running_session().await.ok().flatten();
        let mut processes = d.app.processes(&home).await.unwrap_or_default();
        let active = state.as_ref().and_then(|s| s.active.clone());
        rules::mark_stale(&mut processes, active.as_ref().map(|a| a.applied_at));
        let pat_active = active.as_ref().and_then(|a| a.auth_mode) == Some(AuthMode::Pat);
        if pat_active {
            let missing = processes
                .iter()
                .filter(|p| rules::consumer_group(&p.kind).is_some())
                .filter(|p| matches!(p.authapi_env, EnvObservation::Absent))
                .count();
            if missing > 0 {
                out.push(finding(
                    "missing_env",
                    Severity::Danger,
                    text("switch.doctor.missing_env").arg("count", missing),
                ));
            }
        }
        let stale = processes
            .iter()
            .filter(|p| p.stale && rules::consumer_group(&p.kind).is_some())
            .count();
        if stale > 0 {
            out.push(finding(
                "stale_processes",
                Severity::Info,
                text("switch.doctor.stale_processes").arg("count", stale),
            ));
        }
        if let Some(s) = &session {
            if let Some(app_home) = &s.codex_home
                && app_home != home.root()
            {
                out.push(finding(
                    "codex_home_mismatch",
                    Severity::Warning,
                    text("switch.doctor.codex_home_mismatch")
                        .arg("app_home", superba_core::paths::display_path(app_home))
                        .arg("home", superba_core::paths::display_path(home.root())),
                ));
            }
            if s.cdp.is_none() {
                out.push(DoctorFinding {
                    fixable: true,
                    ..finding(
                        "app_not_ours",
                        Severity::Info,
                        text("switch.doctor.app_not_ours"),
                    )
                });
            }
            if pat_active
                && let (Some(origin), Some(line_id)) = (
                    s.authapi_env.value(),
                    active.as_ref().and_then(|a| a.line_id.as_ref()),
                )
            {
                let set = d.profiles.load().await.unwrap_or_default();
                let sites = all_sites(&set, self.deps.ctx.env.as_ref());
                // Line ids are unique per site only: look the line up on the
                // applied site.
                let site = active
                    .as_ref()
                    .and_then(|a| a.site_id.as_ref())
                    .and_then(|id| sites.iter().find(|s| &s.id == id));
                let current = site.and_then(|s| s.line(line_id));
                if current.is_some_and(|l| {
                    superba_core::Origin::parse(origin).ok() != Some(l.origin.clone())
                }) {
                    // Name the line the app authenticates through when it is one
                    // of the applied site's lines.
                    let text = match site.and_then(|s| rules::line_for_origin(s, origin)) {
                        Some(l) => text("switch.doctor.authapi_other_line_named")
                            .arg_text("line", l.name.clone())
                            .arg("origin", origin),
                        None => text("switch.doctor.authapi_other_line").arg("origin", origin),
                    };
                    out.push(finding("authapi_other_line", Severity::Info, text));
                }
            }
        }
        if let Ok(binaries) = d.app.codex_binaries().await
            && let Some(cli) = binaries.iter().find(|b| b.path_rank == Some(0))
            && let Some(v) = &cli.version
        {
            let min = rules::min_version(superba_core::codex::VersionCapability::WhamAccountsCheck);
            if !v.meets(&min) {
                out.push(finding(
                    "cli_too_old",
                    Severity::Warning,
                    text("switch.doctor.cli_too_old")
                        .arg("consumer", superba_core::paths::display_path(&cli.path))
                        .arg("found", v)
                        .arg("required", min),
                ));
            }
        }
        for var in ["OPENAI_BASE_URL", "OPENAI_API_KEY", "CODEX_API_KEY"] {
            if d.ctx.env.var_non_empty(var).is_some() {
                out.push(finding(
                    "openai_env_conflict",
                    Severity::Warning,
                    text("switch.doctor.openai_env").arg("var", var),
                ));
            }
        }
        if d.secrets.backend() == SecretBackend::PlaintextFile {
            out.push(finding(
                "plaintext_secrets",
                Severity::Warning,
                text("switch.doctor.plaintext_secrets"),
            ));
        }
        match crate::privilege::current(d.ctx.env.as_ref()) {
            crate::privilege::RunAs::Sudo => out.push(finding(
                "elevated",
                Severity::Danger,
                text("switch.doctor.elevated"),
            )),
            crate::privilege::RunAs::OtherUser {
                account,
                desktop_user,
            } => out.push(finding(
                "other_user",
                Severity::Danger,
                superba_core::BlockReason::OtherUser {
                    account,
                    desktop_user,
                }
                .text(),
            )),
            // An elevated token of the signed-in user is fine: the GUI says so once.
            crate::privilege::RunAs::ElevatedUser | crate::privilege::RunAs::User => {}
        }
        Ok(out)
    }

    /// Start-up recovery: config journal, interrupted session migrations, then the
    /// switch journal (roll back entries that crashed while applying or undoing, complete
    /// entries whose writes were committed).
    pub(crate) async fn recover_impl(&self) -> Result<RecoveryReport> {
        let mut report = self.recover_journals().await?;
        // After the lock is released: a model catalog file made for another
        // Codex version is checked (and the configuration repaired) here.
        report.notices.extend(self.catalog_startup_check().await);
        Ok(report)
    }

    async fn recover_journals(&self) -> Result<RecoveryReport> {
        let d = &self.deps;
        let _lock = StateLock::acquire(&d.ctx.data_dir)?;
        let home = d.ctx.codex_home();
        let mut report = d.config.recover(&home).await?;
        match d.sessions.recover(&home).await {
            Ok(_) => {}
            Err(e) if e.is_not_implemented() => {}
            Err(e) => tracing::warn!(error = %e, "session migration recovery failed"),
        }
        let journal = SwitchJournal::new(&d.ctx.data_dir);
        for mut entry in journal.unfinished()? {
            let mut files = Vec::new();
            if let Some(c) = &entry.config {
                files.extend(c.files.iter().map(|f| f.path.clone()));
            }
            let action = if matches!(
                entry.status,
                JournalStatus::Applying | JournalStatus::RollingBack
            ) {
                match self.recover_rollback(&mut entry, &journal).await {
                    Ok(()) => {
                        entry.status = JournalStatus::RolledBack;
                        JournalAction::RolledBack
                    }
                    Err(e) => {
                        // Leave the entry unfinished: an open app or a changed
                        // file may prevent rollback now, but recovery must retry.
                        tracing::warn!(journal_id = %entry.id, error = %e, "interrupted switch rollback remains pending");
                        JournalAction::Drift
                    }
                }
            } else {
                entry.status = JournalStatus::Done;
                JournalAction::Completed
            };
            entry.updated_at = d.clock.now();
            journal.write(&entry)?;
            report.entries.push(RecoveredJournal {
                journal_id: entry.id.clone(),
                action,
                files,
            });
        }
        Ok(report)
    }

    /// Do not restore config/env after a failed migration rollback: they must
    /// continue to describe the sessions the app will find on its next launch.
    async fn recover_rollback(
        &self,
        entry: &mut JournalEntry,
        journal: &SwitchJournal,
    ) -> Result<()> {
        let d = &self.deps;
        let home = superba_core::CodexHome::new(entry.codex_home.clone());
        if !entry.rollback.migration
            && let Some(m) = &entry.migration
        {
            d.sessions.rollback(&home, &m.id).await?;
            entry.rollback.migration = true;
            journal.write(entry)?;
        }
        if !entry.rollback.env
            && let Some(env) = &entry.env
        {
            d.env.revert(env).await?;
            entry.rollback.env = true;
            journal.write(entry)?;
        }
        if !entry.rollback.config
            && let Some(c) = &entry.config
        {
            d.config.rollback(c).await?;
            entry.rollback.config = true;
            journal.write(entry)?;
        }
        Ok(())
    }
}

/// True for the built-in "original" / "official" profiles.
pub fn is_builtin_id(id: &str) -> bool {
    matches!(id, catalog::PROFILE_ORIGINAL | catalog::PROFILE_OFFICIAL)
}

/// The built-in kind for an id.
pub fn builtin_kind(id: &str) -> Option<BuiltinProfile> {
    match id {
        catalog::PROFILE_ORIGINAL => Some(BuiltinProfile::Original),
        catalog::PROFILE_OFFICIAL => Some(BuiltinProfile::Official),
        _ => None,
    }
}

/// A user-defined site checked like [`superba_core::SwitchService::save_site`]
/// does (compatible kind, line ids, no built-in Krill hosts, the prober's
/// key-safety rules), also for probing a site that is not saved yet.
pub(crate) fn validated_custom_site(mut site: Site) -> Result<Site> {
    if !valid_id(site.id.as_str()) {
        return Err(Error::invalid(format!(
            "invalid site id {:?}",
            site.id.as_str()
        )));
    }
    if site.kind != SiteKind::Compatible {
        return Err(Error::invalid("only compatible sites can be added"));
    }
    let mut seen = Vec::new();
    for line in &mut site.lines {
        if !valid_id(line.id.as_str()) || seen.contains(&line.id) {
            return Err(Error::invalid(format!(
                "invalid or duplicate line id {}",
                line.id
            )));
        }
        if catalog::is_krill_host(line.origin.host()) {
            return Err(Error::invalid(
                "Krill lines are built in; add a Krill profile instead",
            ));
        }
        if line.source == LineSource::Builtin {
            line.source = LineSource::User;
        }
        seen.push(line.id.clone());
    }
    if let Some(def) = &site.default_line
        && site.line(def).is_none()
    {
        return Err(Error::NotFound {
            what: format!("default line {def}"),
        });
    }
    // Structural and key-safety rules shared with the prober (endpoint paths,
    // ids, origins): a site that could send its key elsewhere is refused.
    superba_net::validate_custom_site(&site)?;
    Ok(site)
}
