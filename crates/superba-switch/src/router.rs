//! Router mode (v2 decision 6) in the switch service: building the router
//! configuration from member profiles, installing / uninstalling the service,
//! the planner's view of the router endpoint, the direct-mode fallback and the
//! watchdog.
//!
//! The router itself is `superba-router`; [`RouterControl`] wraps its
//! [`superba_router::manager::RouterManager`] so tests can use a fake.
//!
//! Flow:
//! - `spb router install` / the GUI: [`Switcher::router_install_impl`] writes the
//!   configuration (members = profiles, each with its own key), installs and starts
//!   the service and waits until it answers. Codex is not touched.
//! - `spb router enable`: install when needed, persist `enabled`, then a normal
//!   switch to the applied profile; the planner points the local Codex home at the
//!   router (API key mode, router token). Remote hosts stay direct.
//! - Switching to another profile while the router is on moves that profile to
//!   the front of the members (a router reload; Codex is restarted only if its own
//!   configuration changes).
//! - `spb router disable`: persist `enabled = false`, then a switch to the applied
//!   profile in direct mode. The service keeps running until `router uninstall`.
//! - Watchdog ([`Switcher::router_watch_impl`]): while Codex goes through the
//!   router, a router that stays down past the grace period makes Superba turn
//!   router mode off and switch the applied profile back to direct mode.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use superba_core::{
    ActiveState, AuthShape, ClearReport, ClearRequest, CodexPath, ConfigState, DataDir, Error,
    ExecuteOptions, InflightRequest, KeyResolution, NoEvents, OutcomeStatus, PriceTable, Profile,
    ProfileId, ProfileSet, RequestDetail, RequestPage, RequestQuery, RequestStats, Result,
    RouterInstallRequest, RouterLineHealth, RouterLineView, RouterMemberView, RouterStatsSettings,
    RouterStatsStatus, RouterView, RouterWatch, RouterWatchState, SafeDefaults, Secret, Site,
    SiteKind, StatsQuery, SwitchIntent, SwitchOutcome, SwitchRequest, SwitchService,
};
use superba_router::config::RouterConfig;
use superba_router::members::{MemberPath, member_from_site, router_id};
use superba_router::status::RouterStatus;
use superba_router::watchdog::{Liveness, Verdict, Watchdog, WatchdogPolicy};
use url::Url;

use crate::Switcher;
use crate::planner::{all_sites, compatible_base_url};

/// How long `install` waits for a new router to answer. A healthy router binds
/// in well under a second, but macOS checks a program the first time it runs
/// (longer for a large universal binary on a busy Mac) and launchd waits 5 s
/// before restarting a job that exited; a crash loop is reported sooner (see
/// `RouterManager::enable`).
pub const ROUTER_START_WAIT: Duration = Duration::from_secs(30);

/// Where Codex sends requests in router mode.
#[derive(Clone, Debug)]
pub struct RouterEndpointInfo {
    /// `openai_base_url` (`http://127.0.0.1:<port>/backend-api/codex`).
    pub base_url: Url,
    /// The router token Codex authenticates with (API key mode).
    pub token: Secret,
    /// Loopback port.
    pub port: u16,
}

/// Router service state as the switch service needs it.
#[derive(Clone, Debug, Default)]
pub struct RouterServiceInfo {
    /// A configuration exists.
    pub configured: bool,
    /// The service definition is installed.
    pub installed: bool,
    /// Port of the configuration.
    pub port: Option<u16>,
    /// Why the service state is unknown (no secrets).
    pub detail: Option<String>,
}

/// The router as seen by the switch service (production: `superba-router`'s
/// manager; tests: a fake).
#[async_trait]
pub trait RouterControl: Send + Sync + std::fmt::Debug {
    /// Endpoint of the stored configuration (reads files only).
    fn endpoint(&self) -> Result<RouterEndpointInfo>;
    /// Write the configuration, install and start the service, wait until it
    /// answers. Keeps the router token (and the port when `listen_port` is 0).
    async fn install(
        &self,
        config: RouterConfig,
        keys: BTreeMap<String, Secret>,
    ) -> Result<RouterEndpointInfo>;
    /// Apply a changed configuration to a running router (reload).
    async fn update(
        &self,
        config: RouterConfig,
        keys: BTreeMap<String, Secret>,
    ) -> Result<RouterEndpointInfo>;
    /// Rewrite the configuration and keys without installing or starting the
    /// service (a running router reloads them). Keeps the port and token.
    async fn rewrite(
        &self,
        config: RouterConfig,
        keys: BTreeMap<String, Secret>,
    ) -> Result<RouterEndpointInfo>;
    /// Stop and remove the service; delete the secrets and runtime files.
    async fn uninstall(&self) -> Result<()>;
    /// Restart the installed service so it runs the binary on disk (after a
    /// Superba update). Configuration, port and token stay.
    async fn restart(&self) -> Result<()> {
        Err(Error::not_implemented("router restart"))
    }
    /// Service state and liveness.
    async fn overview(&self) -> (RouterServiceInfo, Liveness);
    /// Ask the running router (admin API on loopback).
    async fn liveness(&self) -> Liveness;

