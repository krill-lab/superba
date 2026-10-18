//! Importing providers from other tools that manage Codex configurations
//! (cc-switch, AI Toolbox): what a scan found in their stores, how each provider
//! maps to a Superba profile, conflicts with existing profiles, and the result
//! of an import.
//!
//! Scans read the other tools' files only (`superba-import`); an import writes
//! Superba's profiles and its key storage, never Codex's files. Keys never appear
//! in these types: an import reads the source again and carries each key straight
//! into the secret store.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::i18n::LocalizedText;
use crate::ids::{LineId, ProfileId, SiteId};
use crate::origin::Origin;
use crate::site::SiteKind;

/// A tool Superba imports providers from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ImportSource {
    /// cc-switch (`farion1231/cc-switch`).
    #[serde(rename = "ccs")]
    Ccs,
    /// AI Toolbox (`coulsontl/ai-toolbox`).
    #[serde(rename = "ai-toolbox")]
    AiToolbox,
}

impl ImportSource {
    /// Every source, in the order the UIs list them.
    pub const ALL: [ImportSource; 2] = [ImportSource::Ccs, ImportSource::AiToolbox];

    /// Stable id (`ccs`, `ai-toolbox`), as `spb import` takes it.
    pub fn id(self) -> &'static str {
        match self {
            ImportSource::Ccs => "ccs",
            ImportSource::AiToolbox => "ai-toolbox",
        }
    }

    /// The tool's own name (not translated).
    pub fn name(self) -> &'static str {
        match self {
            ImportSource::Ccs => "CC Switch",
            ImportSource::AiToolbox => "AI Toolbox",
        }
    }

    /// Parse an id or a common spelling (`ccs`, `cc-switch`, `ai-toolbox`, ...).
    pub fn parse(value: &str) -> Option<Self> {
        let v: String = value
            .trim()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect();
        match v.as_str() {
            "ccs" | "ccswitch" => Some(ImportSource::Ccs),
            "aitoolbox" | "toolbox" => Some(ImportSource::AiToolbox),
            _ => None,
        }
    }
}

impl fmt::Display for ImportSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

/// What to scan.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportScanRequest {
    /// Sources to scan; empty = every source.
    #[serde(default)]
    pub sources: Vec<ImportSource>,
}

impl ImportScanRequest {
    /// Scan one source.
    pub fn source(source: ImportSource) -> Self {
        Self {
            sources: vec![source],
        }
    }

    /// The sources this request covers (every source when none is named).
    pub fn effective_sources(&self) -> Vec<ImportSource> {
        if self.sources.is_empty() {
            ImportSource::ALL.to_vec()
        } else {
            let mut out = Vec::new();
            for s in &self.sources {
                if !out.contains(s) {
                    out.push(*s);
                }
            }
            out
        }
    }
}

/// Everything a scan found, per source.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportScan {
    /// One entry per scanned source.
    pub sources: Vec<SourceScan>,
}

impl ImportScan {
    /// The scan of `source`.
    pub fn source(&self, source: ImportSource) -> Option<&SourceScan> {
        self.sources.iter().find(|s| s.source == source)
    }

    /// Every candidate of every source.
    pub fn candidates(&self) -> impl Iterator<Item = &ImportCandidate> {
        self.sources.iter().flat_map(|s| s.candidates.iter())
    }

    /// A candidate by id.
    pub fn candidate(&self, id: &str) -> Option<&ImportCandidate> {
        self.candidates().find(|c| c.id == id)
    }

    /// The selection of an import that names no candidates (`spb import`
    /// without `--select`): every importable candidate that is pre-selected or
    /// collides with an existing provider (`on_conflict` decides those).
    pub fn default_selections(&self, on_conflict: ConflictAction) -> Vec<ImportSelection> {
        self.candidates()
            .filter(|c| c.importable() && (c.preselected || c.conflict.is_some()))
            .map(|c| ImportSelection::new(c.id.clone(), on_conflict))
            .collect()
    }
}

