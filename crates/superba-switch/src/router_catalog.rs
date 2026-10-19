//! The model page and the router use the same catalog and override choices.

use superba_core::{
    Error, ModelList, ModelListKind, ModelListSource, ProfileSet, Result, RouterCatalogSettings,
    RouterCatalogView,
};
use superba_router::merged_catalog::{CatalogConfig, MemberCatalog};

use crate::Switcher;

impl Switcher {
    pub(crate) async fn router_catalog_config(
        &self,
        set: &ProfileSet,
        router: &superba_router::RouterConfig,
    ) -> CatalogConfig {
        let facts = self.monitor.facts().await;
        let (app, cli) = crate::models::local_codexes(
            set.settings.preferred_app_id.as_deref(),
            &facts.installs,
            &facts.binaries,
        );
        let binary = app
            .as_ref()
            .and_then(|c| c.binary.as_deref())
            .or_else(|| cli.as_ref().and_then(|c| c.binary.as_deref()));
        let bundled = match binary {
            Some(binary) => self
                .deps
                .app
                .codex_debug_models(binary, None)
                .await
                .ok()
                .and_then(|output| self.deps.models.parse_bundled(output.as_bytes()).ok())
                .map(|b| b.entries)
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let members = router
            .members
            .iter()
            .filter_map(|member| {
                let profile = set
                    .profiles
                    .iter()
                    .find(|p| superba_router::members::router_id(p.id.as_str()) == member.id)?;
                Some((
                    member.id.clone(),
                    MemberCatalog {
                        name: profile.name.clone(),
                        overrides: profile.model_overrides.clone(),
                        defaults: profile.model.clone(),
                    },
                ))
            })
            .collect();
        CatalogConfig {
            settings: set.settings.router.catalog.clone(),
            bundled,
            members,
        }
    }

    pub(crate) async fn save_router_catalog_impl(
        &self,
        settings: RouterCatalogSettings,
    ) -> Result<()> {
        let mut set = self.deps.profiles.load().await?;
        set.settings.router.catalog = settings;
        // Validate against concrete members before persisting the user's names.
        self.build_router_members(&set, &set.settings.router.members)
            .await?;
        self.deps.profiles.save(&set).await?;
        self.resync_router_members(&set).await
    }

    pub(crate) async fn routed_models(&self) -> Result<(ModelList, RouterCatalogView)> {
        let report = self.deps.router.catalog().await?;
        if report.models.is_empty() {
            return Err(Error::other("the router has no merged model catalog"));
        }
        let mut members = Vec::new();
        let models = report
            .models
            .iter()
            .filter_map(|row| {
                let mut spec = crate::models::spec_from_entry(&row.entry)?;
                for proof in &row.sources {
                    if proof.provider != "builtin" && !members.contains(&proof.provider_name) {
                        members.push(proof.provider_name.clone());
                    }
                    let kind = match proof.layer {
                        superba_core::CatalogLayer::Codex => Some(ModelListKind::Codex),
                        superba_core::CatalogLayer::Standard => Some(ModelListKind::Standard),
                        _ => None,
                    };
                    if let Some(kind) = kind
                        && !spec.lists.contains(&kind)
                    {
                        spec.lists.push(kind);
                    }
                }
                Some(spec)
            })
            .collect();
        Ok((
            ModelList {
                source: ModelListSource::Union { members },
                models,
                fallback_reason: None,
                sources: Vec::new(),
            },
            report,
        ))
    }
}
