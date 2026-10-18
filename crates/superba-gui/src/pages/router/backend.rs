//! Where the router page reads and changes things. Members, the per-user service
//! and router mode are managed only through the switch service
//! (`SwitchService::router_status` / `router_install` / `router_uninstall`, and
//! `SwitchIntent::Router` through the switch flow): the member list lives in
//! `ProfileSet.settings.router.members` and the service renders the router's
//! configuration and key file from it. The page only reads the router's admin API
//! directly, for its recent events and totals.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use superba_core::{
    Error, ProfileId, ProfileSet, Result, RouterInstallRequest, RouterLineHealth, RouterView, Site,
    SiteKind, SwitchIntent, SwitchRequest, SwitchService, TargetSelection,
};
use superba_router::manager::{RouterManager, current_router_exe};
use superba_router::watchdog::Liveness;
use superba_router::{RouteEventKind, RouterStatus};
use superba_switch::router_doctor::{RouterCandidate, router_candidates};

use super::model::{
    Candidate, EventView, LineHealth, LineMetrics, LineView, Live, MemberKind, MemberView,
    ServiceView, Snapshot, Totals,
};
use crate::pages::Cx;

/// A boxed future returned by [`RouterBackend`].
pub type BoxFut<T> = Pin<Box<dyn Future<Output = Result<T>> + Send>>;

/// Operations of the router page. The production implementation talks to the
/// switch service; tests pass a fake.
pub trait RouterBackend: Send + Sync {
    /// Members, service state and live status.
    fn snapshot(&self) -> BoxFut<Snapshot>;
    /// Reorder the members (profile ids in the new priority order).
    fn set_order(&self, ids: Vec<String>) -> BoxFut<()>;
    /// Add a profile as a member (the service copies its key for the router).
    fn add_member(&self, candidate: Candidate) -> BoxFut<()>;
    /// Remove a member (the last one uninstalls the router).
    fn remove_member(&self, id: String) -> BoxFut<()>;
    /// Install (or reinstall) and start the per-user service with the members.
    fn install(&self) -> BoxFut<()>;
    /// Restart the router (re-render its configuration and wait until it is up).
    fn restart(&self) -> BoxFut<()>;
    /// Remove the service and the router's files (refused while Codex is routed).
    fn uninstall(&self) -> BoxFut<()>;
}

/// The production backend.
pub struct SystemRouter {
    service: Arc<dyn SwitchService>,
    /// Read-only access to the service manager and the admin API.
    manager: Option<Arc<RouterManager>>,
    /// The router program, when it lives on a network share (checked once:
    /// the program does not move while Superba runs).
    network_exe: Option<String>,
}

impl std::fmt::Debug for SystemRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemRouter").finish_non_exhaustive()
    }
}

/// The production backend.
pub fn default_backend(cx: &Cx<'_>) -> Option<Arc<dyn RouterBackend>> {
    let manager = superba_switch::detect_context().ok().map(|ctx| {
        let exe = current_router_exe();
        Arc::new(RouterManager::system(
            &ctx.data_dir,
            ctx.home_dir.clone(),
            exe,
        ))
    });
    Some(Arc::new(SystemRouter {
        service: cx.service.clone(),
        network_exe: manager
            .as_ref()
            .map(|m| m.spec().exe.clone())
            .filter(|exe| superba_router::manager::is_network_location(exe))
            .map(|exe| superba_core::paths::display_path(&exe)),
        manager,
    }))
}

