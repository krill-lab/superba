//! `state.json`: per Codex home, what Superba applied (generation history, owned
//! key values it last wrote, written hashes), the baseline and the first-write
//! backups. Read-modify-write goes through [`with_state`], serialised by a
//! process-wide mutex (the caller additionally holds `state.lock` across processes).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use superba_core::{
    ActiveState, Baseline, CodexFileKind, DataDir, GenerationRecord, KeySnapshot, Result, Timestamp,
};

use crate::fsx;

/// Current `state.json` schema.
const STATE_SCHEMA: u32 = 1;

static STATE_MUTEX: Mutex<()> = Mutex::new(());

/// Whole file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct StateFile {
    #[serde(default)]
    pub schema: u32,
    /// Keyed by the Codex home root path.
    #[serde(default)]
    pub homes: BTreeMap<String, HomeState>,
}

/// Pre-image of a Codex file before Superba's first write to it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FirstWrite {
    /// The file existed.
    pub existed: bool,
    /// sha256 of the original.
    #[serde(default)]
    pub sha256: Option<String>,
    /// Encrypted copy (only when it existed).
    #[serde(default)]
    pub backup: Option<PathBuf>,
}

/// Originals from a completed takeover cycle.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ArchivedBaseline {
    pub archived_at: Timestamp,
    pub baseline: Baseline,
    pub first_write: BTreeMap<CodexFileKind, FirstWrite>,
}

/// One successful apply (generation).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GenerationEntry {
    /// Public record.
    pub record: GenerationRecord,
    /// Journal / plan id that produced it.
    pub journal_id: String,
    /// What became active.
    pub active: ActiveState,
    /// Owned key values written by this generation (`value: None` = removed).
    pub last_written: Vec<KeySnapshot>,
    /// Hashes right after the apply.
    #[serde(default)]
    pub config_sha256: Option<String>,
    /// Hashes right after the apply.
    #[serde(default)]
    pub auth_sha256: Option<String>,
    /// Encrypted copy of auth.json right after the apply (point-in-time restore).
    #[serde(default)]
    pub auth_snapshot: Option<PathBuf>,
    /// Encrypted copy of config.toml right after the apply (byte-exact
    /// point-in-time restore when only owned keys changed since).
    #[serde(default)]
    pub config_snapshot: Option<PathBuf>,
    /// Identity hash of the ChatGPT login left in auth.json by this apply, if any.
    #[serde(default)]
    pub login_identity: Option<String>,
    /// This generation handed the configuration over to another tool (GH#5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handed_off: Option<superba_core::ChangeSource>,
}

/// State of one Codex home.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct HomeState {
    /// Highest generation number ever assigned (never reused).
    #[serde(default)]
    pub max_generation: u64,
    /// History, ascending by generation (rolled-back generations are removed).
    #[serde(default)]
    pub history: Vec<GenerationEntry>,
    /// Baseline captured at the start of the current takeover cycle.
    #[serde(default)]
    pub baseline: Option<Baseline>,
    /// The five most recently released takeover cycles.
    #[serde(default)]
    pub archived_baselines: Vec<ArchivedBaseline>,
    /// First-write pre-images per file.
    #[serde(default)]
    pub first_write: BTreeMap<CodexFileKind, FirstWrite>,
    /// Identities of ChatGPT logins the user signed out of (never restored again
    /// from a stash, pre-image or snapshot until the user signs in with them).
    #[serde(default)]
    pub signed_out: Vec<String>,
}

impl HomeState {
    /// The latest (active) generation.
    pub fn current(&self) -> Option<&GenerationEntry> {
        self.history.last()
    }

    /// What is active.
    pub fn active(&self) -> Option<ActiveState> {
        self.current().map(|e| e.active.clone())
    }

    /// Owned key values Superba wrote last (empty = owns nothing).
    pub fn last_written(&self) -> &[KeySnapshot] {
        self.current()
            .map(|e| e.last_written.as_slice())
            .unwrap_or(&[])
    }

    /// Identity of the ChatGPT login Superba last left in place.
    pub fn observed_login(&self) -> Option<&str> {
        self.current().and_then(|e| e.login_identity.as_deref())
    }
}

/// Map key of a home.
pub(crate) fn home_key(root: &Path) -> String {
    root.to_string_lossy().into_owned()
}

fn load(data: &DataDir) -> Result<StateFile> {
    Ok(fsx::read_json::<StateFile>(&data.state_json(), "state.json")?.unwrap_or_default())
}

/// Read the state of one home (default when unknown).
pub(crate) fn read_home(data: &DataDir, root: &Path) -> Result<HomeState> {
    let _g = STATE_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
    Ok(load(data)?
        .homes
        .get(&home_key(root))
        .cloned()
        .unwrap_or_default())
}

/// Read-modify-write one home's state atomically (in-process serialised).
pub(crate) fn with_home<T>(
    data: &DataDir,
    root: &Path,
    f: impl FnOnce(&mut HomeState) -> Result<T>,
) -> Result<T> {
    let _g = STATE_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
    let mut file = load(data)?;
    file.schema = STATE_SCHEMA;
    let key = home_key(root);
    let mut home = file.homes.get(&key).cloned().unwrap_or_default();
    let out = f(&mut home)?;
    file.homes.insert(key, home);
    fsx::write_json(&data.state_json(), &file)?;
    Ok(out)
}
