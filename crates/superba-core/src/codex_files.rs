//! Editing Codex's own files by hand (GitHub issue 9): `config.toml` and
//! `auth.json` of this computer, of a remote host or of a WSL distribution,
//! opened, checked, saved and restored through Superba.
//!
//! The service ([`crate::SwitchService::codex_file`] and the methods next to
//! it) reads a file with what is needed to check it while it is edited (the
//! Codex versions reading that home and their settings catalog, the keys
//! Superba manages there), saves an edit after an encrypted backup of the file
//! it replaces, lists the backups of both files (the editor's own, the
//! pre-images of Superba's switches, the original before Superba's first
//! write) and reads one back for a restore, which is saved like an edit.
//!
//! A saved edit is the user's own change (GH#5): Superba never writes over it
//! by itself. A key Superba manages that the edit changed stays the user's
//! value; the next switch asks whether to keep it or take it over, a restore
//! keeps it, and the background check names the editor as its source
//! ([`crate::ChangeSource::SuperbaEditor`]). Running Codex reads the files only
//! when it starts, so the result says which of them still run the old file
//! ([`CodexFileSaved::runtime`]).
//!
//! The pure parts (syntax and catalog checks, formatting that keeps comments,
//! masking of `auth.json` secrets, line diffs) live in
//! `superba_config::codex_files`, so the GUI and `spb` check exactly alike.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::config::{CodexFileKind, KeyChange};
use crate::i18n::{LocalizedText, Notice};
use crate::ids::{ProfileId, TargetId};
use crate::ownership::AuthDrift;
use crate::runtime::RuntimeView;
use crate::secret::SecretBytes;
use crate::settings::{CatalogSource, SettingsConsumer};
use crate::time::Timestamp;

impl CodexFileKind {
    /// The file name (`config.toml`, `auth.json`).
    pub fn file_name(self) -> &'static str {
        match self {
            CodexFileKind::ConfigToml => "config.toml",
            CodexFileKind::AuthJson => "auth.json",
        }
    }

    /// The short name used on the command line (`config`, `auth`).
    pub fn short_name(self) -> &'static str {
        match self {
            CodexFileKind::ConfigToml => "config",
            CodexFileKind::AuthJson => "auth",
        }
    }

    /// Parse `config` / `config.toml` / `auth` / `auth.json`.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "config" | "config.toml" | "toml" => Some(CodexFileKind::ConfigToml),
            "auth" | "auth.json" | "json" => Some(CodexFileKind::AuthJson),
            _ => None,
        }
    }
}

/// What the catalog check of a `config.toml` needs: every Codex that reads
/// the home, and the settings catalog that describes their versions. No
/// consumers (or no catalog) means only the syntax is checked.
#[derive(Clone, Default)]
pub struct FileSchema {
    /// The Codex installs reading the home (the app's bundled Codex and the
    /// CLI for this computer; the host's Codex for a remote home).
    pub consumers: Vec<SettingsConsumer>,
    /// The settings catalog of that home's operating system.
    pub catalog: Option<Arc<dyn CatalogSource>>,
}

impl std::fmt::Debug for FileSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileSchema")
            .field("consumers", &self.consumers)
            .field("catalog", &self.catalog.is_some())
            .finish()
    }
}

/// A key Superba manages in a Codex file (it wrote it with its last switch).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedKey {
    /// Dotted path (`openai_base_url`, `features.fast_mode`).
    pub path: String,
    /// The value Superba wrote, as TOML (`None` = it removed the key, or the
    /// value is not known, e.g. on a remote host).
    #[serde(default)]
    pub written: Option<String>,
}

/// A Codex file opened for editing ([`crate::SwitchService::codex_file`]).
///
/// The contents may hold keys (`auth.json`, MCP tokens in `config.toml`):
/// they are [`SecretBytes`] and never printed by `Debug`.
#[derive(Clone)]
pub struct CodexFileDoc {
    /// Where the file lives (`local`, `ssh:<host>`, `wsl:<distro>`).
    pub target: TargetId,
    /// "This computer", the host's name, "Ubuntu (WSL)".
    pub target_label: String,
    /// Which file.
    pub kind: CodexFileKind,
    /// Its path (a POSIX path on a remote host).
    pub path: String,
    /// The bytes (`None` = the file does not exist yet).
    pub contents: Option<SecretBytes>,
    /// sha256 of the bytes (`None` = absent): the save refuses to replace a
    /// file that changed since it was opened.
    pub sha256: Option<String>,
    /// What the catalog check needs (`config.toml` only).
    pub schema: FileSchema,
    /// Keys Superba manages in this file (`config.toml`).
    pub owned: Vec<OwnedKey>,
    /// Superba wrote the credential in this `auth.json` and still manages it.
    pub owns_credential: bool,
    /// The provider Superba applied here, for "managed for <provider>".
    pub profile: Option<ProfileId>,
    /// Why the file cannot be saved now (a switch did not finish, the file is
    /// not readable or not UTF-8 text); `None` = it can be.
    pub read_only: Option<LocalizedText>,
    /// Remarks shown above the editor.
    pub notes: Vec<Notice>,
    /// When it was read.
    pub read_at: Timestamp,
}

