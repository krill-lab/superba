//! cc-switch: every storage format and schema the research found (JSON v1, JSON
//! v2, SQLite schema 1, 20 fresh and 20 upgraded from 1, a newer schema, an
//! unknown layout), where its data lives (home, the `app_paths.json` override,
//! the Windows `$HOME` fallback), the current provider (device file, stale
//! pointer), Codex fields (official / managed / Copilot rows, chat-format rows,
//! a key in the TOML), and that nothing in its directory changes.

mod support;

use superba_core::{ForeignApp, ImportSource, StoreStatus};
use superba_import::{ForeignKey, ForeignProvider, Os, SourceRead};
use support::{Home, fixture, listing, native};

fn codex(read: &SourceRead) -> Vec<&ForeignProvider> {
    read.providers
        .iter()
        .filter(|p| p.app == ForeignApp::Codex)
        .collect()
}

fn by_name<'a>(read: &'a SourceRead, name: &str) -> &'a ForeignProvider {
    read.providers
        .iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("{name} not found in {:?}", names(read)))
}

fn names(read: &SourceRead) -> Vec<&str> {
    read.providers.iter().map(|p| p.name.as_str()).collect()
}

fn key(p: &ForeignProvider) -> Option<String> {
    p.key.secret().map(|s| s.expose().to_owned())
}

#[test]
fn nothing_installed_reports_the_missing_database() {
    let h = Home::new();
    let read = h.read(ImportSource::Ccs, Os::Linux);
    assert_eq!(read.stores.len(), 1);
    assert_eq!(read.stores[0].status, StoreStatus::Missing);
    assert_eq!(
        read.stores[0].display_path,
        native("~/.cc-switch/cc-switch.db")
    );
    assert!(read.providers.is_empty());
}

#[test]
fn json_v1_has_claude_providers_only() {
    let h = Home::new();
    h.write(".cc-switch/config.json", &fixture("ccs/config.v1.json"));
    let read = h.read(ImportSource::Ccs, Os::Linux);
    assert_eq!(read.stores[0].status, StoreStatus::Read);
    assert_eq!(read.stores[0].format, "cc-switch config.json v1");
    assert_eq!(names(&read), ["Claude Official", "Example Relay"]);
    assert!(read.providers.iter().all(|p| p.app == ForeignApp::Claude));
    assert!(by_name(&read, "Example Relay").current);
}

