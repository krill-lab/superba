//! Remote hosts in v0.3 (feedback 1, 3): only the hosts the desktop app knows
//! are listed, probed, followed or synced; ssh config entries alone never are.
//!
//! - A local switch changes a host only when the host follows this computer
//!   ([`superba_core::HostSettings::follow_switches`]), and then only its
//!   connection (routing and auth keys, always direct; see the planner).
//! - Settings reach a host only through a settings sync: the local profile's
//!   settings filtered with the catalog of the host's probed Codex version, shown
//!   as write / skip / remove / conflict rows ([`Switcher::host_sync_plan_impl`])
//!   and executed with [`superba_core::SwitchIntent::HostSync`] (settings scope,
//!   hot restart of the host's app-server, verify, roll back on failure).
//! - Sessions of a host are scanned read-only.
//! - Only SSH hosts are managed (user bug 36): the app's remote-control devices
//!   and WSL distributions are listed read-only (no probe, follow, sync, Codex
//!   install, session scan or hot restart: [`Switcher::app_host`] refuses them),
//!   hosts run by ChatGPT and unknown ids are not listed at all.
//!
//! The last probe of each host is cached in memory and in
//! `<data dir>/cache/hosts.json` (without the config bytes), so lists show it
//! without connecting.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use superba_config::settings::{HomeInput, plan_settings, read_config_values};
use superba_core::{
    AppSession, BlockReason, CodexSettings, ConfigState, ConsumerKind, Error, HostSessions,
    HostSettings, HostSync, HostSyncAction, HostSyncPlan, HostSyncRow, HostSyncSelection, HostView,
    Notice, ProcessInfo, Profile, ProfileSet, RemoteHost, RemoteInstallReport, RemoteState, Result,
    SettingEntry, SettingValue, SettingsConsumer, SettingsPlan, TargetId, Timestamp,
    codex::PROVIDER_OPENAI, is_valid_setting_path, reserved_owner,
};

use crate::Switcher;
use crate::messages::text;
use crate::planner::all_sites;

/// File of the probe cache under the data dir's `cache/`.
pub const HOSTS_CACHE_FILE: &str = "hosts.json";

/// The last probe of one host.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct CachedHost {
    /// Probe result (`config_toml` is kept only in memory).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<RemoteState>,
    /// Probe error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
    /// The last Codex install (rollback target: `previous`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_install: Option<RemoteInstallReport>,
}

/// In-memory probe cache (shared by clones of the service).
#[derive(Debug, Default)]
pub(crate) struct HostCache {
    loaded: bool,
    map: BTreeMap<TargetId, CachedHost>,
}

/// sha256 hex of a value as written (`None` = absent).
pub fn value_hash(repr: Option<&str>) -> String {
    hex::encode(Sha256::digest(repr.unwrap_or("").as_bytes()))
}

fn repr(v: &SettingValue) -> String {
    v.to_toml_repr()
}

/// Paths a sync may release: the setting keys Superba owns on the host (written
/// there and not released since), never routing or other reserved keys. A key
/// Superba released (a restore) holds the host's own value again and is no
/// longer offered, even if an earlier sync wrote it.
fn removable(owned: &[String]) -> BTreeSet<String> {
    owned
        .iter()
        .filter(|p| is_valid_setting_path(p) && reserved_owner(p).is_none())
        .cloned()
        .collect()
}