impl std::fmt::Debug for CodexFileDoc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexFileDoc")
            .field("target", &self.target)
            .field("kind", &self.kind)
            .field("path", &self.path)
            .field("contents", &self.contents.as_ref().map(SecretBytes::len))
            .field("sha256", &self.sha256)
            .field("owned", &self.owned)
            .field("read_only", &self.read_only)
            .finish_non_exhaustive()
    }
}

impl CodexFileDoc {
    /// The text of the file (an absent file is empty; a byte order mark is
    /// left out). `None` when the file is not UTF-8.
    pub fn text(&self) -> Option<String> {
        match &self.contents {
            None => Some(String::new()),
            Some(b) => text_of(b.expose()),
        }
    }

    /// The file starts with a UTF-8 byte order mark (kept on save).
    pub fn has_bom(&self) -> bool {
        self.contents
            .as_ref()
            .is_some_and(|b| b.expose().starts_with(BOM))
    }
}

/// The UTF-8 byte order mark.
pub const BOM: &[u8] = b"\xef\xbb\xbf";

/// The text of file bytes without a byte order mark; `None` when not UTF-8.
pub fn text_of(bytes: &[u8]) -> Option<String> {
    let bytes = bytes.strip_prefix(BOM).unwrap_or(bytes);
    std::str::from_utf8(bytes).ok().map(str::to_owned)
}

/// How bad a problem is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProblemSeverity {
    /// Codex would refuse the file or misread the value.
    Error,
    /// Worth a look (an unknown or deprecated key).
    Warning,
    /// For information (a key Superba manages).
    Info,
}

/// What kind of check found a problem.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProblemKind {
    /// The file is not valid TOML / JSON: never saved (Codex could not start).
    Syntax,
    /// Checked against the settings catalog of the Codex versions reading the
    /// home: an error here can be saved anyway after an explicit confirmation
    /// (the catalog may lag a newer Codex).
    Schema,
    /// The shape of `auth.json` (fields Codex reads, their types).
    Auth,
    /// A masked secret was changed only partly (reveal the secrets to edit it).
    Secret,
    /// A key Superba manages.
    Owned,
}

/// One finding of the check of an edited file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileProblem {
    /// Severity.
    pub severity: ProblemSeverity,
    /// Which check.
    pub kind: ProblemKind,
    /// 1-based line.
    pub line: usize,
    /// 1-based column (characters).
    pub column: usize,
    /// Byte range in the text (underlined by the editor).
    #[serde(default)]
    pub start: usize,
    /// End of the byte range (exclusive).
    #[serde(default)]
    pub end: usize,
    /// The dotted key path, when the problem is about a key.
    #[serde(default)]
    pub path: Option<String>,
    /// What is wrong (never quotes a value of `auth.json`).
    pub message: LocalizedText,
}

impl FileProblem {
    /// This problem keeps the file from being saved at all.
    pub fn blocks_save(&self) -> bool {
        self.severity == ProblemSeverity::Error
            && matches!(self.kind, ProblemKind::Syntax | ProblemKind::Secret)
    }

    /// Saving needs an explicit "save anyway".
    pub fn needs_force(&self) -> bool {
        self.severity == ProblemSeverity::Error && !self.blocks_save()
    }
}

/// Why a file is saved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SaveReason {
    /// An edit (the GUI's editor, `spb config edit`).
    Edit,
    /// A backup put back ([`CodexFileBackup::id`]).
    Restore {
        /// The backup.
        backup: String,
    },
}

/// An edited file to save ([`crate::SwitchService::save_codex_file`]).
#[derive(Clone)]
pub struct CodexFileSave {
    /// Where.
    pub target: TargetId,
    /// Which file.
    pub kind: CodexFileKind,
    /// The new contents.
    pub contents: SecretBytes,
    /// sha256 of the file when it was opened (`None` = it did not exist): a
    /// file that changed since is not replaced ([`crate::Error::Conflict`]).
    pub base_sha256: Option<String>,
    /// Why.
    pub reason: SaveReason,
    /// Save although the catalog check found errors.
    pub force: bool,
}

impl std::fmt::Debug for CodexFileSave {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexFileSave")
            .field("target", &self.target)
            .field("kind", &self.kind)
            .field("contents", &self.contents.len())
            .field("base_sha256", &self.base_sha256)
            .field("reason", &self.reason)
            .field("force", &self.force)
            .finish()
    }
}

/// Where a backup of a Codex file came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum BackupSource {
    /// Taken before an edit was saved.
    BeforeEdit,
    /// Taken before another backup was restored.
    BeforeRestore {
        /// The backup that was restored.
        restored: String,
    },
    /// Taken by Superba before a switch wrote the file.
    BeforeSwitch {
        /// The switch's generation.
        generation: u64,
        /// The provider it applied.
        #[serde(default)]
        profile: Option<ProfileId>,
    },
    /// The file as it was before Superba first wrote it.
    Original,
}