#[test]
fn json_v2_reads_codex_providers_with_their_endpoints() {
    let h = Home::new();
    h.write(".cc-switch/config.json", &fixture("ccs/config.v2.json"));
    let read = h.read(ImportSource::Ccs, Os::Linux);
    assert_eq!(read.stores[0].format, "cc-switch config.json v2");
    let codex = codex(&read);
    assert_eq!(
        codex.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        ["OpenAI Official", "PackyCode", "My Gateway"]
    );
    // The official login (a backfilled ChatGPT `tokens` blob) is not a key.
    assert!(by_name(&read, "OpenAI Official").official);
    let packy = by_name(&read, "PackyCode");
    assert!(packy.current && !packy.official);
    assert_eq!(
        packy.base_url.as_deref(),
        Some("https://codex-api.packycode.example/v1")
    );
    assert_eq!(packy.model.as_deref(), Some("gpt-5-codex"));
    assert_eq!(packy.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(
        key(packy).as_deref(),
        Some("sk-test-v2-codex-packy-000000000000")
    );
    assert_eq!(
        packy.extra_urls,
        [
            "https://codex-api-hk.packycode.example/v1",
            "https://codex-api-us.packycode.example/v1"
        ]
    );
    // `env_key` names a variable, but the key is stored too.
    let gw = by_name(&read, "My Gateway");
    assert_eq!(
        gw.base_url.as_deref(),
        Some("https://gw.example.org/openai/v1")
    );
    assert_eq!(
        key(gw).as_deref(),
        Some("sk-test-v2-codex-mygw-000000000000")
    );
    // Claude and Gemini rows are listed (not importable later).
    assert!(read.providers.iter().any(|p| p.app == ForeignApp::Claude));
    assert!(read.providers.iter().any(|p| p.app == ForeignApp::Gemini));
}

#[test]
fn json_with_an_unknown_version_is_skipped() {
    let h = Home::new();
    h.write(
        ".cc-switch/config.json",
        r#"{"version": 3, "codex": {"providers": {}}}"#,
    );
    let read = h.read(ImportSource::Ccs, Os::Linux);
    assert_eq!(read.stores[0].status, StoreStatus::Skipped);
    assert_eq!(
        read.stores[0].note.as_ref().unwrap().key,
        "import.store.unknown_version"
    );
    assert!(read.providers.is_empty());
}

fn v20(h: &Home) {
    h.db(
        ".cc-switch/cc-switch.db",
        &fixture("ccs/schema-v20.sql"),
        false,
    );
}

#[test]
fn schema_20_reads_every_codex_case() {
    let h = Home::new();
    v20(&h);
    let read = h.read(ImportSource::Ccs, Os::Linux);
    assert_eq!(read.stores[0].format, "cc-switch database, schema 20");
    assert_eq!(read.stores[0].status, StoreStatus::Read);
    assert!(read.stores[0].note.is_none());
    // Codex first, in cc-switch's order (sort index, creation time, id).
    assert_eq!(
        codex(&read)
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        [
            "OpenAI Official",
            "OpenAI Official (alice@example.com)",
            "Kimi",
            "FluxA Token Plan",
            "Bearer-in-TOML Relay",
            "PackyCode",
            "My NewAPI",
            "GitHub Copilot",
            // No sort index: last, as in cc-switch.
            "default"
        ]
    );
    assert_eq!(read.providers[0].app, ForeignApp::Codex);
    for official in [
        "default",
        "OpenAI Official",
        "OpenAI Official (alice@example.com)",
    ] {
        assert!(by_name(&read, official).official, "{official}");
    }
    let copilot = by_name(&read, "GitHub Copilot");
    assert_eq!(
        copilot.blocked.as_ref().unwrap().key,
        "import.reason.managed_account"
    );
    let kimi = by_name(&read, "Kimi");
    assert!(kimi.current, "is_current = 1 and no device file");
    assert_eq!(
        kimi.base_url.as_deref(),
        Some("https://api.moonshot.example/v1")
    );
    assert_eq!(
        kimi.extra_urls,
        [
            "https://api.moonshot.example/v1",
            "https://api-intl.moonshot.example/v1"
        ]
    );
    assert_eq!(kimi.wire_api.as_deref(), Some("responses"));
    // meta.apiFormat = openai_chat: the upstream is Chat Completions.
    assert_eq!(
        by_name(&read, "FluxA Token Plan").wire_api.as_deref(),
        Some("chat")
    );
    let bearer = by_name(&read, "Bearer-in-TOML Relay");
    assert_eq!(
        key(bearer).as_deref(),
        Some("sk-test-v20-bearer-000000000000000")
    );
    // The active table, not the first one.
    let packy = by_name(&read, "PackyCode");
    assert_eq!(
        packy.base_url.as_deref(),
        Some("https://codex-api.packycode.example/v1")
    );
    assert!(
        packy
            .notes
            .iter()
            .any(|n| n.key == "import.note.ignored_settings"
                && n.args.iter().any(|(_, v)| v.contains("mcp_servers")))
    );
    // Other apps are listed with their names.
    assert!(
        read.providers
            .iter()
            .any(|p| p.app == ForeignApp::Other("Grok Build".into()))
    );
}

#[test]
fn the_device_file_names_the_current_provider_and_a_stale_pointer_falls_back() {
    let h = Home::new();
    v20(&h);
    // The fixture's device file points at Kimi, which is also is_current.
    let mut settings: serde_json::Value =
        serde_json::from_str(&fixture("ccs/settings.json")).unwrap();
    settings["currentProviderCodex"] = "universal-codex-u-test-01".into();
    h.write(".cc-switch/settings.json", &settings.to_string());
    let read = h.read(ImportSource::Ccs, Os::Linux);
    assert!(by_name(&read, "My NewAPI").current);
    assert!(!by_name(&read, "Kimi").current);
    h.write(
        ".cc-switch/settings.json",
        &fixture("ccs/settings.stale-pointer.json"),
    );
    let read = h.read(ImportSource::Ccs, Os::Linux);
    assert!(by_name(&read, "Kimi").current);
    assert_eq!(codex(&read).iter().filter(|p| p.current).count(), 1);
}

#[test]
fn schema_1_and_an_upgraded_column_order_read_alike() {
    let h = Home::new();
    h.db(
        ".cc-switch/cc-switch.db",
        &fixture("ccs/schema-v1.sql"),
        false,
    );
    // The JSON file v3.8 renamed is not read; a leftover config.json is reported.
    h.write(".cc-switch/config.json", &fixture("ccs/config.v2.json"));
    let read = h.read(ImportSource::Ccs, Os::Linux);
    assert_eq!(read.stores[0].format, "cc-switch database, schema 1");
    assert_eq!(read.stores[1].status, StoreStatus::Superseded);
    let packy = by_name(&read, "PackyCode");
    assert!(packy.current);
    assert_eq!(packy.extra_urls.len(), 2);

    let h = Home::new();
    h.db(
        ".cc-switch/cc-switch.db",
        &fixture("ccs/schema-v20-upgraded.sql"),
        false,
    );
    let read = h.read(ImportSource::Ccs, Os::Linux);
    let relay = by_name(&read, "New Relay");
    assert!(relay.current);
    assert_eq!(
        key(relay).as_deref(),
        Some("sk-test-v20u-newrelay-00000000000000")
    );
    assert_eq!(by_name(&read, "PackyCode").extra_urls.len(), 1);
}

#[test]
fn a_newer_schema_is_read_when_the_provider_table_is_known_and_skipped_otherwise() {
    let h = Home::new();
    let sql = fixture("ccs/schema-v20.sql")
        .replace("PRAGMA user_version = 20;", "PRAGMA user_version = 21;");
    h.db(".cc-switch/cc-switch.db", &sql, false);
    let read = h.read(ImportSource::Ccs, Os::Linux);
    assert_eq!(read.stores[0].status, StoreStatus::Read);
    assert_eq!(
        read.stores[0].note.as_ref().unwrap().key,
        "import.store.newer_schema"
    );
    assert!(!codex(&read).is_empty());

    let h = Home::new();
    h.db(
        ".cc-switch/cc-switch.db",
        "CREATE TABLE providers (pid TEXT, payload BLOB); PRAGMA user_version = 99;",
        false,
    );
    let read = h.read(ImportSource::Ccs, Os::Linux);
    assert_eq!(read.stores[0].status, StoreStatus::Skipped);
    assert_eq!(read.stores[0].format, "cc-switch database, schema 99");
    assert_eq!(
        read.stores[0].note.as_ref().unwrap().key,
        "import.store.unknown_layout"
    );
    assert!(read.providers.is_empty());
}

#[test]
fn a_damaged_file_is_skipped_with_a_reason() {
    let h = Home::new();
    h.write(".cc-switch/cc-switch.db", "not a database");
    let read = h.read(ImportSource::Ccs, Os::Linux);
    assert_eq!(read.stores[0].status, StoreStatus::Skipped);
    assert_eq!(
        read.stores[0].note.as_ref().unwrap().key,
        "import.store.unreadable"
    );
}

#[test]
fn the_app_paths_override_moves_the_data_directory() {
    let h = Home::new();
    // Override (with ~) in the Tauri app data directory; the old default copy
    // is reported, not read.
    h.write(
        ".local/share/com.ccswitch.desktop/app_paths.json",
        r#"{"app_config_dir_override": "~/Dropbox/cc-switch"}"#,
    );
    h.db(
        "Dropbox/cc-switch/cc-switch.db",
        &fixture("ccs/schema-v20.sql"),
        false,
    );
    h.db(
        ".cc-switch/cc-switch.db",
        &fixture("ccs/schema-v1.sql"),
        false,
    );
    let read = h.read(ImportSource::Ccs, Os::Linux);
    assert_eq!(
        read.stores[0].display_path,
        native("~/Dropbox/cc-switch/cc-switch.db")
    );
    assert_eq!(read.stores[1].status, StoreStatus::Superseded);
    assert_eq!(
        read.stores[1].note.as_ref().unwrap().key,
        "import.store.moved"
    );
    by_name(&read, "Kimi");

    // An override to a directory that does not exist is ignored (as cc-switch does).
    let h = Home::new();
    h.write(
        ".local/share/com.ccswitch.desktop/app_paths.json",
        r#"{"app_config_dir_override": "/nonexistent/cc-switch"}"#,
    );
    v20(&h);
    let read = h.read(ImportSource::Ccs, Os::Linux);
    assert_eq!(
        read.stores[0].display_path,
        native("~/.cc-switch/cc-switch.db")
    );

    // macOS keeps the store file under Application Support.
    let h = Home::new();
    h.write(
        "Library/Application Support/com.ccswitch.desktop/app_paths.json",
        r#"{"app_config_dir_override": "~/Sync"}"#,
    );
    h.db("Sync/cc-switch.db", &fixture("ccs/schema-v20.sql"), false);
    let read = h.read(ImportSource::Ccs, Os::MacOs);
    assert_eq!(read.stores[0].display_path, native("~/Sync/cc-switch.db"));
}

#[test]
fn windows_falls_back_to_home_when_the_profile_has_no_database() {
    let h = Home::new();
    let git_home = h.tmp.path().join("githome");
    let h = h.with_env("HOME", git_home.as_os_str());
    std::fs::create_dir_all(git_home.join(".cc-switch")).unwrap();
    let conn = rusqlite::Connection::open(git_home.join(".cc-switch/cc-switch.db")).unwrap();
    conn.execute_batch(&fixture("ccs/schema-v20.sql")).unwrap();
    drop(conn);
    let read = h.read(ImportSource::Ccs, Os::Windows);
    assert_eq!(read.stores[0].status, StoreStatus::Read);
    assert!(read.stores[0].path.starts_with(&git_home));
    // Not on Linux, and not when the profile directory has a database.
    assert_eq!(
        h.read(ImportSource::Ccs, Os::Linux).stores[0].status,
        StoreStatus::Missing
    );
}

#[test]
fn cc_switch_test_home_replaces_the_home_directory() {
    let h = Home::new();
    let other = h.tmp.path().join("other");
    let h = h.with_env("CC_SWITCH_TEST_HOME", other.as_os_str());
    std::fs::create_dir_all(other.join(".cc-switch")).unwrap();
    std::fs::write(
        other.join(".cc-switch/config.json"),
        fixture("ccs/config.v2.json"),
    )
    .unwrap();
    let read = h.read(ImportSource::Ccs, Os::Linux);
    assert_eq!(read.stores[0].status, StoreStatus::Read);
    by_name(&read, "PackyCode");
}

#[test]
fn reading_changes_nothing_in_the_tools_directory() {
    let h = Home::new();
    v20(&h);
    h.write(".cc-switch/settings.json", &fixture("ccs/settings.json"));
    let before = listing(&h.path(".cc-switch"));
    let read = h.read(ImportSource::Ccs, Os::Linux);
    assert!(!read.providers.is_empty());
    assert_eq!(listing(&h.path(".cc-switch")), before);
    // A hot journal (a crash mid-write) is read from a private copy, and the
    // copy is gone afterwards.
    std::fs::write(h.path(".cc-switch/cc-switch.db-journal"), b"").unwrap();
    std::fs::write(h.path(".cc-switch/cc-switch.db-journal"), vec![0u8; 512]).unwrap();
    let before = listing(&h.path(".cc-switch"));
    let read = h.read(ImportSource::Ccs, Os::Linux);
    assert!(!read.providers.is_empty(), "{:?}", read.stores);
    assert_eq!(listing(&h.path(".cc-switch")), before);
    assert!(
        std::fs::read_dir(h.scratch())
            .map(|d| d.count())
            .unwrap_or(0)
            == 0
    );
}

#[test]
fn keys_stay_out_of_debug_output() {
    let h = Home::new();
    v20(&h);
    let read = h.read(ImportSource::Ccs, Os::Linux);
    let debug = format!("{read:?}");
    assert!(!debug.contains("sk-test-v20-kimi"), "{debug}");
    assert!(matches!(by_name(&read, "Kimi").key, ForeignKey::Secret(_)));
}
