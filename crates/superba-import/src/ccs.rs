//! cc-switch (`farion1231/cc-switch`).
//!
//! Where it keeps its data (every OS): `<home>/.cc-switch/`, or the directory
//! named by `app_config_dir_override` in the Tauri store `app_paths.json`
//! (`<app data>/com.ccswitch.desktop/`, used only when that directory exists);
//! on Windows also `$HOME\.cc-switch` when the profile directory has no
//! database but that one does. `CC_SWITCH_TEST_HOME` replaces the home
//! directory, as in cc-switch itself.
//!
//! Formats, newest first:
//!
//! - `cc-switch.db` (v3.8.0 and later): SQLite, rollback journal,
//!   `PRAGMA user_version` 1 to 20 (v4.0.15). `providers(id, app_type, name,
//!   settings_config, category, meta, is_current, sort_index, created_at, ...)`;
//!   alternative addresses in `provider_endpoints`; the current provider in
//!   `<home>/.cc-switch/settings.json` (`currentProviderCodex`), else
//!   `is_current`. A `config.json` next to it is left over from before the
//!   move to SQLite and is not read.
//! - `config.json` v2 (v3.1.0 to v3.7.1): `{"version": 2, "codex": {"providers":
//!   {...}, "current": id}, "claude": ..., "gemini": ...}`; alternative
//!   addresses in `meta.custom_endpoints`.
//! - `config.json` v1 (up to v3.0.x): Claude Code providers only.
//!
//! A Codex provider's `settings_config` is `{"auth": {"OPENAI_API_KEY": ...},
//! "config": "<config.toml text>"}`; keys are plain text (cc-switch keeps no
//! provider key in the OS keychain).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde_json::Value;
use superba_core::{ForeignApp, ImportSource, ImportStore, LocalizedText, StoreStatus};

use crate::codex::{self, Protocol};
use crate::foreign::{ForeignKey, ForeignProvider, SourceRead};
use crate::{Os, ScanEnv, sqlite, text};

/// Tauri bundle identifier (the directory of `app_paths.json`).
pub const APP_ID: &str = "com.ccswitch.desktop";
/// Newest database schema (`PRAGMA user_version`) Superba was written against.
pub const KNOWN_SCHEMA: i64 = 20;
/// Environment variable cc-switch reads as its home directory (tests, debugging).
pub const ENV_TEST_HOME: &str = "CC_SWITCH_TEST_HOME";

const TOOL: &str = "CC Switch";

/// cc-switch's home directory.
fn home(env: &ScanEnv<'_>) -> PathBuf {
    env.env
        .var_non_empty(ENV_TEST_HOME)
        .map(PathBuf::from)
        .unwrap_or_else(|| env.home.to_path_buf())
}

/// Expand `~`, `~/x`, `~\x`.
pub(crate) fn expand_tilde(path: &str, home: &Path) -> PathBuf {
    let p = path.trim();
    if p == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        return home.join(rest);
    }
    PathBuf::from(p)
}

/// The data directory override of `app_paths.json`, when set and existing.
fn data_dir_override(env: &ScanEnv<'_>, home: &Path) -> Option<PathBuf> {
    let file = env.app_data_dir().join(APP_ID).join("app_paths.json");
    let text = std::fs::read_to_string(file).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let raw = value.get("app_config_dir_override")?.as_str()?;
    if raw.trim().is_empty() {
        return None;
    }
    let dir = expand_tilde(raw, home);
    dir.is_dir().then_some(dir)
}

/// The data directory cc-switch uses.
pub fn data_dir(env: &ScanEnv<'_>) -> PathBuf {
    let home = home(env);
    if let Some(dir) = data_dir_override(env, &home) {
        return dir;
    }
    let default = home.join(".cc-switch");
    if env.os == Os::Windows
        && !default.join("cc-switch.db").is_file()
        && let Some(h) = env.env.var_non_empty("HOME")
    {
        let alt = PathBuf::from(h).join(".cc-switch");
        if alt.join("cc-switch.db").is_file() {
            return alt;
        }
    }
    default
}

