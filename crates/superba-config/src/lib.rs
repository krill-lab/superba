//! # superba-config
//!
//! Local persistence for Superba (owner: config agent):
//!
//! - [`TomlConfigEngine`] implements [`superba_core::ConfigEngine`]: owned-key
//!   patching of `config.toml` with `toml_edit` (every other byte preserved),
//!   `auth.json` forms for PAT / API key / ChatGPT login, sha256-checked atomic
//!   writes with rebase on concurrent change, a journal (Pending -> Publishing ->
//!   committed) with crash recovery, baseline capture and key-level restore,
//!   point-in-time restore by generation, and the encrypted ChatGPT login stash
//!   (plan 9).
//! - [`ShellEnvPersister`] implements [`superba_core::EnvPersister`]: marker blocks
//!   in shell rc files, Linux `environment.d`, macOS `launchctl` + LaunchAgent,
//!   Windows `HKCU\Environment`; legacy tutorial line takeover (plan 6.1, 9.9).
//! - [`KeyringSecretStore`] implements [`superba_core::SecretStore`] (OS keyring,
//!   0600 file fallback; `SUPERBA_SECRET_BACKEND=file|keyring|auto`).
//! - [`JsonProfileStore`] implements [`superba_core::ProfileStore`] (`profiles.json`).
//! - [`settings`]: the settings engine: a profile's Codex settings filtered and
//!   validated per Codex home against the settings catalog, legacy-key
//!   migrations, the diff preview and the `codex features list` cross-check.
//! - [`claude`]: Claude Code's `settings.json` (user bug 79): the keys Superba
//!   owns there written, restored and backed up, and Superba's own Claude Code
//!   configuration (`claude-code.json`).
//! - [`codex_files`]: the file editor (GitHub issue 9): checks, formatting
//!   that keeps comments, masked `auth.json` secrets, line diffs, and the
//!   encrypted backups taken before every save.
//! - [`harness`]: the other coding-agent harnesses' files (dsh, opencode, pi,
//!   ZCode, WorkBuddy): Superba's provider written, read back and restored
//!   with editors that keep comments and every other entry.
//! - [`toml_keys`], [`authjson`], [`version`]: reusable pure helpers.
//!
//! Everything is rooted in an injected [`superba_core::LocalContext`]; nothing here
//! reads the real `$HOME` or process environment directly.
//!
//! Files owned in the data dir: `profiles.json`, `state.json`, `env-state.json`,
//! `journal/config/`, `backups/{first-write,switch,login-stash,rc,editor}/`, `secrets/`.
//! Backups and stashes of `auth.json` / `config.toml` are encrypted
//! (ChaCha20-Poly1305, data key in the OS keyring or `secrets/vault-key`).

pub mod authjson;
pub mod autostart;
pub mod claude;
pub mod codex_files;
pub mod compaction_keys;
mod engine;
mod env;
mod fsx;
pub mod harness;
pub mod installation;
mod journal;
mod keystore;
mod profiles;
mod render;
mod secrets;
pub mod settings;
mod stash;
mod state;
pub mod toml_keys;
mod vault;
pub mod version;

pub use engine::{KEEP_SWITCH_BACKUPS, STALE_LOGIN_DAYS, TomlConfigEngine};
pub use env::{
    EnvPlatform, MemoryNativeEnv, NativeEnv, ShellEnvPersister, SystemNativeEnv, snapshots_from,
};
pub use keystore::ENV_SUPERBA_SECRET_BACKEND;
pub use profiles::{JsonProfileStore, PROFILES_SCHEMA};
pub use secrets::KeyringSecretStore;

/// English and Simplified Chinese catalogs for the `config.note.*` and
/// `env.scope.*` texts this crate returns; binaries layer them with
/// `superba_core::Catalog::add_toml`.
pub fn catalog_sources() -> [(superba_core::Locale, &'static str); 2] {
    [
        (superba_core::Locale::En, include_str!("../locales/en.toml")),
        (
            superba_core::Locale::ZhCn,
            include_str!("../locales/zh-CN.toml"),
        ),
    ]
}
