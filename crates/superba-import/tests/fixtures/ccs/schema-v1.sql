-- Fixture: cc-switch.db at SQLite schema v1 (PRAGMA user_version = 1)
-- Produced by cc-switch v3.8.0 .. v3.8.3 (2025-11-28 .. 2025-12-24), typically right after the
-- one-shot JSON -> SQLite migration of the v2 config.json (fixtures/ccs/v2-json/config.json),
-- which renames config.json to config.json.migrated.
-- DDL copied verbatim from cc-switch tag v3.8.0, src-tauri/src/database/schema.rs (create_tables_on_conn).
-- NOTE: no in_failover_queue / cost_multiplier / limit_* / provider_type columns yet.
-- All keys/tokens are fake.

PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS providers (
                id TEXT NOT NULL,
                app_type TEXT NOT NULL,
                name TEXT NOT NULL,
                settings_config TEXT NOT NULL,
                website_url TEXT,
                category TEXT,
                created_at INTEGER,
                sort_index INTEGER,
                notes TEXT,
                icon TEXT,
                icon_color TEXT,
                meta TEXT NOT NULL DEFAULT '{}',
                is_current BOOLEAN NOT NULL DEFAULT 0,
                PRIMARY KEY (id, app_type)
            );

CREATE TABLE IF NOT EXISTS provider_endpoints (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                provider_id TEXT NOT NULL,
                app_type TEXT NOT NULL,
                url TEXT NOT NULL,
                added_at INTEGER,
                FOREIGN KEY (provider_id, app_type) REFERENCES providers(id, app_type) ON DELETE CASCADE
            );

CREATE TABLE IF NOT EXISTS mcp_servers (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                server_config TEXT NOT NULL,
                description TEXT,
                homepage TEXT,
                docs TEXT,
                tags TEXT NOT NULL DEFAULT '[]',
                enabled_claude BOOLEAN NOT NULL DEFAULT 0,
                enabled_codex BOOLEAN NOT NULL DEFAULT 0,
                enabled_gemini BOOLEAN NOT NULL DEFAULT 0
            );

CREATE TABLE IF NOT EXISTS prompts (
                id TEXT NOT NULL,
                app_type TEXT NOT NULL,
                name TEXT NOT NULL,
                content TEXT NOT NULL,
                description TEXT,
                enabled BOOLEAN NOT NULL DEFAULT 1,
                created_at INTEGER,
                updated_at INTEGER,
                PRIMARY KEY (id, app_type)
            );

CREATE TABLE IF NOT EXISTS skills (
                key TEXT PRIMARY KEY,
                installed BOOLEAN NOT NULL DEFAULT 0,
                installed_at INTEGER NOT NULL DEFAULT 0
            );

CREATE TABLE IF NOT EXISTS skill_repos (
                owner TEXT NOT NULL,
                name TEXT NOT NULL,
                branch TEXT NOT NULL DEFAULT 'main',
                enabled BOOLEAN NOT NULL DEFAULT 1,
                PRIMARY KEY (owner, name)
            );

CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT
            );

-- ---------------------------------------------------------------------------
-- Rows as written by Database::migrate_from_json (src-tauri/src/database/migration.rs):
-- settings_config = compact JSON of settingsConfig; meta = ProviderMeta minus custom_endpoints
-- (custom_endpoints moved to provider_endpoints); is_current = (id == <app>.current).
-- ---------------------------------------------------------------------------

INSERT INTO providers (id, app_type, name, settings_config, website_url, category, created_at, sort_index, notes, icon, icon_color, meta, is_current) VALUES
('3f6a9b2c-2222-4d1e-8f00-000000000001', 'codex', 'OpenAI Official',
 '{"auth":{"OPENAI_API_KEY":null,"tokens":{"id_token":"eyJ-test-id-token.payload.sig","access_token":"eyJ-test-access-token.payload.sig","refresh_token":"rt-test-refresh-0000","account_id":"00000000-aaaa-bbbb-cccc-000000000000"},"last_refresh":"2025-11-01T08:00:00.000000Z"},"config":"model = \"gpt-5-codex\"\nmodel_reasoning_effort = \"high\"\n\n[mcp_servers.fetch]\ncommand = \"uvx\"\nargs = [\"mcp-server-fetch\"]\n\n[projects.\"/home/alice/src/demo\"]\ntrust_level = \"trusted\"\n"}',
 'https://chatgpt.com/codex', 'official', 1760000000001, 0, NULL, NULL, NULL, '{}', 0),

