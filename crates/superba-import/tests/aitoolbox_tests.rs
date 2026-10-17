//! AI Toolbox: the released SQLite schemas (4, 8, 16, 22) and HEAD (29), a newer
//! and an unknown layout, the old SurrealDB store, where its data lives (default,
//! `app_paths.json` override, `AI_TOOLBOX_DATA_DIR`), the Codex fields (common
//! config merge, legacy bearer token, `env_key`, gateway-only protocols, model
//! rewrites, disabled rows, ChatGPT accounts), a key kept in the keychain, recent
//! commits that sit only in the WAL, and that nothing in its directory changes.

mod support;

use superba_core::{ForeignApp, ImportSource, StoreStatus};
use superba_import::{ForeignKey, ForeignProvider, Os, SourceRead};
use support::{Home, fixture, listing};

const DB: &str = ".local/share/com.ai-toolbox/ai-toolbox.db";

fn by_name<'a>(read: &'a SourceRead, name: &str) -> &'a ForeignProvider {
    read.providers
        .iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| {
            panic!(
                "{name} not in {:?}",
                read.providers.iter().map(|p| &p.name).collect::<Vec<_>>()
            )
        })
}

fn key(p: &ForeignProvider) -> Option<String> {
    p.key.secret().map(|s| s.expose().to_owned())
}

fn load(h: &Home, version: &str) -> SourceRead {
    h.db(
        DB,
        &fixture(&format!("ai-toolbox/schema-v{version}.sql")),
        true,
    );
    h.read(ImportSource::AiToolbox, Os::Linux)
}

#[test]
fn nothing_installed_reports_the_missing_database() {
    let h = Home::new();
    let read = h.read(ImportSource::AiToolbox, Os::Linux);
    assert_eq!(read.stores[0].status, StoreStatus::Missing);
    assert_eq!(
        read.stores[0].display_path,
        "~/.local/share/com.ai-toolbox/ai-toolbox.db"
    );
    // Per OS.
    let read = h.read(ImportSource::AiToolbox, Os::MacOs);
    assert_eq!(
        read.stores[0].display_path,
        "~/Library/Application Support/com.ai-toolbox/ai-toolbox.db"
    );
    let h = h.with_env("APPDATA", "C:\\Users\\alice\\AppData\\Roaming");
    let read = h.read(ImportSource::AiToolbox, Os::Windows);
    assert!(
        read.stores[0]
            .path
            .to_string_lossy()
            .starts_with("C:\\Users\\alice\\AppData\\Roaming"),
        "{:?}",
        read.stores[0].path
    );
}

#[test]
fn every_released_schema_resolves_the_same_providers() {
    for version in ["04", "08", "16", "22", "29"] {
        let h = Home::new();
        let read = load(&h, version);
        let store = &read.stores[0];
        assert_eq!(store.status, StoreStatus::Read, "{version}");
        assert_eq!(
            store.format,
            format!(
                "AI Toolbox database, schema {}",
                version.trim_start_matches('0')
            )
        );
        assert!(store.note.is_none(), "{version}");
        let official = by_name(&read, "默认配置");
        assert!(official.official, "{version}");
        let relay = by_name(&read, "Example Relay");
        assert!(relay.current, "{version}");
        assert_eq!(
            relay.base_url.as_deref(),
            Some("https://relay.example.com/v1")
        );
        assert_eq!(relay.model.as_deref(), Some("gpt-5.5"));
        assert_eq!(key(relay).as_deref(), Some("sk-test-relay-4f9c2a7e1b"));
        // The shared TOML supplies the reasoning effort the row omits.
        assert_eq!(relay.reasoning_effort.as_deref(), Some("high"), "{version}");
        let backup = by_name(&read, "备用中转 Backup");
        assert!(backup.deselect, "disabled in AI Toolbox");
        assert_eq!(backup.reasoning_effort.as_deref(), Some("xhigh"));
        let legacy = by_name(&read, "Manual TOML relay");
        assert_eq!(key(legacy).as_deref(), Some("sk-test-bearer-2b7d9e"));
        assert_eq!(
            legacy.base_url.as_deref(),
            Some("https://legacy-relay.example.org/v1")
        );
        let azure = by_name(&read, "Azure OpenAI (env key)");
        assert_eq!(azure.key, ForeignKey::Env("AZURE_OPENAI_API_KEY".into()));
        let mirror = by_name(&read, "Mirror (model from common config)");
        assert_eq!(mirror.model.as_deref(), Some("gpt-5.4"));
        // The ChatGPT account is listed, not importable.
        let account = by_name(&read, "alice@example.com");
        assert!(account.official && account.blocked.is_some());
        assert_eq!(
            read.providers
                .iter()
                .filter(|p| p.current && p.app == ForeignApp::Codex && !p.official)
                .count(),
            1
        );
    }
}