/// The rows of a sync preview (pure): `wanted` are the local profile's settings,
/// `sp` their plan for the host's Codex, `host` the host's current values,
/// `last` the last sync and `owned` the setting keys Superba owns on the host.
pub fn sync_rows(
    wanted: &CodexSettings,
    sp: &SettingsPlan,
    host: &BTreeMap<String, SettingValue>,
    last: Option<&HostSync>,
    owned: &[String],
) -> Vec<HostSyncRow> {
    let mut rows: BTreeMap<String, HostSyncRow> = BTreeMap::new();
    let remote_of = |path: &str| host.get(path).map(repr);
    let changed_since_sync = |path: &str, remote: Option<&str>| {
        last.and_then(|l| l.value_hashes.get(path))
            .is_some_and(|h| h != &value_hash(remote))
    };
    for (path, entry) in sp.apply.iter() {
        let local = entry.to_toml_repr();
        let remote = remote_of(path);
        let same = match (entry, host.get(path)) {
            (SettingEntry::Set(v), Some(h)) => v == h || local.as_deref() == remote.as_deref(),
            (SettingEntry::Unset, None) => true,
            _ => false,
        };
        if same {
            continue;
        }
        let conflict = changed_since_sync(path, remote.as_deref());
        rows.insert(
            path.to_owned(),
            HostSyncRow {
                path: path.to_owned(),
                local,
                remote,
                action: if conflict {
                    HostSyncAction::Conflict
                } else {
                    HostSyncAction::Write
                },
                selected: !conflict,
            },
        );
    }
    for skip in &sp.skipped {
        rows.insert(
            skip.path.clone(),
            HostSyncRow {
                path: skip.path.clone(),
                local: skip.entry.to_toml_repr(),
                remote: remote_of(&skip.path),
                action: HostSyncAction::Skip {
                    reason: skip.reason.clone(),
                },
                selected: false,
            },
        );
    }
    for path in removable(owned) {
        if wanted.contains(&path) || rows.contains_key(&path) {
            continue;
        }
        let Some(remote) = remote_of(&path) else {
            continue;
        };
        let conflict = changed_since_sync(&path, Some(&remote));
        rows.insert(
            path.clone(),
            HostSyncRow {
                path: path.clone(),
                local: None,
                remote: Some(remote),
                action: if conflict {
                    HostSyncAction::Conflict
                } else {
                    HostSyncAction::Remove
                },
                selected: !conflict,
            },
        );
    }
    rows.into_values().collect()
}

/// Rows to apply for `selection` (`None` = the rows selected by default; skips
/// are never applied).
pub fn selected_rows<'a>(
    rows: &'a [HostSyncRow],
    selection: &HostSyncSelection,
) -> Vec<&'a HostSyncRow> {
    rows.iter()
        .filter(|r| !matches!(r.action, HostSyncAction::Skip { .. }))
        .filter(|r| match &selection.paths {
            Some(paths) => paths.contains(&r.path),
            None => r.selected,
        })
        .collect()
}

/// What a sync changes on the host for the selected rows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SyncChange {
    /// Rows the local profile sets: its value is written (or the key removed
    /// when the profile removes it).
    pub settings: CodexSettings,
    /// Remove rows and conflict rows the local profile does not set: Superba
    /// releases its key, so the host gets back its own value from before
    /// Superba wrote it (or loses the key if it had none).
    pub release: Vec<String>,
}

/// The change a sync makes for the selected rows (see [`SyncChange`]).
pub fn sync_settings(rows: &[&HostSyncRow], sp: &SettingsPlan) -> SyncChange {
    let mut out = SyncChange::default();
    for r in rows {
        match sp.apply.get(&r.path) {
            Some(entry) => out.settings.insert_unchecked(r.path.clone(), entry.clone()),
            None => out.release.push(r.path.clone()),
        }
    }
    out
}

/// The record stored after a successful sync.
pub fn sync_record(previous: Option<&HostSync>, applied: &SyncChange, at: Timestamp) -> HostSync {
    let mut paths: BTreeSet<String> = previous
        .map(|p| p.paths.iter().cloned().collect())
        .unwrap_or_default();
    let mut hashes = previous.map(|p| p.value_hashes.clone()).unwrap_or_default();
    let mut hasher = Sha256::new();
    for (path, entry) in applied.settings.iter() {
        match entry.to_toml_repr() {
            Some(r) => {
                paths.insert(path.to_owned());
                hashes.insert(path.to_owned(), value_hash(Some(&r)));
                hasher.update(format!("{path}={r}\n").as_bytes());
            }
            None => {
                paths.remove(path);
                hashes.remove(path);
                hasher.update(format!("{path}\n").as_bytes());
            }
        }
    }
    for path in &applied.release {
        paths.remove(path);
        hashes.remove(path);
        hasher.update(format!("{path}\n").as_bytes());
    }
    HostSync {
        at,
        values_hash: hex::encode(hasher.finalize()),
        paths: paths.into_iter().collect(),
        value_hashes: hashes,
    }
}

