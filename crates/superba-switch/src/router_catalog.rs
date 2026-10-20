//! The model page and the router use the same catalog and override choices.

use superba_core::{
    CatalogLayer, ModelList, ModelListKind, ModelListSource, ProfileSet, Result,
    RouterCatalogSettings, RouterCatalogView,
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

    /// The router's merged list as the Models page shows it: one row per
    /// canonical model (never one per provider alias). An empty list is a
    /// valid answer (Codex mode with only converted providers), not an error:
    /// the page still offers the mode that would show their models.
    pub(crate) async fn routed_models(&self) -> Result<(ModelList, RouterCatalogView)> {
        let report = self.deps.router.catalog().await?;
        let mut members = Vec::new();
        for source in &report.sources {
            if let Some(name) = report
                .models
                .iter()
                .flat_map(|m| &m.sources)
                .find(|p| p.provider == source.provider)
                .map(|p| p.provider_name.clone())
                .filter(|n| !members.contains(n))
            {
                members.push(name);
            }
        }
        let models = report
            .models
            .iter()
            .filter_map(|row| {
                let mut spec = crate::models::spec_from_entry(&row.entry)?;
                // A family estimate must not cap what the user may set.
                spec.estimated_context_window = row.estimated();
                for proof in &row.sources {
                    if !members.contains(&proof.provider_name) {
                        members.push(proof.provider_name.clone());
                    }
                    let kind = match proof.layer {
                        CatalogLayer::Codex => Some(ModelListKind::Codex),
                        CatalogLayer::Standard
                        | CatalogLayer::StandardKnown
                        | CatalogLayer::Synthesized => Some(ModelListKind::Standard),
                        CatalogLayer::Builtin => None,
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
