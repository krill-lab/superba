//! Account data of a profile ([`superba_core::SwitchService::account`]): identity
//! and balance from the standard billing endpoints, and on request the usage
//! report (Krill organization costs / usage APIs). Read-only and never blocking:
//! each part fails on its own and is reported next to the other.

use superba_core::{
    AccountReport, AccountRequest, ActiveState, Error, Line, Profile, ProfileSet, Result, Site,
    SiteKind,
};

use crate::Switcher;
use crate::planner::all_sites;

/// True for profiles whose key can query account data (a stored key on a
/// key-based site).
fn has_account_key(profile: &Profile, sites: &[Site]) -> bool {
    profile.builtin.is_none()
        && profile.key_ref.is_some()
        && sites
            .iter()
            .any(|s| s.id == profile.site_id && s.kind != SiteKind::Official)
}

/// The profile to report on: the requested one, else the applied one, else the
/// last selected one, else the first profile with a key.
pub(crate) fn pick_profile<'a>(
    set: &'a ProfileSet,
    sites: &[Site],
    requested: Option<&superba_core::ProfileId>,
    active: Option<&ActiveState>,
) -> Result<&'a Profile> {
    if let Some(id) = requested {
        let profile = set.profile(id).ok_or_else(|| Error::NotFound {
            what: format!("profile {id}"),
        })?;
        if !has_account_key(profile, sites) {
            return Err(Error::invalid(format!(
                "profile {id} has no key, so it has no account data"
            )));
        }
        return Ok(profile);
    }
    let usable = |id: Option<&superba_core::ProfileId>| {
        id.and_then(|id| set.profile(id))
            .filter(|p| has_account_key(p, sites))
    };
    usable(active.and_then(|a| a.profile_id.as_ref()))
        .or_else(|| usable(set.last_selected.as_ref()))
        .or_else(|| set.profiles.iter().find(|p| has_account_key(p, sites)))
        .ok_or_else(|| Error::NotFound {
            what: "a profile with a key".into(),
        })
}

/// The line to query: the requested one, else the applied line when `profile`
/// is applied, else the profile's line, else the site's default line.
pub(crate) fn pick_line<'a>(
    site: &'a Site,
    profile: &Profile,
    requested: Option<&superba_core::LineId>,
    active: Option<&ActiveState>,
) -> Result<&'a Line> {
    if let Some(id) = requested {
        return site.line(id).ok_or_else(|| Error::NotFound {
            what: format!("line {id} of site {}", site.id),
        });
    }
    let applied = active
        .filter(|a| a.profile_id.as_ref() == Some(&profile.id))
        .and_then(|a| a.line_id.as_ref())
        .and_then(|id| site.line(id));
    applied
        .or_else(|| profile.line_id.as_ref().and_then(|id| site.line(id)))
        .or_else(|| site.default_line())
        .ok_or_else(|| Error::NotFound {
            what: format!("a line of site {}", site.id),
        })
}

impl Switcher {
    pub(crate) async fn account_impl(&self, request: AccountRequest) -> Result<AccountReport> {
        let d = &self.deps;
        if let Some(q) = &request.usage {
            q.validate()?;
        }
        let set = d.profiles.load().await?;
        let sites = all_sites(&set, d.ctx.env.as_ref());
        let active = d
            .config
            .read_state(&d.ctx.codex_home())
            .await
            .ok()
            .and_then(|s| s.active);
        let profile = pick_profile(&set, &sites, request.profile_id.as_ref(), active.as_ref())?;
        let site = sites
            .iter()
            .find(|s| s.id == profile.site_id)
            .ok_or_else(|| Error::NotFound {
                what: format!("site {}", profile.site_id),
            })?;
        let line = pick_line(site, profile, request.line_id.as_ref(), active.as_ref())?;
        let key_ref = profile.key_ref.as_ref().ok_or_else(|| Error::NotFound {
            what: format!("the key of profile {}", profile.id),
        })?;
        let key = d.secrets.get(key_ref).await?;

        let summary = d.prober.account_summary(site, line, &key);
        let usage = async {
            match &request.usage {
                None => None,
                Some(_) if !site.capabilities.usage_api => Some(Err(Error::Unsupported {
                    what: format!("usage data on site {}", site.id),
                })),
                Some(q) => Some(d.prober.usage_report(site, line, &key, q).await),
            }
        };
        let (summary, usage) = futures_util::join!(summary, usage);
        let (summary, summary_error) = match summary {
            Ok(s) => (s, None),
            Err(e) => (None, Some(e)),
        };
        let (usage, usage_error) = match usage {
            None => (None, None),
            Some(Ok(u)) => (Some(u), None),
            Some(Err(e)) => (None, Some(e)),
        };
        Ok(AccountReport {
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
            site_id: site.id.clone(),
            line_id: line.id.clone(),
            summary,
            summary_error,
            usage,
            usage_error,
        })
    }
}