    // --- v0.3 request log (read-only database; admin API for live data) ------

    /// Request log rows, totals and groups (works while the router is stopped).
    async fn requests(&self, query: &RequestQuery) -> Result<RequestPage> {
        let _ = query;
        Err(Error::not_implemented("router request log"))
    }
    /// One request with timing, attempts and recorded content.
    async fn request(&self, id: i64) -> Result<RequestDetail> {
        let _ = id;
        Err(Error::not_implemented("router request detail"))
    }
    /// Rows of these threads (thread id or prompt cache key) started in
    /// `[from, to)`: what Codex rollout rows are matched against (empty without
    /// a request log).
    async fn requests_for_threads(
        &self,
        from: Option<superba_core::Timestamp>,
        to: Option<superba_core::Timestamp>,
        threads: Vec<String>,
    ) -> Result<Vec<superba_core::RequestRow>> {
        let _ = (from, to, threads);
        Ok(Vec::new())
    }
    /// Grouped aggregates.
    async fn request_stats(&self, query: &StatsQuery) -> Result<RequestStats> {
        let _ = query;
        Err(Error::not_implemented("router statistics"))
    }
    /// Delete rows and/or content.
    async fn clear_requests(&self, request: &ClearRequest) -> Result<ClearReport> {
        let _ = request;
        Err(Error::not_implemented("router request log clearing"))
    }
    /// Requests in flight (empty when the router is not running).
    async fn inflight(&self) -> Result<Vec<InflightRequest>> {
        Err(Error::not_implemented("router in-flight requests"))
    }
    /// Recording, stats line and prices for the router (a reload, no restart);
    /// `locale` is the language of the default stats line template.
    async fn configure_stats(
        &self,
        settings: &RouterStatsSettings,
        prices: &PriceTable,
        locale: superba_core::Locale,
    ) -> Result<()> {
        let _ = (settings, prices, locale);
        Err(Error::not_implemented("router statistics settings"))
    }
    /// Recorder state of the running router.
    async fn stats_status(&self) -> Option<RouterStatsStatus> {
        None
    }

    /// Diagnostic facts for `spb router doctor` (program, service, files,
    /// port, log; never a secret).
    async fn doctor(&self) -> Result<superba_router::doctor::RouterFacts> {
        Err(Error::not_implemented("router doctor"))
    }
}

/// Production [`RouterControl`] over [`superba_router::manager::RouterManager`].
#[derive(Debug)]
pub struct SystemRouterControl {
    manager: superba_router::manager::RouterManager,
}

impl SystemRouterControl {
    /// The router service for `data_dir`, running the `superba-router` that ships
    /// with the current executable
    /// ([`superba_router::manager::current_router_exe`]: next to it in every
    /// release layout, the macOS bundle, else on the PATH).
    pub fn new(data_dir: &DataDir, home: PathBuf) -> Self {
        let exe = superba_router::manager::current_router_exe();
        Self {
            manager: superba_router::manager::RouterManager::system(data_dir, home, exe),
        }
    }
}

fn info(e: superba_router::manager::RouterEndpoint) -> RouterEndpointInfo {
    RouterEndpointInfo {
        base_url: e.base_url,
        token: e.token,
        port: e.port,
    }
}

/// A fallback for [`superba_router::manager::RouterManager::disable`] used by
/// uninstall: the switch service has already pointed Codex away from the router.
struct AlreadyDirect;

