//! Convert known Magpie Krill IDs only when the destination lists their bare ID.

use std::collections::{BTreeMap, BTreeSet};

use superba_core::{
    AuthMode, CodexHome, CodexPath, ModelInfo, ProfileSet, ResolvedEndpoints, SiteKind,
};

use crate::Switcher;
use crate::messages::text;

/// Explicit legacy Krill provider namespaces; never strip arbitrary prefixes.
pub fn krill_replacements(models: &[String], available: &[ModelInfo]) -> BTreeMap<String, String> {
    let known: BTreeSet<&str> = available.iter().map(|m| m.id.as_str()).collect();
    models
        .iter()
        .filter_map(|model| {
            if known.contains(model.as_str()) {
                return None;
            }
            let bare = ["krill/", "krill-usage/", "krill-balance/"]
                .iter()
                .find_map(|prefix| model.strip_prefix(prefix))?;
            (!bare.contains('/') && known.contains(bare)).then(|| (model.clone(), bare.to_owned()))
        })
        .collect()
}

impl Switcher {
    /// Resolve recent IDs against the real Krill profile, also in router mode.
    /// A failed discovery never authorizes a guessed replacement.
    pub(crate) async fn recent_model_replacements(
        &self,
        home: &CodexHome,
        profile: Option<&superba_core::ProfileId>,
        set: &ProfileSet,
    ) -> superba_core::Result<BTreeMap<String, String>> {
        if !profile
            .and_then(|id| set.profile(id))
            .is_some_and(|p| p.site_id.as_str() == superba_core::catalog::KRILL_SITE_ID)
        {
            return Ok(BTreeMap::new());
        }
        let models = self.deps.config.recent_models(home).await?;
        self.verified_krill_replacements(profile, set, &models)
            .await
    }

    pub(crate) async fn verified_krill_replacements(
        &self,
        profile: Option<&superba_core::ProfileId>,
        set: &ProfileSet,
        models: &[String],
    ) -> superba_core::Result<BTreeMap<String, String>> {
        let Some(profile) = profile.and_then(|id| set.profile(id)) else {
            return Ok(BTreeMap::new());
        };
        let Some(site) = crate::planner::all_sites(set, self.deps.ctx.env.as_ref())
            .into_iter()
            .find(|s| s.id == profile.site_id && s.kind == SiteKind::Krill)
        else {
            return Ok(BTreeMap::new());
        };
        if !models.iter().any(|m| {
            ["krill/", "krill-usage/", "krill-balance/"]
                .iter()
                .any(|p| m.starts_with(p))
        }) {
            return Ok(BTreeMap::new());
        }
        let Some(reference) = profile.key_ref.as_ref() else {
            return Ok(BTreeMap::new());
        };
        let key = self.deps.secrets.get(reference).await?;
        let base = if let Some(base) = profile.base_url_override.as_ref() {
            base.clone()
        } else {
            let line = profile
                .line_id
                .as_ref()
                .and_then(|id| site.line(id))
                .or_else(|| site.default_line())
                .ok_or_else(|| superba_core::Error::invalid("Krill profile has no line"))?;
            ResolvedEndpoints::for_line(&line.origin, CodexPath::BackendApiCodex, AuthMode::ApiKey)?
                .openai_base_url
        };
        let available = self.deps.prober.list_models(&base, &key).await?;
        Ok(krill_replacements(models, &available))
    }

    pub(crate) fn recent_models_note(count: usize) -> superba_core::LocalizedText {
        text("switch.models.recent_repair").arg("count", count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_verified_legacy_krill_names_are_mapped() {
        let available = vec![
            ModelInfo {
                id: "gpt-6.1-sol".into(),
                ..ModelInfo::default()
            },
            ModelInfo {
                id: "krill/native".into(),
                ..ModelInfo::default()
            },
        ];
        let recent = [
            "krill/gpt-6.1-sol",
            "krill-usage/gpt-6.1-sol",
            "other/gpt-6.1-sol",
            "krill/unknown",
            "krill/other/gpt-6.1-sol",
            "krill/native",
            "gpt-6.1-sol",
        ]
        .map(str::to_owned);
        assert_eq!(
            krill_replacements(&recent, &available),
            BTreeMap::from([
                ("krill/gpt-6.1-sol".into(), "gpt-6.1-sol".into()),
                ("krill-usage/gpt-6.1-sol".into(), "gpt-6.1-sol".into()),
            ])
        );
        assert!(krill_replacements(&recent, &[]).is_empty());
    }
}
