//! What the service returns for the harnesses (`SwitchService::harness_*`):
//! the list, one harness's state, apply / restore plans and reports, and the
//! backups of its files.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{HarnessId, HarnessProtocol, HarnessSettings, OpencodeMajor, ProtocolEndpoint};
use crate::codex_files::BackupSource;
use crate::i18n::Notice;
use crate::ids::ProfileId;
use crate::time::Timestamp;

/// The harness program (or app) found on this computer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessInstall {
    /// The program or app bundle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<PathBuf>,
    /// Its version, when it told.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// opencode: which product the version belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opencode_major: Option<OpencodeMajor>,
}

/// The state of one configuration file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HarnessFileStatus {
    /// The file does not exist (Superba creates it).
    Absent,
    /// Readable in its format.
    Valid,
    /// Not valid in its format (Superba refuses to write it).
    Invalid {
        /// 1-based line (0 = unknown).
        line: usize,
        /// 1-based column (0 = unknown).
        column: usize,
        /// What is wrong.
        message: String,
    },
    /// Valid JSON but not an object.
    NotObject,
    /// It cannot be read.
    Unreadable {
        /// Why.
        message: String,
    },
}

impl HarnessFileStatus {
    /// Superba can write the file.
    pub fn writable(&self) -> bool {
        matches!(self, HarnessFileStatus::Absent | HarnessFileStatus::Valid)
    }
}

/// One configuration file of a harness.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessFileView {
    /// Stable key within the harness (`models.json`,
    /// `profiles/web/cordis.patch.yml`, `cn/models.json`).
    pub key: String,
    /// Its path.
    pub path: PathBuf,
    /// Its state.
    pub status: HarnessFileStatus,
    /// It holds a key in plain text once Superba wrote it.
    #[serde(default)]
    pub holds_key: bool,
    /// Encrypted backups available in the shared editor.
    #[serde(default)]
    pub backups: usize,
}

/// What Superba applied last (no values: the files hold a key).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessApplied {
    /// Counter of applies.
    pub generation: u64,
    /// When.
    pub at: Timestamp,
    /// The provider applied.
    #[serde(default)]
    pub provider: Option<ProfileId>,
    /// The protocol applied.
    #[serde(default)]
    pub protocol: Option<HarnessProtocol>,
    /// Owned entries (`<file> <slot>`).
    #[serde(default)]
    pub owned: Vec<String>,
    /// Owned entries someone changed since (they stay that person's until an
    /// apply takes them over, which it asks first).
    #[serde(default)]
    pub drift: Vec<String>,
}

/// One harness at a glance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessSummary {
    /// Saved settings, so a row reflects its provider and route before opening it.
    #[serde(default)]
    pub settings: HarnessSettings,
    /// The harness.
    pub harness: HarnessId,
    /// Found on this computer (program, app or configuration directory).
    pub installed: bool,
    /// The program.
    #[serde(default)]
    pub install: Option<HarnessInstall>,
    /// The provider Superba applied (`None` = nothing applied).
    #[serde(default)]
    pub applied_provider: Option<ProfileId>,
    /// Superba manages entries there now.
    #[serde(default)]
    pub applied: bool,
    /// Owned entries someone changed since.
    #[serde(default)]
    pub drift: usize,
}

/// The state of one harness.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HarnessView {
    /// The harness.
    pub harness: HarnessId,
    /// Superba's settings for it.
    pub settings: HarnessSettings,
    /// The program, when found.
    #[serde(default)]
    pub install: Option<HarnessInstall>,
    /// Its configuration files (those Superba would write now and those it
    /// wrote before).
    #[serde(default)]
    pub files: Vec<HarnessFileView>,
    /// The protocols the configured provider offers that the harness speaks.
    #[serde(default)]
    pub protocols: Vec<HarnessProtocol>,
    /// The endpoint an apply would write (`None` = no usable provider yet).
    #[serde(default)]
    pub endpoint: Option<ProtocolEndpoint>,
    /// What Superba applied last.
    #[serde(default)]
    pub applied: Option<HarnessApplied>,
    /// Owned entries whose value differs from what the settings want now
    /// ("not applied yet").
    #[serde(default)]
    pub pending: Vec<String>,
    /// Remarks (hot reload, plain-text keys, login walls, overrides).
    #[serde(default)]
    pub notes: Vec<Notice>,
}

/// Apply the settings, or restore what was there before Superba.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessPlanKind {
    /// Write the owned entries for the settings.
    #[default]
    Apply,
    /// Put back what the owned entries held before Superba.
    Restore,
}