#[async_trait]
impl superba_router::watchdog::DirectModeFallback for AlreadyDirect {
    async fn fall_back_to_direct(
        &self,
        _reason: &superba_router::watchdog::FallbackReason,
    ) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl RouterControl for SystemRouterControl {
    async fn restart(&self) -> Result<()> {
        self.manager.restart(ROUTER_START_WAIT).await
    }

    fn endpoint(&self) -> Result<RouterEndpointInfo> {
        self.manager.endpoint().map(info)
    }

    async fn install(
        &self,
        config: RouterConfig,
        keys: BTreeMap<String, Secret>,
    ) -> Result<RouterEndpointInfo> {
        let running = superba_router::watchdog::check(self.manager.paths())
            .await
            .is_up();
        if running {
            self.manager
                .update(&config, keys, ROUTER_START_WAIT)
                .await
                .map(info)
        } else {
            self.manager
                .enable(&config, keys, ROUTER_START_WAIT)
                .await
                .map(info)
        }
    }

    async fn update(
        &self,
        config: RouterConfig,
        keys: BTreeMap<String, Secret>,
    ) -> Result<RouterEndpointInfo> {
        self.manager
            .update(&config, keys, ROUTER_START_WAIT)
            .await
            .map(info)
    }

    async fn rewrite(
        &self,
        config: RouterConfig,
        keys: BTreeMap<String, Secret>,
    ) -> Result<RouterEndpointInfo> {
        self.manager.rewrite(&config, keys).await.map(info)
    }

    async fn uninstall(&self) -> Result<()> {
        self.manager.disable(&AlreadyDirect).await
    }

    async fn overview(&self) -> (RouterServiceInfo, Liveness) {
        let o = self.manager.overview().await;
        let port = RouterConfig::load(self.manager.paths())
            .ok()
            .map(|c| c.listen_port)
            .filter(|p| *p != 0);
        let (installed, detail) = match o.service {
            Ok(s) => (s.installed, s.detail),
            Err(e) => (false, Some(e.to_string())),
        };
        (
            RouterServiceInfo {
                configured: o.configured,
                installed,
                port,
                detail,
            },
            o.liveness,
        )
    }

    async fn liveness(&self) -> Liveness {
        superba_router::watchdog::check(self.manager.paths()).await
    }

    async fn requests(&self, query: &RequestQuery) -> Result<RequestPage> {
        let (paths, query) = (self.manager.paths().clone(), query.clone());
        blocking(move || superba_router::store::Reader::open(&paths)?.query(&query)).await
    }

    async fn request(&self, id: i64) -> Result<RequestDetail> {
        let paths = self.manager.paths().clone();
        blocking(move || superba_router::store::Reader::open(&paths)?.detail(id)).await
    }

    async fn requests_for_threads(
        &self,
        from: Option<superba_core::Timestamp>,
        to: Option<superba_core::Timestamp>,
        threads: Vec<String>,
    ) -> Result<Vec<superba_core::RequestRow>> {
        let paths = self.manager.paths().clone();
        blocking(move || {
            superba_router::store::Reader::open(&paths)?.rows_for_threads(from, to, &threads)
        })
        .await
    }

    async fn request_stats(&self, query: &StatsQuery) -> Result<RequestStats> {
        let (paths, query) = (self.manager.paths().clone(), query.clone());
        blocking(move || superba_router::store::Reader::open(&paths)?.stats(&query)).await
    }

    async fn clear_requests(&self, request: &ClearRequest) -> Result<ClearReport> {
        superba_router::store::clear(self.manager.paths(), request).await
    }

    async fn inflight(&self) -> Result<Vec<InflightRequest>> {
        superba_router::client::AdminClient::from_runtime(self.manager.paths())?
            .inflight()
            .await
    }

    async fn configure_stats(
        &self,
        settings: &RouterStatsSettings,
        prices: &PriceTable,
        locale: superba_core::Locale,
    ) -> Result<()> {
        let stats =
            superba_router::stats::StatsConfig::from_settings(settings, prices.clone(), locale);
        self.manager.configure(stats).await.map(|_| ())
    }

    async fn stats_status(&self) -> Option<RouterStatsStatus> {
        let client =
            superba_router::client::AdminClient::from_runtime(self.manager.paths()).ok()?;
        client.status().await.ok()?.stats
    }

    async fn doctor(&self) -> Result<superba_router::doctor::RouterFacts> {
        Ok(self.manager.doctor().await)
    }
}

/// Run a blocking read of the request log off the async workers.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::other(format!("request log read failed: {e}")))?
}

/// Router configuration built from member profiles, with their keys.
#[derive(Clone, Debug)]
pub struct BuiltMembers {
    /// Configuration (`listen_port` 0 = keep the current port).
    pub config: RouterConfig,
    /// Member keys by member id.
    pub keys: BTreeMap<String, Secret>,
}

