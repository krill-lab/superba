//! Path resolution with injectable environment and home directory.
//!
//! Nothing in Superba reads `$HOME`, `CODEX_HOME` or the data dir directly: every
//! implementation receives a [`LocalContext`] (or the pieces of it) so tests run
//! against temp dirs and never touch the real user's `~/.codex`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::codex::{ENV_CODEX_HOME, ENV_CODEX_SQLITE_HOME};

/// Read-only view of environment variables.
pub trait EnvSource: Send + Sync + fmt::Debug {
    /// Raw value.
    fn var_os(&self, key: &str) -> Option<OsString>;

    /// UTF-8 value; `None` if unset or not UTF-8.
    fn var(&self, key: &str) -> Option<String> {
        self.var_os(key).and_then(|v| v.into_string().ok())
    }

    /// Non-empty UTF-8 value.
    fn var_non_empty(&self, key: &str) -> Option<String> {
        self.var(key).filter(|v| !v.trim().is_empty())
    }
}

/// The real process environment.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn var_os(&self, key: &str) -> Option<OsString> {
        std::env::var_os(key)
    }
}

/// A fixed map, for tests.
#[derive(Debug, Clone, Default)]
pub struct MapEnv(pub BTreeMap<String, OsString>);

impl MapEnv {
    /// Empty environment.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: add a variable.
    pub fn with(mut self, key: &str, value: impl Into<OsString>) -> Self {
        self.0.insert(key.to_owned(), value.into());
        self
    }
}

impl EnvSource for MapEnv {
    fn var_os(&self, key: &str) -> Option<OsString> {
        self.0.get(key).cloned()
    }
}

/// File name of the Codex state database (threads table).
pub const STATE_DB_FILE: &str = "state_5.sqlite";
/// Superba's directory inside a Codex home (model catalog files).
pub const CODEX_HOME_SUPERBA_DIR: &str = "superba";

/// A resolved Codex home directory (`CODEX_HOME` or `<home>/.codex`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CodexHome {
    root: PathBuf,
    sqlite_home: PathBuf,
}

impl CodexHome {
    /// Resolve like Codex does: `CODEX_HOME` if set and non-empty, else
    /// `<home_dir>/.codex`; the sqlite directory is `CODEX_SQLITE_HOME` if set, else
    /// the root.
    pub fn resolve(env: &dyn EnvSource, home_dir: &Path) -> Self {
        let root = env
            .var_os(ENV_CODEX_HOME)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir.join(".codex"));
        let sqlite_home = env
            .var_os(ENV_CODEX_SQLITE_HOME)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| root.clone());
        Self { root, sqlite_home }
    }

    /// A home rooted at `root` with the sqlite db in the same directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            sqlite_home: root.clone(),
            root,
        }
    }

    /// Override the sqlite directory.
    pub fn with_sqlite_home(mut self, dir: impl Into<PathBuf>) -> Self {
        self.sqlite_home = dir.into();
        self
    }

    /// The Codex home directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `config.toml`.
    pub fn config_toml(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    /// `auth.json`.
    pub fn auth_json(&self) -> PathBuf {
        self.root.join("auth.json")
    }

    /// Directory holding the sqlite state database.
    pub fn sqlite_home(&self) -> &Path {
        &self.sqlite_home
    }

    /// `state_5.sqlite` (threads table with `model_provider`).
    pub fn state_db(&self) -> PathBuf {
        self.sqlite_home.join(STATE_DB_FILE)
    }

    /// `sessions/` (rollout-*.jsonl[.zst], nested by date).
    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    /// `archived_sessions/`.
    pub fn archived_sessions_dir(&self) -> PathBuf {
        self.root.join("archived_sessions")
    }

    /// `.codex-global-state.json` (desktop app state; may list remote hosts).
    pub fn global_state_json(&self) -> PathBuf {
        self.root.join(".codex-global-state.json")
    }

    /// `app-server-control/` (CLI daemon).
    pub fn app_server_control_dir(&self) -> PathBuf {
        self.root.join("app-server-control")
    }

    /// `tmp/`.
    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    /// `tmp/superba-migrate.lock`, held during session migration.
    pub fn migrate_lock(&self) -> PathBuf {
        self.tmp_dir().join("superba-migrate.lock")
    }

    /// `superba/`: files Superba writes into the Codex home (removed on
    /// uninstall after the restore).
    pub fn superba_dir(&self) -> PathBuf {
        self.root.join(CODEX_HOME_SUPERBA_DIR)
    }

    /// `superba/models-<sha8>.json`: a content-addressed model catalog file
    /// (written before `config.toml` names it, never rewritten).
    pub fn model_catalog_file(&self, sha8: &str) -> PathBuf {
        self.superba_dir().join(format!("models-{sha8}.json"))
    }
}

