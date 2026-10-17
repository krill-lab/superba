//! Site probing ([`superba_core::SwitchService::probe_site`]): latency and
//! reachability, key access, the model catalog with the site's model map checked
//! against it, and an optional minimal Responses request. Read-only.

use superba_core::{
    AuthMode, CodexPath, Error, Line, MappedModel, ModelInfo, ResolvedEndpoints, Result, Secret,
    Site, SiteKind, SiteProbeReport, SiteProbeRequest,
};
use url::Url;

use crate::Switcher;
use crate::planner::compatible_base_url;

/// The model map checked against the catalog (`None` = no catalog).
pub(crate) fn mapped_models(site: &Site, catalog: Option<&[ModelInfo]>) -> Vec<MappedModel> {
    site.model_map
        .iter()
        .map(|(codex, mapped)| MappedModel {
            codex: codex.clone(),
            site: mapped.clone(),
            available: catalog.map(|ms| ms.iter().any(|m| &m.id == mapped)),
        })
        .collect()
}

/// API-key endpoints of `line` (compatible sites: their Responses base URL; relay
/// sites: the probed Codex path, else the first one the site offers).
fn apikey_endpoints(
    site: &Site,
    line: &Line,
    path: Option<CodexPath>,
) -> Result<ResolvedEndpoints> {
    if site.kind == SiteKind::Compatible {
        let base: Url = compatible_base_url(site, Some(&line.id))
            .ok_or_else(|| Error::invalid(format!("site {} has no Responses base URL", site.id)))?;
        return ResolvedEndpoints::for_base_url(&base);
    }
    let path = path
        .or_else(|| site.capabilities.codex_paths.first().copied())
        .unwrap_or(CodexPath::BackendApiCodex);
    ResolvedEndpoints::for_line(&line.origin, path, AuthMode::ApiKey)
}

impl Switcher {
    pub(crate) async fn probe_site_impl(
        &self,
        request: SiteProbeRequest,
    ) -> Result<SiteProbeReport> {
        let site = match request.site {
            // A site being added: checked like `save_site`, never stored.
            Some(site) => crate::service::validated_custom_site(site)?,
            None => self.site(&request.site_id).await?,
        };
        if site.kind == SiteKind::Official {
            return Err(Error::invalid(
                "the official ChatGPT login has no lines to probe",
            ));
        }
        let line = match &request.line_id {
            Some(id) => site.line(id),
            None => site.default_line(),
        }
        .cloned()
        .ok_or_else(|| Error::NotFound {
            what: match &request.line_id {
                Some(id) => format!("line {id} of site {}", site.id),
                None => format!("a line of site {}", site.id),
            },
        })?;
        let key: Option<Secret> = match request.key {
            Some(k) => Some(self.resolve_key(k).await?),
            None => None,
        };
        let prober = &self.deps.prober;
        let caps = prober.probe(&site, &line).await?;
        let mut report = SiteProbeReport {
            site_id: site.id.clone(),
            line_id: line.id.clone(),
            caps,
            key_check: None,
            models: None,
            models_error: None,
            mapped: mapped_models(&site, None),
            model: None,
            responses: None,
            responses_error: None,
            account: None,
            account_error: None,
        };
        let Some(key) = key else {
            return Ok(report);
        };
        let mode = if site.supports(AuthMode::ApiKey) {
            AuthMode::ApiKey
        } else {
            AuthMode::Pat
        };
        let (check, account) = tokio::join!(
            prober.check_key(&site, &line, mode, &key),
            prober.account_summary(&site, &line, &key),
        );
        let check = check?;
        match account {
            Ok(a) => report.account = a,
            // A site without the endpoints is not an error here.
            Err(e) if e.is_not_implemented() || matches!(e, Error::Unsupported { .. }) => {}
            Err(e) => report.account_error = Some(e),
        }
        let path = check.codex_path.or(report.caps.codex_path);
        report.key_check = Some(check);
        let endpoints = match apikey_endpoints(&site, &line, path) {
            Ok(e) => e,
            Err(e) => {
                report.models_error = Some(e.clone());
                report.responses_error = request.responses.then_some(e);
                return Ok(report);
            }
        };
        if site.capabilities.model_catalog {
            match prober.list_models(&endpoints.openai_base_url, &key).await {
                Ok(models) => {
                    report.mapped = mapped_models(&site, Some(&models));
                    report.models = Some(models);
                }
                Err(e) => report.models_error = Some(e),
            }
        }
        if !request.responses {
            return Ok(report);
        }
        let model = request
            .model
            .as_deref()
            .map(|m| site.site_model(m).to_owned())
            .or_else(|| {
                report
                    .models
                    .as_ref()
                    .and_then(|ms| ms.first())
                    .map(|m| m.id.clone())
            });
        let Some(model) = model else {
            report.responses_error = Some(Error::invalid(
                "no model to test with: the site lists no models; pass a model",
            ));
            return Ok(report);
        };
        match prober.probe_responses(&endpoints, &key, &model).await {
            Ok(r) => report.responses = Some(r),
            Err(e) => report.responses_error = Some(e),
        }
        report.model = Some(model);
        Ok(report)
    }
}