/// The path below each line origin a member serves.
fn member_path(site: &Site, profile: &Profile) -> Result<MemberPath> {
    match site.kind {
        SiteKind::Krill => Ok(MemberPath::Codex(CodexPath::BackendApiCodex)),
        SiteKind::Compatible => {
            let base = profile
                .base_url_override
                .clone()
                .or_else(|| compatible_base_url(site, profile.line_id.as_ref()))
                .ok_or_else(|| {
                    Error::invalid(format!("site {} has no Responses base URL", site.id))
                })?;
            Ok(MemberPath::of_base_url(&base))
        }
        _ => Err(Error::invalid(format!(
            "profile {} uses the official ChatGPT login, which is never routed",
            profile.id
        ))),
    }
}

/// The base URL the router uses for `line` of `profile`'s member (what the
/// transport checks probe).
pub(crate) fn member_line_base(
    site: &Site,
    profile: &Profile,
    line: &superba_core::Line,
) -> Result<url::Url> {
    let suffix = match member_path(site, profile)? {
        MemberPath::Codex(p) => p.suffix().to_owned(),
        MemberPath::Custom(p) => p,
    };
    Ok(line.origin.join(&suffix))
}

/// Line order of a member: the profile's priority, then the site's other lines,
/// so the router can fail over across every line of the site.
pub(crate) fn member_lines(site: &Site, profile: &Profile) -> Vec<superba_core::LineId> {
    let mut lines = profile.line_priority();
    lines.retain(|l| site.line(l).is_some());
    for l in &site.lines {
        if !lines.contains(&l.id) {
            lines.push(l.id.clone());
        }
    }
    lines
}

impl Switcher {
    /// Router members for `members` (profile ids, in priority order) with their
    /// keys. The member id is the profile id.
    pub(crate) async fn build_router_members(
        &self,
        set: &ProfileSet,
        members: &[ProfileId],
    ) -> Result<BuiltMembers> {
        if members.is_empty() {
            return Err(Error::invalid(
                "the router needs at least one member profile",
            ));
        }
        let sites = all_sites(set, self.deps.ctx.env.as_ref());
        let mut out = Vec::new();
        let mut keys = BTreeMap::new();
        for id in members {
            let profile = set.profile(id).ok_or_else(|| Error::NotFound {
                what: format!("profile {id}"),
            })?;
            let site = sites
                .iter()
                .find(|s| s.id == profile.site_id)
                .ok_or_else(|| Error::NotFound {
                    what: format!("site {}", profile.site_id),
                })?;
            if profile.is_builtin() || site.kind == SiteKind::Official {
                return Err(Error::invalid(format!(
                    "profile {id} uses the official ChatGPT login, which is never routed"
                )));
            }
            let key_ref = profile.key_ref.as_ref().ok_or_else(|| Error::Blocked {
                reasons: vec![superba_core::BlockReason::MissingKey],
            })?;
            let key = self.deps.secrets.get(key_ref).await?;
            if key.is_empty() {
                return Err(Error::Blocked {
                    reasons: vec![superba_core::BlockReason::MissingKey],
                });
            }
            let path = member_path(site, profile)?;
            let mut member = member_from_site(site, &member_lines(site, profile), &path)?;
            member.id = router_id(id.as_str());
            member.name = profile.name.clone();
            member.model_map = site.model_map.clone();
            // What the transport checks found per line (zstd, WebSocket).
            self.apply_transport_hints(&mut member, site, profile, &key);
            if out
                .iter()
                .any(|m: &superba_router::config::MemberConfig| m.id == member.id)
            {
                continue;
            }
            keys.insert(member.id.clone(), key);
            out.push(member);
        }
        let config = RouterConfig::new(0, out);
        config.validate()?;
        Ok(BuiltMembers { config, keys })
    }

    /// Keep the router in step with `set` (already saved) after a member profile
    /// was deleted or changed: rewrite its members and keys, or uninstall it when
    /// no member is left. Never installs or starts the service; nothing to do
    /// when it was never installed (no key file exists).
    pub(crate) async fn resync_router_members(&self, set: &ProfileSet) -> Result<()> {
        let d = &self.deps;
        if d.router.endpoint().is_err() {
            return Ok(());
        }
        let members = &set.settings.router.members;
        let result = if members.is_empty() {
            d.router.uninstall().await
        } else {
            match self.build_router_members(set, members).await {
                Ok(built) => d.router.rewrite(built.config, built.keys).await.map(|_| ()),
                Err(e) => Err(e),
            }
        };
        result.map_err(|e| {
            Error::other(format!(
                "the change was saved, but the local router could not be updated: {e}"
            ))
        })
    }

    /// Forget the current outage (router mode changed).
    pub(crate) fn reset_router_watchdog(&self) {
        if let Ok(mut w) = self.watchdog.lock() {
            w.reset();
        }
    }

