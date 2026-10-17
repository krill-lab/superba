//! Materialise the fixture stores into a home directory, where each tool keeps
//! them, for screenshots and manual checks (not a test of its own):
//!
//! ```sh
//! SUPERBA_IMPORT_FIXTURE_HOME=/path/to/home \
//!     cargo test -p superba-import --test fixture_homes -- --ignored
//! ```
//!
//! cc-switch: `~/.cc-switch/cc-switch.db` (schema 20) and `settings.json`, plus a
//! Krill provider on two lines and one named like the screenshot home's Krill
//! profile. AI Toolbox: `~/.local/share/com.ai-toolbox/ai-toolbox.db` (schema
//! 29, WAL), plus a provider whose key AI Toolbox would keep in the keychain.
//! All keys are fake.

mod support;

use std::path::Path;

use support::fixture;

const CCS_EXTRA: &str = r#"
INSERT INTO providers (id, app_type, name, settings_config, website_url, category, created_at, sort_index, notes, icon, icon_color, meta, is_current, in_failover_queue) VALUES
('a1b2c3d4-1111-4a2b-9c00-000000000001', 'codex', 'Krill 海外',
 '{"auth":{"OPENAI_API_KEY":"nb_shotfixture_0000000000000000000a1"},"config":"model_provider = \"custom\"\nmodel = \"gpt-5.5\"\nmodel_reasoning_effort = \"high\"\n\n[model_providers.custom]\nname = \"krill\"\nbase_url = \"https://api.krill-code.net/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = true"}',
 'https://www.krill-code.com', 'third_party', 1779000000000, 1, NULL, NULL, NULL, '{"apiFormat":"openai_responses"}', 0, 0),
('a1b2c3d4-1111-4a2b-9c00-000000000002', 'codex', 'Krill',
 '{"auth":{"OPENAI_API_KEY":"nb_shotfixture_0000000000000000000b2"},"config":"model_provider = \"custom\"\nmodel = \"gpt-5.5\"\n\n[model_providers.custom]\nname = \"krill\"\nbase_url = \"https://api.cdn-krill-ai.com/codex/v1\"\nwire_api = \"responses\""}',
 'https://www.krill-code.com', 'third_party', 1779000000001, 1, NULL, NULL, NULL, '{}', 0, 0);
INSERT INTO provider_endpoints (provider_id, app_type, url, added_at) VALUES
('a1b2c3d4-1111-4a2b-9c00-000000000001', 'codex', 'https://api.cdn-krill-ai.com/v1', 1779000000500);
"#;

const TOOLBOX_EXTRA: &str = r#"
INSERT INTO codex_provider (id, data, created_at, updated_at) VALUES ('kc0000000000000000000000000000a1', jsonb('{"name":"Team Relay (keychain)","category":"custom","settings_config":"{\"auth\":{},\"credential_ref\":\"keyring:ai-toolbox-codex-team\",\"config\":\"model = \\\"gpt-5.5\\\"\\nmodel_provider = \\\"custom\\\"\\n\\n[model_providers.custom]\\nname = \\\"Team\\\"\\nbase_url = \\\"https://team-relay.example.com/v1\\\"\\nwire_api = \\\"responses\\\"\"}","sort_index":2,"is_applied":false,"is_disabled":false}'), '2026-10-01T10:00:00+08:00', '2026-10-01T10:00:00+08:00');
"#;

fn db(path: &Path, sql: &str, wal: bool) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(path);
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(sql).unwrap();
    if wal {
        conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))
            .unwrap();
    }
}

#[test]
#[ignore = "writes fixture stores into $SUPERBA_IMPORT_FIXTURE_HOME"]
fn materialize() {
    let Some(home) = std::env::var_os("SUPERBA_IMPORT_FIXTURE_HOME") else {
        panic!("set SUPERBA_IMPORT_FIXTURE_HOME");
    };
    let home = Path::new(&home);
    let mut ccs = fixture("ccs/schema-v20.sql");
    ccs.push_str(CCS_EXTRA);
    db(&home.join(".cc-switch/cc-switch.db"), &ccs, false);
    std::fs::write(
        home.join(".cc-switch/settings.json"),
        fixture("ccs/settings.json"),
    )
    .unwrap();
    let mut toolbox = fixture("ai-toolbox/schema-v29.sql");
    toolbox.push_str(TOOLBOX_EXTRA);
    db(
        &home.join(".local/share/com.ai-toolbox/ai-toolbox.db"),
        &toolbox,
        true,
    );
}