#[test]
fn head_schema_covers_gateway_rows_and_other_tools() {
    let h = Home::new();
    let read = load(&h, "29");
    let deepseek = by_name(&read, "DeepSeek");
    assert_eq!(
        deepseek.base_url.as_deref(),
        Some("https://api.deepseek.com")
    );
    assert_eq!(deepseek.wire_api.as_deref(), Some("responses"));
    assert!(deepseek.blocked.is_none());
    let gateway = by_name(&read, "Claude via Codex (gateway)");
    assert_eq!(
        gateway.blocked.as_ref().unwrap().key,
        "import.reason.gateway_protocol"
    );
    let aihub = by_name(&read, "AiHub (from CC Switch)");
    assert_eq!(key(aihub).as_deref(), Some("sk-test-aihub-7f3b1d"));
    assert_eq!(aihub.reasoning_effort.as_deref(), Some("medium"));
    assert_eq!(by_name(&read, "Claude Relay").app, ForeignApp::Claude);
    assert_eq!(by_name(&read, "Gemini Relay").app, ForeignApp::Gemini);
    // Deleted providers kept in the shared library are not offered.
    assert!(
        read.providers
            .iter()
            .all(|p| !p.name.starts_with("Old Relay"))
    );
}

#[test]
fn model_rewrites_become_the_model_map_and_keychain_references_are_never_read() {
    let h = Home::new();
    let mut sql = fixture("ai-toolbox/schema-v29.sql");
    sql.push_str(
        r#"
INSERT INTO codex_provider (id, data, created_at, updated_at) VALUES ('kc01', jsonb('{"name":"Keychain Relay","category":"custom","settings_config":"{\"auth\":{},\"credential_ref\":\"keyring:ai-toolbox-codex-kc01\",\"config\":\"model = \\\"gpt-5.5\\\"\\nmodel_provider = \\\"custom\\\"\\n\\n[model_providers.custom]\\nname = \\\"Keychain\\\"\\nbase_url = \\\"https://keychain-relay.example.com/v1\\\"\\nwire_api = \\\"responses\\\"\"}","sort_index":20,"is_applied":false,"is_disabled":false}'), 'x', 'x');
INSERT INTO codex_provider (id, data, created_at, updated_at) VALUES ('kc02', jsonb('{"name":"Keychain Key Field","category":"custom","settings_config":"{\"auth\":{\"OPENAI_API_KEY\":\"keyring:ai-toolbox-codex-kc02\"},\"config\":\"model_provider = \\\"custom\\\"\\n\\n[model_providers.custom]\\nbase_url = \\\"https://keychain-relay2.example.com/v1\\\"\"}","sort_index":21,"is_applied":false,"is_disabled":false}'), 'x', 'x');
INSERT INTO codex_provider (id, data, created_at, updated_at) VALUES ('mr01', jsonb('{"name":"Rewrites","category":"custom","settings_config":"{\"auth\":{\"OPENAI_API_KEY\":\"sk-test-rewrites\"},\"config\":\"model = \\\"gpt-5.5\\\"\\nmodel_provider = \\\"custom\\\"\\n\\n[model_providers.custom]\\nbase_url = \\\"https://rewrites.example.com/v1\\\"\"}","meta":{"modelRewrites":[{"from":"gpt-5.5","to":"vendor/gpt-5.5"},{"from":" ","to":"x"}]},"sort_index":22,"is_applied":false,"is_disabled":false}'), 'x', 'x');
INSERT INTO codex_provider (id, data, created_at, updated_at) VALUES ('ch01', jsonb('{"name":"Chat Relay","category":"custom","settings_config":"{\"auth\":{\"OPENAI_API_KEY\":\"sk-test-chat\"},\"config\":\"model_provider = \\\"custom\\\"\\n\\n[model_providers.custom]\\nbase_url = \\\"https://chat.example.com/v1/chat/completions\\\"\"}","sort_index":23,"is_applied":false,"is_disabled":false}'), 'x', 'x');
INSERT INTO codex_provider (id, data, created_at, updated_at) VALUES ('fu01', jsonb('{"name":"Full URL","category":"custom","settings_config":"{\"auth\":{\"OPENAI_API_KEY\":\"sk-test-full\"},\"config\":\"model_provider = \\\"custom\\\"\\n\\n[model_providers.custom]\\nbase_url = \\\"https://full.example.com/v1/responses##\\\"\"}","sort_index":24,"is_applied":false,"is_disabled":false}'), 'x', 'x');
INSERT INTO codex_provider (id, data, created_at, updated_at) VALUES ('bad1', jsonb('{"name":"Broken TOML","category":"custom","settings_config":"{\"auth\":{\"OPENAI_API_KEY\":\"sk-test-broken\"},\"config\":\"model_provider = [\"}","sort_index":25}'), 'x', 'x');
"#,
    );
    h.db(DB, &sql, true);
    let read = h.read(ImportSource::AiToolbox, Os::Linux);
    assert_eq!(by_name(&read, "Keychain Relay").key, ForeignKey::Keychain);
    assert_eq!(
        by_name(&read, "Keychain Key Field").key,
        ForeignKey::Keychain
    );
    let rewrites = by_name(&read, "Rewrites");
    assert_eq!(
        rewrites.model_map.get("gpt-5.5").map(String::as_str),
        Some("vendor/gpt-5.5")
    );
    assert_eq!(rewrites.model_map.len(), 1);
    assert_eq!(
        by_name(&read, "Chat Relay").wire_api.as_deref(),
        Some("chat")
    );
    assert_eq!(
        by_name(&read, "Full URL").blocked.as_ref().unwrap().key,
        "import.reason.full_url"
    );
    assert_eq!(
        by_name(&read, "Broken TOML").blocked.as_ref().unwrap().key,
        "import.reason.bad_toml"
    );
}

