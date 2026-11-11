//! Provider import from other tools (cc-switch, AI Toolbox):
//! [`superba_core::SwitchService::import_scan`] and `import_apply`.
//!
//! `superba-import` reads the other tools' stores (read-only) and maps what it
//! finds; this module adds Superba's side: the profile set and sites for the
//! preview, and the saves of an import, which go through the ordinary
//! `save_site` / `save_profile` paths (validation, the secret store, router
//! member resync). Nothing is written to Codex and nothing restarts.
//!
//! A `superba://import` link from the Krill website (feature 140,
//! [`superba_core::SwitchService::import_link`]) takes the same path: its key
//! becomes a foreign provider of the Krill link source, mapped and planned by
//! `superba-import` and saved here. The link always means the Krill provider,
//! so its conflict is the existing Krill provider (not a name or address
//! match), an update keeps that provider's lines, and the result always uses
//! API-key auth.

use superba_core::catalog::KRILL_SITE_ID;
use superba_core::{
    ActiveState, AuthChoice, AuthMode, ConflictAction, Error, ForeignApp, ImportApplyRequest,
    ImportConflict, ImportItem, ImportOutcome, ImportReport, ImportScan, ImportScanRequest,
    ImportSelection, ImportSource, LinkImportReport, LinkImportRequest, Profile, ProfileSet,
    Result, Site,
};
use superba_import::{ForeignKey, ForeignProvider, Os, PlannedAction, ScanEnv, SourceRead};

use crate::Switcher;
use crate::planner::all_sites;

/// Directory (under Superba's cache) for private copies of databases that had
/// pending changes; each copy is removed right after reading.
const SCRATCH_DIR: &str = "import";

impl Switcher {
    /// Read `sources` on a blocking thread.
    async fn import_read(&self, sources: Vec<ImportSource>) -> Result<Vec<SourceRead>> {
        let home = self.deps.ctx.home_dir.clone();
        let env = self.deps.ctx.env.clone();
        let scratch = self.deps.ctx.data_dir.cache_dir().join(SCRATCH_DIR);
        tokio::task::spawn_blocking(move || {
            let scan_env = ScanEnv {
                home: &home,
                env: env.as_ref(),
                os: Os::current(),
                scratch: &scratch,
            };
            sources
                .into_iter()
                .map(|s| superba_import::read_source(s, &scan_env))
                .collect()
        })
        .await
        .map_err(|e| Error::other(format!("import scan: {e}")))
    }

    pub(crate) async fn import_scan_impl(&self, request: ImportScanRequest) -> Result<ImportScan> {
        let reads = self.import_read(request.effective_sources()).await?;
        let set = self.deps.profiles.load().await?;
        let sites = all_sites(&set, self.deps.ctx.env.as_ref());
        Ok(superba_import::preview(&reads, &set, &sites))
    }

    pub(crate) async fn import_apply_impl(
        &self,
        request: ImportApplyRequest,
    ) -> Result<ImportReport> {
        if !request.dry_run {
            self.refuse_elevated()?;
        }
        let mut sources: Vec<ImportSource> = Vec::new();
        for sel in &request.selections {
            let source = sel
                .candidate
                .split_once(':')
                .and_then(|(s, _)| ImportSource::parse(s))
                .ok_or_else(|| {
                    Error::invalid(format!("unknown import candidate {:?}", sel.candidate))
                })?;
            if !sources.contains(&source) {
                sources.push(source);
            }
        }
        let mut report = ImportReport {
            dry_run: request.dry_run,
            items: Vec::new(),
        };
        if sources.is_empty() {
            return Ok(report);
        }
        // A fresh scan: the preview the user saw may be stale, and keys never
        // left this process.
        let reads = self.import_read(sources).await?;
        let set = self.deps.profiles.load().await?;
        let sites = all_sites(&set, self.deps.ctx.env.as_ref());
        let scan = superba_import::preview(&reads, &set, &sites);
        let make_site: superba_import::MakeSite<'_> =
            &|id, name, url| superba_net::new_compatible_site(id, name, url);
        let planned =
            superba_import::plan(&reads, &scan, &set, &sites, &request.selections, make_site);
        for item in planned {
            let (source, name, notes) = match &item.candidate {
                Some(c) => (c.source, c.name.clone(), c.notes.clone()),
                None => (
                    item.selection
                        .candidate
                        .split_once(':')
                        .and_then(|(s, _)| ImportSource::parse(s))
                        .unwrap_or(ImportSource::Ccs),
                    item.selection.candidate.clone(),
                    Vec::new(),
                ),
            };
            let outcome = self
                .save_planned(item.action, request.dry_run, &set.custom_sites)
                .await;
            report.items.push(ImportItem {
                candidate: item.selection.candidate,
                source,
                name,
                outcome,
                notes,
            });
        }
        Ok(report)
    }

