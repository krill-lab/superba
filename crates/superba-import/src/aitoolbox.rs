//! AI Toolbox (`coulsontl/ai-toolbox`, Tauri identifier `com.ai-toolbox`).
//!
//! Where it keeps its data: `<app data>/com.ai-toolbox/` (`~/Library/Application
//! Support` on macOS, `$XDG_DATA_HOME` or `~/.local/share` on Linux,
//! `%APPDATA%` on Windows), unless `AI_TOOLBOX_DATA_DIR` names another
//! directory, or `app_paths.json` in the default directory has a
//! `data_dir_override` (v1.1.5 and later; the old copy in the default directory
//! is then stale and not read).
//!
//! Formats:
//!
//! - `ai-toolbox.db` (v0.9.2 and later): SQLite in WAL mode, `PRAGMA
//!   user_version` 4 to 29 (HEAD; v1.1.9 is 22). Every table is `(id, data
//!   JSONB, created_at, updated_at)`; `codex_provider.data` holds `name`,
//!   `category`, `settings_config` (a JSON string: `auth.OPENAI_API_KEY`,
//!   `config` TOML), `meta` (`apiFormat`, `modelRewrites`), `is_applied`,
//!   `is_disabled`, `sort_index`. The provider TOML is laid over the shared one
//!   in `codex_common_config` (`common`). Reading JSONB needs SQLite 3.45 (the
//!   bundled one is newer).
//! - `database/` (v0.1.0 to v0.9.1): SurrealDB (SurrealKV), which Superba cannot
//!   read; AI Toolbox v0.9.2 to v1.1.3 converts it.
//!
//! Keys are plain text in the database (AI Toolbox keeps only its backup
//! password in the OS keyring). A key given as a `keyring:` reference is
//! recognised and never read.

use std::path::{Path, PathBuf};

use serde_json::Value;
use superba_core::{ForeignApp, ImportSource, ImportStore, LocalizedText, StoreStatus};

use crate::ccs::expand_tilde;
use crate::codex::{self, Protocol};
use crate::foreign::{ForeignKey, ForeignProvider, SourceRead};
use crate::{ScanEnv, sqlite, text};

/// Tauri bundle identifier (the data directory's name).
pub const APP_ID: &str = "com.ai-toolbox";
/// The database file.
pub const DB_FILE: &str = "ai-toolbox.db";
/// Oldest SQLite schema (`PRAGMA user_version`) AI Toolbox released (v0.9.2).
pub const MIN_SCHEMA: i64 = 4;
/// Newest schema Superba was written against (AI Toolbox HEAD, 2026-10-17).
pub const KNOWN_SCHEMA: i64 = 29;
/// Environment variable AI Toolbox reads as its data directory.
pub const ENV_DATA_DIR: &str = "AI_TOOLBOX_DATA_DIR";

const TOOL: &str = "AI Toolbox";

/// Provider tables of other coding tools (listed as not importable).
const OTHER_TABLES: &[(&str, &str)] = &[
    ("claude_provider", "claude"),
    ("gemini_cli_provider", "gemini"),
    ("grok_provider", "Grok CLI"),
    ("kimi_provider", "Kimi Code"),
    ("claude_desktop_provider", "Claude Desktop"),
    ("antigravity_provider", "Antigravity"),
    ("zcode_provider", "ZCode"),
];

/// The platform default data directory.
pub fn default_dir(env: &ScanEnv<'_>) -> PathBuf {
    env.app_data_dir().join(APP_ID)
}

/// The data directory AI Toolbox uses: (directory, it is an override).
pub fn data_dir(env: &ScanEnv<'_>) -> (PathBuf, bool) {
    if let Some(dir) = env.env.var_non_empty(ENV_DATA_DIR) {
        let dir = expand_tilde(&dir, env.home);
        if dir.is_absolute() {
            return (dir, true);
        }
    }
    let default = default_dir(env);
    let file = default.join("app_paths.json");
    if let Some(dir) = std::fs::read_to_string(file)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| {
            v.get("data_dir_override")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .filter(|s| !s.trim().is_empty())
        .map(|s| expand_tilde(&s, env.home))
        .filter(|d| d.is_absolute() && d != &default)
    {
        return (dir, true);
    }
    (default, false)
}

/// True when a legacy SurrealDB directory has real entries.
fn legacy_store(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.flatten().any(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            !(name == ".DS_Store" || name.starts_with("._") || name == ".backup_marker")
        })
    })
}

