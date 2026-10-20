//! Claude Code through the local router (user request 79, design §6): the
//! router's `claude` section, its state for the Router page, and the fallback
//! to the provider when the router goes away.
//!
//! - **Section** ([`Switcher::claude_router_section`], part of every router
//!   configuration Superba writes): while Claude Code's route is the router,
//!   its members are Claude Code's own provider first, then the router's member
//!   profiles; native endpoints or enabled IR conversions determine eligibility. Each one's endpoint becomes a path below its lines' origin
//!   (every line keeps failing over) or, on another host, that URL for every
//!   line; Bearer / `x-api-key` as the provider sends it. A provider that is
//!   not a router member joins as a Claude-only member (Codex never uses it).
//!   Every model the provider's configuration names (slots, default,
//!   subagent, background, `/model` rows, models added by hand) gets a route:
//!   the provider first, the others as failover. Claude Code's own background
//!   ids (`claude-*haiku*`, …) map to the slot models. Other models go to the
//!   members whose model list names them (the router reads the lists).
//! - **Routed**: Claude Code's `settings.json` holds the router's base URL
//!   that Superba wrote.
//! - **Fallback** ([`Switcher::claude_fall_back`]): the route goes back to
//!   direct and `settings.json` is written with the provider's endpoint again
//!   (or Superba's keys are taken out when the provider has none); the notice
//!   says so.

use std::collections::BTreeMap;

use superba_core::claude::{
    ClaudeRoute, ClaudeSlot, ENV_BASE_URL, effective_endpoint, env_path, get_path,
};
use superba_core::{
    AnthropicAuth, ClaudeIneligible, ClaudePlanKind, Notice, Profile, ProfileId, ProfileSet,
    Result, RouterClaudeMember, RouterClaudeView, Secret, Site, SiteKind,
};
use superba_router::claude::{
    AnthropicAuth as RouterAuth, ClaudeConfig as Section, ClaudeMember as SectionMember,
    ClaudeRoute as SectionRoute, ClaudeTarget, Endpoint, ModelAlias,
};
use superba_router::config::{LineConfig, MemberConfig, MemberKind};
use superba_router::members::{member_from_site, router_id};
use url::Url;

use crate::Switcher;
use crate::messages::text;
use crate::planner::all_sites;

/// The router side of Claude Code for a configuration being written.
#[derive(Debug, Default)]
pub(crate) struct ClaudeSection {
    /// The `claude` section (empty while Claude Code goes direct).
    pub section: Section,
    /// Providers that serve only Claude Code (not router member profiles).
    pub extra: Vec<MemberConfig>,
    /// Their keys.
    pub keys: BTreeMap<String, Secret>,
    /// Native OpenAI endpoints for extra members used through conversion.
    pub openai: BTreeMap<String, superba_router::openai::OpenAiMember>,
}

/// Background id patterns of Claude Code per slot.
const SLOT_PATTERNS: &[(ClaudeSlot, &str)] = &[
    (ClaudeSlot::Haiku, "claude-*haiku*"),
    (ClaudeSlot::Sonnet, "claude-*sonnet*"),
    (ClaudeSlot::Opus, "claude-*opus*"),
    (ClaudeSlot::Fable, "claude-*fable*"),
];

/// `http://127.0.0.1:<port>/claude`: the router's Claude Code ingress.
pub(crate) fn is_router_url(url: &str) -> bool {
    let Ok(u) = Url::parse(url) else {
        return false;
    };
    u.scheme() == "http"
        && matches!(u.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback())
        && u.path().trim_end_matches('/') == superba_router::claude::PREFIX
}

/// The endpoint of `base` for a member with `lines`: a path below their
/// origin when it is on one of them, else that URL for every line.
pub(crate) fn endpoint_for(base: &Url, lines: &[LineConfig]) -> Endpoint {
    let same_origin = lines.iter().any(|l| l.base_url.origin() == base.origin());
    if same_origin {
        Endpoint {
            path: base.path().trim_end_matches('/').to_owned(),
            lines: BTreeMap::new(),
        }
    } else {
        Endpoint {
            path: String::new(),
            lines: lines.iter().map(|l| (l.id.clone(), base.clone())).collect(),
        }
    }
}

