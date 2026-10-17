//! Fake service, area "import": providers found in other tools (cc-switch, AI
//! Toolbox) and the import of the selected ones.
//!
//! Per-area state and the bodies of this area's `SwitchService` methods; the one
//! `impl SwitchService for FakeService` in `fake.rs` delegates here.

use superba_core::*;

use super::FakeService;

/// Fake state of the area.
#[derive(Debug, Default)]
pub(super) struct State {
    scan: ImportScan,
    report: Option<ImportReport>,
    scans: Vec<ImportScanRequest>,
    applied: Vec<ImportApplyRequest>,
}

impl FakeService {
    /// What scans return (filtered by the requested sources).
    pub fn set_import_scan(&self, scan: ImportScan) {
        self.areas.import.lock().unwrap().scan = scan;
    }

    /// What imports return (default: every selection created under its
    /// candidate's target name, with a key when the candidate has one).
    pub fn set_import_report(&self, report: ImportReport) {
        self.areas.import.lock().unwrap().report = Some(report);
    }

    /// Imports requested.
    pub fn import_requests(&self) -> Vec<ImportApplyRequest> {
        self.areas.import.lock().unwrap().applied.clone()
    }

    /// Scans requested.
    pub fn import_scans(&self) -> Vec<ImportScanRequest> {
        self.areas.import.lock().unwrap().scans.clone()
    }
}

pub(super) async fn scan(fake: &FakeService, request: ImportScanRequest) -> Result<ImportScan> {
    let mut area = fake.areas.import.lock().unwrap();
    area.scans.push(request.clone());
    let wanted = request.effective_sources();
    Ok(ImportScan {
        sources: area
            .scan
            .sources
            .iter()
            .filter(|s| wanted.contains(&s.source))
            .cloned()
            .collect(),
    })
}

pub(super) async fn apply(fake: &FakeService, request: ImportApplyRequest) -> Result<ImportReport> {
    let mut area = fake.areas.import.lock().unwrap();
    area.applied.push(request.clone());
    if let Some(r) = &area.report {
        return Ok(r.clone());
    }
    let items = request
        .selections
        .iter()
        .map(|sel| {
            let c = area.scan.candidate(&sel.candidate);
            let outcome = match c.and_then(|c| c.target.as_ref().map(|t| (c, t))) {
                Some((c, _)) if c.conflict.is_some() && sel.on_conflict == ConflictAction::Skip => {
                    ImportOutcome::Skipped {
                        reason: LocalizedText::new(
                            "import.skip.conflict",
                            "{name} already exists in Superba; it was left as it is.",
                        )
                        .arg("name", &c.conflict.as_ref().unwrap().profile_name),
                    }
                }
                Some((c, t)) => ImportOutcome::Created {
                    profile: ProfileId::new(t.site_id.as_str()),
                    profile_name: t.profile_name.clone(),
                    site: (t.kind != SiteKind::Krill).then(|| t.site_id.clone()),
                    key_stored: c.key.is_present(),
                },
                None => ImportOutcome::Skipped {
                    reason: LocalizedText::new(
                        "import.skip.gone",
                        "It is no longer in the other tool's configuration.",
                    ),
                },
            };
            ImportItem {
                candidate: sel.candidate.clone(),
                source: c.map_or(ImportSource::Ccs, |c| c.source),
                name: c.map_or_else(|| sel.candidate.clone(), |c| c.name.clone()),
                outcome,
                notes: c.map(|c| c.notes.clone()).unwrap_or_default(),
            }
        })
        .collect();
    Ok(ImportReport {
        dry_run: request.dry_run,
        items,
    })
}
