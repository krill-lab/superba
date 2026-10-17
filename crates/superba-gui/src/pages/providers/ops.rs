//! Background operations of the Providers page: testing a site (probe, key
//! check, models, balance, optionally one Responses call), saving or removing a
//! custom site, adding a preset provider, checking a key on every line, and the
//! account report of one provider. They run on page-owned tasks
//! ([`crate::runtime::task`]) against the switching service; nothing here plans
//! or executes a switch.

use std::sync::Arc;

use superba_core::{
    AccountRequest, AccountSummary, AuthMode, Error, KeyCheck, KeyInput, LineCaps, ModelInfo,
    Profile, ProfileId, Result, Secret, Site, SiteId, SiteKind, SwitchService, Timestamp,
};

use super::form::Prepared;

/// Everything a site test found. Each part fails on its own.
#[derive(Clone, Debug, Default)]
pub struct SiteReport {
    /// When the test finished.
    pub at: Option<Timestamp>,
    /// Reachability and the Responses path (`/models` probe).
    pub caps: Option<Result<LineCaps>>,
    /// The key on the site's models endpoint.
    pub key: Option<Result<KeyCheck>>,
    /// Models the key can see (suggestions for the mapping).
    pub models: Option<Result<Vec<ModelInfo>>>,
    /// Balance from the standard billing endpoints (`Ok(None)` = not offered).
    pub account: Option<Result<Option<AccountSummary>>>,
    /// The site's model map checked against its model list.
    pub mapped: Vec<superba_core::MappedModel>,
    /// The optional minimal Responses request.
    pub responses: Option<Result<superba_core::ResponsesProbe>>,
}

impl SiteReport {
    /// The key works on the site.
    pub fn key_ok(&self) -> bool {
        matches!(&self.key, Some(Ok(k)) if k.access.is_ok())
    }

    /// The site answered (reachable).
    pub fn reachable(&self) -> bool {
        matches!(&self.caps, Some(Ok(c)) if c.reachable)
    }

    /// Model ids, sorted and deduplicated.
    pub fn model_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = match &self.models {
            Some(Ok(models)) => models.iter().map(|m| m.id.clone()).collect(),
            _ => Vec::new(),
        };
        ids.sort();
        ids.dedup();
        ids
    }
}

/// Test a site that is not saved yet with a typed key through the switching
/// service (`SiteProbeRequest::site`: checked like a save, never stored; keys go
/// only to the site's own origin): probe, key check, model list with the model
/// map checked against it, balance, optionally one Responses call.
pub async fn test_unsaved(
    service: Arc<dyn SwitchService>,
    site: Site,
    key: Secret,
    responses: bool,
) -> SiteReport {
    let mut request = superba_core::SiteProbeRequest::unsaved(site);
    request.key = Some(KeyInput::Secret(key));
    request.responses = responses;
    let probe = service.probe_site(request).await;
    let account = probe
        .as_ref()
        .ok()
        .map(|p| match (&p.account, &p.account_error) {
            (Some(a), _) => Ok(Some(a.clone())),
            (None, Some(e)) => Err(e.clone()),
            (None, None) => Ok(None),
        });
    report_of(probe, account)
}

/// Test a saved site with its stored key through the switching service: probe,
/// key check, model list with the model map checked against it
/// (`SwitchService::probe_site`), and the balance (`SwitchService::account`).
pub async fn test_saved(
    service: Arc<dyn SwitchService>,
    site: SiteId,
    profile: ProfileId,
) -> SiteReport {
    let mut request = superba_core::SiteProbeRequest::new(site);
    request.key = Some(KeyInput::Profile(profile.clone()));
    let account_request = AccountRequest {
        profile_id: Some(profile),
        line_id: None,
        usage: None,
    };
    let (probe, account) = tokio::join!(
        service.probe_site(request),
        service.account(account_request)
    );
    let account = account.and_then(|report| match (report.summary, report.summary_error) {
        (Some(s), _) => Ok(Some(s)),
        (None, Some(e)) => Err(e),
        (None, None) => Ok(None),
    });
    report_of(probe, Some(account))
}

/// A [`SiteReport`] from a probe report and the balance.
fn report_of(
    probe: Result<superba_core::SiteProbeReport>,
    account: Option<Result<Option<AccountSummary>>>,
) -> SiteReport {
    match probe {
        Ok(p) => SiteReport {
            at: Some(Timestamp::now()),
            caps: Some(Ok(p.caps)),
            key: p.key_check.map(Ok),
            models: match (p.models, p.models_error) {
                (Some(m), _) => Some(Ok(m)),
                (None, Some(e)) => Some(Err(e)),
                (None, None) => None,
            },
            account,
            mapped: p.mapped,
            responses: match (p.responses, p.responses_error) {
                (Some(r), _) => Some(Ok(r)),
                (None, Some(e)) => Some(Err(e)),
                (None, None) => None,
            },
        },
        Err(e) => SiteReport {
            at: Some(Timestamp::now()),
            caps: Some(Err(e)),
            account,
            ..SiteReport::default()
        },
    }
}