/// A Claude-only member for a provider that is not a router member: its site's
/// lines when it has a Codex path, else one line at its Anthropic endpoint.
fn claude_only_member(site: &Site, profile: &Profile, base: Option<&Url>) -> Option<MemberConfig> {
    let path = crate::router::member_path(site, profile).ok();
    let from_site = path.and_then(|p| {
        let lines = crate::router::member_lines(site, profile).ok()?;
        member_from_site(site, &lines, &p).ok()
    });
    let mut member = from_site.or_else(|| {
        base.map(|base| MemberConfig {
            id: String::new(),
            name: String::new(),
            kind: MemberKind::Compatible,
            lines: vec![LineConfig {
                id: "anthropic".into(),
                name: base.host_str().unwrap_or("anthropic").to_owned(),
                base_url: base.clone(),
                probe_url: None,
                zstd: None,
                websocket: None,
            }],
            websocket: false,
            model_map: BTreeMap::new(),
            compaction: Default::default(),
        })
    })?;
    member.id = router_id(profile.id.as_str());
    member.name = profile.name.clone();
    Some(member)
}

impl Switcher {
    /// The provider Claude Code uses (configured, else Codex's) and the
    /// providers for the router in Claude Code's order: that one first, then
    /// the router's member profiles.
    async fn claude_candidates(
        &self,
        set: &ProfileSet,
    ) -> Result<(
        superba_core::ClaudeConfig,
        Option<ProfileId>,
        Vec<ProfileId>,
    )> {
        let config = self.claude_store().load()?;
        let active = self
            .deps
            .config
            .read_state(&self.deps.ctx.codex_home())
            .await
            .ok()
            .and_then(|s| s.active)
            .and_then(|a| a.profile_id);
        let preferred = crate::claude::pick_provider(set, &config, active.as_ref())
            .0
            .map(|p| p.id.clone());
        let mut order: Vec<ProfileId> = preferred.iter().cloned().collect();
        for id in &set.settings.router.members {
            if !order.contains(id) {
                order.push(id.clone());
            }
        }
        Ok((config, preferred, order))
    }