/// Superba's own data directory and its fixed layout (plan 3.3).
///
/// Ownership: `superba-config` owns profiles/state, `journal/config/` and the
/// `first-write`, `switch`, `login-stash`, `rc` backups and `secrets/`;
/// `superba-switch` owns `state.lock` (held for a whole execution) and
/// `journal/switch/`; `superba-sessions` owns `backups/migrate-*`;
/// `superba-appctl` owns `app-sessions.json`; `superba-remote` owns `remote/`;
/// `superba-router` owns `router/`; `superba-catalog` owns `feeds/`;
/// `superba-versions` owns `codex/` and `downloads/`; binaries own `logs/`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DataDir {
    root: PathBuf,
}

impl DataDir {
    /// Use `root` as the data directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `profiles.json` ([`crate::ProfileSet`]).
    pub fn profiles_json(&self) -> PathBuf {
        self.root.join("profiles.json")
    }

    /// `state.json` (per-home generation, active profile, last written hashes).
    pub fn state_json(&self) -> PathBuf {
        self.root.join("state.json")
    }

    /// `state.lock`: taken by `superba-switch` (GUI and `spb` alike) for the whole
    /// of `execute` / `recover`. Components never take it themselves (a second
    /// `flock` in the same process would deadlock); they rely on the caller.
    pub fn state_lock(&self) -> PathBuf {
        self.root.join("state.lock")
    }

    /// `journal/`.
    pub fn journal_dir(&self) -> PathBuf {
        self.root.join("journal")
    }

    /// `journal/config/` (config engine apply journal).
    pub fn config_journal_dir(&self) -> PathBuf {
        self.journal_dir().join("config")
    }

    /// `journal/switch/` (switch transaction journal: step receipts of config,
    /// env, migration and remote hosts, for start-up recovery).
    pub fn switch_journal_dir(&self) -> PathBuf {
        self.journal_dir().join("switch")
    }

    /// `backups/`.
    pub fn backups_dir(&self) -> PathBuf {
        self.root.join("backups")
    }

    /// `backups/first-write/` (original of each file before Superba's first write).
    pub fn first_write_backups_dir(&self) -> PathBuf {
        self.backups_dir().join("first-write")
    }

    /// `backups/switch/` (per-switch pre-images).
    pub fn switch_backups_dir(&self) -> PathBuf {
        self.backups_dir().join("switch")
    }

    /// `backups/login-stash/` (encrypted ChatGPT logins).
    pub fn login_stash_dir(&self) -> PathBuf {
        self.backups_dir().join("login-stash")
    }

    /// `backups/rc/` (shell rc originals before legacy lines are commented out).
    pub fn rc_backups_dir(&self) -> PathBuf {
        self.backups_dir().join("rc")
    }

    /// `backups/migrate-<id>/` for one session migration.
    pub fn migrate_backup_dir(&self, migration_id: &str) -> PathBuf {
        self.backups_dir().join(format!("migrate-{migration_id}"))
    }

    /// `secrets/` (0700; plaintext key files when no keyring is available).
    pub fn secrets_dir(&self) -> PathBuf {
        self.root.join("secrets")
    }

    /// `app-sessions.json` (apps launched by Superba: pid, CDP port, start time).
    pub fn app_sessions_json(&self) -> PathBuf {
        self.root.join("app-sessions.json")
    }

    /// `remote/` (per-host cache and journals).
    pub fn remote_dir(&self) -> PathBuf {
        self.root.join("remote")
    }

    /// `logs/`.
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// `cache/` (data that can be refetched, e.g. usage reports; never secrets).
    pub fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }

    /// `feeds/`: the verified feed cache (owned by `superba-catalog`).
    pub fn feeds_dir(&self) -> PathBuf {
        self.root.join("feeds")
    }

    /// `codex/`: verified, extracted Codex packages `<ver>-<target>/` (owned by
    /// `superba-versions`).
    pub fn codex_store_dir(&self) -> PathBuf {
        self.root.join("codex")
    }

    /// `downloads/`: verified desktop app packages (owned by `superba-versions`).
    pub fn downloads_dir(&self) -> PathBuf {
        self.root.join("downloads")
    }
}

/// Everything an implementation needs to know about "this machine".
///
/// Production code builds it once (`superba_switch::detect_context`); tests use
/// [`LocalContext::for_tests`] with a temp dir.
#[derive(Clone, Debug)]
pub struct LocalContext {
    /// The user's home directory (never read from `$HOME` elsewhere).
    pub home_dir: PathBuf,
    /// Superba's data directory.
    pub data_dir: DataDir,
    /// Environment view (process env in production, a [`MapEnv`] in tests).
    pub env: Arc<dyn EnvSource>,
}

impl LocalContext {
    /// Assemble a context.
    pub fn new(home_dir: impl Into<PathBuf>, data_dir: DataDir, env: Arc<dyn EnvSource>) -> Self {
        Self {
            home_dir: home_dir.into(),
            data_dir,
            env,
        }
    }

    /// `<root>/home` as home and `<root>/data` as data dir with an empty environment.
    pub fn for_tests(root: &Path) -> Self {
        Self {
            home_dir: root.join("home"),
            data_dir: DataDir::new(root.join("data")),
            env: Arc::new(MapEnv::new()),
        }
    }

    /// The local Codex home.
    pub fn codex_home(&self) -> CodexHome {
        CodexHome::resolve(self.env.as_ref(), &self.home_dir)
    }
}