    pub(crate) async fn router_install_impl(
        &self,
        request: RouterInstallRequest,
    ) -> Result<RouterView> {
        let d = &self.deps;
        let mut set = d.profiles.load().await?;
        let members = if request.members.is_empty() {
            set.settings.router.members.clone()
        } else {
            request.members.clone()
        };
        let built = self.build_router_members(&set, &members).await?;
        d.router.install(built.config, built.keys).await?;
        set.settings.router.members = members;
        d.profiles.save(&set).await?;
        // The stored recording / stats line / price settings (v0.3) reach a new
        // router too; a router without stats support keeps running unchanged.
        self.push_stats(&set).await?;
        self.router_status_impl().await
    }

    /// Hand the stored stats settings and the feed's prices to the router.
    /// [`superba_core::SwitchService::price_table`]: the feed's prices (cache,
    /// else built-in), the same table the router is configured with.
    pub(crate) async fn price_table_impl(&self) -> Result<PriceTable> {
        let set = self.deps.profiles.load().await?;
        let feed = (self.deps.feeds)(&set.settings.network)
            .snapshot(false)
            .await
            .ok()
            .map(|s| s.prices);
        Ok(price_table(feed))
    }

    pub(crate) async fn push_stats(&self, set: &ProfileSet) -> Result<()> {
        let d = &self.deps;
        let feed = (d.feeds)(&set.settings.network)
            .snapshot(false)
            .await
            .ok()
            .map(|s| s.prices);
        let locale = self.stats_locale(&set.settings);
        match d
            .router
            .configure_stats(&set.settings.router.stats, &price_table(feed), locale)
            .await
        {
            Ok(()) => Ok(()),
            Err(e) if e.is_not_implemented() => {
                tracing::debug!(error = %e, "router stats settings not applied");
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    pub(crate) async fn router_set_enabled_impl(&self, enabled: bool) -> Result<RouterView> {
        let d = &self.deps;
        let mut set = d.profiles.load().await?;
        if enabled {
            let sites = all_sites(&set, self.deps.ctx.env.as_ref());
            Self::require_eligible_member(&set, &sites)?;
            if !d.router.liveness().await.is_up() {
                // Start (or repair) it with the stored members first.
                let built = self
                    .build_router_members(&set, &set.settings.router.members)
                    .await?;
                d.router.install(built.config, built.keys).await?;
            }
        }
        set.settings.router.enabled = enabled;
        d.profiles.save(&set).await?;
        if let Ok(mut w) = self.watchdog.lock() {
            w.reset();
        }
        self.router_status_impl().await
    }

    pub(crate) async fn router_uninstall_impl(&self) -> Result<RouterView> {
        let d = &self.deps;
        let state = d.config.read_state(&d.ctx.codex_home()).await?;
        if self.codex_routed(&state) {
            return Err(Error::Conflict {
                message: "Codex still goes through the local router; turn router mode off \
                          first (spb router disable)"
                    .into(),
            });
        }
        d.router.uninstall().await?;
        let mut set = d.profiles.load().await?;
        set.settings.router.enabled = false;
        d.profiles.save(&set).await?;
        self.router_status_impl().await
    }

    /// The local Codex home points at the router: its loopback endpoint, or an
    /// earlier port of it (`http://127.0.0.1:<port>/backend-api/codex` with the
    /// router token in `auth.json`), so a router that moved can never make Codex
    /// look direct to the watchdog, `disable` or the uninstall guard.
    pub(crate) fn codex_routed(&self, state: &ConfigState) -> bool {
        let Ok(endpoint) = self.deps.router.endpoint() else {
            return false;
        };
        let Some(url) = state.openai_base_url.as_deref() else {
            return false;
        };
        if url.trim_end_matches('/') == endpoint.base_url.as_str().trim_end_matches('/') {
            return true;
        }
        is_router_url(url)
            && matches!(&state.auth, AuthShape::ApiKey { key_last4 }
                if !endpoint.token.is_empty() && *key_last4 == endpoint.token.last4())
    }

    pub(crate) async fn router_status_impl(&self) -> Result<RouterView> {
        let d = &self.deps;
        let set = d.profiles.load().await?;
        let state = d.config.read_state(&d.ctx.codex_home()).await.ok();
        let (service, liveness) = d.router.overview().await;
        let (running, status, detail) = match liveness {
            Liveness::Up(s) => (true, Some(*s), None),
            Liveness::Down { detail } => (false, None, Some(detail)),
        };
        let members = set
            .settings
            .router
            .members
            .iter()
            .map(|id| member_view(&set, id, status.as_ref()))
            .collect();
        let stats = if running {
            d.router.stats_status().await
        } else {
            None
        };
        Ok(RouterView {
            stats,
            settings: set.settings.router.clone(),
            configured: service.configured,
            installed: service.installed,
            running,
            port: service.port,
            codex_routed: state.as_ref().is_some_and(|s| self.codex_routed(s)),
            members,
            requests: status.as_ref().map_or(0, |s| s.totals.requests),
            failovers: status.as_ref().map_or(0, |s| s.totals.failovers),
            local: status.as_ref().map(|s| s.local.view()),
            // Nothing configured is not a failure worth a detail.
            detail: if service.configured {
                detail.or(service.detail)
            } else {
                None
            },
        })
    }

    pub(crate) async fn router_fall_back_impl(
        &self,
        reason: String,
    ) -> Result<Option<SwitchOutcome>> {
        let d = &self.deps;
        tracing::warn!(%reason, "router fallback: switching back to direct mode");
        let state = d.config.read_state(&d.ctx.codex_home()).await?;
        if !self.codex_routed(&state) {
            // Nothing goes through the router: only the stored mode is left.
            let mut set = d.profiles.load().await?;
            if set.settings.router.enabled {
                set.settings.router.enabled = false;
                d.profiles.save(&set).await?;
            }
            return Ok(None);
        }
        let set = d.profiles.load().await?;
        let Some(target) = fallback_profile(state.active.as_ref(), &set) else {
            return Err(Error::NotFound {
                what: "a profile to fall back to".into(),
            });
        };
        // A router-off switch: router mode is stored as off only after it
        // succeeded, so a failed attempt keeps the watchdog running and is
        // retried at a later tick.
        let mut request = SwitchRequest {
            intent: SwitchIntent::Router { enable: false },
            ..SwitchRequest::profile(target)
        };
        request.targets.remotes.clear();
        // Nothing may wait for an answer: keep the user's foreign environment
        // lines and legacy sessions as they are, and keep values the app or the
        // user changed in keys Superba owns (except the routing keys, which must
        // leave the dead router).
        request.take_over_foreign_env = Some(false);
        request.legacy_sessions = Some(superba_core::LegacySessionChoice::Skip);
        for k in &state.drift {
            let routing = [
                superba_core::codex::config_keys::OPENAI_BASE_URL,
                superba_core::codex::config_keys::CHATGPT_BASE_URL,
                superba_core::codex::config_keys::MODEL_PROVIDER,
            ]
            .contains(&k.path.as_str());
            request.key_resolutions.insert(
                k.path.clone(),
                if routing {
                    KeyResolution::Overwrite
                } else {
                    KeyResolution::KeepCurrent
                },
            );
        }
        let plan = self.plan(request).await?;
        let full = matches!(
            plan.primary,
            superba_core::RestartKind::FullAppRestart { .. }
        ) || plan.migration.is_some();
        let opts = ExecuteOptions {
            force_turns: true,
            // Never quit the app on the user's behalf: write the configuration and
            // let the next start pick it up.
            write_only: full,
            ..ExecuteOptions::default()
        };
        let outcome = self
            .execute(
                plan,
                opts,
                std::sync::Arc::new(SafeDefaults),
                std::sync::Arc::new(NoEvents),
            )
            .await?;
        if !matches!(
            outcome.status,
            OutcomeStatus::Switched | OutcomeStatus::WrittenOnly | OutcomeStatus::Partial
        ) {
            return Err(Error::other(format!(
                "the switch back to direct mode did not finish ({:?})",
                outcome.status
            )));
        }
        Ok(Some(outcome))
    }

    pub(crate) async fn router_watch_impl(&self) -> Result<RouterWatch> {
        let d = &self.deps;
        let state = d.config.read_state(&d.ctx.codex_home()).await?;
        // Gated on what Codex is pointed at, not on the stored flag: a Codex
        // home left on the router is watched whatever the flag says.
        if !self.codex_routed(&state) {
            if let Ok(mut w) = self.watchdog.lock() {
                w.reset();
            }
            return Ok(RouterWatch {
                state: RouterWatchState::Inactive,
                outcome: None,
                error: None,
            });
        }
        let liveness = d.router.liveness().await;
        let now = self.watch_base + d.clock.monotonic();
        let verdict = match self.watchdog.lock() {
            Ok(mut w) => w.observe(now, &liveness),
            Err(_) => return Err(Error::other("router watchdog state poisoned")),
        };
        Ok(match verdict {
            Verdict::Healthy => RouterWatch {
                state: RouterWatchState::Healthy,
                outcome: None,
                error: None,
            },
            Verdict::Suspect { detail, .. } => RouterWatch {
                state: RouterWatchState::Suspect { detail },
                outcome: None,
                error: None,
            },
            Verdict::FallBack { reason } => {
                let detail = match reason {
                    superba_router::watchdog::FallbackReason::Dead { detail } => detail,
                    other => format!("{other:?}"),
                };
                match self.router_fall_back_impl(detail.clone()).await {
                    Ok(outcome) => RouterWatch {
                        state: RouterWatchState::FellBack { detail },
                        outcome,
                        error: None,
                    },
                    Err(e) => {
                        // Not done: try again at the next tick.
                        if let Ok(mut w) = self.watchdog.lock() {
                            w.reset();
                        }
                        RouterWatch {
                            state: RouterWatchState::Suspect { detail },
                            outcome: None,
                            error: Some(e),
                        }
                    }
                }
            }
            Verdict::FellBack => RouterWatch {
                state: RouterWatchState::FellBack {
                    detail: match liveness {
                        Liveness::Down { detail } => detail,
                        Liveness::Up(_) => String::new(),
                    },
                },
                outcome: None,
                error: None,
            },
        })
    }
}

/// `http://127.0.0.1:<port>/backend-api/codex` (any port): the shape of a
/// router endpoint.
fn is_router_url(url: &str) -> bool {
    let Ok(u) = Url::parse(url) else {
        return false;
    };
    u.scheme() == "http"
        && u.host_str() == Some("127.0.0.1")
        && u.path().trim_end_matches('/') == superba_router::config::ROUTER_BASE_PATH
}

/// Watchdog state kept by the service between ticks.
pub(crate) fn new_watchdog() -> Mutex<Watchdog> {
    Mutex::new(Watchdog::new(WatchdogPolicy::default()))
}

/// The base instant monotonic clock readings are added to.
pub(crate) fn watch_base() -> Instant {
    Instant::now()
}

/// The profile direct mode falls back to: the applied one, else the first router
/// member, else the last selected one.
fn fallback_profile(active: Option<&ActiveState>, set: &ProfileSet) -> Option<ProfileId> {
    let usable = |id: &ProfileId| {
        set.profile(id)
            .is_some_and(|p| !p.is_builtin() && p.auth_mode.uses_key())
    };
    active
        .and_then(|a| a.profile_id.clone())
        .filter(usable)
        .or_else(|| {
            set.settings
                .router
                .members
                .iter()
                .find(|id| usable(id))
                .cloned()
        })
        .or_else(|| set.last_selected.clone().filter(usable))
}

fn member_view(
    set: &ProfileSet,
    id: &ProfileId,
    status: Option<&RouterStatus>,
) -> RouterMemberView {
    let rid = router_id(id.as_str());
    let lines = status
        .and_then(|s| s.members.iter().find(|m| m.id == rid))
        .map(|m| {
            m.lines
                .iter()
                .map(|l| RouterLineView {
                    id: l.id.clone(),
                    name: l.name.clone(),
                    health: match l.state {
                        superba_router::LineState::Healthy => RouterLineHealth::Healthy,
                        superba_router::LineState::Open => RouterLineHealth::Open,
                        superba_router::LineState::HalfOpen => RouterLineHealth::HalfOpen,
                    },
                    ttfb_ms: l.ttfb_ms,
                    requests: l.requests,
                    failovers_away: l.failovers_away,
                    last_error: l.last_error.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    RouterMemberView {
        profile_id: id.clone(),
        name: set
            .profile(id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| id.to_string()),
        lines,
    }
}

// ---------------------------------------------------------------------------
// v0.3: request log and stats
// ---------------------------------------------------------------------------

/// The UI language when no UI reported one ([`Switcher::stats_locale`]), the
/// way the GUI and `spb` pick theirs: the chosen one (Preferences), else
/// `SUPERBA_LOCALE`, else the system language, else English. The system
/// language comes from the OS preferences on macOS and Windows (an app started
/// from the Finder has no `LANG`) and from `LANGUAGE` / `LC_ALL` /
/// `LC_MESSAGES` / `LANG` elsewhere.
pub fn ui_locale(
    settings: &superba_core::Settings,
    env: &dyn superba_core::EnvSource,
) -> superba_core::Locale {
    settings
        .locale
        .or_else(|| {
            env.var_non_empty("SUPERBA_LOCALE")
                .and_then(|s| superba_core::Locale::parse(&s))
        })
        .or_else(|| sys_locale::get_locale().and_then(|s| superba_core::Locale::parse(&s)))
        .unwrap_or(superba_core::Locale::En)
}

/// The price table the router computes costs with: the feed's public list
/// prices (per-profile ratios and overrides travel in the stats settings).
pub(crate) fn price_table(feed: Option<PriceTable>) -> PriceTable {
    feed.unwrap_or_default()
}

impl Switcher {
    pub(crate) async fn router_requests_impl(&self, query: RequestQuery) -> Result<RequestPage> {
        let page = match self.deps.router.requests(&query).await {
            Ok(page) => page,
            Err(e) if e.is_not_implemented() => RequestPage::default(),
            Err(e) => return Err(e),
        };
        // Codex's rollouts fill in what the router did not record (also when
        // there is no router at all); a live tail of router rows skips them.
        let mut page = if query.after_id.is_some() {
            page
        } else {
            let (rollout, truncated) = self
                .rollout_rows(query.from, query.to)
                .await
                .unwrap_or_default();
            crate::usage::merge_page(page, &rollout, &query, truncated)
        };
        // The database cannot know whether rows are being written: ask the
        // running router (a stopped router records nothing).
        page.recording = match self.deps.router.liveness().await {
            Liveness::Up(_) => self
                .deps
                .router
                .stats_status()
                .await
                .is_some_and(|s| s.recording),
            Liveness::Down { .. } => false,
        };
        Ok(page)
    }

    pub(crate) async fn router_request_impl(&self, id: i64) -> Result<RequestDetail> {
        // Rollout rows have negative ids.
        if id < 0 {
            return self.rollout_detail(id).await;
        }
        self.deps.router.request(id).await
    }

    pub(crate) async fn router_stats_impl(&self, query: StatsQuery) -> Result<RequestStats> {
        let stats = match self.deps.router.request_stats(&query).await {
            Ok(stats) => stats,
            Err(e) if e.is_not_implemented() => RequestStats::default(),
            Err(e) => return Err(e),
        };
        let (rollout, _) = self
            .rollout_rows(query.from, query.to)
            .await
            .unwrap_or_default();
        Ok(crate::usage::merge_stats(stats, &rollout, &query))
    }

    pub(crate) async fn router_clear_impl(&self, request: ClearRequest) -> Result<ClearReport> {
        self.deps.router.clear_requests(&request).await
    }

    pub(crate) async fn router_inflight_impl(&self) -> Result<Vec<InflightRequest>> {
        match self.deps.router.liveness().await {
            Liveness::Up(_) => self.deps.router.inflight().await,
            // Nothing is in flight through a router that does not run.
            Liveness::Down { .. } => Ok(Vec::new()),
        }
    }

    /// The language the default stats line template is resolved in: the one
    /// the running UI reported, else [`ui_locale`].
    pub(crate) fn stats_locale(&self, settings: &superba_core::Settings) -> superba_core::Locale {
        let reported = self.reported_locale.lock().ok().and_then(|l| *l);
        reported.unwrap_or_else(|| ui_locale(settings, self.deps.ctx.env.as_ref()))
    }

    /// Remember the UI language; when it changes the language of the line a
    /// configured router writes (the line is on with the default template),
    /// push the newly resolved template (a reload, never a Codex restart).
    pub(crate) async fn set_ui_locale_impl(&self, locale: superba_core::Locale) -> Result<()> {
        let previous = self
            .reported_locale
            .lock()
            .ok()
            .and_then(|mut l| l.replace(locale));
        if previous == Some(locale) {
            return Ok(());
        }
        let set = self.deps.profiles.load().await?;
        let before =
            previous.unwrap_or_else(|| ui_locale(&set.settings, self.deps.ctx.env.as_ref()));
        let line = &set.settings.router.stats.line;
        if before == locale
            || !line.enabled
            || line.custom_template().is_some()
            || self.refuse_elevated().is_err()
        {
            return Ok(());
        }
        let (service, _) = self.deps.router.overview().await;
        if service.configured {
            self.push_stats(&set).await?;
        }
        Ok(())
    }

    /// Store the stats settings, then hand them (with the feed's prices) to a
    /// configured router; a router that is not configured picks them up at its
    /// next install. Never restarts Codex.
    pub(crate) async fn router_set_stats_impl(
        &self,
        settings: RouterStatsSettings,
    ) -> Result<RouterView> {
        let d = &self.deps;
        let mut set = d.profiles.load().await?;
        set.settings.router.stats = settings.clone();
        d.profiles.save(&set).await?;
        let (service, _) = d.router.overview().await;
        if service.configured {
            self.push_stats(&set).await?;
        }
        self.router_status_impl().await
    }
}
