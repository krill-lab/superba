//! Router endpoints for the provider a tool explicitly applied. Provider-bound
//! tokens keep that choice stable when Codex switches to a different provider.
use crate::{
    Switcher,
    planner::all_sites,
    router::{member_lines, member_path},
};
use std::collections::{BTreeMap, BTreeSet};
use superba_core::harness::HarnessProtocol;
use superba_core::{ProfileSet, Result, Secret, SiteKind};
use superba_router::claude::{AnthropicAuth, ClaudeMember};
use superba_router::config::RouterConfig;
use superba_router::members::{member_from_site, router_id};
use superba_router::openai::{OpenAiEndpoint, OpenAiMember};

impl Switcher {
    /// Include key-based tool providers without adding them to Codex's failover.
    pub(crate) async fn add_harness_router_members(
        &self,
        set: &ProfileSet,
        config: &mut RouterConfig,
        keys: &mut BTreeMap<String, Secret>,
    ) -> Result<()> {
        let sites = all_sites(set, self.deps.ctx.env.as_ref());
        let saved = superba_config::harness::HarnessConfigStore::new(&self.deps.ctx).load()?;
        let mut needed = BTreeSet::new();
        let active = self
            .deps
            .config
            .read_state(&self.deps.ctx.codex_home())
            .await
            .ok()
            .and_then(|s| s.active)
            .and_then(|a| a.profile_id);
        for harness in superba_core::harness::HarnessId::ALL {
            let wanted = saved.settings(harness);
            if wanted.route == superba_core::harness::HarnessRoute::Routed
                && let Some(id) = wanted.provider.or_else(|| active.clone())
            {
                needed.insert(id);
            }
            let writer =
                superba_config::harness::HarnessWriter::new(self.deps.ctx.clone(), harness);
            if let Some(applied) = writer.state()?
                && applied.route == superba_core::harness::HarnessRoute::Routed
                && let Some(id) = applied.provider
            {
                needed.insert(id);
            }
        }
        for profile in set
            .profiles
            .iter()
            .filter(|p| needed.contains(&p.id) && !p.is_builtin() && p.key_ref.is_some())
        {
            let Some(site) = sites
                .iter()
                .find(|s| s.id == profile.site_id && s.kind != SiteKind::Official)
            else {
                continue;
            };
            let Ok((endpoints, key)) = self.harness_endpoints(set, profile, &saved).await else {
                continue;
            };
            let id = router_id(profile.id.as_str());
            if config.member(&id).is_none() {
                let Ok(path) = member_path(site, profile) else {
                    continue;
                };
                let Ok(mut member) = member_from_site(site, &member_lines(site, profile), &path)
                else {
                    continue;
                };
                member.id = id.clone();
                member.name = profile.name.clone();
                member.model_map = site.model_map.clone();
                config.tool_only_members.push(id.clone());
                config.members.push(member);
            }
            keys.insert(id.clone(), key);
            let member = config.member(&id).expect("added provider");
            let lines = member.lines.clone();
            let mut openai = OpenAiMember::default();
            for endpoint in endpoints {
                let e = crate::claude_router::endpoint_for(&endpoint.base_url, &lines);
                match endpoint.protocol {
                    HarnessProtocol::Responses => {
                        openai.responses = Some(OpenAiEndpoint {
                            path: Some(e.path),
                            lines: e.lines,
                        })
                    }
                    HarnessProtocol::Chat => {
                        openai.chat = Some(OpenAiEndpoint {
                            path: Some(e.path),
                            lines: e.lines,
                        })
                    }
                    HarnessProtocol::Anthropic => {
                        let anth = superba_core::claude::effective_endpoint(site, profile);
                        config
                            .claude
                            .get_or_insert_with(Default::default)
                            .members
                            .entry(id.clone())
                            .or_insert_with(|| ClaudeMember {
                                anthropic: Some(e),
                                auth: if anth
                                    .is_some_and(|e| e.auth == superba_core::AnthropicAuth::Bearer)
                                {
                                    AnthropicAuth::Bearer
                                } else {
                                    AnthropicAuth::XApiKey
                                },
                                ..Default::default()
                            });
                    }
                }
            }
            config
                .openai
                .get_or_insert_with(Default::default)
                .members
                .insert(id, openai);
        }
        Ok(())
    }

    /// Ensure the local router serves the reviewed target before files point at it.
    pub(crate) async fn ready_harness_router(
        &self,
        plan: &superba_core::harness::HarnessPlan,
    ) -> Result<()> {
        let Some(target) = plan
            .desired
            .as_ref()
            .filter(|t| t.route == superba_core::harness::HarnessRoute::Routed)
        else {
            return Ok(());
        };
        // Explicit external routes remain a CLI option. Only our own endpoint is managed here.
        let Ok(endpoint) = self.deps.router.endpoint() else {
            return Ok(());
        };
        if endpoint.base_url.host() != target.endpoint.base_url.host()
            || endpoint.port
                != target
                    .endpoint
                    .base_url
                    .port_or_known_default()
                    .unwrap_or(0)
        {
            return Ok(());
        }
        let set = self.deps.profiles.load().await?;
        let built = self
            .build_router_members(&set, &set.settings.router.members)
            .await?;
        let provider = target
            .provider
            .as_ref()
            .ok_or_else(|| superba_core::Error::invalid("The tool has no provider"))?;
        if built.config.member(&router_id(provider.as_str())).is_none() {
            return Err(superba_core::Error::invalid(
                "The applied tool provider is unavailable in the router",
            ));
        }
        if self.deps.router.liveness().await.is_up() {
            self.deps.router.rewrite(built.config, built.keys).await?;
        } else {
            self.deps.router.install(built.config, built.keys).await?;
        }
        // A restart can move a taken port. A fresh review must show that new address.
        if self.deps.router.endpoint()?.port != endpoint.port {
            return Err(superba_core::Error::Conflict {
                message: "The router moved to another port; review the tool configuration again."
                    .into(),
            });
        }
        Ok(())
    }
}