    /// The `claude` section and the Claude-only members for a router
    /// configuration whose (Codex) members are `codex`.
    pub(crate) async fn claude_router_section(
        &self,
        set: &ProfileSet,
        codex: &[MemberConfig],
    ) -> Result<ClaudeSection> {
        let (config, preferred, order) = self.claude_candidates(set).await?;
        let mut out = ClaudeSection::default();
        if config.route != ClaudeRoute::Router {
            return Ok(out);
        }
        let sites = all_sites(set, self.deps.ctx.env.as_ref());
        let mut ids: Vec<String> = Vec::new();
        for id in &order {
            let Some(profile) = set.profile(id).filter(|p| !p.is_builtin()) else {
                continue;
            };
            let Some(site) = sites.iter().find(|s| s.id == profile.site_id) else {
                continue;
            };
            if site.kind == SiteKind::Official {
                continue;
            }
            let endpoint = effective_endpoint(site, profile);
            let rid = router_id(id.as_str());
            let (lines, claude_only) = match codex.iter().find(|m| m.id == rid) {
                Some(m) => (m.lines.clone(), false),
                None => {
                    let Some(key_ref) = &profile.key_ref else {
                        continue;
                    };
                    let key = self.deps.secrets.get(key_ref).await?;
                    if key.is_empty() {
                        continue;
                    }
                    let Some(member) =
                        claude_only_member(site, profile, endpoint.as_ref().map(|e| &e.base_url))
                    else {
                        continue;
                    };
                    if endpoint.is_none() {
                        let native = self
                            .deps
                            .router
                            .configuration()
                            .and_then(|c| c.openai_member(&member.id))
                            .unwrap_or_else(|| {
                                superba_router::openai::OpenAiMember::default_for(&member, true)
                            });
                        out.openai.insert(member.id.clone(), native);
                    }
                    let lines = member.lines.clone();
                    out.keys.insert(member.id.clone(), key);
                    out.extra.push(member);
                    (lines, true)
                }
            };
            let models = config.models_of(id);
            out.section.members.insert(
                rid.clone(),
                SectionMember {
                    anthropic: endpoint.as_ref().map(|e| endpoint_for(&e.base_url, &lines)),
                    auth: match endpoint.as_ref().map(|e| e.auth) {
                        Some(AnthropicAuth::Bearer) => RouterAuth::Bearer,
                        _ => RouterAuth::XApiKey,
                    },
                    models: models
                        .extra_models
                        .iter()
                        .map(|m| superba_router::claude::ClaudeModel {
                            id: m.clone(),
                            display_name: None,
                        })
                        .collect(),
                    claude_only,
                    ..SectionMember::default()
                },
            );
            ids.push(rid);
        }
        out.section.order = ids.clone();
        // The provider's own models: routes with it first.
        if let Some(pid) = &preferred
            && let p = router_id(pid.as_str())
            && ids.contains(&p)
        {
            let models = config.models_of(pid);
            let targets: Vec<ClaudeTarget> = std::iter::once(p.clone())
                .chain(ids.iter().filter(|i| **i != p).cloned())
                .map(|member| ClaudeTarget {
                    member,
                    model: None,
                })
                .collect();
            for model in models.named_models() {
                out.section.routes.push(SectionRoute {
                    model,
                    targets: targets.clone(),
                });
            }
            for (slot, pattern) in SLOT_PATTERNS {
                let to = match slot {
                    ClaudeSlot::Haiku => models
                        .small_fast
                        .clone()
                        .or_else(|| models.slots.get(slot).map(|s| s.model.clone())),
                    _ => models.slots.get(slot).map(|s| s.model.clone()),
                };
                if let Some(to) = to.filter(|t| !t.trim().is_empty()) {
                    out.section.aliases.push(ModelAlias {
                        from: (*pattern).to_owned(),
                        to,
                    });
                }
            }
        }
        Ok(out)
    }

    /// Claude Code's `settings.json` points at the router: Superba wrote the
    /// router's base URL there and it is still there.
    pub(crate) fn claude_routed(&self) -> bool {
        let writer = self.claude_writer();
        let Ok(Some(state)) = writer.state() else {
            return false;
        };
        let path = env_path(ENV_BASE_URL);
        let Some(serde_json::Value::String(written)) = state
            .files
            .get("settings.json")
            .and_then(|f| f.slots.get(&path))
            .and_then(|s| s.written.as_ref())
        else {
            return false;
        };
        if !is_router_url(written) {
            return false;
        }
        writer
            .read()
            .ok()
            .and_then(|f| f.value)
            .and_then(|v| get_path(&v, &path).cloned())
            .is_some_and(|v| v.as_str() == Some(written.as_str()))
    }

    /// The port of the router base URL in Claude Code's `settings.json`, when
    /// it differs from the router's.
    pub(crate) fn claude_stale_port(&self) -> Option<(u16, u16)> {
        if !self.claude_routed() {
            return None;
        }
        let state = self.claude_writer().state().ok()??;
        let written = state
            .files
            .get("settings.json")?
            .slots
            .get(&env_path(ENV_BASE_URL))?
            .written
            .as_ref()?;
        let from = Url::parse(written.as_str()?)
            .ok()?
            .port_or_known_default()?;
        let to = self.deps.router.endpoint().ok()?.port;
        (from != to).then_some((from, to))
    }