/// What was found for one source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceScan {
    /// The tool.
    pub source: ImportSource,
    /// Every store looked at (found or not), in the order they were checked.
    pub stores: Vec<ImportStore>,
    /// Providers found in the stores that were read.
    pub candidates: Vec<ImportCandidate>,
}

impl SourceScan {
    /// True when some store of the tool exists.
    pub fn found(&self) -> bool {
        self.stores.iter().any(|s| s.status != StoreStatus::Missing)
    }
}

/// One configuration store of a tool (a database or a JSON file).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportStore {
    /// The file.
    pub path: PathBuf,
    /// The path for display (`~/...` under the home directory).
    pub display_path: String,
    /// What the file is ("cc-switch database, schema 5"; not translated).
    pub format: String,
    /// What happened to it.
    pub status: StoreStatus,
    /// Why it was not read ([`StoreStatus::Skipped`], [`StoreStatus::Superseded`]),
    /// or a remark on how it was read (a newer schema than Superba knows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<LocalizedText>,
}

/// What a scan did with a store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StoreStatus {
    /// Not there.
    Missing,
    /// Read; its providers are among the candidates.
    Read,
    /// There, but not read: an unknown version, a format Superba cannot read, or
    /// an error (see [`ImportStore::note`]).
    Skipped,
    /// There, but a newer store of the same tool replaced it (an old JSON file
    /// the tool kept after moving to its database).
    Superseded,
}

/// The coding tool a provider belongs to in the other tool.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ForeignApp {
    /// OpenAI Codex (importable).
    Codex,
    /// Claude Code.
    Claude,
    /// Gemini CLI.
    Gemini,
    /// Anything else, by the tool's own name.
    Other(String),
}

impl ForeignApp {
    /// Short label (not translated: tool names).
    pub fn label(&self) -> &str {
        match self {
            ForeignApp::Codex => "Codex",
            ForeignApp::Claude => "Claude Code",
            ForeignApp::Gemini => "Gemini CLI",
            ForeignApp::Other(name) => name,
        }
    }
}

/// Where the key of a found provider is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ImportKey {
    /// In the tool's own file; imported into Superba's key storage.
    Present {
        /// `••••abcd`.
        masked: String,
    },
    /// In the operating system's keychain (the tool keeps only a reference):
    /// Superba never reads another program's keychain entries, so the provider
    /// is imported without a key.
    Keychain,
    /// In an environment variable the tool names; not imported.
    EnvVar {
        /// Variable name.
        name: String,
    },
    /// No key.
    Missing,
}

impl ImportKey {
    /// A key will be imported.
    pub fn is_present(&self) -> bool {
        matches!(self, ImportKey::Present { .. })
    }
}

/// Whether a found provider can be imported.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CandidateStatus {
    /// It can (see [`ImportCandidate::target`]).
    Ready,
    /// It cannot, with the reason.
    NotImportable {
        /// Why.
        reason: LocalizedText,
    },
}

/// One line of an import target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportLine {
    /// Line id (a Krill line, or the new site's line).
    pub id: LineId,
    /// Display name.
    pub name: LocalizedText,
    /// Origin.
    pub origin: Origin,
}

/// What a found provider becomes in Superba.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportTarget {
    /// Krill (the Krill preset) or a custom OpenAI-compatible site.
    pub kind: SiteKind,
    /// The site the profile uses: the Krill site, or the new site's id.
    pub site_id: SiteId,
    /// Name of the new profile (unique among Superba's providers).
    pub profile_name: String,
    /// The Responses base URL of a new custom site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<Url>,
    /// Lines in priority order: the Krill line(s) the base URL matched, or the
    /// new site's line(s).
    pub lines: Vec<ImportLine>,
    /// Default model (`model = ...`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reasoning effort (`model_reasoning_effort = ...`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Codex model name -> the site's model name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_map: BTreeMap<String, String>,
}

/// An existing Superba provider a found provider collides with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportConflict {
    /// The existing profile.
    pub profile: ProfileId,
    /// Its name.
    pub profile_name: String,
    /// Same name (ignoring case and surrounding spaces).
    pub same_name: bool,
    /// Same base URL (custom sites) or the same Krill line.
    pub same_base_url: bool,
}

