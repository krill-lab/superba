-- Fixture: cc-switch.db created by v3.8.x (schema v1) and later upgraded in place to v20 by v4.0.x.
-- Same logical schema as the fresh v20 DB, but a DIFFERENT physical column order in `providers`,
-- because migrate_v1_to_v2 (schema.rs:666-687) appends columns to the v1 table in this order:
--   cost_multiplier, limit_daily_usd, limit_monthly_usd, provider_type, in_failover_queue
-- => importers must select columns BY NAME (and check PRAGMA table_info for in_failover_queue,
--    which is absent in genuine v1 databases).
-- The v1 -> v2 step also drops the never-shipped `failover_queue` table and adds
-- idx_providers_failover. Other tables omitted (see sqlite-v20 fixture). All keys are fake.

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
ALTER TABLE providers ADD COLUMN cost_multiplier TEXT NOT NULL DEFAULT '1.0';
ALTER TABLE providers ADD COLUMN limit_daily_usd TEXT;
ALTER TABLE providers ADD COLUMN limit_monthly_usd TEXT;
ALTER TABLE providers ADD COLUMN provider_type TEXT;
ALTER TABLE providers ADD COLUMN in_failover_queue BOOLEAN NOT NULL DEFAULT 0;
CREATE INDEX IF NOT EXISTS idx_providers_failover
             ON providers(app_type, in_failover_queue, sort_index);

CREATE TABLE IF NOT EXISTS provider_endpoints (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                provider_id TEXT NOT NULL,
                app_type TEXT NOT NULL,
                url TEXT NOT NULL,
                added_at INTEGER,
                FOREIGN KEY (provider_id, app_type) REFERENCES providers(id, app_type) ON DELETE CASCADE
            );

CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT
            );

INSERT INTO providers (id, app_type, name, settings_config, website_url, category, created_at, sort_index, notes, icon, icon_color, meta, is_current, in_failover_queue) VALUES
('3f6a9b2c-2222-4d1e-8f00-000000000002', 'codex', 'PackyCode',
 '{"auth":{"OPENAI_API_KEY":"sk-test-v2-codex-packy-000000000000"},"config":"model_provider = \"packycode\"\nmodel = \"gpt-5-codex\"\nmodel_reasoning_effort = \"high\"\ndisable_response_storage = true\n\n[model_providers.packycode]\nname = \"packycode\"\nbase_url = \"https://codex-api.packycode.example/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = true"}',
 'https://codex.packycode.example/', 'third_party', 1760000000002, 1, 'main relay', NULL, NULL,
 '{"isPartner":true,"partnerPromotionKey":"packycode"}', 0, 0),
('codex-official', 'codex', 'OpenAI Official', '{"auth":{},"config":""}', 'https://chatgpt.com/codex', 'official', 1775600000000, 3, NULL, 'openai', '#00A67E', '{}', 0, 0),
('f0e1d2c3-9999-4a8b-b006-000000000001', 'codex', 'New Relay',
 '{"auth":{"OPENAI_API_KEY":"sk-test-v20u-newrelay-00000000000000"},"config":"model_provider = \"custom\"\nmodel = \"gpt-5.6-sol\"\nmodel_reasoning_effort = \"high\"\ndisable_response_storage = true\n\n[model_providers.custom]\nname = \"New Relay\"\nbase_url = \"https://new-relay.example.com/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = true"}',
 'https://new-relay.example.com', 'third_party', 1781000000000, 4, NULL, NULL, NULL,
 '{"commonConfigEnabled":true,"endpointAutoSelect":true,"apiFormat":"openai_responses"}', 1, 0);

INSERT INTO provider_endpoints (provider_id, app_type, url, added_at) VALUES
('3f6a9b2c-2222-4d1e-8f00-000000000002', 'codex', 'https://codex-api-hk.packycode.example/v1', 1760000100000);

INSERT INTO settings (key, value) VALUES ('official_providers_seeded', 'true');

PRAGMA user_version = 20;
