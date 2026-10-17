-- Fixture: cc-switch.db at SQLite schema v20 (PRAGMA user_version = 20), FRESH database
-- Produced by cc-switch v4.0.2 .. v4.0.15 (HEAD 79e754a, 2026-10-17).
-- DDL copied verbatim from src-tauri/src/database/schema.rs (create_tables_on_conn) at HEAD.
-- A fresh DB starts at user_version 0, so apply_schema_migrations() runs v0->v1->...->v20;
-- migrate_v1_to_v2 then APPENDS 4 columns to providers via ALTER TABLE ADD COLUMN
-- (cost_multiplier, limit_daily_usd, limit_monthly_usd, provider_type). They are unused by HEAD
-- code (HEAD keeps these values in meta) but physically present. Column order therefore is:
--   id, app_type, name, settings_config, website_url, category, created_at, sort_index, notes,
--   icon, icon_color, meta, is_current, in_failover_queue,
--   cost_multiplier, limit_daily_usd, limit_monthly_usd, provider_type
-- Only provider-relevant tables are reproduced (providers, provider_endpoints, settings,
-- profiles, provider_health). Other tables (mcp_servers, prompts, skills, skill_repos,
-- proxy_config, proxy_request_logs, model_pricing, stream_check_logs, proxy_live_backup,
-- usage_daily_rollups, session_log_sync, session_usage_dedup) are omitted.
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
                in_failover_queue BOOLEAN NOT NULL DEFAULT 0,
                PRIMARY KEY (id, app_type)
            );
-- migrate_v1_to_v2 (schema.rs:666-687), applied to every DB including fresh ones:
ALTER TABLE providers ADD COLUMN cost_multiplier TEXT NOT NULL DEFAULT '1.0';
ALTER TABLE providers ADD COLUMN limit_daily_usd TEXT;
ALTER TABLE providers ADD COLUMN limit_monthly_usd TEXT;
ALTER TABLE providers ADD COLUMN provider_type TEXT;

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

CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT);

CREATE TABLE IF NOT EXISTS provider_health (
            provider_id TEXT NOT NULL, app_type TEXT NOT NULL, is_healthy INTEGER NOT NULL DEFAULT 1,
            consecutive_failures INTEGER NOT NULL DEFAULT 0, last_success_at TEXT, last_failure_at TEXT,
            last_error TEXT, updated_at TEXT NOT NULL,
            PRIMARY KEY (provider_id, app_type),
            FOREIGN KEY (provider_id, app_type) REFERENCES providers(id, app_type) ON DELETE CASCADE
        );

CREATE TABLE IF NOT EXISTS profiles (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                payload TEXT NOT NULL,
                sort_order INTEGER,
                created_at INTEGER,
                updated_at INTEGER
            );

-- ---------------------------------------------------------------------------
-- Codex rows (app_type = 'codex'). Written by Database::save_provider (dao/providers.rs:266),
-- which lists columns explicitly (the 4 appended columns keep their defaults).
-- ---------------------------------------------------------------------------
INSERT INTO providers (id, app_type, name, settings_config, website_url, category, created_at, sort_index, notes, icon, icon_color, meta, is_current, in_failover_queue) VALUES

-- (1) Built-in official seed (dao/providers_seed.rs): ChatGPT login via the Codex CLI itself. NOT importable.
('codex-official', 'codex', 'OpenAI Official',
 '{"auth":{},"config":""}',
 'https://chatgpt.com/codex', 'official', 1775600000000, 0, NULL, 'openai', '#00A67E', '{}', 0, 0),

-- (2) Official card bound to a cc-switch-managed ChatGPT account (tokens live in
--     <app_config_dir>/codex_oauth_auth.json, never in the row). NOT importable.
('5a0e7d1c-4444-4b6a-a001-000000000001', 'codex', 'OpenAI Official (alice@example.com)',
 '{"auth":{},"config":""}',
 'https://chatgpt.com/codex', 'official', 1780000000000, 1, NULL, 'openai', '#00A67E',
 '{"providerType":"codex_oauth","authBinding":{"source":"managed_account","authProvider":"codex_oauth","accountId":"acct-test-0001"}}', 0, 0),