#[test]
fn newer_and_unknown_schemas() {
    let h = Home::new();
    let sql = fixture("ai-toolbox/schema-v29.sql")
        .replace("PRAGMA user_version = 29;", "PRAGMA user_version = 31;");
    h.db(DB, &sql, true);
    let read = h.read(ImportSource::AiToolbox, Os::Linux);
    assert_eq!(read.stores[0].status, StoreStatus::Read);
    assert_eq!(
        read.stores[0].note.as_ref().unwrap().key,
        "import.store.newer_schema"
    );
    by_name(&read, "Example Relay");

    let h = Home::new();
    h.db(
        DB,
        "CREATE TABLE codex_provider (uid TEXT, blob BLOB); PRAGMA user_version = 99;",
        true,
    );
    let read = h.read(ImportSource::AiToolbox, Os::Linux);
    assert_eq!(read.stores[0].status, StoreStatus::Skipped);
    assert_eq!(
        read.stores[0].note.as_ref().unwrap().key,
        "import.store.unknown_layout"
    );

    let h = Home::new();
    h.db(
        DB,
        "CREATE TABLE settings (id TEXT PRIMARY KEY, data BLOB);",
        true,
    );
    let read = h.read(ImportSource::AiToolbox, Os::Linux);
    assert_eq!(read.stores[0].status, StoreStatus::Skipped);
    assert_eq!(
        read.stores[0].note.as_ref().unwrap().key,
        "import.store.not_initialised"
    );
}

#[test]
fn the_old_surrealdb_store_is_skipped_with_how_to_convert_it() {
    let h = Home::new();
    h.write(".local/share/com.ai-toolbox/database/000001.vlog", "opaque");
    h.write(".local/share/com.ai-toolbox/database/.DS_Store", "");
    let read = h.read(ImportSource::AiToolbox, Os::Linux);
    assert_eq!(read.stores[0].status, StoreStatus::Skipped);
    assert_eq!(
        read.stores[0].note.as_ref().unwrap().key,
        "import.store.surrealdb"
    );
    // An empty leftover directory (only ignorable entries) is just "missing".
    let h = Home::new();
    h.write(".local/share/com.ai-toolbox/database/.DS_Store", "");
    let read = h.read(ImportSource::AiToolbox, Os::Linux);
    assert_eq!(read.stores[0].status, StoreStatus::Missing);
}