/// Save a new site and its profile; a stale form never overwrites an existing id,
/// and the site is removed again when the profile cannot be saved.
pub async fn add(service: Arc<dyn SwitchService>, prepared: Prepared) -> Result<Profile> {
    let Prepared { site, profile, key } = prepared;
    let (sites, profiles) = tokio::try_join!(service.sites(), service.profile_set())?;
    let clash = |id: &str| {
        sites.iter().any(|s| s.id.as_str() == id)
            || profiles.profiles.iter().any(|p| p.id.as_str() == id)
    };
    if clash(site.id.as_str()) || clash(profile.id.as_str()) {
        return Err(Error::Conflict {
            message: "site.id_taken".into(),
        });
    }
    let id = site.id.clone();
    service.save_site(site).await?;
    let result = service.save_profile(profile, key).await;
    if result.is_err() {
        let _ = service.delete_site(&id).await;
    }
    result
}

/// Update an existing site and its profile (the stored key is kept unless a new
/// one is given). Restores the previous site when the profile cannot be saved.
pub async fn update(service: Arc<dyn SwitchService>, prepared: Prepared) -> Result<Profile> {
    let Prepared { site, profile, key } = prepared;
    let sites = service.sites().await?;
    let previous = sites
        .into_iter()
        .find(|s| s.id == site.id && s.kind == SiteKind::Compatible)
        .ok_or_else(|| Error::NotFound {
            what: site.id.to_string(),
        })?;
    // The switch service copies a changed site (lines, model map) into the
    // router's configuration when one of its profiles is a router member.
    service.save_site(site).await?;
    let result = service.save_profile(profile, key).await;
    if result.is_err() {
        let _ = service.save_site(previous).await;
    }
    result
}

/// Remove a custom site and its profiles; refused while one of them is applied.
pub async fn remove(service: Arc<dyn SwitchService>, id: SiteId) -> Result<()> {
    let (sites, profiles, overview) =
        tokio::try_join!(service.sites(), service.profile_set(), service.overview())?;
    if !sites
        .iter()
        .any(|s| s.id == id && s.kind == SiteKind::Compatible)
    {
        return Err(Error::NotFound {
            what: id.to_string(),
        });
    }
    let profiles: Vec<_> = profiles
        .profiles
        .iter()
        .filter(|p| p.site_id == id)
        .collect();
    if overview.active.as_ref().is_some_and(|active| {
        active.site_id.as_ref() == Some(&id)
            || profiles
                .iter()
                .any(|p| active.profile_id.as_ref() == Some(&p.id))
    }) {
        return Err(Error::Conflict {
            message: "site.remove_active".into(),
        });
    }
    for profile in profiles {
        service.delete_profile(&profile.id).await?;
    }
    service.delete_site(&id).await
}

/// Add a provider on a preset site: save the profile with its key (the service
/// stores the key in the secret backend).
pub async fn add_preset(
    service: Arc<dyn SwitchService>,
    profile: Profile,
    key: Secret,
) -> Result<Profile> {
    service.save_profile(profile, Some(key)).await
}

/// Remove a provider: a custom site goes with all its profiles (refused while
/// one is applied); a preset provider removes only its profile and key.
pub async fn remove_provider(
    service: Arc<dyn SwitchService>,
    profile: Option<ProfileId>,
    site: SiteId,
    custom: bool,
) -> Result<()> {
    if custom {
        return remove(service, site).await;
    }
    let Some(id) = profile else {
        return Err(Error::invalid("nothing to remove"));
    };
    let overview = service.overview().await?;
    if overview
        .active
        .as_ref()
        .is_some_and(|a| a.profile_id.as_ref() == Some(&id))
    {
        return Err(Error::Conflict {
            message: "site.remove_active".into(),
        });
    }
    service.delete_profile(&id).await
}

/// Check one key (stored or typed) on every line of `site` with the mode each
/// line resolves to.
pub async fn check_lines(
    service: Arc<dyn SwitchService>,
    site: SiteId,
    lines: Vec<(superba_core::LineId, AuthMode)>,
    key: KeyInput,
) -> Result<Vec<(superba_core::LineId, AuthMode, Result<KeyCheck>)>> {
    let mut out = Vec::new();
    for (line, mode) in lines {
        let r = service.check_key(&site, &line, mode, key.clone()).await;
        out.push((line, mode, r));
    }
    Ok(out)
}

/// The account report of one provider (identity, balance and, for sites with a
/// usage API, the usage of the last `days` days).
pub async fn account(
    service: Arc<dyn SwitchService>,
    profile: ProfileId,
    days: Option<u32>,
    refresh: bool,
) -> Result<superba_core::AccountReport> {
    let usage = days.map(|d| {
        let mut q = superba_core::UsageQuery::last_days(d);
        q.refresh = refresh;
        q
    });
    service
        .account(AccountRequest {
            profile_id: Some(profile),
            line_id: None,
            usage,
        })
        .await
}