/// Read AI Toolbox's stores.
pub fn read(env: &ScanEnv<'_>) -> SourceRead {
    let (dir, overridden) = data_dir(env);
    let db = dir.join(DB_FILE);
    let mut stores = Vec::new();
    let mut providers = Vec::new();
    if db.is_file() {
        let (store, found) = read_db(env, &db);
        stores.push(store);
        providers = found;
    } else if legacy_store(&dir.join("database")) {
        stores.push(env.store(
            &dir.join("database"),
            "AI Toolbox SurrealDB (v0.9.1 or older)",
            StoreStatus::Skipped,
            Some(text(
                "import.store.surrealdb",
                "This is AI Toolbox's old database format (v0.9.1 and older), which Superba cannot read. Open AI Toolbox v0.9.2 to v1.1.3 once to convert it, then import again.",
            )),
        ));
    } else {
        stores.push(env.store(&db, "AI Toolbox database", StoreStatus::Missing, None));
    }
    let stale = default_dir(env).join(DB_FILE);
    if overridden && stale != db && stale.is_file() {
        stores.push(
            env.store(
                &stale,
                "AI Toolbox database",
                StoreStatus::Superseded,
                Some(
                    text(
                        "import.store.moved",
                        "{tool} keeps its data in {path} now; this copy is not read.",
                    )
                    .arg("tool", TOOL)
                    .arg("path", env.display(&dir)),
                ),
            ),
        );
    }
    SourceRead {
        source: ImportSource::AiToolbox,
        stores,
        providers,
    }
}

fn unreadable(error: impl std::fmt::Display) -> LocalizedText {
    text(
        "import.store.unreadable",
        "It could not be read: {error}. Close {tool} and try again.",
    )
    .arg("error", error)
    .arg("tool", TOOL)
}

/// Rows of a JSONB table as decoded JSON text (`None` for an undecodable row).
pub(crate) fn rows(
    conn: &rusqlite::Connection,
    table: &str,
) -> rusqlite::Result<Vec<(String, Option<Value>)>> {
    // Table names come from this module's constants.
    let sql = format!(
        "SELECT id, CASE WHEN json_valid(data, 5) THEN json(data) END FROM \"{table}\" \
         ORDER BY CAST(json_extract(CASE WHEN json_valid(data, 5) THEN data END, '$.sort_index') AS INTEGER), id"
    );
    let mut stmt = conn.prepare(&sql)?;
    stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?
                .and_then(|t| serde_json::from_str(&t).ok()),
        ))
    })?
    .collect()
}

type DbRead = (String, Option<LocalizedText>, Vec<ForeignProvider>);