    pub(crate) async fn import_link_impl(
        &self,
        request: LinkImportRequest,
    ) -> Result<LinkImportReport> {
        if !request.dry_run {
            self.refuse_elevated()?;
        }
        let set = self.deps.profiles.load().await?;
        let sites = all_sites(&set, self.deps.ctx.env.as_ref());
        let krill = sites
            .iter()
            .find(|s| s.id.as_str() == KRILL_SITE_ID)
            .ok_or_else(|| Error::NotFound {
                what: format!("site {KRILL_SITE_ID}"),
            })?;
        let active = self
            .deps
            .config
            .read_state(&self.deps.ctx.codex_home())
            .await
            .ok()
            .and_then(|s| s.active);
        let existing = existing_krill(&set, active.as_ref());
        let conflict = existing.map(|p| ImportConflict {
            profile: p.id.clone(),
            profile_name: p.name.clone(),
            same_name: false,
            same_base_url: true,
        });
        let updating = existing.is_some() && request.on_conflict == ConflictAction::Update;
        // The address the mapping resolves to a Krill line: an update's own
        // first line (its lines stay as they are), else the requested line,
        // else the site's default line.
        let line = match existing.filter(|_| updating) {
            Some(p) => p.line_id.as_ref().and_then(|l| krill.line(l)),
            None => request.line.as_ref().and_then(|l| krill.line(l)),
        }
        .or_else(|| krill.default_line())
        .ok_or_else(|| Error::NotFound {
            what: format!("a line of site {KRILL_SITE_ID}"),
        })?;
        let name = request
            .link
            .name
            .clone()
            .unwrap_or_else(|| krill.name.render_fallback());
        let mut provider =
            ForeignProvider::new(ImportSource::KrillLink, "link", name, ForeignApp::Codex);
        provider.current = true;
        provider.base_url = Some(line.origin.to_string());
        provider.key = ForeignKey::Secret(request.link.key.clone());
        let candidate = provider.candidate_id();
        let reads = vec![SourceRead {
            source: ImportSource::KrillLink,
            stores: Vec::new(),
            providers: vec![provider],
        }];
        let mut scan = superba_import::preview(&reads, &set, &sites);
        for c in scan
            .sources
            .iter_mut()
            .flat_map(|s| s.candidates.iter_mut())
        {
            c.conflict = conflict.clone();
            if updating && let Some(t) = c.target.as_mut() {
                // `plan`'s update puts the target's lines first: none, so the
                // provider's line choice stays.
                t.lines.clear();
            }
        }
        let make_site: superba_import::MakeSite<'_> =
            &|id, name, url| superba_net::new_compatible_site(id, name, url);
        let planned = superba_import::plan(
            &reads,
            &scan,
            &set,
            &sites,
            &[ImportSelection::new(candidate.clone(), request.on_conflict)],
            make_site,
        )
        .into_iter()
        .next()
        .ok_or_else(|| Error::other("import link: nothing planned"))?;
        let (notes, display) = match &planned.candidate {
            Some(c) => (c.notes.clone(), c.name.clone()),
            None => (Vec::new(), candidate.clone()),
        };
        let mut action = planned.action;
        let mut first_line = None;
        if let PlannedAction::Create { profile, .. } | PlannedAction::Update { profile, .. } =
            &mut action
        {
            use_api_key(profile);
            first_line = profile.line_priority().into_iter().next();
        }
        let outcome = self
            .save_planned(action, request.dry_run, &set.custom_sites)
            .await;
        Ok(LinkImportReport {
            existing: conflict,
            item: ImportItem {
                candidate,
                source: ImportSource::KrillLink,
                name: display,
                outcome,
                notes,
            },
            line: first_line,
            masked_key: request.link.key.masked(),
        })
    }