-- (3) "default" row auto-imported from the live ~/.codex (import_default_config, services/provider/live.rs:995):
--     auth = the whole live auth.json (ChatGPT login snapshot!), config = the whole live config.toml.
--     category 'official' because auth has login material and no API key. NOT importable (and sensitive).
('default', 'codex', 'default',
 '{"auth":{"auth_mode":"chatgpt","OPENAI_API_KEY":null,"tokens":{"id_token":"eyJ-test-id-token.payload.sig","access_token":"eyJ-test-access-token.payload.sig","refresh_token":"rt-test-refresh-0000","account_id":"00000000-aaaa-bbbb-cccc-000000000000"},"last_refresh":"2026-09-30T08:00:00.000000Z"},"config":"model = \"gpt-5.6-sol\"\nmodel_reasoning_effort = \"high\"\n\n[mcp_servers.fetch]\ncommand = \"uvx\"\nargs = [\"mcp-server-fetch\"]\n"}',
 NULL, 'official', 1775500000000, NULL, NULL, NULL, NULL, '{}', 0, 0),

-- (4) Third-party, native Responses upstream, key in auth (current shape of every preset since v3.16:
--     model_provider = "custom" + [model_providers.custom], see generateThirdPartyConfig). IMPORTABLE.
('b7e2c9a4-5555-4e3f-b002-000000000001', 'codex', 'Kimi',
 '{"auth":{"OPENAI_API_KEY":"sk-test-v20-kimi-000000000000000000"},"config":"model_provider = \"custom\"\nmodel = \"kimi-k3\"\nmodel_reasoning_effort = \"high\"\ndisable_response_storage = true\n\n[model_providers.custom]\nname = \"kimi\"\nbase_url = \"https://api.moonshot.example/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = true","modelCatalog":{"models":[{"model":"kimi-k3","displayName":"Kimi K3","contextWindow":1048576,"supportsParallelToolCalls":true,"reasoningLevels":["low","high","max"]},{"model":"kimi-k2.7-code","displayName":"Kimi K2.7 Code","contextWindow":262144,"supportsParallelToolCalls":true,"reasoningLevels":["high"]}]}}',
 'https://platform.kimi.example', 'cn_official', 1780000001000, 2, 'team key', 'kimi', '#6366F1',
 '{"commonConfigEnabled":true,"endpointAutoSelect":true,"partnerPromotionKey":"kimi","apiFormat":"openai_responses"}', 1, 0),