    /// Write the router's `claude` section (and the Claude-only members) for
    /// the stored members. Nothing when the router was never configured;
    /// `start` starts a stopped router (with that configuration).
    pub(crate) async fn sync_claude_router(&self, start: bool) -> Result<()> {
        let d = &self.deps;
        if d.router.endpoint().is_err() {
            return Ok(());
        }
        let set = d.profiles.load().await?;
        let built = self
            .build_router_members(&set, &set.settings.router.members)
            .await?;
        if start && !d.router.liveness().await.is_up() {
            d.router.install(built.config, built.keys).await?;
        } else {
            d.router.rewrite(built.config, built.keys).await?;
        }
        Ok(())
    }

    /// Claude Code back to its provider: the route goes direct and
    /// `settings.json` is written with the provider's endpoint (or Superba's
    /// keys are taken out when it has none). The notice says what happened.
    pub(crate) async fn claude_fall_back(&self, reason: &str) -> Result<Notice> {
        tracing::info!(%reason, "Claude Code falls back from the local router to its provider");
        let mut config = self.claude_store().load()?;
        config.route = ClaudeRoute::Direct;
        self.claude_store().save(&config)?;
        let (_, _, pair, _) = self.claude_context(None).await?;
        let name = pair
            .as_ref()
            .map(|(p, _)| p.name.clone())
            .unwrap_or_default();
        match self.claude_plan_impl(ClaudePlanKind::Apply).await {
            Ok(plan) => {
                self.claude_apply_impl(plan, false).await?;
                Ok(Notice::warning(
                    text("switch.claude.router.fell_back")
                        .arg("provider", name)
                        .arg("detail", reason),
                ))
            }
            Err(_) => {
                let plan = self.claude_plan_impl(ClaudePlanKind::Restore).await?;
                self.claude_apply_impl(plan, false).await?;
                Ok(Notice::warning(
                    text("switch.claude.router.fell_back_restored").arg("detail", reason),
                ))
            }
        }
    }

    /// Point Claude Code at the router again (its port moved).
    pub(crate) async fn claude_repoint(&self) -> Result<()> {
        let plan = self.claude_plan_impl(ClaudePlanKind::Apply).await?;
        self.claude_apply_impl(plan, false).await.map(|_| ())
    }

    /// Claude Code on the router for the Router page and `spb`.
    pub(crate) async fn claude_router_view(&self, set: &ProfileSet) -> Option<RouterClaudeView> {
        let (config, preferred, order) = self.claude_candidates(set).await.ok()?;
        let routed = self.claude_routed();
        let route_router = config.route == ClaudeRoute::Router;
        if !route_router && !routed {
            return None;
        }
        let sites = all_sites(set, self.deps.ctx.env.as_ref());
        let base_url = crate::claude::claude_router_route(self.deps.router.as_ref())
            .await
            .map(|r| r.base_url.to_string());
        let built = self
            .build_router_members(set, &set.settings.router.members)
            .await
            .ok();
        let members = order
            .iter()
            .filter_map(|id| {
                let profile = set.profile(id)?;
                let endpoint = sites
                    .iter()
                    .find(|s| s.id == profile.site_id)
                    .and_then(|s| effective_endpoint(s, profile));
                let protocol = built.as_ref().and_then(|b| {
                    b.config.upstream_protocol(
                        &router_id(id.as_str()),
                        superba_router::translate::ir::Protocol::Messages,
                    )
                });
                Some(RouterClaudeMember {
                    profile_id: id.clone(),
                    name: profile.name.clone(),
                    ineligible: protocol
                        .is_none()
                        .then_some(ClaudeIneligible::NoProtocolRoute),
                    translated: protocol
                        .is_some_and(|p| p != superba_router::translate::ir::Protocol::Messages),
                    endpoint: endpoint.map(|e| e.base_text()),
                    preferred: preferred.as_ref() == Some(id),
                    claude_only: !set.settings.router.members.contains(id),
                })
            })
            .collect();
        Some(RouterClaudeView {
            route_router,
            routed,
            base_url,
            members,
        })
    }
}