/// Read cc-switch's stores.
pub fn read(env: &ScanEnv<'_>) -> SourceRead {
    let home = home(env);
    let dir = data_dir(env);
    let db = dir.join("cc-switch.db");
    let json = dir.join("config.json");
    let settings = home.join(".cc-switch").join("settings.json");
    let mut stores = Vec::new();
    let mut providers = Vec::new();
    if db.is_file() {
        let (store, found) = read_db(env, &db, &settings);
        stores.push(store);
        providers = found;
        if json.is_file() {
            stores.push(env.store(
                &json,
                "cc-switch config.json",
                StoreStatus::Superseded,
                Some(text(
                    "import.store.ccs_json_superseded",
                    "Left over from before CC Switch moved to its database; not read.",
                )),
            ));
        }
        let default_db = home.join(".cc-switch").join("cc-switch.db");
        if default_db != db && default_db.is_file() {
            stores.push(
                env.store(
                    &default_db,
                    "cc-switch database",
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
    } else if json.is_file() {
        let (store, found) = read_json(env, &json);
        stores.push(store);
        providers = found;
    } else {
        stores.push(env.store(&db, "cc-switch database", StoreStatus::Missing, None));
    }
    SourceRead {
        source: ImportSource::Ccs,
        stores,
        providers,
    }
}

fn app_of(app_type: &str) -> ForeignApp {
    match app_type {
        "codex" => ForeignApp::Codex,
        "claude" => ForeignApp::Claude,
        "gemini" => ForeignApp::Gemini,
        "claude-desktop" => ForeignApp::Other("Claude Desktop".into()),
        "grokbuild" => ForeignApp::Other("Grok Build".into()),
        "opencode" => ForeignApp::Other("OpenCode".into()),
        "openclaw" => ForeignApp::Other("OpenClaw".into()),
        "hermes" => ForeignApp::Other("Hermes".into()),
        "pi" => ForeignApp::Other("Pi".into()),
        other => ForeignApp::Other(other.to_owned()),
    }
}

/// One provider row, from either format.
#[derive(Debug, Default)]
struct Row {
    id: String,
    app_type: String,
    name: String,
    settings: Value,
    category: Option<String>,
    meta: Value,
    current: bool,
    extra_urls: Vec<String>,
}

fn skipped(env: &ScanEnv<'_>, path: &Path, format: String, reason: LocalizedText) -> ImportStore {
    env.store(path, format, StoreStatus::Skipped, Some(reason))
}

/// What a readable database gave: (format, store note, rows).
type DbRows = (String, Option<LocalizedText>, Vec<Row>);
/// Why a database was not read: (format, reason).
type Skip = (String, LocalizedText);

fn read_db(env: &ScanEnv<'_>, db: &Path, settings: &Path) -> (ImportStore, Vec<ForeignProvider>) {
    let format = "cc-switch database".to_owned();
    let snapshot = match sqlite::open(db, env.scratch) {
        Ok(s) => s,
        Err(e) => {
            return (
                skipped(
                    env,
                    db,
                    format,
                    text(
                        "import.store.unreadable",
                        "It could not be read: {error}. Close {tool} and try again.",
                    )
                    .arg("error", e)
                    .arg("tool", TOOL),
                ),
                Vec::new(),
            );
        }
    };
    let conn = snapshot.conn();
    let result = (|| -> rusqlite::Result<Result<DbRows, Skip>> {
        let version = sqlite::user_version(conn)?;
        let format = format!("cc-switch database, schema {version}");
        let cols = sqlite::columns(conn, "providers")?;
        let has = |c: &str| cols.iter().any(|x| x == c);
        if !["id", "app_type", "name", "settings_config"]
            .iter()
            .all(|c| has(c))
        {
            return Ok(Err((
                format,
                text(
                    "import.store.unknown_layout",
                    "Its layout (schema {version}) is not one Superba knows; nothing was read.",
                )
                .arg("version", version),
            )));
        }
        let note = (version > KNOWN_SCHEMA).then(|| {
            text(
                "import.store.newer_schema",
                "Schema {version} is newer than the newest Superba knows ({known}); its provider table is unchanged, so it was read.",
            )
            .arg("version", version)
            .arg("known", KNOWN_SCHEMA)
        });
        let opt = |c: &str, fallback: &str| {
            if has(c) {
                c.to_owned()
            } else {
                fallback.to_owned()
            }
        };
        let sql = format!(
            "SELECT id, app_type, name, settings_config, {cat}, {meta}, {cur} \
             FROM providers ORDER BY app_type, COALESCE({sort}, 999999), {created}, id",
            cat = opt("category", "NULL"),
            meta = opt("meta", "'{}'"),
            cur = opt("is_current", "0"),
            sort = opt("sort_index", "NULL"),
            created = opt("created_at", "NULL"),
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows: Vec<Row> = stmt
            .query_map([], |r| {
                let settings: Option<String> = r.get(3)?;
                let meta: Option<String> = r.get(5)?;
                Ok(Row {
                    id: r.get(0)?,
                    app_type: r.get(1)?,
                    name: r.get(2)?,
                    settings: settings
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or(Value::Null),
                    category: r.get(4)?,
                    meta: meta
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_else(|| Value::Object(Default::default())),
                    current: r.get::<_, Option<i64>>(6)?.unwrap_or(0) != 0,
                    extra_urls: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let ecols = sqlite::columns(conn, "provider_endpoints")?;
        if ["provider_id", "app_type", "url"]
            .iter()
            .all(|c| ecols.iter().any(|x| x == c))
        {
            let order = if ecols.iter().any(|x| x == "added_at") {
                "added_at, url"
            } else {
                "url"
            };
            let mut stmt = conn.prepare(&format!(
                "SELECT provider_id, app_type, url FROM provider_endpoints ORDER BY {order}"
            ))?;
            let mut urls: HashMap<(String, String), Vec<String>> = HashMap::new();
            for row in stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })? {
                let (id, app, url) = row?;
                urls.entry((app, id)).or_default().push(url);
            }
            for row in &mut rows {
                if let Some(u) = urls.remove(&(row.app_type.clone(), row.id.clone())) {
                    row.extra_urls = u;
                }
            }
        }
        Ok(Ok((format, note, rows)))
    })();
    let (format, note, mut rows) = match result {
        Ok(Ok(v)) => v,
        Ok(Err((format, reason))) => return (skipped(env, db, format, reason), Vec::new()),
        Err(e) => {
            return (
                skipped(
                    env,
                    db,
                    format,
                    text(
                        "import.store.unreadable",
                        "It could not be read: {error}. Close {tool} and try again.",
                    )
                    .arg("error", e)
                    .arg("tool", TOOL),
                ),
                Vec::new(),
            );
        }
    };
    drop(snapshot);
    // The device file's pointer wins when it names a provider that exists.
    let pointers = device_pointers(settings);
    for app in rows
        .iter()
        .map(|r| r.app_type.clone())
        .collect::<std::collections::BTreeSet<_>>()
    {
        if let Some(id) = pointers.get(&app)
            && rows.iter().any(|r| r.app_type == app && &r.id == id)
        {
            for r in rows.iter_mut().filter(|r| r.app_type == app) {
                r.current = &r.id == id;
            }
        }
    }
    (
        env.store(db, format, StoreStatus::Read, note),
        providers_of(rows),
    )
}

/// `currentProvider<App>` of cc-switch's `settings.json`, by app type.
fn device_pointers(settings: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(value) = std::fs::read_to_string(settings)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    else {
        return out;
    };
    for (app, key) in [
        ("codex", "currentProviderCodex"),
        ("claude", "currentProviderClaude"),
        ("gemini", "currentProviderGemini"),
    ] {
        if let Some(id) = value
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            out.insert(app.to_owned(), id.to_owned());
        }
    }
    out
}

fn read_json(env: &ScanEnv<'_>, path: &Path) -> (ImportStore, Vec<ForeignProvider>) {
    let value: Value = match std::fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
    {
        Ok(v) => v,
        Err(e) => {
            return (
                skipped(
                    env,
                    path,
                    "cc-switch config.json".into(),
                    text("import.store.bad_json", "It is not valid JSON: {error}").arg("error", e),
                ),
                Vec::new(),
            );
        }
    };
    let version = value.get("version").and_then(Value::as_u64);
    let mut rows = Vec::new();
    let format = match version {
        Some(2) => {
            for app in ["codex", "claude", "gemini"] {
                let Some(manager) = value.get(app) else {
                    continue;
                };
                rows.extend(manager_rows(app, manager));
            }
            "cc-switch config.json v2".to_owned()
        }
        None if value.get("providers").is_some_and(Value::is_object) => {
            // v1: Claude Code only.
            rows.extend(manager_rows("claude", &value));
            "cc-switch config.json v1".to_owned()
        }
        other => {
            return (
                skipped(
                    env,
                    path,
                    "cc-switch config.json".into(),
                    text(
                        "import.store.unknown_version",
                        "Version {version} is not one Superba knows; nothing was read.",
                    )
                    .arg(
                        "version",
                        other.map_or_else(|| "?".to_owned(), |v| v.to_string()),
                    ),
                ),
                Vec::new(),
            );
        }
    };
    (
        env.store(path, format, StoreStatus::Read, None),
        providers_of(rows),
    )
}

/// The providers of one app section of `config.json` (`{providers, current}`),
/// in cc-switch's order: `sortIndex`, `createdAt`, id.
fn manager_rows(app: &str, manager: &Value) -> Vec<Row> {
    let current = manager.get("current").and_then(Value::as_str).unwrap_or("");
    let Some(map) = manager.get("providers").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut rows: Vec<(i64, i64, Row)> = map
        .iter()
        .map(|(key, p)| {
            let id = p
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or(key)
                .to_owned();
            let meta = p.get("meta").cloned().unwrap_or(Value::Null);
            let extra_urls = meta
                .get("custom_endpoints")
                .and_then(Value::as_object)
                .map(|m| {
                    let mut v: Vec<(i64, String)> = m
                        .iter()
                        .map(|(k, e)| {
                            let url = e.get("url").and_then(Value::as_str).unwrap_or(k);
                            let at = e.get("addedAt").and_then(Value::as_i64).unwrap_or(0);
                            (at, url.to_owned())
                        })
                        .collect();
                    v.sort();
                    v.into_iter().map(|(_, u)| u).collect()
                })
                .unwrap_or_default();
            let row = Row {
                current: id == current,
                name: p
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_owned(),
                id,
                app_type: app.to_owned(),
                settings: p.get("settingsConfig").cloned().unwrap_or(Value::Null),
                category: p.get("category").and_then(Value::as_str).map(str::to_owned),
                meta,
                extra_urls,
            };
            let sort = p
                .get("sortIndex")
                .and_then(Value::as_i64)
                .unwrap_or(999_999);
            let created = p.get("createdAt").and_then(Value::as_i64).unwrap_or(0);
            (sort, created, row)
        })
        .collect();
    rows.sort_by(|a, b| (a.0, a.1, &a.2.id).cmp(&(b.0, b.1, &b.2.id)));
    rows.into_iter().map(|(_, _, r)| r).collect()
}

/// Providers in cc-switch's order, Codex first.
fn providers_of(rows: Vec<Row>) -> Vec<ForeignProvider> {
    let mut out: Vec<ForeignProvider> = rows.into_iter().map(provider).collect();
    out.sort_by_key(|p| p.app != ForeignApp::Codex);
    out
}

fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn provider(row: Row) -> ForeignProvider {
    let app = app_of(&row.app_type);
    let mut p = ForeignProvider::new(
        ImportSource::Ccs,
        format!("{}/{}", row.app_type, row.id),
        row.name.trim(),
        app.clone(),
    );
    p.current = row.current;
    if app != ForeignApp::Codex {
        return p;
    }
    codex_provider(&mut p, row);
    p
}

/// Fill a Codex provider from its row (cc-switch's own extraction rules).
fn codex_provider(p: &mut ForeignProvider, row: Row) {
    let settings = row.settings;
    if !settings.is_object() {
        p.blocked = Some(
            text(
                "import.reason.unreadable_settings",
                "Its settings in {tool} could not be read.",
            )
            .arg("tool", TOOL),
        );
        return;
    }
    let meta = row.meta;
    match str_at(&meta, "providerType") {
        Some("github_copilot") => {
            p.blocked = Some(
                text(
                    "import.reason.managed_account",
                    "A {what} account that only works through {tool}'s local proxy.",
                )
                .arg("what", "GitHub Copilot")
                .arg("tool", TOOL),
            );
            return;
        }
        Some("xai_oauth") => {
            p.blocked = Some(
                text(
                    "import.reason.managed_account",
                    "A {what} account that only works through {tool}'s local proxy.",
                )
                .arg("what", "xAI")
                .arg("tool", TOOL),
            );
            return;
        }
        Some("codex_oauth") => {
            p.official = true;
            return;
        }
        _ => {}
    }
    if meta
        .get("authBinding")
        .and_then(|b| str_at(b, "source"))
        .is_some_and(|s| s == "managed_account")
    {
        p.official = true;
        return;
    }
    let config_text = settings.get("config").and_then(Value::as_str).unwrap_or("");
    let toml = if config_text.trim().is_empty() {
        codex::CodexToml::default()
    } else {
        match codex::parse_toml(config_text) {
            Ok(t) => t,
            Err(e) => {
                p.blocked = Some(
                    text(
                        "import.reason.bad_toml",
                        "Its config.toml part could not be read: {error}",
                    )
                    .arg("error", e),
                );
                return;
            }
        }
    };
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
        .or_else(|| toml.openai_base_url.clone());
    let has_upstream = base.is_some() && matches!(key, ForeignKey::Secret(_));
    let official_marked = row.category.as_deref() == Some("official") || row.id == "codex-official";
    if (official_marked || (auth.chatgpt_login && key == ForeignKey::Missing)) && !has_upstream {
        p.official = true;
        return;
    }
    p.base_url = base;
    p.extra_urls = row.extra_urls;
    p.model = toml.model.clone();
    p.reasoning_effort = toml.reasoning_effort.clone();
    p.key = key;
    let declared = str_at(&meta, "apiFormat")
        .or_else(|| str_at(&settings, "apiFormat"))
        .or_else(|| str_at(&settings, "api_format"))
        .map(str::to_owned)
        .or_else(|| toml.wire_api().map(str::to_owned));
    match declared.as_deref().and_then(codex::protocol) {
        Some(Protocol::Chat) => p.wire_api = Some("chat".into()),
        Some(Protocol::Anthropic) => {
            p.blocked = Some(gateway_only("Anthropic Messages"));
        }
        Some(Protocol::Gemini) => {
            p.blocked = Some(gateway_only("Gemini"));
        }
        _ => p.wire_api = Some("responses".into()),
    }
    if let Some(keys) = toml.ignored_list() {
        p.notes.push(
            text(
                "import.note.ignored_settings",
                "Not imported from its config.toml: {keys}.",
            )
            .arg("keys", keys),
        );
    }
}

fn gateway_only(protocol: &str) -> LocalizedText {
    text(
        "import.reason.gateway_protocol",
        "It talks to a {protocol} API through {tool}'s local proxy; Codex needs a Responses API.",
    )
    .arg("protocol", protocol)
    .arg("tool", TOOL)
}