fn read_db(env: &ScanEnv<'_>, db: &Path) -> (ImportStore, Vec<ForeignProvider>) {
    let snapshot = match sqlite::open(db, env.scratch) {
        Ok(s) => s,
        Err(e) => {
            return (
                env.store(
                    db,
                    "AI Toolbox database",
                    StoreStatus::Skipped,
                    Some(unreadable(e)),
                ),
                Vec::new(),
            );
        }
    };
    let conn = snapshot.conn();
    let result = (|| -> rusqlite::Result<Result<DbRead, (String, LocalizedText)>> {
        let version = sqlite::user_version(conn)?;
        let format = format!("AI Toolbox database, schema {version}");
        let cols = sqlite::columns(conn, "codex_provider")?;
        if !(cols.iter().any(|c| c == "id") && cols.iter().any(|c| c == "data")) {
            let reason = if version < MIN_SCHEMA {
                text(
                    "import.store.not_initialised",
                    "It has no providers yet (schema {version}); open {tool} once.",
                )
                .arg("version", version)
                .arg("tool", TOOL)
            } else {
                text(
                    "import.store.unknown_layout",
                    "Its layout (schema {version}) is not one Superba knows; nothing was read.",
                )
                .arg("version", version)
            };
            return Ok(Err((format, reason)));
        }
        let note = (version > KNOWN_SCHEMA).then(|| {
            text(
                "import.store.newer_schema",
                "Schema {version} is newer than the newest Superba knows ({known}); its provider table is unchanged, so it was read.",
            )
            .arg("version", version)
            .arg("known", KNOWN_SCHEMA)
        });
        let common = if sqlite::has_table(conn, "codex_common_config")? {
            rows(conn, "codex_common_config")?
                .into_iter()
                .find(|(id, _)| id == "common")
                .and_then(|(_, v)| v)
                .and_then(|v| v.get("config").and_then(Value::as_str).map(str::to_owned))
                .and_then(|t| codex::parse_table(&t).ok())
                .unwrap_or_default()
        } else {
            toml::Table::new()
        };
        let mut providers: Vec<ForeignProvider> = rows(conn, "codex_provider")?
            .into_iter()
            .map(|(id, data)| codex_provider(&id, data, &common))
            .collect();
        if sqlite::has_table(conn, "codex_official_account")? {
            for (id, data) in rows(conn, "codex_official_account")? {
                let data = data.unwrap_or(Value::Null);
                let name = str_at(&data, "name")
                    .or_else(|| str_at(&data, "email"))
                    .unwrap_or(&id)
                    .to_owned();
                let mut p = ForeignProvider::new(
                    ImportSource::AiToolbox,
                    format!("account/{id}"),
                    name,
                    ForeignApp::Codex,
                );
                p.current = data.get("is_applied").and_then(Value::as_bool) == Some(true);
                p.blocked = Some(text(
                    "import.reason.chatgpt_account",
                    "A ChatGPT account signed in through {tool}: Superba has the official login built in (Official ChatGPT); sign in there.",
                ).arg("tool", TOOL));
                p.official = true;
                providers.push(p);
            }
        }
        for (table, app) in OTHER_TABLES {
            if !sqlite::has_table(conn, table)? {
                continue;
            }
            let app = match *app {
                "claude" => ForeignApp::Claude,
                "gemini" => ForeignApp::Gemini,
                other => ForeignApp::Other(other.to_owned()),
            };
            for (id, data) in rows(conn, table)? {
                let name = data
                    .as_ref()
                    .and_then(|d| str_at(d, "name"))
                    .unwrap_or(&id)
                    .to_owned();
                let mut p = ForeignProvider::new(
                    ImportSource::AiToolbox,
                    format!("{table}/{id}"),
                    name,
                    app.clone(),
                );
                p.current = data
                    .as_ref()
                    .and_then(|d| d.get("is_applied"))
                    .and_then(Value::as_bool)
                    == Some(true);
                providers.push(p);
            }
        }
        Ok(Ok((format, note, providers)))
    })();
    match result {
        Ok(Ok((format, note, providers))) => {
            (env.store(db, format, StoreStatus::Read, note), providers)
        }
        Ok(Err((format, reason))) => (
            env.store(db, format, StoreStatus::Skipped, Some(reason)),
            Vec::new(),
        ),
        Err(e) => (
            env.store(
                db,
                "AI Toolbox database",
                StoreStatus::Skipped,
                Some(unreadable(e)),
            ),
            Vec::new(),
        ),
    }
}

fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Keys AI Toolbox never stores for a provider (protected Codex sections).
const PROTECTED: &[&str] = &["mcp_servers", "plugins", "projects"];