/// The switch request that turns router mode on (`true`) or off. It goes through
/// the normal switch flow (plan, preview, countdown or confirmation); the plan is
/// blocked with a reason when the router is not installed.
pub fn mode_request(cx: &Cx<'_>, on: bool) -> std::result::Result<SwitchRequest, &'static str> {
    let _ = cx;
    Ok(router_request(on))
}

/// The switch request that turns router mode on or off on this computer
/// (the Router page, the tray menu).
pub fn router_request(on: bool) -> SwitchRequest {
    let mut request = SwitchRequest::profile(ProfileId::new(""));
    request.intent = SwitchIntent::Router { enable: on };
    request.targets = TargetSelection {
        local: true,
        remotes: Vec::new(),
    };
    request
}

fn kind_of(site: Option<&Site>) -> MemberKind {
    match site.map(|s| s.kind) {
        Some(SiteKind::Krill) => MemberKind::Krill,
        _ => MemberKind::Compatible,
    }
}

fn health_of(health: RouterLineHealth) -> LineHealth {
    match health {
        RouterLineHealth::Healthy => LineHealth::Up,
        RouterLineHealth::Open => LineHealth::Down,
        RouterLineHealth::HalfOpen => LineHealth::Probing,
        _ => LineHealth::Unknown,
    }
}

/// Members in priority order (`view.settings.members`), with live line state from
/// the status and line details from the member's site; without live data the
/// site's lines are listed in the profile's priority order.
pub fn members(view: &RouterView, sites: &[Site], profiles: &ProfileSet) -> Vec<MemberView> {
    let mut order: Vec<ProfileId> = view.settings.members.clone();
    for m in &view.members {
        if !order.contains(&m.profile_id) {
            order.push(m.profile_id.clone());
        }
    }
    order
        .iter()
        .map(|id| {
            let profile = profiles.profile(id);
            let site = profile.and_then(|p| sites.iter().find(|s| s.id == p.site_id));
            let live = view.members.iter().find(|m| &m.profile_id == id);
            let kind = kind_of(site);
            let base_url = |line: &str| {
                site.and_then(|s| s.line(&superba_core::LineId::new(line)))
                    .map(|l| l.origin.as_str().to_owned())
                    .unwrap_or_default()
            };
            let lines: Vec<LineView> = match live {
                Some(m) if !m.lines.is_empty() => m
                    .lines
                    .iter()
                    .map(|l| LineView {
                        id: l.id.clone(),
                        name: l.name.clone(),
                        base_url: base_url(&l.id),
                        health: health_of(l.health),
                        metrics: Some(LineMetrics {
                            ttfb_ms: l.ttfb_ms,
                            requests: l.requests,
                            failovers_away: l.failovers_away,
                            last_error: l.last_error.clone(),
                            ..LineMetrics::default()
                        }),
                    })
                    .collect(),
                _ => match (site, profile) {
                    (Some(site), Some(profile)) => {
                        crate::pages::shared::ordered_lines(site, Some(profile))
                            .into_iter()
                            .map(|l| LineView {
                                id: l.id.to_string(),
                                name: l.name.fallback.clone(),
                                base_url: l.origin.as_str().to_owned(),
                                health: LineHealth::Unknown,
                                metrics: None,
                            })
                            .collect()
                    }
                    _ => Vec::new(),
                },
            };
            MemberView {
                id: id.to_string(),
                name: live
                    .map(|m| m.name.clone())
                    .or_else(|| profile.map(|p| p.name.clone()))
                    .unwrap_or_else(|| id.to_string()),
                kind,
                lines,
                websocket: kind == MemberKind::Krill,
                model_mappings: site
                    .filter(|s| s.kind != SiteKind::Krill)
                    .map_or(0, |s| s.model_map.len()),
                has_key: profile.is_some_and(|p| p.key_ref.is_some()),
            }
        })
        .collect()
}

/// Profiles that can join: key-based, non-built-in profiles on Krill or custom
/// sites that are not members yet, one per site (the official login never can,
/// nor a profile set to PAT only: the router sends an API key, and the Router
/// page explains how to fix those).
pub fn candidates(sites: &[Site], profiles: &ProfileSet, members: &[ProfileId]) -> Vec<Candidate> {
    let member_sites: Vec<_> = members
        .iter()
        .filter_map(|id| profiles.profile(id))
        .map(|p| p.site_id.clone())
        .collect();
    let mut out: Vec<Candidate> = Vec::new();
    for p in profiles
        .profiles
        .iter()
        .filter(|p| p.builtin.is_none() && p.key_ref.is_some())
    {
        let Some(site) = sites.iter().find(|s| s.id == p.site_id) else {
            continue;
        };
        if !superba_switch::router_doctor::eligibility(p, Some(site)).is_eligible() {
            continue;
        }
        let kind = match site.kind {
            SiteKind::Krill => MemberKind::Krill,
            SiteKind::Compatible => MemberKind::Compatible,
            _ => continue,
        };
        if members.contains(&p.id)
            || member_sites.contains(&site.id)
            || out.iter().any(|c| c.site_id == site.id.as_str())
        {
            continue;
        }
        out.push(Candidate {
            site_id: site.id.to_string(),
            profile_id: p.id.to_string(),
            name: if p.name.is_empty() {
                site.name.fallback.clone()
            } else {
                p.name.clone()
            },
            kind,
        });
    }
    out
}

/// Every provider as a possible member, with the members this snapshot lists
/// (`view.settings.members`): the 开启 gate and its explanation read the same
/// load as the member list, never the shell's older copy of the profile set
/// (user bug 39: a member added on this page or with `spb` was listed, yet 开启
/// stayed disabled with "添加 Krill 为成员").
pub fn eligibility(
    view: &RouterView,
    sites: &[Site],
    profiles: &ProfileSet,
) -> Vec<RouterCandidate> {
    let mut set = profiles.clone();
    set.settings.router.members = view.settings.members.clone();
    router_candidates(&set, sites)
}

/// Custom sites without a profile that has a key (they cannot be members yet).
pub fn keyless_sites(sites: &[Site], profiles: &ProfileSet) -> Vec<String> {
    sites
        .iter()
        .filter(|s| s.kind == SiteKind::Compatible)
        .filter(|s| {
            !profiles
                .profiles
                .iter()
                .any(|p| p.site_id == s.id && p.key_ref.is_some())
        })
        .map(|s| s.name.fallback.clone())
        .collect()
}

fn event_key(kind: RouteEventKind) -> (&'static str, bool) {
    match kind {
        RouteEventKind::Failover => ("router.event.failover", true),
        RouteEventKind::LineDown => ("router.event.line_down", true),
        RouteEventKind::LineUp => ("router.event.line_up", false),
        RouteEventKind::Unavailable => ("router.event.unavailable", true),
        RouteEventKind::StreamBroken => ("router.event.stream_broken", true),
        RouteEventKind::MemberChanged => ("router.event.member_changed", false),
        RouteEventKind::Reloaded => ("router.event.reloaded", false),
        RouteEventKind::LocalLimit => ("router.event.local_limit", true),
        RouteEventKind::Compaction => ("router.event.compaction", false),
    }
}

/// Live part of the snapshot from the admin API's status answer; member ids are
/// named after `members`.
pub fn live(status: &RouterStatus, members: &[MemberView]) -> Live {
    let name_of_member = |id: &str| {
        status
            .members
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.name.clone())
            .or_else(|| members.iter().find(|m| m.id == id).map(|m| m.name.clone()))
            .unwrap_or_else(|| id.to_owned())
    };
    Live {
        version: status.version.clone(),
        listen: status.listen.clone(),
        uptime_ms: status.uptime_ms,
        totals: Totals {
            requests: status.totals.requests,
            active_requests: status.totals.active_requests,
            failovers: status.totals.failovers,
            unavailable: status.totals.unavailable,
            active_websockets: status.totals.active_websockets,
            sticky_sessions: status.sticky_sessions,
        },
        events: status
            .recent
            .iter()
            .map(|e| {
                let (kind_key, severe) = event_key(e.kind);
                EventView {
                    at_ms: e.at_ms,
                    kind_key,
                    severe,
                    member: e.member.as_deref().map(name_of_member),
                    line: e.line.clone(),
                    detail: e.detail.clone(),
                }
            })
            .collect(),
        local: status.local.view(),
    }
}