impl BackupSource {
    /// "Before an edit", "Before switch 12 (Krill)", "Original (before Superba)".
    pub fn text(&self) -> LocalizedText {
        let t = |key: &str, fallback: &str| {
            LocalizedText::new(format!("codex_files.source.{key}"), fallback)
        };
        match self {
            BackupSource::BeforeEdit => t("before_edit", "Before an edit"),
            BackupSource::BeforeRestore { restored } => {
                t("before_restore", "Before restoring {backup}").arg("backup", restored)
            }
            BackupSource::BeforeSwitch {
                generation,
                profile: Some(p),
            } => t(
                "before_switch_named",
                "Before switch {generation} (to {profile})",
            )
            .arg("generation", generation)
            .arg("profile", p),
            BackupSource::BeforeSwitch { generation, .. } => {
                t("before_switch", "Before switch {generation}").arg("generation", generation)
            }
            BackupSource::Original => t("original", "Original, before Superba first wrote it"),
        }
    }
}

/// One backup of a Codex file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexFileBackup {
    /// Stable id (`spb config restore <id>`).
    pub id: String,
    /// Where the file belongs.
    pub target: TargetId,
    /// "This computer", the host's name.
    pub target_label: String,
    /// Which file.
    pub kind: CodexFileKind,
    /// When the backup was taken.
    pub at: Timestamp,
    /// Why.
    pub source: BackupSource,
    /// Size in bytes.
    pub size: u64,
    /// sha256 of the backed-up bytes, when recorded.
    #[serde(default)]
    pub sha256: Option<String>,
}

/// Which backups to list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexFileBackupQuery {
    /// Only this home (`None` = every home).
    #[serde(default)]
    pub target: Option<TargetId>,
    /// Only this file (`None` = both).
    #[serde(default)]
    pub kind: Option<CodexFileKind>,
}

/// A backup read back for a restore (decrypted; never printed by `Debug`).
#[derive(Clone)]
pub struct CodexFileBackupContents {
    /// The backup.
    pub backup: CodexFileBackup,
    /// Its bytes.
    pub contents: SecretBytes,
}

impl std::fmt::Debug for CodexFileBackupContents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexFileBackupContents")
            .field("backup", &self.backup)
            .field("contents", &self.contents.len())
            .finish()
    }
}

/// The result of a save.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexFileSaved {
    /// Where.
    pub target: TargetId,
    /// Which file.
    pub kind: CodexFileKind,
    /// Its path.
    pub path: String,
    /// sha256 of the saved bytes.
    pub sha256: String,
    /// The backup of the replaced file (`None` = there was none).
    #[serde(default)]
    pub backup: Option<CodexFileBackup>,
    /// Keys Superba manages that the save changed: `before` = what Superba
    /// wrote, `after` = the saved value. They stay the user's (GH#5).
    #[serde(default)]
    pub owned_changed: Vec<KeyChange>,
    /// The `auth.json` credential Superba wrote was replaced (shapes only).
    #[serde(default)]
    pub credential: Option<AuthDrift>,
    /// The running Codex consumers of this computer and which file each runs
    /// (`None` for a remote home; see [`CodexFileSaved::notes`]).
    #[serde(default)]
    pub runtime: Option<RuntimeView>,
    /// What to know (restart needed, kept values, ...).
    #[serde(default)]
    pub notes: Vec<Notice>,
    /// When.
    pub saved_at: Timestamp,
}

/// The last save of the editor in a local Codex home ("in effect", GH#4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualEdit {
    /// When it was saved.
    pub at: Timestamp,
    /// Which file.
    pub kind: CodexFileKind,
}

/// Text helpers shared by the GUI and `spb`.
pub mod texts {
    use super::*;

    /// "Superba manages this key (it wrote it with the last switch)."
    pub fn owned_key(profile: Option<&str>) -> LocalizedText {
        match profile {
            Some(p) => LocalizedText::new(
                "codex_files.owned_named",
                "Superba manages this key for {profile}: your change stays yours; the next switch asks before it changes it.",
            )
            .arg("profile", p),
            None => LocalizedText::new(
                "codex_files.owned",
                "Superba manages this key: your change stays yours; the next switch asks before it changes it.",
            ),
        }
    }

    /// The promise shown with the managed keys an edit changes (GH#5).
    pub fn owned_promise() -> LocalizedText {
        LocalizedText::new(
            "codex_files.owned_promise",
            "Superba never changes these back by itself: the next switch asks whether to keep your values or take them over, and a restore keeps them.",
        )
    }

    /// Codex reads the files only at start.
    pub fn restart_needed(file: &str) -> LocalizedText {
        LocalizedText::new(
            "codex_files.restart_needed",
            "Codex reads {file} only when it starts: Codex that is running now keeps the old file until it restarts.",
        )
        .arg("file", file)
    }
}