('3f6a9b2c-2222-4d1e-8f00-000000000002', 'codex', 'PackyCode',
 '{"auth":{"OPENAI_API_KEY":"sk-test-v2-codex-packy-000000000000"},"config":"model_provider = \"packycode\"\nmodel = \"gpt-5-codex\"\nmodel_reasoning_effort = \"high\"\ndisable_response_storage = true\n\n[model_providers.packycode]\nname = \"packycode\"\nbase_url = \"https://codex-api.packycode.example/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = true"}',
 'https://codex.packycode.example/', 'third_party', 1760000000002, 1, 'main relay', NULL, NULL,
 '{"usage_script":{"enabled":false,"language":"javascript","code":"({ request: { url: \"{{baseUrl}}/user/balance\", method: \"GET\" }, extractor: function (r) { return { remaining: r.balance, unit: \"USD\" }; } })","timeout":10},"isPartner":true,"partnerPromotionKey":"packycode"}',
 1),

('3f6a9b2c-2222-4d1e-8f00-000000000003', 'codex', 'My Gateway',
 '{"auth":{"OPENAI_API_KEY":"sk-test-v2-codex-mygw-000000000000"},"config":"model_provider = \"my_gateway\"\nmodel = \"gpt-5\"\nmodel_reasoning_effort = \"medium\"\ndisable_response_storage = true\n\n[model_providers.my_gateway]\nname = \"my_gateway\"\nbase_url = \"https://gw.example.org/openai/v1\"\nwire_api = \"responses\"\nenv_key = \"my_gateway\"\nrequires_openai_auth = true\n\n[model_providers.old_unused]\nname = \"old_unused\"\nbase_url = \"https://stale.example.net/v1\"\nwire_api = \"chat\"\n\n[mcp_servers.fetch]\ncommand = \"uvx\"\nargs = [\"mcp-server-fetch\"]\n"}',
 NULL, 'custom', 1760000000003, 2, NULL, NULL, NULL, '{}', 0),

('8d2c1f0e-1111-4c3b-9a55-000000000001', 'claude', 'Example Relay (Claude)',
 '{"env":{"ANTHROPIC_AUTH_TOKEN":"sk-test-v2-claude-0000000000000000","ANTHROPIC_BASE_URL":"https://relay.example.com"}}',
 'https://relay.example.com', 'third_party', 1760000000000, 0, NULL, NULL, NULL, '{}', 1),

('6b1d0c7e-3333-4f2a-9e10-000000000001', 'gemini', 'Example Gemini Relay',
 '{"env":{"GEMINI_API_KEY":"sk-test-v2-gemini-0000000000000000","GOOGLE_GEMINI_BASE_URL":"https://gemini-relay.example.com","GEMINI_MODEL":"gemini-2.5-pro"},"config":{}}',
 NULL, 'third_party', 1760000000004, NULL, NULL, NULL, NULL, '{}', 1);

-- meta.custom_endpoints of the PackyCode provider, moved here by the migration
-- (added_at is the JSON addedAt, epoch milliseconds; lastUsed is NOT persisted).
INSERT INTO provider_endpoints (provider_id, app_type, url, added_at) VALUES
('3f6a9b2c-2222-4d1e-8f00-000000000002', 'codex', 'https://codex-api-hk.packycode.example/v1', 1760000100000),
('3f6a9b2c-2222-4d1e-8f00-000000000002', 'codex', 'https://codex-api-us.packycode.example/v1', 1760000200000);

-- Migrated common config snippet (Database::migrate_common_config -> settings key common_config_<app>)
INSERT INTO settings (key, value) VALUES
('common_config_codex', 'approval_policy = "on-request"
');

PRAGMA user_version = 1;