/// The command shown for a remote host's sessions under another provider.
pub fn migrate_command(report: &superba_core::SessionReport) -> Option<String> {
    report
        .by_provider
        .iter()
        .filter(|(p, _)| p.as_str() != PROVIDER_OPENAI)
        .max_by_key(|(_, c)| c.distinct.max(c.threads))
        .map(|(p, _)| migrate_command_for([p.as_str()]))
}

/// `spb sessions migrate --from <id> ...` for `providers`, to copy and run in a
/// POSIX shell. Provider ids come from rollout files (any character but `"`),
/// so an id that is not a plain name (`[A-Za-z0-9._-]+`) is single-quoted: it
/// stays one argument and never becomes shell code.
pub fn migrate_command_for<'a>(providers: impl IntoIterator<Item = &'a str>) -> String {
    let mut cmd = String::from("spb sessions migrate");
    for p in providers {
        cmd.push_str(" --from ");
        let plain = !p.is_empty()
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if plain {
            cmd.push_str(p);
        } else {
            cmd.push_str(&superba_remote::script::sh_quote(p));
        }
    }
    cmd
}

impl Switcher {
    fn hosts_cache_path(&self) -> PathBuf {
        self.deps.ctx.data_dir.cache_dir().join(HOSTS_CACHE_FILE)
    }

    /// The cached probe of every host (loaded from disk once).
    pub(crate) fn cached_hosts(&self) -> BTreeMap<TargetId, CachedHost> {
        let mut cache = self.hosts.lock().unwrap_or_else(|p| p.into_inner());
        if !cache.loaded {
            cache.loaded = true;
            if let Ok(bytes) = std::fs::read(self.hosts_cache_path())
                && let Ok(map) = serde_json::from_slice::<BTreeMap<TargetId, CachedHost>>(&bytes)
            {
                for (k, v) in map {
                    cache.map.entry(k).or_insert(v);
                }
            }
        }
        cache.map.clone()
    }