/// One owned entry a plan changes (values for people: keys masked).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessChange {
    /// The file's key.
    pub file: String,
    /// The entry (`providers.superba`, `models[id=gpt-5.5]`,
    /// `[id=llm-pi-ai].config.providers.superba`, `SUPERBA_API_KEY`).
    pub slot: String,
    /// Now (`None` = absent; one line).
    #[serde(default)]
    pub before: Option<String>,
    /// After (`None` = removed).
    #[serde(default)]
    pub after: Option<String>,
    /// Someone changed it after Superba wrote it: written only with "take
    /// over", else kept.
    #[serde(default)]
    pub drift: bool,
    /// It holds a key.
    #[serde(default)]
    pub secret: bool,
}

/// One file a plan reads or writes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessPlanFile {
    /// Key.
    pub key: String,
    /// Path.
    pub path: PathBuf,
    /// sha256 of the file planned against (`None` = absent): a file that
    /// changed since is not replaced.
    #[serde(default)]
    pub base_sha256: Option<String>,
}

/// What an apply or restore would change; never writes. Execute it with
/// `SwitchService::harness_apply`, passing back this value: the wanted
/// values (with the key) are not serialized.
#[derive(Clone, Debug, Serialize)]
pub struct HarnessPlan {
    /// Apply or restore.
    pub kind: HarnessPlanKind,
    /// The harness.
    pub harness: HarnessId,
    /// The provider applied (`Apply`).
    pub provider: Option<ProfileId>,
    /// The protocol applied (`Apply`).
    pub protocol: Option<HarnessProtocol>,
    /// The files involved, with their hashes.
    pub files: Vec<HarnessPlanFile>,
    /// What changes.
    pub changes: Vec<HarnessChange>,
    /// Remarks.
    pub notes: Vec<Notice>,
    /// What the plan writes (holds the key; never serialized).
    #[serde(skip)]
    pub desired: Option<super::HarnessTarget>,
}

impl HarnessPlan {
    /// Nothing would change.
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Entries someone changed after Superba wrote them.
    pub fn drift(&self) -> impl Iterator<Item = &HarnessChange> {
        self.changes.iter().filter(|c| c.drift)
    }
}

/// One file an apply or restore wrote.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessWrittenFile {
    /// Key.
    pub key: String,
    /// Path.
    pub path: PathBuf,
    /// sha256 now (`None` = removed again: Superba created it and nothing
    /// else is in it).
    #[serde(default)]
    pub sha256: Option<String>,
    /// The backup of the replaced file.
    #[serde(default)]
    pub backup: Option<HarnessBackup>,
}

/// The result of an apply or restore.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessApplyReport {
    /// Apply or restore.
    pub kind: HarnessPlanKind,
    /// The harness.
    pub harness: HarnessId,
    /// Files written or removed.
    #[serde(default)]
    pub files: Vec<HarnessWrittenFile>,
    /// Entries written or removed (`<file> <slot>`).
    #[serde(default)]
    pub changed: Vec<String>,
    /// Changed-by-someone entries left as they are.
    #[serde(default)]
    pub kept: Vec<String>,
    /// Remarks (when the harness picks the change up).
    #[serde(default)]
    pub notes: Vec<Notice>,
}

/// An encrypted copy of a harness file taken before Superba wrote it (the
/// file editor's backup store; the first one, "Original", is never pruned).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessBackup {
    /// Stable id (`spb harness backups`, `spb harness restore --backup`).
    pub id: String,
    /// The harness.
    pub harness: HarnessId,
    /// The file's key.
    pub file: String,
    /// The file's path when the copy was taken.
    pub path: PathBuf,
    /// When.
    pub at: Timestamp,
    /// Why.
    pub source: BackupSource,
    /// Size in bytes.
    pub size: u64,
    /// sha256 of the copy.
    #[serde(default)]
    pub sha256: Option<String>,
}

/// One file opened in the shared configuration editor.
#[derive(Clone, Debug)]
pub struct HarnessFileDoc {
    /// Harness.
    pub harness: HarnessId,
    /// File and status.
    pub file: HarnessFileView,
    /// Real bytes, never logged.
    pub contents: Option<crate::SecretBytes>,
    /// Original hash for compare-and-swap.
    pub sha256: Option<String>,
}
/// An explicit edit, backed up before writing.
#[derive(Clone, Debug)]
pub struct HarnessFileSave {
    /// Harness.
    pub harness: HarnessId,
    /// Stable file key, resolved by the writer; not an arbitrary path.
    pub file: String,
    /// Hash when the editor opened the file.
    pub base_sha256: Option<String>,
    /// Edited bytes, never logged.
    pub contents: crate::SecretBytes,
}