-- (5) Third-party whose real upstream is Chat Completions: config says wire_api = "responses" (talks to
--     cc-switch's local proxy), meta.apiFormat = "openai_chat" means cc-switch translates. NOT directly
--     importable as a Responses line (flag/skip).
('c1d4f6e8-6666-4a2b-b003-000000000001', 'codex', 'FluxA Token Plan',
 '{"auth":{"OPENAI_API_KEY":"sk-test-v20-fluxa-00000000000000000"},"config":"model_provider = \"custom\"\nmodel = \"deepseek-v4-pro\"\nmodel_reasoning_effort = \"high\"\ndisable_response_storage = true\n\n[model_providers.custom]\nname = \"fluxa_tokenplan\"\nbase_url = \"https://api.fluxa.example/v2/tokenplan/team\"\nwire_api = \"responses\"\nrequires_openai_auth = true","modelCatalog":{"models":[{"model":"deepseek-v4-pro","displayName":"DeepSeek V4 Pro"}]}}',
 'https://agentmarket.fluxa.example/', 'aggregator', 1780000002000, 3, NULL, NULL, NULL,
 '{"commonConfigEnabled":true,"endpointAutoSelect":true,"apiFormat":"openai_chat","codexChatReasoning":{"supportsThinking":true,"supportsEffort":true}}', 0, 1),

-- (6) "Mobile compat" shape: empty auth, key as experimental_bearer_token inside the active
--     [model_providers.<id>] table (extract_codex_experimental_bearer_token, codex_config.rs:2939). IMPORTABLE.
('d9a0b1c2-7777-4c5d-b004-000000000001', 'codex', 'Bearer-in-TOML Relay',
 '{"auth":{},"config":"model_provider = \"custom\"\nmodel = \"gpt-5.6-sol\"\nmodel_reasoning_effort = \"medium\"\n\n[model_providers.custom]\nname = \"bearer-relay\"\nbase_url = \"https://bearer-relay.example.com/v1\"\nwire_api = \"responses\"\nexperimental_bearer_token = \"sk-test-v20-bearer-000000000000000\""}',
 NULL, 'custom', 1780000003000, 4, NULL, NULL, NULL, '{"apiFormat":"openai_responses"}', 0, 0),

-- (7) Legacy row created in the JSON era (v3.1-v3.7) and carried over by the v3.8 migration:
--     provider-specific model_provider id, plus backfilled global config (MCP, other provider tables).
--     Base URL must be read from [model_providers.<model_provider>], NOT from the first table. IMPORTABLE.
('3f6a9b2c-2222-4d1e-8f00-000000000002', 'codex', 'PackyCode',
 '{"auth":{"OPENAI_API_KEY":"sk-test-v2-codex-packy-000000000000"},"config":"model_provider = \"packycode\"\nmodel = \"gpt-5-codex\"\nmodel_reasoning_effort = \"high\"\ndisable_response_storage = true\n\n[model_providers.aaa_unused]\nname = \"aaa_unused\"\nbase_url = \"https://stale.example.net/v1\"\nwire_api = \"chat\"\n\n[model_providers.packycode]\nname = \"packycode\"\nbase_url = \"https://codex-api.packycode.example/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = true\n\n[mcp_servers.fetch]\ncommand = \"uvx\"\nargs = [\"mcp-server-fetch\"]\n"}',
 'https://codex.packycode.example/', 'third_party', 1760000000002, 5, 'main relay', NULL, NULL,
 '{"usage_script":{"enabled":false,"language":"javascript","code":"({ request: { url: \"{{baseUrl}}/user/balance\", method: \"GET\" }, extractor: function (r) { return { remaining: r.balance, unit: \"USD\" }; } })","timeout":10},"isPartner":true,"partnerPromotionKey":"packycode"}', 0, 0),

-- (8) Row materialized from a "universal provider" (provider.rs:884 to_codex_provider; id = universal-codex-<uid>).
--     IMPORTABLE like (4).
('universal-codex-u-test-01', 'codex', 'My NewAPI',
 '{"auth":{"OPENAI_API_KEY":"sk-test-v20-newapi-0000000000000000"},"config":"model_provider = \"custom\"\nmodel = \"gpt-5.6-sol\"\nmodel_reasoning_effort = \"high\"\ndisable_response_storage = true\n\n[model_providers.custom]\nname = \"NewAPI\"\nbase_url = \"https://newapi.example.com/v1\"\nwire_api = \"responses\"\nrequires_openai_auth = true"}',
 'https://newapi.example.com', 'aggregator', 1780000004000, 6, NULL, NULL, NULL, '{}', 0, 0),

-- (9) GitHub Copilot via cc-switch's proxy (managed GitHub login, no static key). NOT importable.
('e3f4a5b6-8888-4d7e-b005-000000000001', 'codex', 'GitHub Copilot',
 '{"auth":{},"config":"model_provider = \"custom\"\nmodel = \"gpt-6-astra\"\nmodel_reasoning_effort = \"high\"\ndisable_response_storage = true\n\n[model_providers.custom]\nname = \"github-copilot\"\nbase_url = \"https://api.githubcopilot.com\"\nwire_api = \"responses\"\nrequires_openai_auth = false"}',
 'https://github.com/features/copilot', 'third_party', 1780000005000, 7, NULL, 'github', '#000000',
 '{"providerType":"github_copilot","authBinding":{"source":"managed_account","authProvider":"github_copilot","accountId":"gh-test-0001"},"githubAccountId":"gh-test-0001"}', 0, 0);

-- Non-Codex rows (shape reference only; not importable as Codex lines)
INSERT INTO providers (id, app_type, name, settings_config, website_url, category, created_at, sort_index, notes, icon, icon_color, meta, is_current, in_failover_queue) VALUES
('claude-official', 'claude', 'Claude Official', '{"env":{}}', 'https://www.anthropic.com/claude-code', 'official', 1775600000000, 0, NULL, 'anthropic', '#D4915D', '{}', 0, 0),
('8d2c1f0e-1111-4c3b-9a55-000000000001', 'claude', 'Example Relay (Claude)',
 '{"env":{"ANTHROPIC_AUTH_TOKEN":"sk-test-v20-claude-0000000000000000","ANTHROPIC_BASE_URL":"https://relay.example.com","ANTHROPIC_MODEL":"claude-sonnet-5"}}',
 'https://relay.example.com', 'third_party', 1760000000000, 1, NULL, NULL, NULL, '{"commonConfigEnabled":true}', 1, 0),
('gemini-official', 'gemini', 'Google Official', '{"env":{},"config":{}}', 'https://ai.google.dev/', 'official', 1775600000000, 0, NULL, 'gemini', '#4285F4', '{}', 0, 0),
('6b1d0c7e-3333-4f2a-9e10-000000000001', 'gemini', 'Example Gemini Relay',
 '{"env":{"GEMINI_API_KEY":"sk-test-v20-gemini-0000000000000000","GOOGLE_GEMINI_BASE_URL":"https://gemini-relay.example.com","GEMINI_MODEL":"gemini-3.6-flash"},"config":{}}',
 NULL, 'third_party', 1760000000004, 1, NULL, NULL, NULL, '{}', 1, 0),
('grokbuild-official', 'grokbuild', 'Grok Official', '{"config":""}', 'https://x.ai/grok', 'official', 1775600000000, 0, NULL, 'grok', 'currentColor', '{}', 1, 0);

-- User-added alternative base URLs ("custom endpoints", endpoint speed test). Only url + added_at (ms) persist.
INSERT INTO provider_endpoints (provider_id, app_type, url, added_at) VALUES
('b7e2c9a4-5555-4e3f-b002-000000000001', 'codex', 'https://api.moonshot.example/v1', 1780000001000),
('b7e2c9a4-5555-4e3f-b002-000000000001', 'codex', 'https://api-intl.moonshot.example/v1', 1780000001500),
('3f6a9b2c-2222-4d1e-8f00-000000000002', 'codex', 'https://codex-api-hk.packycode.example/v1', 1760000100000),
('3f6a9b2c-2222-4d1e-8f00-000000000002', 'codex', 'https://codex-api-us.packycode.example/v1', 1760000200000);

INSERT INTO settings (key, value) VALUES
('official_providers_seeded', 'true'),
('common_config_codex', 'approval_policy = "on-request"
'),
('universal_providers', '{"u-test-01":{"id":"u-test-01","name":"My NewAPI","providerType":"newapi","apps":{"claude":false,"codex":true,"gemini":false},"baseUrl":"https://newapi.example.com","apiKey":"sk-test-v20-newapi-0000000000000000","models":{"codex":{"model":"gpt-5.6-sol","reasoningEffort":"high"}},"websiteUrl":"https://newapi.example.com","createdAt":1780000004000,"sortIndex":6}}'),
('current_profile_id_codex', 'prof-test-01');

INSERT INTO profiles (id, name, payload, sort_order, created_at, updated_at) VALUES
-- ProfilePayload (services/profile.rs:124) only stores provider IDs per app, never provider settings.
('prof-test-01', 'Work', '{"providers":{"claude":null,"claude-desktop":null,"codex":"b7e2c9a4-5555-4e3f-b002-000000000001"},"mcp":{"claude":null,"claude-desktop":null,"codex":null},"skills":{"claude":null,"claude-desktop":null,"codex":null},"prompts":{"claude":null,"claude-desktop":null,"codex":null}}', 0, 1780000006000, 1780000006000);

PRAGMA user_version = 20;