    /// Update one host's cache entry (memory, then disk; disk errors are logged).
    pub(crate) fn cache_host(&self, id: &TargetId, update: impl FnOnce(&mut CachedHost)) {
        let _ = self.cached_hosts();
        let snapshot = {
            let mut cache = self.hosts.lock().unwrap_or_else(|p| p.into_inner());
            update(cache.map.entry(id.clone()).or_default());
            cache.map.clone()
        };
        let path = self.hosts_cache_path();
        let write = || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let bytes = serde_json::to_vec_pretty(&snapshot).map_err(std::io::Error::other)?;
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, bytes)?;
            std::fs::rename(&tmp, &path)
        };
        if let Err(e) = write() {
            tracing::debug!(error = %e, "cannot store the host probe cache");
        }
    }

    /// Hosts the desktop app knows (its global state, the renderer through CDP,
    /// live app-server links) and lists among its connections; ssh config entries
    /// alone, ChatGPT-hosted and unknown hosts are left out.
    pub(crate) async fn app_hosts(
        &self,
        session: Option<&AppSession>,
        processes: Option<&[ProcessInfo]>,
    ) -> Result<Vec<RemoteHost>> {
        let d = &self.deps;
        let home = d.ctx.codex_home();
        let owned;
        let processes = match processes {
            Some(p) => p,
            None => {
                owned = d.app.processes(&home).await.unwrap_or_default();
                &owned
            }
        };
        let live: Vec<ProcessInfo> = processes
            .iter()
            .filter(|p| matches!(p.kind, superba_core::CodexProcessKind::RemoteLink { .. }))
            .cloned()
            .collect();
        let from_app = match session.filter(|s| s.cdp.is_some()) {
            Some(s) => d.app.remote_connections(s).await.unwrap_or_default(),
            None => Vec::new(),
        };
        let hosts = d.remote.discover(&home, &from_app, &live).await?;
        Ok(hosts
            .into_iter()
            .filter(|h| h.is_app_known() && h.is_listed())
            .collect())
    }

    /// An SSH host the app knows, for an SSH operation (probe, follow, sync,
    /// Codex install, session scan): other kinds are refused.
    pub(crate) async fn app_host(&self, id: &TargetId) -> Result<(RemoteHost, Option<AppSession>)> {
        let session = self.deps.app.running_session().await.ok().flatten();
        let hosts = self.app_hosts(session.as_ref(), None).await?;
        let host = hosts
            .into_iter()
            .find(|h| &h.target_id == id)
            .ok_or_else(|| Error::NotFound {
                what: format!("remote host {id} (only hosts the ChatGPT app connects to)"),
            })?;
        if !host.is_managed() {
            return Err(Error::Unsupported {
                what: format!(
                    "{} ({}): the ChatGPT app does not reach it over SSH, so Superba cannot probe, sync, switch, install Codex on or scan it",
                    host.title(),
                    host.kind.text()
                ),
            });
        }
        Ok((host, session))
    }

    fn host_view(
        &self,
        host: RemoteHost,
        set: &ProfileSet,
        cached: Option<&CachedHost>,
        session: Option<&AppSession>,
        pending_sync: Option<u32>,
    ) -> HostView {
        let hot_restart =
            host.is_managed() && host.host_id.is_some() && session.is_some_and(|s| s.cdp.is_some());
        // A host Superba does not manage has no probe (a 0.1.0 cache may hold one).
        let cached = cached.filter(|_| host.is_managed());
        HostView {
            settings: set
                .settings
                .hosts
                .get(&host.target_id)
                .filter(|_| host.is_managed())
                .cloned()
                .unwrap_or_default(),
            state: cached.and_then(|c| c.state.clone()),
            probe_error: cached.and_then(|c| c.error.clone()),
            pending_sync,
            hot_restart,
            host,
        }
    }

    pub(crate) async fn hosts_impl(&self) -> Result<Vec<HostView>> {
        let set = self.deps.profiles.load().await?;
        let session = self.deps.app.running_session().await.ok().flatten();
        let hosts = self.app_hosts(session.as_ref(), None).await?;
        let config = self
            .deps
            .config
            .read_state(&self.deps.ctx.codex_home())
            .await
            .ok();
        let cache = self.cached_hosts();
        let mut out = Vec::new();
        for host in hosts {
            let cached = cache.get(&host.target_id).filter(|_| host.is_managed());
            let pending = match cached.and_then(|c| c.state.as_ref()) {
                // Only an in-memory probe has the config bytes needed for rows.
                Some(state) if state.config_toml.is_some() => self
                    .sync_preview(&set, config.as_ref(), &host, state)
                    .ok()
                    .map(|p| pending_count(&p)),
                _ => None,
            };
            out.push(self.host_view(host, &set, cached, session.as_ref(), pending));
        }
        Ok(out)
    }

    pub(crate) async fn host_probe_impl(&self, id: &TargetId) -> Result<HostView> {
        let (host, session) = self.app_host(id).await?;
        let set = self.deps.profiles.load().await?;
        let config = self
            .deps
            .config
            .read_state(&self.deps.ctx.codex_home())
            .await
            .ok();
        let probed = self.deps.remote.probe(&host).await;
        let pending = match &probed {
            Ok(state) => self
                .sync_preview(&set, config.as_ref(), &host, state)
                .ok()
                .map(|p| pending_count(&p)),
            Err(_) => None,
        };
        self.cache_host(id, |c| match probed {
            Ok(state) => {
                c.state = Some(state);
                c.error = None;
            }
            Err(e) => c.error = Some(e),
        });
        let cache = self.cached_hosts();
        Ok(self.host_view(host, &set, cache.get(id), session.as_ref(), pending))
    }

    pub(crate) async fn set_host_follow_impl(
        &self,
        id: &TargetId,
        follow: bool,
    ) -> Result<HostView> {
        let (host, session) = if follow {
            self.app_host(id).await?
        } else {
            // Turning it off also clears a follow a 0.1.0 Superba let the user set
            // on a host it does not manage.
            let session = self.deps.app.running_session().await.ok().flatten();
            let host = self
                .app_hosts(session.as_ref(), None)
                .await?
                .into_iter()
                .find(|h| &h.target_id == id);
            match host {
                Some(host) => (host, session),
                None => self.app_host(id).await?,
            }
        };
        let mut set = self.deps.profiles.load().await?;
        let entry = set.settings.hosts.entry(id.clone()).or_default();
        if host.kind == superba_core::HostKind::Wsl {
            // A WSL distribution follows by default: turning it off is a choice
            // to remember (user bug 45).
            entry.choose_follow(follow);
        } else {
            entry.follow_switches = follow;
            entry.follow_chosen = false;
        }
        if *entry == HostSettings::default() {
            set.settings.hosts.remove(id);
        }
        self.deps.profiles.save(&set).await?;
        let cache = self.cached_hosts();
        Ok(self.host_view(host, &set, cache.get(id), session.as_ref(), None))
    }

    /// The sync preview for one probed host (pure apart from reading the
    /// profile set it is given).
    /// The synced profile is the applied key-based profile (from `config`),
    /// else the last selected, else the first key-based one.
    pub(crate) fn sync_preview(
        &self,
        set: &ProfileSet,
        config: Option<&ConfigState>,
        host: &RemoteHost,
        state: &RemoteState,
    ) -> Result<(HostSyncPlan, SettingsPlan, CodexSettings)> {
        let profile = crate::settings::pick_profile(set, None, config)?;
        let wanted = profile.codex_settings.clone();
        let consumers = [SettingsConsumer::new(
            ConsumerKind::Remote {
                target_id: host.target_id.clone(),
            },
            host.label.clone(),
            state.codex_version.clone(),
        )];
        let catalog = (self.deps.remote_catalog)(&state.os);
        let config = state.config_toml.as_ref().map(|b| b.expose());
        let sp = plan_settings(
            &wanted,
            &HomeInput::new(&host.target_id, &consumers, catalog.as_ref())
                .config_toml(config)
                .owned_paths(&state.owned_paths),
        );
        let values = read_config_values(config).unwrap_or_default();
        let last = set
            .settings
            .hosts
            .get(&host.target_id)
            .and_then(|h| h.last_sync.as_ref());
        let rows = sync_rows(&wanted, &sp, &values, last, &state.owned_paths);
        let mut notes = Vec::new();
        if state.codex_version.is_none() {
            notes.push(Notice::warning(
                text("switch.hosts.version_unknown").arg("host", &host.label),
            ));
        }
        if host.host_id.is_none() {
            notes.push(Notice::info(
                text("switch.hosts.no_hot_restart").arg("host", &host.label),
            ));
        }
        for n in &sp.notes {
            if !notes.contains(n) {
                notes.push(n.clone());
            }
        }
        let plan = HostSyncPlan {
            target_id: host.target_id.clone(),
            profile_id: Some(profile.id.clone()),
            host_version: state.codex_version.clone(),
            rows,
            blocked: Vec::new(),
            notes,
        };
        Ok((plan, sp, wanted))
    }

    pub(crate) async fn host_sync_plan_impl(&self, id: &TargetId) -> Result<HostSyncPlan> {
        let d = &self.deps;
        let config = d.config.read_state(&d.ctx.codex_home()).await.ok();
        let (host, _) = self.app_host(id).await?;
        let set = d.profiles.load().await?;
        match d.remote.probe(&host).await {
            Ok(state) => {
                let result = self.sync_preview(&set, config.as_ref(), &host, &state);
                self.cache_host(id, |c| {
                    c.state = Some(state);
                    c.error = None;
                });
                result.map(|(plan, _, _)| plan)
            }
            Err(e) => {
                let detail = e.to_string();
                self.cache_host(id, |c| c.error = Some(e));
                Ok(HostSyncPlan {
                    target_id: id.clone(),
                    profile_id: None,
                    host_version: None,
                    rows: Vec::new(),
                    blocked: vec![BlockReason::UnsupportedTarget { detail }],
                    notes: Vec::new(),
                })
            }
        }
    }

    pub(crate) async fn host_sessions_impl(&self, id: &TargetId) -> Result<HostSessions> {
        if id.is_local() {
            let home = self.deps.ctx.codex_home();
            let report = self.deps.sessions.scan(&home).await?;
            let history = self.deps.sessions.history(&home).await?;
            return Ok(HostSessions {
                target_id: id.clone(),
                report,
                history,
                read_only: false,
                migrate_command: None,
            });
        }
        let (host, _) = self.app_host(id).await?;
        let report = self.deps.remote.scan_sessions(&host).await?;
        // A WSL distribution migrates from this computer (Superba's Linux
        // helper runs inside it, user bug 45): the command is this computer's.
        let wsl = host
            .wsl_distro()
            .filter(|_| host.kind == superba_core::HostKind::Wsl);
        let migrate_command = match &wsl {
            Some(distro) => migrate_command(&report).map(|c| {
                c.replacen(
                    "spb sessions migrate",
                    &format!("spb sessions --host wsl:{distro} migrate"),
                    1,
                )
            }),
            None => migrate_command(&report),
        };
        Ok(HostSessions {
            target_id: id.clone(),
            migrate_command,
            report,
            history: Vec::new(),
            read_only: wsl.is_none(),
        })
    }
}