/// The members after adding `id` at the end / removing it.
pub fn with_member(current: &[ProfileId], id: &ProfileId, add: bool) -> Vec<ProfileId> {
    let mut v: Vec<ProfileId> = current.iter().filter(|m| *m != id).cloned().collect();
    if add {
        v.push(id.clone());
    }
    v
}

impl SystemRouter {
    /// A backend on `service` alone, without the service manager and admin API
    /// reads (tests; the live part of its snapshots is then unavailable).
    pub fn for_service(service: Arc<dyn SwitchService>) -> Self {
        Self {
            service,
            manager: None,
            network_exe: None,
        }
    }

    fn clone_parts(&self) -> SystemRouter {
        SystemRouter {
            service: self.service.clone(),
            manager: self.manager.clone(),
            network_exe: self.network_exe.clone(),
        }
    }

    async fn install_members(&self, members: Vec<ProfileId>) -> Result<()> {
        if members.is_empty() {
            return Err(Error::invalid("add a member first"));
        }
        self.service
            .router_install(RouterInstallRequest { members })
            .await
            .map(|_| ())
    }

    /// Change the member list. An installed router gets the new members (and
    /// reloads); a router that was never installed only has the list saved, so
    /// editing members never installs a login service as a side effect (that is
    /// "Install and start" or turning router mode on).
    async fn change_members(&self, members: Vec<ProfileId>) -> Result<()> {
        let view = self.service.router_status().await?;
        if view.installed {
            return self.install_members(members).await;
        }
        self.save_members(members).await
    }