/// What to do with a selected provider that collides with an existing one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConflictAction {
    /// Leave the existing provider alone and import nothing (default).
    #[default]
    Skip,
    /// Import it as a new provider under another name.
    Rename,
    /// Update the existing provider: key (when the source has one), lines or
    /// base URL, model; its name, Codex settings and model overrides stay.
    Update,
}

impl ConflictAction {
    /// Every action, in the order the UIs offer them.
    pub const ALL: [ConflictAction; 3] = [
        ConflictAction::Skip,
        ConflictAction::Rename,
        ConflictAction::Update,
    ];

    /// Stable id (`skip`, `rename`, `update`).
    pub fn id(self) -> &'static str {
        match self {
            ConflictAction::Skip => "skip",
            ConflictAction::Rename => "rename",
            ConflictAction::Update => "update",
        }
    }
}

/// A provider found in another tool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportCandidate {
    /// Stable id within a scan (`ccs:<provider id>`); an import names candidates
    /// by it.
    pub id: String,
    /// The tool.
    pub source: ImportSource,
    /// Its name there.
    pub name: String,
    /// The coding tool it configures there.
    pub app: ForeignApp,
    /// The tool's current provider for that coding tool.
    #[serde(default)]
    pub current: bool,
    /// The base URL as the tool stores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// The model the tool sets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Where its key is.
    pub key: ImportKey,
    /// Importable or not (with the reason).
    pub status: CandidateStatus,
    /// What it becomes (importable candidates).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<ImportTarget>,
    /// An existing provider it collides with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<ImportConflict>,
    /// Things worth knowing (settings that are not imported, a key that stays
    /// behind, a Krill address that is not a known line, ...).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<LocalizedText>,
    /// Selected by default: importable, and nothing argues against it.
    #[serde(default)]
    pub preselected: bool,
}

impl ImportCandidate {
    /// True when it can be imported.
    pub fn importable(&self) -> bool {
        self.status == CandidateStatus::Ready && self.target.is_some()
    }

    /// The reason it cannot be imported.
    pub fn not_importable_reason(&self) -> Option<&LocalizedText> {
        match &self.status {
            CandidateStatus::NotImportable { reason } => Some(reason),
            _ => None,
        }
    }
}

/// One provider to import.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportSelection {
    /// [`ImportCandidate::id`].
    pub candidate: String,
    /// What to do when it collides with an existing provider.
    #[serde(default)]
    pub on_conflict: ConflictAction,
}

impl ImportSelection {
    /// Select `candidate` with `on_conflict`.
    pub fn new(candidate: impl Into<String>, on_conflict: ConflictAction) -> Self {
        Self {
            candidate: candidate.into(),
            on_conflict,
        }
    }
}

/// An import: the selected candidates of a fresh scan of their sources.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportApplyRequest {
    /// Selected candidates.
    pub selections: Vec<ImportSelection>,
    /// Plan only: report what would happen, write nothing.
    #[serde(default)]
    pub dry_run: bool,
}

/// What an import did (or would do) with one selected candidate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ImportOutcome {
    /// A new provider.
    Created {
        /// Its profile.
        profile: ProfileId,
        /// Its name.
        profile_name: String,
        /// The new custom site (`None` for Krill).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        site: Option<SiteId>,
        /// A key went into Superba's key storage.
        key_stored: bool,
    },
    /// An existing provider was updated.
    Updated {
        /// Its profile.
        profile: ProfileId,
        /// Its name.
        profile_name: String,
        /// A key went into Superba's key storage.
        key_stored: bool,
    },
    /// Not imported.
    Skipped {
        /// Why.
        reason: LocalizedText,
    },
    /// Importing it failed (nothing of it was written).
    Failed {
        /// What went wrong (no secrets).
        error: String,
    },
}

impl ImportOutcome {
    /// The profile created or updated.
    pub fn profile(&self) -> Option<&ProfileId> {
        match self {
            ImportOutcome::Created { profile, .. } | ImportOutcome::Updated { profile, .. } => {
                Some(profile)
            }
            _ => None,
        }
    }
}