/// One `codex_provider` row.
fn codex_provider(id: &str, data: Option<Value>, common: &toml::Table) -> ForeignProvider {
    let data = data.unwrap_or(Value::Null);
    let name = str_at(&data, "name").unwrap_or(id).to_owned();
    let mut p = ForeignProvider::new(
        ImportSource::AiToolbox,
        format!("codex/{id}"),
        name,
        ForeignApp::Codex,
    );
    if !data.is_object() {
        p.blocked = Some(
            text(
                "import.reason.unreadable_settings",
                "Its settings in {tool} could not be read.",
            )
            .arg("tool", TOOL),
        );
        return p;
    }
    p.current = data.get("is_applied").and_then(Value::as_bool) == Some(true);
    let disabled = data
        .get("is_disabled")
        .or_else(|| data.get("isDisabled"))
        .and_then(Value::as_bool)
        == Some(true);
    // `settings_config` is a JSON string inside the document (an object is
    // accepted too).
    let settings = match data.get("settings_config") {
        Some(Value::String(s)) if s.trim().is_empty() => Value::Object(Default::default()),
        Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
            Ok(v) if v.is_object() => v,
            _ => {
                p.blocked = Some(
                    text(
                        "import.reason.unreadable_settings",
                        "Its settings in {tool} could not be read.",
                    )
                    .arg("tool", TOOL),
                );
                return p;
            }
        },
        Some(v @ Value::Object(_)) => v.clone(),
        _ => Value::Object(Default::default()),
    };
    let meta = data.get("meta").cloned().unwrap_or(Value::Null);
    let config_text = settings.get("config").and_then(Value::as_str).unwrap_or("");
    let own = if config_text.trim().is_empty() {
        toml::Table::new()
    } else {
        match codex::parse_table(config_text) {
            Ok(t) => t,
            Err(e) => {
                p.blocked = Some(
                    text(
                        "import.reason.bad_toml",
                        "Its config.toml part could not be read: {error}",
                    )
                    .arg("error", e),
                );
                return p;
            }
        }
    };
    // The shared TOML first, the provider's own on top.
    let mut merged = common.clone();
    for key in PROTECTED {
        merged.remove(*key);
    }
    codex::merge(&mut merged, &own);
    let toml = codex::read_toml(&merged);
    let ignored = codex::read_toml(&own).ignored_list();
    let auth = codex::parse_auth(settings.get("auth"), toml.env_key());
    let mut key = auth.key;
    if key == ForeignKey::Missing {
        key = codex::key_value(toml.bearer_token());
    }
    if key == ForeignKey::Missing && codex::keychain_ref_in(Some(&settings)) {
        key = ForeignKey::Keychain;
    }
    if key == ForeignKey::Missing
        && let Some(var) = toml.env_key()
    {
        key = ForeignKey::Env(var.to_owned());
    }
    let base = toml
        .active
        .as_ref()
        .and_then(|t| t.base_url.clone())
        .or_else(|| toml.root_base_url.clone())
        .or_else(|| {
            toml.first_with_base_url
                .as_ref()
                .and_then(|t| t.base_url.clone())
        })
        .or_else(|| toml.openai_base_url.clone());
    let category = str_at(&data, "category").unwrap_or("custom");
    // AI Toolbox reports a provider without a key and without a base URL as
    // its official one.
    if category == "official"
        || (key == ForeignKey::Missing && base.is_none() && !auth.chatgpt_login)
        || (auth.chatgpt_login && key == ForeignKey::Missing)
    {
        p.official = true;
        return p;
    }
    p.model = toml.chat_model.clone().or_else(|| toml.model.clone());
    p.reasoning_effort = toml.reasoning_effort.clone();
    p.key = key;
    if let Some(b) = &base
        && b.trim_end().ends_with("##")
    {
        p.base_url = base.clone();
        p.blocked = Some(
            text(
                "import.reason.full_url",
                "Its address is a full endpoint URL that only {tool}'s local gateway understands.",
            )
            .arg("tool", TOOL),
        );
        return p;
    }
    let declared = str_at(&meta, "apiFormat")
        .or_else(|| str_at(&meta, "api_format"))
        .map(str::to_owned)
        .or_else(|| toml.wire_api().map(str::to_owned));
    let protocol = declared
        .as_deref()
        .and_then(codex::protocol)
        .unwrap_or_else(|| {
            if base
                .as_deref()
                .is_some_and(|b| b.trim_end_matches('/').ends_with("/chat/completions"))
            {
                Protocol::Chat
            } else {
                Protocol::Responses
            }
        });
    p.base_url = base;
    match protocol {
        Protocol::Chat => p.wire_api = Some("chat".into()),
        Protocol::Anthropic => p.blocked = Some(gateway_only("Anthropic Messages")),
        Protocol::Gemini => p.blocked = Some(gateway_only("Gemini")),
        Protocol::Responses => p.wire_api = Some("responses".into()),
    }
    for rule in meta
        .get("modelRewrites")
        .or_else(|| meta.get("model_rewrites"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let (Some(from), Some(to)) = (str_at(rule, "from"), str_at(rule, "to"))
            && from != to
        {
            p.model_map.insert(from.to_owned(), to.to_owned());
        }
    }
    if disabled {
        p.deselect = true;
        p.notes
            .push(text("import.note.disabled", "It is disabled in {tool}.").arg("tool", TOOL));
    }
    if let Some(keys) = ignored {
        p.notes.push(
            text(
                "import.note.ignored_settings",
                "Not imported from its config.toml: {keys}.",
            )
            .arg("keys", keys),
        );
    }
    p
}

fn gateway_only(protocol: &str) -> LocalizedText {
    text(
        "import.reason.gateway_protocol",
        "It talks to a {protocol} API through {tool}'s local proxy; Codex needs a Responses API.",
    )
    .arg("protocol", protocol)
    .arg("tool", TOOL)
}
