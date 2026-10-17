//! Provider import from other tools (cc-switch, AI Toolbox):
//! [`superba_core::SwitchService::import_scan`] and `import_apply`.
//!
//! `superba-import` reads the other tools' stores (read-only) and maps what it
//! finds; this module adds Superba's side: the profile set and sites for the
//! preview, and the saves of an import, which go through the ordinary
//! `save_site` / `save_profile` paths (validation, the secret store, router
//! member resync). Nothing is written to Codex and nothing restarts.

use superba_core::{
    Error, ImportApplyRequest, ImportItem, ImportOutcome, ImportReport, ImportScan,
    ImportScanRequest, ImportSource, Result, Site,
};
use superba_import::{Os, PlannedAction, ScanEnv, SourceRead};

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
            let outcome = match item.action {
                PlannedAction::Skip { reason } => ImportOutcome::Skipped { reason },
                PlannedAction::Create { site, profile, key } => {
                    let key_stored = key.is_some();
                    let site_id = site.as_ref().map(|s| s.id.clone());
                    if request.dry_run {
                        ImportOutcome::Created {
                            profile: profile.id.clone(),
                            profile_name: profile.name.clone(),
                            site: site_id,
                            key_stored,
                        }
                    } else {
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
                }
                PlannedAction::Update { site, profile, key } => {
                    let key_stored = key.is_some();
                    if request.dry_run {
                        ImportOutcome::Updated {
                            profile: profile.id.clone(),
                            profile_name: profile.name.clone(),
                            key_stored,
                        }
                    } else {
                        match self
                            .import_update(site, profile, key, &set.custom_sites)
                            .await
                        {
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
            };
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