/// One line of an import report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportItem {
    /// [`ImportCandidate::id`].
    pub candidate: String,
    /// The tool.
    pub source: ImportSource,
    /// The name there.
    pub name: String,
    /// What happened.
    pub outcome: ImportOutcome,
    /// Notes of the candidate (key left behind, settings not imported, ...).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<LocalizedText>,
}

/// The result of [`ImportApplyRequest`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportReport {
    /// Nothing was written.
    pub dry_run: bool,
    /// One entry per selection, in request order.
    pub items: Vec<ImportItem>,
}

impl ImportReport {
    /// Providers created or updated.
    pub fn imported(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.outcome.profile().is_some())
            .count()
    }

    /// The first profile created or updated.
    pub fn first_profile(&self) -> Option<&ProfileId> {
        self.items.iter().find_map(|i| i.outcome.profile())
    }
}

// ---------------------------------------------------------------------------
// Usage history (GitHub issue 1)
// ---------------------------------------------------------------------------

/// What [`crate::SwitchService::import_usage`] does with one tool's usage
/// history.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageImportRequest {
    /// The tool.
    pub source: ImportSource,
    /// Preview only: read and count, write nothing.
    #[serde(default)]
    pub dry_run: bool,
    /// Remove what earlier imports of this tool added (the tool is not read).
    #[serde(default)]
    pub remove: bool,
}

impl UsageImportRequest {
    /// A preview of `source`.
    pub fn preview(source: ImportSource) -> Self {
        Self {
            source,
            dry_run: true,
            remove: false,
        }
    }

    /// An import of `source`.
    pub fn import(source: ImportSource) -> Self {
        Self {
            source,
            dry_run: false,
            remove: false,
        }
    }
}

/// Preview or result of a usage import: what the tool's database holds for
/// Codex and what of it Superba adds to its usage history. Requests keep the
/// tool's own ids (imported once; a second import adds only what is new), its
/// recorded costs and its provider names; the Usage page lists them as
/// "imported from …".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageImportReport {
    /// The tool.
    pub source: ImportSource,
    /// Nothing was written.
    pub dry_run: bool,
    /// Imported rows were removed instead ([`UsageImportRequest::remove`]).
    #[serde(default)]
    pub removed: bool,
    /// The databases looked at.
    #[serde(default)]
    pub stores: Vec<ImportStore>,
    /// Requests added (with `dry_run`: that would be added).
    pub requests: u64,
    /// Requests an earlier import already added.
    pub already: u64,
    /// Daily totals added: days the tool keeps only as totals (older than its
    /// detail retention); they count in the statistics, not in the request list.
    pub days: u64,
    /// Requests those daily totals stand for.
    pub day_requests: u64,
    /// Rows the tool read from Codex's own session files for conversations
    /// Superba reads itself (not imported again).
    pub covered: u64,
    /// Rows of other coding tools (Claude Code, Gemini CLI, …): not imported.
    pub other_apps: u64,
    /// Earliest imported time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first: Option<crate::Timestamp>,
    /// Latest imported time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<crate::Timestamp>,
    /// Cost of the added rows and days, as the tool recorded it.
    pub cost: crate::Usd,
    /// Added rows and days without a recorded cost (shown without one).
    pub unpriced: u64,
    /// Remarks (a newer schema than Superba knows, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<LocalizedText>,
}

impl UsageImportReport {
    /// An empty report of `source`.
    pub fn new(source: ImportSource, dry_run: bool) -> Self {
        Self {
            source,
            dry_run,
            removed: false,
            stores: Vec::new(),
            requests: 0,
            already: 0,
            days: 0,
            day_requests: 0,
            covered: 0,
            other_apps: 0,
            first: None,
            last: None,
            cost: crate::Usd::ZERO,
            unpriced: 0,
            notes: Vec::new(),
        }
    }

    /// True when a store of the tool was read.
    pub fn found(&self) -> bool {
        self.stores.iter().any(|s| s.status == StoreStatus::Read)
    }
}