#[test]
fn the_data_directory_override_and_the_environment_variable() {
    let h = Home::new();
    h.write(
        ".local/share/com.ai-toolbox/app_paths.json",
        r#"{"data_dir_override": "~/ai-toolbox-data"}"#,
    );
    h.db(
        "ai-toolbox-data/ai-toolbox.db",
        &fixture("ai-toolbox/schema-v22.sql"),
        true,
    );
    // The copy left in the default directory is stale.
    h.db(DB, &fixture("ai-toolbox/schema-v04.sql"), true);
    let read = h.read(ImportSource::AiToolbox, Os::Linux);
    assert_eq!(
        read.stores[0].display_path,
        "~/ai-toolbox-data/ai-toolbox.db"
    );
    assert_eq!(read.stores[0].format, "AI Toolbox database, schema 22");
    assert_eq!(read.stores[1].status, StoreStatus::Superseded);

    // A reset override is ignored.
    h.write(
        ".local/share/com.ai-toolbox/app_paths.json",
        r#"{"data_dir_override": null}"#,
    );
    let read = h.read(ImportSource::AiToolbox, Os::Linux);
    assert_eq!(read.stores[0].format, "AI Toolbox database, schema 4");

    let other = h.tmp.path().join("isolated");
    let h = h.with_env("AI_TOOLBOX_DATA_DIR", other.as_os_str());
    std::fs::create_dir_all(&other).unwrap();
    let conn = rusqlite::Connection::open(other.join("ai-toolbox.db")).unwrap();
    conn.execute_batch(&fixture("ai-toolbox/schema-v16.sql"))
        .unwrap();
    drop(conn);
    let read = h.read(ImportSource::AiToolbox, Os::Linux);
    assert_eq!(read.stores[0].format, "AI Toolbox database, schema 16");
}

#[test]
fn commits_only_in_the_wal_are_read_from_a_private_copy_and_nothing_changes() {
    let h = Home::new();
    let db = h.db(DB, &fixture("ai-toolbox/schema-v29.sql"), true);
    // AI Toolbox running: a long-lived connection with an uncheckpointed commit.
    let app = rusqlite::Connection::open(&db).unwrap();
    app.execute_batch("PRAGMA wal_autocheckpoint = 0;").unwrap();
    app.execute(
        r#"INSERT INTO codex_provider (id, data, created_at, updated_at) VALUES ('new1', jsonb('{"name":"Fresh Relay","category":"custom","settings_config":"{\"auth\":{\"OPENAI_API_KEY\":\"sk-test-fresh\"},\"config\":\"model_provider = \\\"custom\\\"\\n\\n[model_providers.custom]\\nbase_url = \\\"https://fresh.example.com/v1\\\"\"}","sort_index":30,"is_applied":false}'), 'x', 'x')"#,
        [],
    )
    .unwrap();
    let wal = h.path(&format!("{DB}-wal"));
    assert!(std::fs::metadata(&wal).unwrap().len() > 0);
    let dir = h.path(".local/share/com.ai-toolbox");
    let before = listing(&dir);
    let read = h.read(ImportSource::AiToolbox, Os::Linux);
    assert_eq!(
        key(by_name(&read, "Fresh Relay")).as_deref(),
        Some("sk-test-fresh")
    );
    assert_eq!(listing(&dir), before, "the tool's files are untouched");
    assert_eq!(
        std::fs::read_dir(h.scratch()).unwrap().count(),
        0,
        "the private copy is removed"
    );
    drop(app);
}

#[test]
fn reading_a_closed_database_creates_no_side_files() {
    let h = Home::new();
    load(&h, "22");
    let dir = h.path(".local/share/com.ai-toolbox");
    let before = listing(&dir);
    let read = h.read(ImportSource::AiToolbox, Os::Linux);
    by_name(&read, "Example Relay");
    assert_eq!(listing(&dir), before);
    assert!(!h.path(&format!("{DB}-shm")).exists());
}