/// Rows a sync would apply by default.
fn pending_count(p: &(HostSyncPlan, SettingsPlan, CodexSettings)) -> u32 {
    u32::try_from(
        p.0.rows
            .iter()
            .filter(|r| r.selected && !matches!(r.action, HostSyncAction::Skip { .. }))
            .count(),
    )
    .unwrap_or(u32::MAX)
}

impl Switcher {
    /// A profile's direct token resolution without probing (API-key form of its
    /// first line, or its compatible base URL): the input of remote plans that
    /// never touch routing or auth (settings sync) and of model lists.
    pub(crate) async fn direct_resolution(
        &self,
        set: &ProfileSet,
        profile: &Profile,
    ) -> Result<(superba_core::Site, superba_core::ResolvedProfile)> {
        use superba_core::{AuthMode, CodexPath, ResolvedEndpoints, ResolvedMode, SiteKind};
        let site = all_sites(set, self.deps.ctx.env.as_ref())
            .into_iter()
            .find(|s| s.id == profile.site_id)
            .ok_or_else(|| Error::NotFound {
                what: format!("site {}", profile.site_id),
            })?;
        let key_ref = profile.key_ref.as_ref().ok_or(Error::Blocked {
            reasons: vec![BlockReason::MissingKey],
        })?;
        let key = self.deps.secrets.get(key_ref).await?;
        if key.is_empty() {
            return Err(Error::Blocked {
                reasons: vec![BlockReason::MissingKey],
            });
        }
        let base = if site.kind == SiteKind::Compatible {
            profile
                .base_url_override
                .clone()
                .or_else(|| crate::planner::compatible_base_url(&site, profile.line_id.as_ref()))
        } else {
            None
        };
        let (endpoints, line_id) = match base {
            Some(b) => (ResolvedEndpoints::for_base_url(&b)?, None),
            None => {
                let line = profile
                    .line_priority()
                    .iter()
                    .find_map(|l| site.line(l))
                    .or_else(|| site.default_line())
                    .cloned()
                    .ok_or_else(|| Error::NotFound {
                        what: format!("line for site {}", site.id),
                    })?;
                let path = site
                    .capabilities
                    .codex_paths
                    .first()
                    .copied()
                    .unwrap_or(CodexPath::BackendApiCodex);
                (
                    ResolvedEndpoints::for_line(&line.origin, path, AuthMode::ApiKey)?,
                    Some(line.id),
                )
            }
        };
        let resolved = superba_core::ResolvedProfile {
            model_catalog: None,
            profile_id: Some(profile.id.clone()),
            site_id: Some(site.id.clone()),
            line_id,
            mode: ResolvedMode::Token {
                site_kind: site.kind,
                auth: AuthMode::ApiKey,
                endpoints,
                key,
            },
            model: Default::default(),
            key_resolutions: Default::default(),
            settings: Default::default(),
        };
        Ok((site, resolved))
    }

