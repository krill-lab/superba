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
//!   router (local PAT and scoped TLS trust).
//! - Switching to another profile while the router is on moves that profile to
//!   the front of the members (a router reload; Codex is restarted only if its own
//!   configuration changes).
//! - `spb router disable`: persist `enabled = false`, then a switch to the applied
//!   profile in direct mode. The service keeps running until `router uninstall`.
//! - Watchdog ([`Switcher::router_watch_impl`]): while Codex goes through the
//!   router, a router that stays down past the grace period makes Superba turn
//!   router mode off and switch the applied profile back to direct mode, unless
//!   it is down because another program holds its port (user bug 65): then the
//!   router moves to a free stable port and a router-on switch points Codex
//!   at it. A router that answers on another port than Codex uses (moved by a
//!   reinstall, or a repair that failed before) gets Codex pointed at it at
//!   once.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use superba_core::{
    ActiveState, AuthShape, ClearReport, ClearRequest, CodexPath, ConfigState, DataDir, Error,
    ExecuteOptions, InflightRequest, KeyResolution, NoEvents, OutcomeStatus, PriceTable, Profile,
    ProfileId, ProfileSet, RequestDetail, RequestPage, RequestQuery, RequestSource, RequestStats,
    Result, RouterInstallRequest, RouterLineHealth, RouterLineView, RouterMemberView,
    RouterStatsSettings, RouterStatsStatus, RouterView, RouterWatch, RouterWatchState,
    SafeDefaults, Secret, Site, SiteKind, StatsQuery, SwitchIntent, SwitchOutcome, SwitchRequest,
    SwitchService,
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
    /// The router token Codex authenticates with.
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
    /// Stored protocol capabilities, used when rebuilding member settings.
    fn configuration(&self) -> Option<RouterConfig> {
        None
    }
    /// Persist one conversion direction and reload without starting the service.
    async fn set_conversion(&self, conversion: superba_core::RouterConversion) -> Result<()> {
        let _ = conversion;
        Err(Error::not_implemented(
            "router protocol conversion settings",
        ))
    }
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
    /// Move a router that is down because another program holds its port to
    /// a free stable port, restart it and wait until it answers (user bug 65).
    /// Codex is not touched. `Ok(None)` when nothing had to move (the router
    /// answers, or its port is free or held by a router).
    async fn move_off_taken_port(&self) -> Result<Option<RouterEndpointInfo>> {
        Ok(None)
    }
    /// Listen also on these WSL adapter addresses (user bug 45; the stored
    /// configuration, reloaded by a running router). Returns whether a running
    /// router reloaded.
    async fn set_wsl_listen(&self, addresses: Vec<std::net::Ipv4Addr>) -> Result<bool> {
        let _ = addresses;
        Err(Error::not_implemented("router WSL listeners"))
    }
    /// The WSL addresses the stored configuration listens on.
    fn wsl_listen(&self) -> Vec<std::net::Ipv4Addr> {
        Vec::new()
    }
    /// Mixed login: accept the user's own ChatGPT login of `account_id` from
    /// local Codex (`None`: router tokens only). Returns whether it changed.
    async fn set_official_client(&self, account_id: Option<String>) -> Result<bool> {
        let _ = account_id;
        Err(Error::not_implemented("router mixed login"))
    }
    /// Add or update the Windows Firewall rule that lets WSL reach the router at
    /// `address:port` (asks for administrator approval). The rule covers the
    /// router program on `address` whatever its port, so a later move of the
    /// port (user bug 65) needs no new approval.
    async fn allow_wsl_firewall(
        &self,
        address: std::net::Ipv4Addr,
        port: u16,
    ) -> Result<superba_router::firewall::FirewallChange> {
        let _ = (address, port);
        Err(Error::not_implemented("Windows Firewall rules"))
    }
    /// Service state and liveness.
    async fn overview(&self) -> (RouterServiceInfo, Liveness);
    /// Ask the running router (admin API on loopback).
    async fn liveness(&self) -> Liveness;

    /// Merged catalog diagnostics from the running router.
    async fn catalog(&self) -> Result<superba_core::RouterCatalogView> {
        Err(Error::not_implemented("router catalog"))
    }

    /// Test one model through the running router.
    async fn test_model(&self, _model: &str) -> Result<superba_core::RouterModelTest> {
        Err(Error::not_implemented("router model test"))
    }

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
    fn configuration(&self) -> Option<RouterConfig> {
        RouterConfig::load(self.manager.paths()).ok()
    }
    async fn set_conversion(&self, conversion: superba_core::RouterConversion) -> Result<()> {
        self.manager.set_conversion(conversion).await.map(|_| ())
    }
    async fn restart(&self) -> Result<()> {
        self.manager.restart(ROUTER_START_WAIT).await
    }

    async fn move_off_taken_port(&self) -> Result<Option<RouterEndpointInfo>> {
        self.manager
            .move_off_taken_port(ROUTER_START_WAIT)
            .await
            .map(|e| e.map(info))
    }

    async fn set_wsl_listen(&self, addresses: Vec<std::net::Ipv4Addr>) -> Result<bool> {
        self.manager.set_wsl_listen(addresses).await
    }

    async fn set_official_client(&self, account_id: Option<String>) -> Result<bool> {
        self.manager
            .set_official_client(
                account_id.map(
                    |account_id| superba_router::official::OfficialClientConfig { account_id },
                ),
            )
            .await
    }

    fn wsl_listen(&self) -> Vec<std::net::Ipv4Addr> {
        superba_router::config::RouterConfig::load(self.manager.paths())
            .map(|c| c.wsl_listen)
            .unwrap_or_default()
    }

    async fn allow_wsl_firewall(
        &self,
        address: std::net::Ipv4Addr,
        _port: u16,
    ) -> Result<superba_router::firewall::FirewallChange> {
        superba_router::firewall::allow_wsl(address, &self.manager.spec().exe).await
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

    async fn catalog(&self) -> Result<superba_core::RouterCatalogView> {
        superba_router::client::AdminClient::from_runtime(self.manager.paths())?
            .catalog()
            .await
    }

    async fn test_model(&self, model: &str) -> Result<superba_core::RouterModelTest> {
        superba_router::client::AdminClient::from_runtime(self.manager.paths())?
            .test_model(model)
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
pub(crate) fn member_path(site: &Site, profile: &Profile) -> Result<MemberPath> {
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
    profile.ordered_line_ids(site)
}

impl Switcher {
    /// Router members for `members` (profile ids, in priority order) with their
    /// keys, and Claude Code's section with the providers only it uses
    /// ([`Switcher::claude_router_section`]). The member id is the profile id.
    pub(crate) async fn build_router_members(
        &self,
        set: &ProfileSet,
        members: &[ProfileId],
    ) -> Result<BuiltMembers> {
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
            // Remote compaction: the check, the user's choice, Krill's fact.
            member.compaction = self.cached_compaction(site, profile, Some(&key)).support;
            if out
                .iter()
                .any(|m: &superba_router::config::MemberConfig| m.id == member.id)
            {
                continue;
            }
            keys.insert(member.id.clone(), key);
            out.push(member);
        }
        let claude = self.claude_router_section(set, &out).await?;
        for m in claude.extra {
            if !out.iter().any(|x| x.id == m.id) {
                out.push(m);
            }
        }
        keys.extend(claude.keys);
        let mut config = RouterConfig::new(0, out);
        if let Some(stored) = self.deps.router.configuration() {
            config.openai = stored.openai;
            config.gemini = stored.gemini;
            config.translation = stored.translation;
        }
        if !claude.openai.is_empty() {
            config
                .openai
                .get_or_insert_with(Default::default)
                .members
                .extend(claude.openai);
        }
        config.claude = Some(claude.section);
        self.add_harness_router_members(set, &mut config, &mut keys)
            .await?;
        if config.members.is_empty() {
            return Err(Error::invalid(
                "the router needs at least one member profile",
            ));
        }
        let ids: Vec<&str> = config.members.iter().map(|m| m.id.as_str()).collect();
        if let Some(openai) = &mut config.openai {
            openai.retain_members(&ids);
        }
        if let Some(gemini) = &mut config.gemini {
            gemini.members.retain(|id, _| ids.contains(&id.as_str()));
            gemini.order.retain(|id| ids.contains(&id.as_str()));
        }
        let allowed: Vec<String> = config
            .members
            .iter()
            .filter(|m| config.claude_eligibility(&m.id).is_ok())
            .map(|m| m.id.clone())
            .collect();
        if let Some(section) = &mut config.claude {
            for route in &mut section.routes {
                route.targets.retain(|t| allowed.contains(&t.member));
            }
            section.routes.retain(|r| !r.targets.is_empty());
        }
        config.catalog = Some(self.router_catalog_config(set, &config).await);
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
        let built = if members.is_empty() {
            // Claude Code may still use it (its own provider).
            self.build_router_members(set, members).await.ok()
        } else {
            Some(self.build_router_members(set, members).await?)
        };
        let result = match built {
            Some(built) => d.router.rewrite(built.config, built.keys).await.map(|_| ()),
            None => {
                if self.claude_routed()
                    && let Err(e) = self
                        .claude_fall_back("the local router has no member left")
                        .await
                {
                    tracing::warn!(error = %e, "Claude Code did not fall back to its provider");
                }
                d.router.uninstall().await
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
        // A reinstall moves a router off a port another program took (user bug
        // 65): Codex, when it goes through the router, follows at once.
        self.follow_moved_router().await;
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
            Self::require_eligible_member(&set, &sites, None)?;
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
        // Claude Code goes back to its provider before the router goes.
        let mut notes = Vec::new();
        if self.claude_routed() {
            notes.push(
                self.claude_fall_back("the local router was uninstalled")
                    .await?,
            );
        }
        d.router.uninstall().await?;
        let mut set = d.profiles.load().await?;
        set.settings.router.enabled = false;
        d.profiles.save(&set).await?;
        let mut view = self.router_status_impl().await?;
        view.notes = notes;
        Ok(view)
    }

    /// Match against the router's recorded Codex ingress.
    pub(crate) fn matches_router_endpoint(&self, url: &str) -> bool {
        self.deps
            .router
            .endpoint()
            .ok()
            .is_some_and(|e| superba_core::router::matches_router_endpoint(url, &e.base_url))
    }

    /// The local Codex home points at the router: its loopback endpoint, or an
    /// earlier port of it (`http://127.0.0.1:<port>/backend-api/codex` with the
    /// router token in `auth.json`), so a router that moved can never make Codex
    /// look direct to the watchdog, `disable` or the uninstall guard.
    pub(crate) fn codex_routed(&self, state: &ConfigState) -> bool {
        // Another provider (cc-switch's and AI Toolbox's `custom`) does not use
        // `openai_base_url` at all.
        if state
            .model_provider
            .as_deref()
            .is_some_and(|p| p != superba_core::codex::PROVIDER_OPENAI)
        {
            return false;
        }
        let Ok(endpoint) = self.deps.router.endpoint() else {
            return false;
        };
        let Some(url) = state.openai_base_url.as_deref() else {
            return false;
        };
        if superba_core::router::matches_router_endpoint(url, &endpoint.base_url) {
            return true;
        }
        let Ok(observed) = Url::parse(url) else {
            return false;
        };
        let mut previous = endpoint.base_url.clone();
        if previous.set_port(observed.port_or_known_default()).is_err() {
            return false;
        }
        superba_core::router::matches_router_endpoint(url, &previous)
            && matches!(&state.auth, AuthShape::ApiKey { key_last4 } | AuthShape::Pat { key_last4 }
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
        let config = d.router.configuration();
        let members = set
            .settings
            .router
            .members
            .iter()
            .map(|id| member_view(&set, id, status.as_ref(), config.as_ref()))
            .collect();
        let stats = if running {
            d.router.stats_status().await
        } else {
            None
        };
        let claude = self.claude_router_view(&set).await;
        Ok(RouterView {
            stats,
            claude,
            notes: Vec::new(),
            settings: set.settings.router.clone(),
            configured: service.configured,
            token_ready: d.router.endpoint().is_ok_and(|e| !e.token.is_empty()),
            translation: config.as_ref().map(|c| c.translation.view()),
            installed: service.installed,
            running,
            port: service.port,
            codex_routed: state.as_ref().is_some_and(|s| {
                superba_core::router::codex_uses_router(
                    s,
                    set.settings.router.enabled,
                    d.router.endpoint().ok().as_ref().map(|e| &e.base_url),
                )
            }),
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
        // GH#5: another tool took Codex over (its own credential or provider):
        // Codex does not go through the router any more, and its configuration
        // is not Superba's to change. Only the stored mode is turned off.
        if taken_over(&state) {
            tracing::warn!("router fallback: Codex was changed by another tool; nothing written");
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
        let request = watchdog_request(&state, target, false);
        self.run_watchdog_switch(request, "the switch back to direct mode")
            .await
            .map(Some)
    }

    /// Point Codex at the router's current port (user bug 65): a router-on
    /// switch to the applied profile, run like the fallback (nothing waits for
    /// an answer; of the keys the app, the user or another tool changed, only
    /// routing keys that still point at a local router are overwritten). Router
    /// mode stays on.
    pub(crate) async fn router_repoint_impl(&self, state: &ConfigState) -> Result<SwitchOutcome> {
        if taken_over(state) {
            return Err(Error::Conflict {
                message: "Codex was changed by another tool; it is not pointed at the local \
                          router's new port"
                    .into(),
            });
        }
        let set = self.deps.profiles.load().await?;
        let target =
            fallback_profile(state.active.as_ref(), &set).ok_or_else(|| Error::NotFound {
                what: "a router member profile to point Codex at the router with".into(),
            })?;
        let request = watchdog_request(state, target, true);
        self.run_watchdog_switch(request, "the switch to the local router's new port")
            .await
    }

    /// Plan and run a watchdog repair without asking anything: turns in flight
    /// are not waited for (Codex cannot reach the router anyway), and a change
    /// that needs a full app restart is only written (the app is never quit on
    /// the user's behalf; its next start picks it up).
    async fn run_watchdog_switch(
        &self,
        request: SwitchRequest,
        what: &str,
    ) -> Result<SwitchOutcome> {
        let plan = self.plan(request).await?;
        let full = matches!(
            plan.primary,
            superba_core::RestartKind::FullAppRestart { .. }
        ) || plan.migration.is_some();
        let opts = ExecuteOptions {
            force_turns: true,
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
                "{what} did not finish ({:?})",
                outcome.status
            )));
        }
        Ok(outcome)
    }

    /// Codex goes through the router, but at another port than the router's
    /// configuration names (the router moved, user bug 65): Codex's port and
    /// the router's.
    pub(crate) fn stale_router_port(&self, state: &ConfigState) -> Option<(u16, u16)> {
        if !self.codex_routed(state) {
            return None;
        }
        let endpoint = self.deps.router.endpoint().ok()?;
        let from = Url::parse(state.openai_base_url.as_deref()?)
            .ok()?
            .port_or_known_default()?;
        (from != endpoint.port).then_some((from, endpoint.port))
    }

    /// After a reinstall: when Codex goes through the router at another port
    /// than the running router's (it moved off a port another program took),
    /// point Codex at it now. A failure is left to the watchdog, which retries.
    pub(crate) async fn follow_moved_router(&self) {
        let d = &self.deps;
        let Ok(state) = d.config.read_state(&d.ctx.codex_home()).await else {
            return;
        };
        let Some((from, to)) = self.stale_router_port(&state) else {
            return;
        };
        if !d.router.liveness().await.is_up() {
            return;
        }
        match self.router_repoint_impl(&state).await {
            Ok(_) => {
                tracing::warn!(from, to, "Codex now uses the local router's new port");
                self.reset_router_watchdog();
            }
            Err(e) => tracing::warn!(
                error = %e,
                from,
                to,
                "Codex was not pointed at the local router's new port; the watchdog retries"
            ),
        }
    }

    pub(crate) async fn router_watch_impl(&self) -> Result<RouterWatch> {
        let d = &self.deps;
        let state = d.config.read_state(&d.ctx.codex_home()).await?;
        // Gated on what Codex and Claude Code are pointed at, not on the
        // stored flag: a home left on the router is watched whatever the
        // flag says.
        let codex = self.codex_routed(&state);
        let claude = self.claude_routed();
        if !codex && !claude {
            if let Ok(mut w) = self.watchdog.lock() {
                w.reset();
            }
            return Ok(RouterWatch {
                state: RouterWatchState::Inactive,
                outcome: None,
                error: None,
                notes: Vec::new(),
            });
        }
        let mut liveness = d.router.liveness().await;
        let mut error = None;
        let mut notes = Vec::new();
        // Claude Code names another port than the router listens on (it
        // moved): write settings.json again (new sessions use it).
        if claude
            && liveness.is_up()
            && let Some((from, to)) = self.claude_stale_port()
        {
            match self.claude_repoint().await {
                Ok(()) => notes.push(superba_core::Notice::info(
                    crate::messages::text("switch.claude.router.moved")
                        .arg("from", from)
                        .arg("to", to),
                )),
                Err(e) => error = Some(e),
            }
        }
        if !codex {
            return self.claude_watch(liveness, error, notes).await;
        }
        // The router answers on another port than Codex uses (it moved off a
        // port another program took, user bug 65): point Codex at it now. Until
        // that works Codex cannot reach it: an outage like a dead router, which
        // falls back to direct mode after the grace period.
        let stale = self.stale_router_port(&state);
        if liveness.is_up()
            && let Some((from, to)) = stale
        {
            match self.router_repoint_impl(&state).await {
                Ok(outcome) => return Ok(self.moved(from, to, outcome)),
                Err(e) => {
                    liveness = Liveness::Down {
                        detail: format!(
                            "Codex is pointed at port {from}, the local router listens on port {to}"
                        ),
                    };
                    error = Some(e);
                }
            }
        }
        let now = self.watch_base + d.clock.monotonic();
        let verdict = match self.watchdog.lock() {
            Ok(mut w) => w.observe(now, &liveness),
            Err(_) => return Err(Error::other("router watchdog state poisoned")),
        };
        let mut watch = match verdict {
            Verdict::Healthy => RouterWatch {
                state: RouterWatchState::Healthy,
                outcome: None,
                error: None,
                notes: Vec::new(),
            },
            Verdict::Suspect { detail, .. } => RouterWatch {
                state: RouterWatchState::Suspect { detail },
                outcome: None,
                error,
                notes: Vec::new(),
            },
            Verdict::FallBack { reason } => {
                let detail = match reason {
                    superba_router::watchdog::FallbackReason::Dead { detail } => detail,
                    other => format!("{other:?}"),
                };
                // The router stayed down. When another program holds its port,
                // it moves to a free one and Codex follows; otherwise (or when
                // that fails) Codex goes back to direct mode.
                if error.is_none()
                    && let Some(watch) = self.move_router(&state, &detail).await
                {
                    return Ok(watch);
                }
                // Claude Code goes back to its provider too.
                if claude {
                    match self.claude_fall_back(&detail).await {
                        Ok(n) => notes.push(n),
                        Err(e) => tracing::warn!(error = %e, "Claude Code did not fall back"),
                    }
                }
                match self.router_fall_back_impl(detail.clone()).await {
                    Ok(outcome) => RouterWatch {
                        state: RouterWatchState::FellBack { detail },
                        outcome,
                        error: None,
                        notes: Vec::new(),
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
                            notes: Vec::new(),
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
                error,
                notes: Vec::new(),
            },
        };
        watch.notes.extend(notes);
        Ok(watch)
    }

    /// One watchdog tick while only Claude Code goes through the router: the
    /// router stays down past the grace period → Claude Code back to its
    /// provider (Codex is not touched).
    async fn claude_watch(
        &self,
        liveness: Liveness,
        error: Option<Error>,
        mut notes: Vec<superba_core::Notice>,
    ) -> Result<RouterWatch> {
        let now = self.watch_base + self.deps.clock.monotonic();
        let verdict = match self.watchdog.lock() {
            Ok(mut w) => w.observe(now, &liveness),
            Err(_) => return Err(Error::other("router watchdog state poisoned")),
        };
        let detail = || match &liveness {
            Liveness::Down { detail } => detail.clone(),
            Liveness::Up(_) => String::new(),
        };
        let state = match verdict {
            Verdict::Healthy => RouterWatchState::Healthy,
            Verdict::Suspect { detail, .. } => RouterWatchState::Suspect { detail },
            Verdict::FallBack { reason } => {
                let detail = match reason {
                    superba_router::watchdog::FallbackReason::Dead { detail } => detail,
                    other => format!("{other:?}"),
                };
                match self.claude_fall_back(&detail).await {
                    Ok(n) => notes.push(n),
                    Err(e) => {
                        if let Ok(mut w) = self.watchdog.lock() {
                            w.reset();
                        }
                        return Ok(RouterWatch {
                            state: RouterWatchState::Suspect { detail },
                            outcome: None,
                            error: Some(e),
                            notes,
                        });
                    }
                }
                RouterWatchState::FellBack { detail }
            }
            Verdict::FellBack => RouterWatchState::FellBack { detail: detail() },
        };
        Ok(RouterWatch {
            state,
            outcome: None,
            error,
            notes,
        })
    }

    /// A router that stayed down because another program holds its port:
    /// move it to a free port and point Codex at it (user bug 65). `None` when
    /// it did not move (nothing held the port, or the move failed): the caller
    /// falls back to direct mode.
    async fn move_router(&self, state: &ConfigState, detail: &str) -> Option<RouterWatch> {
        let endpoint = match self.deps.router.move_off_taken_port().await {
            Ok(Some(endpoint)) => endpoint,
            Ok(None) => return None,
            Err(e) => {
                tracing::warn!(error = %e, "the local router could not move to a free port");
                return None;
            }
        };
        let from = state
            .openai_base_url
            .as_deref()
            .and_then(|u| Url::parse(u).ok())
            .and_then(|u| u.port_or_known_default())
            .unwrap_or(0);
        Some(match self.router_repoint_impl(state).await {
            Ok(outcome) => self.moved(from, endpoint.port, outcome),
            Err(e) => {
                // The router runs on its new port; the next tick points Codex
                // at it again (and falls back after the grace period).
                self.reset_router_watchdog();
                RouterWatch {
                    state: RouterWatchState::Suspect {
                        detail: detail.to_owned(),
                    },
                    outcome: None,
                    error: Some(e),
                    notes: Vec::new(),
                }
            }
        })
    }

    /// The tick result after Codex was pointed at the router's new port.
    fn moved(&self, from: u16, to: u16, outcome: SwitchOutcome) -> RouterWatch {
        tracing::warn!(from, to, "Codex now uses the local router's new port");
        self.reset_router_watchdog();
        RouterWatch {
            state: RouterWatchState::Moved { from, to },
            outcome: Some(outcome),
            error: None,
            notes: Vec::new(),
        }
    }
}

/// GH#5: another tool took Codex over (its own credential or provider).
fn taken_over(state: &ConfigState) -> bool {
    state.auth_drift.is_some()
        || state
            .drift
            .iter()
            .any(|k| k.path == superba_core::codex::config_keys::MODEL_PROVIDER)
}

/// The switch a watchdog repair runs for `target` (router mode `enable`):
/// nothing may wait for an answer, so the user's foreign environment lines and
/// legacy sessions stay as they are, and values the app, the user or another
/// tool changed in keys Superba owns are kept (GH#5), except a routing key that
/// still points at a local router, which must leave it (a dead router, or the
/// port the router moved away from). WSL distributions follow as in any
/// router switch; SSH hosts are never routed.
fn watchdog_request(state: &ConfigState, target: ProfileId, enable: bool) -> SwitchRequest {
    let mut request = SwitchRequest {
        intent: SwitchIntent::Router { enable },
        ..SwitchRequest::profile(target)
    };
    request.targets.remotes.clear();
    request.take_over_foreign_env = Some(false);
    request.legacy_sessions = Some(superba_core::LegacySessionChoice::Skip);
    for k in &state.drift {
        let routing = [
            superba_core::codex::config_keys::OPENAI_BASE_URL,
            superba_core::codex::config_keys::CHATGPT_BASE_URL,
        ]
        .contains(&k.path.as_str());
        let to_router = k
            .after
            .as_deref()
            .and_then(superba_config::toml_keys::parse_repr)
            .and_then(|v| {
                v.as_str().map(|url| {
                    state
                        .openai_base_url
                        .as_deref()
                        .and_then(|u| Url::parse(u).ok())
                        .is_some_and(|expected| {
                            superba_core::router::matches_router_endpoint(url, &expected)
                        })
                })
            })
            .unwrap_or(false);
        request.key_resolutions.insert(
            k.path.clone(),
            if routing && to_router {
                KeyResolution::Overwrite
            } else {
                KeyResolution::KeepCurrent
            },
        );
    }
    request
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
    config: Option<&superba_router::RouterConfig>,
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
        last_route: status
            .and_then(|s| s.members.iter().find(|m| m.id == rid))
            .and_then(|m| m.last_route.clone()),
        name: set
            .profile(id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| id.to_string()),
        lines,
        protocols: config.map(|c| c.protocol_routes(&rid)).unwrap_or_default(),
    }
}

/// The Windows installer's removal step for the login startup, which also
/// runs where the full router removal is refused (an uninstaller running as
/// another account than the signed-in user): delete the Run entry when it starts the router of the
/// install in `dir` (`superba_router::service::remove_run_entry_of`). Whether
/// an entry was removed; always `false` off Windows.
pub fn remove_login_startup(dir: &std::path::Path) -> bool {
    #[cfg(windows)]
    {
        let store = superba_router::service::WindowsRunKey;
        superba_router::service::remove_run_entry_of(&store, dir).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "cannot remove the router's login startup");
            false
        })
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        false
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
        let wants = |s: RequestSource| query.source.is_none_or(|q| q == s);
        let page = if !wants(RequestSource::Router) {
            RequestPage::default()
        } else {
            match self.deps.router.requests(&query).await {
                Ok(page) => page,
                Err(e) if e.is_not_implemented() => RequestPage::default(),
                Err(e) => return Err(e),
            }
        };
        // Codex's rollouts fill in what the router did not record (also when
        // there is no router at all), imported rows add another tool's
        // history; a live tail of router rows skips both.
        let mut page = if query.after_id.is_some() {
            page
        } else {
            let mut extra = Vec::new();
            let mut truncated = false;
            if wants(RequestSource::Rollout) {
                let (rows, t) = self
                    .rollout_rows(query.from, query.to)
                    .await
                    .unwrap_or_default();
                extra.extend(rows);
                truncated |= t;
            }
            if query.source.is_none_or(|s| s.imported_from().is_some()) {
                match self.imported_rows(query.from, query.to).await {
                    Ok((rows, t)) => {
                        extra.extend(rows);
                        truncated |= t;
                    }
                    Err(e) => tracing::warn!(error = %e, "imported usage not read"),
                }
            }
            crate::usage::merge_page(page, &extra, &query, truncated)
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
        // Imported rows have the lowest ids, rollout rows the other negative ones.
        if id < crate::usage_import::ID_LIMIT {
            return self.imported_detail(id).await;
        }
        if id < 0 {
            return self.rollout_detail(id).await;
        }
        self.deps.router.request(id).await
    }

    pub(crate) async fn router_stats_impl(&self, query: StatsQuery) -> Result<RequestStats> {
        let wants = |s: RequestSource| query.source.is_none_or(|q| q == s);
        let stats = if !wants(RequestSource::Router) {
            RequestStats::default()
        } else {
            match self.deps.router.request_stats(&query).await {
                Ok(stats) => stats,
                Err(e) if e.is_not_implemented() => RequestStats::default(),
                Err(e) => return Err(e),
            }
        };
        let mut extra = Vec::new();
        if wants(RequestSource::Rollout) {
            let (rows, _) = self
                .rollout_rows(query.from, query.to)
                .await
                .unwrap_or_default();
            extra.extend(rows);
        }
        let imported = query.source.is_none_or(|s| s.imported_from().is_some());
        if imported && let Ok((rows, _)) = self.imported_rows(query.from, query.to).await {
            extra.extend(rows);
        }
        let stats = crate::usage::merge_stats(stats, &extra, &query);
        if !imported {
            return Ok(stats);
        }
        let days = self.imported_days().await.unwrap_or_default();
        Ok(crate::usage::merge_days(stats, &days, &query))
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

impl Switcher {
    /// Preserve custom roots before replacing their environment variable with
    /// the router bundle. The environment persister owns restoration of the
    /// original values and shell lines.
    pub(crate) async fn prepare_router_certificate(&self) -> Result<()> {
        let paths = superba_router::RouterPaths::new(&self.deps.ctx.data_dir);
        let target = paths.client_certificate_pem();
        let report = self.deps.env.inspect(superba_router::tls::CERT_ENV).await?;
        let mut sources = Vec::new();
        for value in report
            .persisted
            .iter()
            .filter_map(|p| p.value.as_deref())
            .chain(report.foreign_lines.iter().map(|l| l.value.as_str()))
            .chain(report.process_value.value())
        {
            if !value.is_empty() && std::path::Path::new(value) != target {
                sources.push(PathBuf::from(value));
            }
        }
        let extend = report
            .superba_value
            .as_deref()
            .is_some_and(|v| std::path::Path::new(v) == target);
        superba_router::tls::prepare_client_bundle(&paths, &sources, extend)
    }
}