    /// Carry out one planned import action (or, as a dry run, report it).
    async fn save_planned(
        &self,
        action: PlannedAction,
        dry_run: bool,
        before: &[Site],
    ) -> ImportOutcome {
        match action {
            PlannedAction::Skip { reason } => ImportOutcome::Skipped { reason },
            PlannedAction::Create { site, profile, key } => {
                let key_stored = key.is_some();
                let site_id = site.as_ref().map(|s| s.id.clone());
                if dry_run {
                    return ImportOutcome::Created {
                        profile: profile.id.clone(),
                        profile_name: profile.name.clone(),
                        site: site_id,
                        key_stored,
                    };
                }
                match self.import_create(site, profile, key).await {
                    Ok(saved) => ImportOutcome::Created {
                        profile: saved.id,
                        profile_name: saved.name,
                        site: site_id,
                        key_stored,
                    },
                    Err(e) => ImportOutcome::Failed {
                        error: e.to_string(),
                    },
                }
            }
            PlannedAction::Update { site, profile, key } => {
                let key_stored = key.is_some();
                if dry_run {
                    return ImportOutcome::Updated {
                        profile: profile.id.clone(),
                        profile_name: profile.name.clone(),
                        key_stored,
                    };
                }
                match self.import_update(site, profile, key, before).await {
                    Ok(saved) => ImportOutcome::Updated {
                        profile: saved.id,
                        profile_name: saved.name,
                        key_stored,
                    },
                    Err(e) => ImportOutcome::Failed {
                        error: e.to_string(),
                    },
                }
            }
        }
    }

    /// Save a new site (if any) and the new profile with its key; a site saved
    /// for a profile that then fails is removed again.
    async fn import_create(
        &self,
        site: Option<Site>,
        profile: superba_core::Profile,
        key: Option<superba_core::Secret>,
    ) -> Result<superba_core::Profile> {
        let site_id = match site {
            Some(site) => Some(self.save_site_impl(site).await?.id),
            None => None,
        };
        match self.save_profile_impl(profile, key).await {
            Ok(p) => Ok(p),
            Err(e) => {
                if let Some(id) = site_id {
                    let _ = self.delete_site_impl(&id).await;
                }
                Err(e)
            }
        }
    }

    /// Save the updated site (if any) and profile; when the profile cannot be
    /// saved the site gets its earlier definition back.
    async fn import_update(
        &self,
        site: Option<Site>,
        profile: superba_core::Profile,
        key: Option<superba_core::Secret>,
        before: &[Site],
    ) -> Result<superba_core::Profile> {
        let previous = site
            .as_ref()
            .and_then(|s| before.iter().find(|b| b.id == s.id).cloned());
        if let Some(site) = site {
            self.save_site_impl(site).await?;
        }
        match self.save_profile_impl(profile, key).await {
            Ok(p) => Ok(p),
            Err(e) => {
                if let Some(old) = previous {
                    let _ = self.save_site_impl(old).await;
                }
                Err(e)
            }
        }
    }
}

/// The Krill provider a link updates: the applied one, else the last
/// selected one, else the first (built-in profiles never).
fn existing_krill<'a>(set: &'a ProfileSet, active: Option<&ActiveState>) -> Option<&'a Profile> {
    let krill = |id: Option<&superba_core::ProfileId>| {
        id.and_then(|id| set.profile(id))
            .filter(|p| !p.is_builtin() && p.site_id.as_str() == KRILL_SITE_ID)
    };
    krill(active.and_then(|a| a.profile_id.as_ref()))
        .or_else(|| krill(set.last_selected.as_ref()))
        .or_else(|| {
            set.profiles
                .iter()
                .find(|p| !p.is_builtin() && p.site_id.as_str() == KRILL_SITE_ID)
        })
}

/// A link's key is an API key: the provider uses API-key auth (never PAT, also
/// when it was a PAT provider before).
fn use_api_key(profile: &mut Profile) {
    profile.auth_mode = AuthMode::ApiKey;
    profile.set_auth(AuthChoice::ApiKey);
}