    /// Plan a settings sync to one host ([`superba_core::SwitchIntent::HostSync`]):
    /// settings scope only, never local writes; executed through the remote
    /// apply -> hot restart (`killCodexProcess`) -> verify -> roll back path.
    pub(crate) async fn plan_host_sync(
        &self,
        mut request: superba_core::SwitchRequest,
    ) -> Result<superba_core::SwitchPlan> {
        use superba_core::{
            PlanDestination, RemoteApplyScope, RestartKind, SwitchIntent, SwitchPlan,
            TargetSelection,
        };
        let SwitchIntent::HostSync {
            target_id,
            selection,
        } = request.intent.clone()
        else {
            return Err(Error::invalid("not a settings sync"));
        };
        let d = &self.deps;
        request.targets = TargetSelection {
            local: false,
            remotes: vec![target_id.clone()],
        };
        let home = d.ctx.codex_home();
        let set = d.profiles.load().await?;
        let config = d.config.read_state(&home).await?;
        let mut blocks: Vec<BlockReason> = Vec::new();
        let mut warnings: Vec<Notice> = Vec::new();
        if let Some(reason) = crate::planner::account_block(d.ctx.env.as_ref()) {
            blocks.push(reason);
        }
        if let Ok(unfinished) = crate::journal::SwitchJournal::new(&d.ctx.data_dir).unfinished()
            && !unfinished.is_empty()
        {
            blocks.push(BlockReason::JournalPending);
        }
        let session = d.app.running_session().await.ok().flatten();
        let (host, _) = self.app_host(&target_id).await?;
        let label = text("switch.destination.host_sync").arg("host", &host.label);
        let mut remotes = Vec::new();
        let mut active_threads = Vec::new();
        let mut record = None;
        let mut app_server_pids = Vec::new();
        match d.remote.probe(&host).await {
            Err(e) => blocks.push(BlockReason::UnsupportedTarget {
                detail: e.to_string(),
            }),
            Ok(state) => {
                app_server_pids.clone_from(&state.app_server_pids);
                let (preview, sp, _) = self.sync_preview(&set, Some(&config), &host, &state)?;
                warnings.extend(preview.notes.iter().cloned());
                let rows = selected_rows(&preview.rows, &selection);
                if rows.is_empty() {
                    warnings.push(Notice::info(
                        text("switch.hosts.nothing_to_sync").arg("host", &host.label),
                    ));
                } else {
                    let change = sync_settings(&rows, &sp);
                    let profile = crate::settings::pick_profile(&set, None, Some(&config))?;
                    let (_, mut resolved) = self.direct_resolution(&set, profile).await?;
                    resolved.settings = change.settings.clone();
                    // Released keys are resolved, not listed: the settings scope
                    // puts them back to the host's own value (a ticked row is the
                    // user's choice, so a hand edit since is overwritten).
                    for path in &change.release {
                        resolved
                            .key_resolutions
                            .insert(path.clone(), superba_core::KeyResolution::Overwrite);
                    }
                    let last = set
                        .settings
                        .hosts
                        .get(&target_id)
                        .and_then(|h| h.last_sync.as_ref());
                    record = Some(sync_record(last, &change, d.clock.now()));
                    let plan = d
                        .remote
                        .plan_apply_scoped(&host, &resolved, RemoteApplyScope::Settings)
                        .await?;
                    if let Ok(a) = d.remote.turn_activity(&host).await {
                        active_threads.extend(a.busy);
                    }
                    warnings.extend(plan.warnings.iter().cloned());
                    remotes.push(plan);
                }
                self.cache_host(&target_id, |c| {
                    c.state = Some(state);
                    c.error = None;
                });
            }
        }
        let host_restart = match (&blocks.is_empty(), remotes.first()) {
            (true, Some(p)) => crate::rules::decide_remote_probed(
                host.host_id.as_deref(),
                &p.blocked,
                &app_server_pids,
            ),
            (true, None) => RestartKind::None,
            (false, _) => RestartKind::Blocked {
                reasons: blocks.clone(),
            },
        };
        let primary = if blocks.is_empty() {
            RestartKind::None
        } else {
            RestartKind::Blocked {
                reasons: blocks.clone(),
            }
        };
        let restarts = crate::rules::consumer_restarts(
            &primary,
            false,
            &[],
            home.root(),
            &[(target_id.clone(), host_restart)],
        );
        let id = uuid::Uuid::new_v4().simple().to_string();
        let plan = SwitchPlan {
            id: id.clone(),
            created_at: d.clock.now(),
            request,
            from: config.active.clone(),
            to: PlanDestination {
                profile_id: None,
                site_id: None,
                line_id: None,
                auth_mode: None,
                label,
            },
            primary,
            restarts,
            config: None,
            env: None,
            migration: None,
            remotes,
            active_threads,
            model_check: None,
            choices: Vec::new(),
            alternatives: Vec::new(),
            warnings,
            cleanup: Vec::new(),
        };
        let mut pc = crate::planner::PlanContext::bare(home, set.settings.safe_mode);
        pc.session = session;
        pc.hosts = vec![host];
        pc.host_sync = record.map(|r| (target_id, r));
        self.remember_plan(id, pc);
        Ok(plan)
    }
}
