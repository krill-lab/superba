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
    /// Usage history per source (previews answer it, imports mark it imported).
    usage: std::collections::BTreeMap<ImportSource, UsageImportReport>,
    usage_requests: Vec<UsageImportRequest>,
    /// The Krill provider a `superba://import` link updates (feature 140).
    link_existing: Option<ImportConflict>,
    /// Link imports requested (dry runs included).
    link_requests: Vec<LinkImportRequest>,
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

    /// What a usage preview of `report.source` finds.
    pub fn set_usage_report(&self, report: UsageImportReport) {
        self.areas
            .import
            .lock()
            .unwrap()
            .usage
            .insert(report.source, report);
    }

    /// Usage imports and previews requested.
    pub fn usage_requests(&self) -> Vec<UsageImportRequest> {
        self.areas.import.lock().unwrap().usage_requests.clone()
    }

    /// Imports requested.
    pub fn import_requests(&self) -> Vec<ImportApplyRequest> {
        self.areas.import.lock().unwrap().applied.clone()
    }

    /// The Krill provider link imports find (feature 140).
    pub fn set_link_existing(&self, existing: Option<ImportConflict>) {
        self.areas.import.lock().unwrap().link_existing = existing;
    }

    /// Link imports requested, dry runs included.
    pub fn link_requests(&self) -> Vec<LinkImportRequest> {
        self.areas.import.lock().unwrap().link_requests.clone()
    }

    /// Link imports that would have written (not dry runs).
    pub fn link_writes(&self) -> Vec<LinkImportRequest> {
        self.link_requests()
            .into_iter()
            .filter(|r| !r.dry_run)
            .collect()
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

/// A usage preview answers the set report; an import answers it as written
/// and turns later previews into "already imported".
pub(super) async fn import_usage(
    fake: &FakeService,
    request: UsageImportRequest,
) -> Result<UsageImportReport> {
    fake.delay(100).await;
    let mut area = fake.areas.import.lock().unwrap();
    area.usage_requests.push(request);
    let Some(found) = area.usage.get(&request.source).cloned() else {
        return Ok(UsageImportReport::new(request.source, request.dry_run));
    };
    let mut r = found.clone();
    r.dry_run = request.dry_run;
    if !request.dry_run {
        let mut later = found;
        later.already += later.requests + later.days;
        later.requests = 0;
        later.days = 0;
        area.usage.insert(request.source, later);
    }
    Ok(r)
}

pub(super) async fn import_link(
    fake: &FakeService,
    request: LinkImportRequest,
) -> Result<LinkImportReport> {
    let mut area = fake.areas.import.lock().unwrap();
    area.link_requests.push(request.clone());
    let existing = area.link_existing.clone();
    let name = request.link.name.clone().unwrap_or_else(|| "Krill".into());
    let outcome = match (&existing, request.on_conflict) {
        (Some(e), ConflictAction::Update) => ImportOutcome::Updated {
            profile: e.profile.clone(),
            profile_name: e.profile_name.clone(),
            key_stored: true,
        },
        (Some(e), ConflictAction::Skip) => ImportOutcome::Skipped {
            reason: LocalizedText::new("", format!("{} left as it is", e.profile_name)),
        },
        _ => ImportOutcome::Created {
            profile: ProfileId::new("krill-link"),
            profile_name: if existing.is_some() {
                format!("{name} (Krill link)")
            } else {
                name.clone()
            },
            site: None,
            key_stored: true,
        },
    };
    Ok(LinkImportReport {
        existing,
        item: ImportItem {
            candidate: "krill-link:link".into(),
            source: ImportSource::KrillLink,
            name,
            outcome,
            notes: Vec::new(),
        },
        line: request.line.clone(),
        masked_key: request.link.key.masked(),
    })
}