    async fn save_members(&self, members: Vec<ProfileId>) -> Result<()> {
        let mut settings = self.service.profile_set().await?.settings;
        settings.router.members = members;
        self.service.save_settings(settings).await
    }
}

impl RouterBackend for SystemRouter {
    fn snapshot(&self) -> BoxFut<Snapshot> {
        let this = self.clone_parts();
        Box::pin(async move {
            let (view, sites, profiles) = tokio::try_join!(
                this.service.router_status(),
                this.service.sites(),
                this.service.profile_set()
            )?;
            let members = members(&view, &sites, &profiles);
            let overview = match &this.manager {
                Some(m) => Some(m.overview().await),
                None => None,
            };
            let service = overview
                .as_ref()
                .and_then(|o| o.service.as_ref().ok())
                .map(|s| ServiceView {
                    backend: match s.kind {
                        superba_router::service::ServiceKind::SystemdUser => "systemd --user",
                        superba_router::service::ServiceKind::XdgAutostart => "XDG autostart",
                        superba_router::service::ServiceKind::LaunchAgent => "LaunchAgent",
                        superba_router::service::ServiceKind::WindowsRunKey => "Run key",
                    }
                    .to_owned(),
                    installed: view.installed,
                    running: Some(view.running),
                    pid: s.pid,
                    detail: s.detail.clone(),
                });
            let live = match overview.as_ref().map(|o| &o.liveness) {
                Some(Liveness::Up(status)) => Ok(live(status, &members)),
                Some(Liveness::Down { detail }) => Err(detail.clone()),
                None => Err(view.detail.clone().unwrap_or_default()),
            };
            Ok(Snapshot {
                keyless: keyless_sites(&sites, &profiles),
                candidates: candidates(&sites, &profiles, &view.settings.members),
                eligibility: eligibility(&view, &sites, &profiles),
                members,
                service,
                live,
                port: view.port,
                recording: view.stats.clone(),
                network_exe: this.network_exe.clone(),
            })
        })
    }

    fn set_order(&self, ids: Vec<String>) -> BoxFut<()> {
        let this = self.clone_parts();
        Box::pin(async move {
            let members = ids.iter().map(ProfileId::new).collect();
            this.change_members(members).await
        })
    }

    fn add_member(&self, candidate: Candidate) -> BoxFut<()> {
        let this = self.clone_parts();
        Box::pin(async move {
            let view = this.service.router_status().await?;
            let members = with_member(
                &view.settings.members,
                &ProfileId::new(&candidate.profile_id),
                true,
            );
            this.change_members(members).await
        })
    }

    fn remove_member(&self, id: String) -> BoxFut<()> {
        let this = self.clone_parts();
        Box::pin(async move {
            let view = this.service.router_status().await?;
            let members = with_member(&view.settings.members, &ProfileId::new(&id), false);
            if members.is_empty() {
                // The last member: an installed router goes away (refused while
                // Codex goes through it); the empty list is saved either way.
                if view.installed {
                    this.service.router_uninstall().await?;
                }
                this.save_members(members).await
            } else {
                this.change_members(members).await
            }
        })
    }

    fn install(&self) -> BoxFut<()> {
        let this = self.clone_parts();
        Box::pin(async move {
            let view = this.service.router_status().await?;
            this.install_members(view.settings.members).await
        })
    }

    fn restart(&self) -> BoxFut<()> {
        self.install()
    }

    fn uninstall(&self) -> BoxFut<()> {
        let this = self.clone_parts();
        Box::pin(async move { this.service.router_uninstall().await.map(|_| ()) })
    }
}
