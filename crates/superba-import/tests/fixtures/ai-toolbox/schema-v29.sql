-- AI Toolbox (github.com/coulsontl/ai-toolbox) main database fixture: <app_data>/ai-toolbox.db
-- PRAGMA user_version = 29  <=>  releases: master a7c4553 (2026-10-17, unreleased; package.json still says 1.1.9)
-- FAKE DATA ONLY (sk-test-* keys, fake tokens). Generated for read-only importer tests.
-- `data` is SQLite JSONB (binary) -> load with SQLite >= 3.45.0 (jsonb()/json_valid(x,4)).
-- Read back with: SELECT id, json(data), created_at, updated_at FROM <table>.
-- Omitted on purpose (non-JSONB, unrelated to providers): proxy_request_logs, usage_daily_rollups, model_pricing.
-- The real app opens the file with PRAGMA journal_mode=WAL (ai-toolbox.db-wal / -shm next to it).
BEGIN;
CREATE TABLE IF NOT EXISTS settings (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS app_migration (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS claude_provider (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS claude_common_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS claude_prompt_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS codex_provider (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS codex_common_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS codex_prompt_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS codex_official_account (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS codex_plugin_workspace_roots (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS gemini_cli_provider (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS gemini_cli_common_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS gemini_cli_prompt_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS gemini_cli_official_account (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS opencode_common_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS opencode_prompt_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS opencode_favorite_plugin (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS opencode_favorite_provider (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS openclaw_common_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS oh_my_openagent_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS oh_my_openagent_global_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS oh_my_opencode_slim_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS oh_my_opencode_slim_global_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS skill (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS skill_group (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS skill_repo (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS skill_preferences (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS skill_settings (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS custom_tool (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS mcp_server (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS mcp_preferences (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS favorite_mcp (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS wsl_sync_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS wsl_file_mapping (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS ssh_sync_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS ssh_connection (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS ssh_file_mapping (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS proxy_gateway_settings (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS image_channel (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS image_job (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS image_asset (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS pi_settings_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS pi_prompt_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS grok_provider (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS grok_official_account (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS grok_common_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS grok_prompt_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS oh_my_pi_settings_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS oh_my_pi_prompt_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS claude_desktop_provider (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS hermes_settings_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS hermes_prompt_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS claude_desktop_prompt_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS dsh_settings_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS dsh_prompt_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS mcp_group (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS kimi_provider (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS kimi_official_account (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS kimi_common_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS kimi_prompt_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS gateway_session_usage_state (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS oh_my_pi_agents_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS antigravity_provider (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS antigravity_common_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS antigravity_prompt_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS antigravity_official_account (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS omo_native_settings_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS omo_native_agents_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS zcode_provider (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS zcode_common_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS zcode_prompt_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS zcode_official_account (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS omo_native_prompt_config (
    id TEXT PRIMARY KEY NOT NULL,
    data BLOB NOT NULL CHECK (json_valid(data, 4)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_codex_provider_is_applied ON codex_provider (json_extract(data, '$.is_applied'));
CREATE INDEX IF NOT EXISTS idx_codex_provider_sort_index ON codex_provider (json_extract(data, '$.sort_index'));
CREATE INDEX IF NOT EXISTS idx_codex_prompt_config_is_applied ON codex_prompt_config (json_extract(data, '$.is_applied'));
CREATE INDEX IF NOT EXISTS idx_codex_prompt_config_sort_index ON codex_prompt_config (json_extract(data, '$.sort_index'));
CREATE INDEX IF NOT EXISTS idx_codex_official_account_is_applied ON codex_official_account (json_extract(data, '$.is_applied'));
CREATE INDEX IF NOT EXISTS idx_codex_official_account_sort_index ON codex_official_account (json_extract(data, '$.sort_index'));
CREATE INDEX IF NOT EXISTS idx_codex_official_account_provider_id ON codex_official_account (json_extract(data, '$.provider_id'));
CREATE INDEX IF NOT EXISTS idx_claude_provider_is_applied ON claude_provider (json_extract(data, '$.is_applied'));
CREATE INDEX IF NOT EXISTS idx_claude_provider_sort_index ON claude_provider (json_extract(data, '$.sort_index'));
CREATE INDEX IF NOT EXISTS idx_gemini_cli_provider_is_applied ON gemini_cli_provider (json_extract(data, '$.is_applied'));
CREATE INDEX IF NOT EXISTS idx_gemini_cli_provider_sort_index ON gemini_cli_provider (json_extract(data, '$.sort_index'));
CREATE INDEX IF NOT EXISTS idx_opencode_favorite_provider_provider_id ON opencode_favorite_provider (json_extract(data, '$.provider_id'));
CREATE INDEX IF NOT EXISTS idx_opencode_favorite_provider_created_at ON opencode_favorite_provider (created_at);
INSERT INTO settings (id, data, created_at, updated_at) VALUES ('app', jsonb('{"language":"zh-CN","theme":"system","codex_preserve_official_auth_on_switch":false,"provider_last_used":{"codex:4a1d2c3b5e6f47a8b9c0d1e2f3a4b5c6":"2026-10-17T10:05:00.000000000+08:00"},"backup_encryption":{"enabled":true,"credential_ref":"keyring:ai-toolbox-backup-encryption"},"created_at":"2026-05-20T09:29:00.000000000+08:00","updated_at":"2026-10-17T10:05:00.000000000+08:00"}'), '2026-05-20T09:29:00.000000000+08:00', '2026-10-17T10:05:00.000000000+08:00');
INSERT INTO codex_provider (id, data, created_at, updated_at) VALUES ('0f8e3c1a9b7d4e2f8a6c5b4d3e2f1a0b', jsonb('{"name":"默认配置","category":"official","settings_config":"{\"auth\":{},\"config\":\"model = \\\"gpt-5.5\\\"\"}","sort_index":0,"is_applied":false,"is_disabled":false,"created_at":"2026-05-20T09:30:00.123456789+08:00","updated_at":"2026-10-17T10:00:00.000000000+08:00"}'), '2026-05-20T09:30:00.123456789+08:00', '2026-10-17T10:00:00.000000000+08:00');
INSERT INTO codex_provider (id, data, created_at, updated_at) VALUES ('4a1d2c3b5e6f47a8b9c0d1e2f3a4b5c6', jsonb('{"name":"Example Relay","category":"custom","settings_config":"{\"auth\":{\"OPENAI_API_KEY\":\"sk-test-relay-4f9c2a7e1b\"},\"config\":\"model = \\\"gpt-5.5\\\"\\nmodel_provider = \\\"custom\\\"\\n\\n[model_providers.custom]\\nname = \\\"OpenAI\\\"\\nwire_api = \\\"responses\\\"\\nbase_url = \\\"https://relay.example.com/v1\\\"\",\"modelCatalog\":{\"models\":[{\"model\":\"gpt-5.5\",\"displayName\":\"GPT-5.5\",\"contextWindow\":272000,\"reasoningLevels\":[\"low\",\"medium\",\"high\",\"xhigh\"],\"defaultReasoningLevel\":\"high\",\"serviceTiers\":[\"priority\"]},{\"model\":\"gpt-5.4-mini\",\"displayName\":\"GPT-5.4 Mini\",\"supportsImage\":true,\"modalities\":{\"input\":[\"text\",\"image\"],\"output\":[\"text\"]}}]},\"autoReviewModelOverride\":\"gpt-5.4-mini\"}","website_url":"https://relay.example.com","notes":"主力中转（测试数据）","sort_index":1,"meta":{"apiFormat":"openai_responses","costMultiplier":"1.0","pricingModelSource":"upstream"},"is_applied":true,"is_disabled":false,"created_at":"2026-05-20T09:30:00.123456789+08:00","updated_at":"2026-10-17T10:00:00.000000000+08:00"}'), '2026-05-20T09:30:00.123456789+08:00', '2026-10-17T10:00:00.000000000+08:00');
INSERT INTO codex_provider (id, data, created_at, updated_at) VALUES ('7b2e9f10c3d84a56b7e8f9a0b1c2d3e4', jsonb('{"name":"备用中转 Backup","category":"custom","settings_config":"{\"auth\":{\"OPENAI_API_KEY\":\"sk-test-backup-8d3e1f\"},\"config\":\"model = \\\"gpt-5.4\\\"\\nmodel_provider = \\\"custom\\\"\\nmodel_reasoning_effort = \\\"xhigh\\\"\\n\\n[model_providers.custom]\\nname = \\\"OpenAI\\\"\\nwire_api = \\\"responses\\\"\\nbase_url = \\\"https://backup.example.net/v1\\\"\"}","sort_index":2,"is_applied":false,"is_disabled":true,"created_at":"2026-05-20T09:30:00.123456789+08:00","updated_at":"2026-10-17T10:00:00.000000000+08:00"}'), '2026-05-20T09:30:00.123456789+08:00', '2026-10-17T10:00:00.000000000+08:00');
INSERT INTO codex_provider (id, data, created_at, updated_at) VALUES ('k3x9q2m7v1b8n4c6z0wd', jsonb('{"name":"Manual TOML relay","category":"custom","settings_config":"{\"auth\":{},\"config\":\"model = \\\"gpt-5.3-codex\\\"\\nmodel_provider = \\\"legacyrelay\\\"\\n\\n[model_providers.legacyrelay]\\nname = \\\"Legacy Relay\\\"\\nbase_url = \\\"https://legacy-relay.example.org/v1\\\"\\nwire_api = \\\"responses\\\"\\nexperimental_bearer_token = \\\"sk-test-bearer-2b7d9e\\\"\"}","sort_index":3,"is_applied":false,"is_disabled":false,"created_at":"2026-05-20T09:30:00.123456789+08:00","updated_at":"2026-10-17T10:00:00.000000000+08:00"}'), '2026-05-20T09:30:00.123456789+08:00', '2026-10-17T10:00:00.000000000+08:00');
INSERT INTO codex_provider (id, data, created_at, updated_at) VALUES ('9c3f0a21d4e54b67c8d9e0f1a2b3c4d5', jsonb('{"name":"Azure OpenAI (env key)","category":"custom","settings_config":"{\"auth\":{},\"config\":\"model = \\\"gpt-5.4\\\"\\nmodel_provider = \\\"azure\\\"\\n\\n[model_providers.azure]\\nname = \\\"Azure OpenAI\\\"\\nbase_url = \\\"https://example-resource.openai.azure.com/openai/v1\\\"\\nenv_key = \\\"AZURE_OPENAI_API_KEY\\\"\\nwire_api = \\\"responses\\\"\"}","sort_index":4,"is_applied":false,"is_disabled":false,"created_at":"2026-05-20T09:30:00.123456789+08:00","updated_at":"2026-10-17T10:00:00.000000000+08:00"}'), '2026-05-20T09:30:00.123456789+08:00', '2026-10-17T10:00:00.000000000+08:00');
INSERT INTO codex_provider (id, data, created_at, updated_at) VALUES ('2d4f6a8c0e1b3d5f7a9c1e3b5d7f9a1c', jsonb('{"name":"Mirror (model from common config)","category":"custom","settings_config":"{\"auth\":{\"OPENAI_API_KEY\":\"sk-test-mirror-6a1c3e\"},\"config\":\"model_provider = \\\"custom\\\"\\n\\n[model_providers.custom]\\nname = \\\"OpenAI\\\"\\nwire_api = \\\"responses\\\"\\nrequires_openai_auth = true\\nbase_url = \\\"https://mirror.example.com/v1\\\"\"}","sort_index":5,"is_applied":false,"is_disabled":false,"created_at":"2026-05-20T09:30:00.123456789+08:00","updated_at":"2026-10-17T10:00:00.000000000+08:00"}'), '2026-05-20T09:30:00.123456789+08:00', '2026-10-17T10:00:00.000000000+08:00');
INSERT INTO codex_provider (id, data, created_at, updated_at) VALUES ('5e7a9c1b3d5f7e9a1c3e5b7d9f1a3c5e', jsonb('{"name":"DeepSeek","category":"custom","settings_config":"{\"auth\":{\"OPENAI_API_KEY\":\"sk-test-deepseek-0c4b8f\"},\"config\":\"model = \\\"deepseek-v4-flash\\\"\\nmodel_provider = \\\"deepseek\\\"\\n\\n[model_providers.deepseek]\\nname = \\\"DeepSeek\\\"\\nbase_url = \\\"https://api.deepseek.com\\\"\\nwire_api = \\\"responses\\\"\",\"modelCatalog\":{\"models\":[{\"model\":\"deepseek-v4-flash\",\"displayName\":\"DeepSeek V4 Flash\",\"contextWindow\":1000000},{\"model\":\"deepseek-v4-pro\",\"displayName\":\"DeepSeek V4 Pro\",\"contextWindow\":1000000}]}}","website_url":"https://platform.deepseek.com","sort_index":6,"meta":{"gatewayProfile":{"tool":"codex","profileId":"deepseek","endpointId":"openai_responses"}},"is_applied":false,"is_disabled":false,"created_at":"2026-05-20T09:30:00.123456789+08:00","updated_at":"2026-10-17T10:00:00.000000000+08:00"}'), '2026-05-20T09:30:00.123456789+08:00', '2026-10-17T10:00:00.000000000+08:00');
INSERT INTO codex_provider (id, data, created_at, updated_at) VALUES ('6f8b0d2e4a6c8e0b2d4f6a8c0e2b4d6f', jsonb('{"name":"Claude via Codex (gateway)","category":"custom","settings_config":"{\"auth\":{\"OPENAI_API_KEY\":\"sk-test-anthropic-5e2a9d\"},\"config\":\"model = \\\"claude-opus-4-7\\\"\\nmodel_provider = \\\"custom\\\"\\n\\n[model_providers.custom]\\nname = \\\"Anthropic Relay\\\"\\nbase_url = \\\"https://anthropic-relay.example.com\\\"\\nwire_api = \\\"responses\\\"\"}","sort_index":7,"meta":{"apiFormat":"anthropic_messages"},"is_applied":false,"is_disabled":false,"created_at":"2026-05-20T09:30:00.123456789+08:00","updated_at":"2026-10-17T10:00:00.000000000+08:00"}'), '2026-05-20T09:30:00.123456789+08:00', '2026-10-17T10:00:00.000000000+08:00');
INSERT INTO codex_provider (id, data, created_at, updated_at) VALUES ('8a0c2e4b6d8f0a2c4e6b8d0f2a4c6e8b', jsonb('{"name":"AiHub (from CC Switch)","category":"third_party","settings_config":"{\"auth\":{\"OPENAI_API_KEY\":\"sk-test-aihub-7f3b1d\"},\"config\":\"model = \\\"gpt-5.5\\\"\\nmodel_provider = \\\"aihubmix\\\"\\nmodel_reasoning_effort = \\\"medium\\\"\\n\\n[model_providers.aihubmix]\\nname = \\\"AiHub\\\"\\nbase_url = \\\"https://aihub.example.org/v1\\\"\\nwire_api = \\\"responses\\\"\",\"requiresOpenaiAuthMode\":\"keep\"}","source_provider_id":"ccs:codex:aihub-01","website_url":"https://aihub.example.org","icon":"openai","icon_color":"#10A37F","sort_index":8,"is_applied":false,"is_disabled":false,"created_at":"2026-05-20T09:30:00.123456789+08:00","updated_at":"2026-10-17T10:00:00.000000000+08:00"}'), '2026-05-20T09:30:00.123456789+08:00', '2026-10-17T10:00:00.000000000+08:00');
INSERT INTO codex_common_config (id, data, created_at, updated_at) VALUES ('common', jsonb('{"config":"model = \"gpt-5.4\"\nmodel_reasoning_effort = \"high\"\n\n[features]\nweb_search_request = true","updated_at":"2026-10-17T10:05:00.000000000+08:00","created_at":"2026-05-20T09:31:00.000000000+08:00"}'), '2026-05-20T09:31:00.000000000+08:00', '2026-10-17T10:05:00.000000000+08:00');
INSERT INTO codex_official_account (id, data, created_at, updated_at) VALUES ('b1c2d3e4f5a64b7c8d9e0f1a2b3c4d5e', jsonb('{"provider_id":"0f8e3c1a9b7d4e2f8a6c5b4d3e2f1a0b","name":"alice@example.com","kind":"oauth","auth_snapshot":"{\"OPENAI_API_KEY\":null,\"auth_mode\":\"chatgpt\",\"tokens\":{\"id_token\":\"eyJhbGciOiJub25lIn0.TEST-ID-NOT-A-REAL-TOKEN.c2lnbmF0dXJl\",\"access_token\":\"eyJhbGciOiJub25lIn0.TEST-ACCESS-NOT-A-REAL-TOKEN.c2lnbmF0dXJl\",\"refresh_token\":\"rt_test_0000000000000000\",\"account_id\":\"acct-test-0001\"},\"last_refresh\":\"2026-09-30T01:02:03.000000Z\"}","email":"alice@example.com","auth_mode":"chatgpt","account_id":"acct-test-0001","plan_type":"plus","last_refresh":"2026-09-30T01:02:03.000000Z","sort_index":0,"is_applied":true,"created_at":"2026-05-20T09:31:00.000000000+08:00","updated_at":"2026-10-17T10:05:00.000000000+08:00","limit_short_label":"5h 42% · 周 18%","limit_5h_text":"42%","limit_weekly_text":"18%","limit_5h_reset_at":1790000000,"limit_weekly_reset_at":1790400000,"last_limits_fetched_at":"2026-10-17T10:05:00.000000000+08:00"}'), '2026-05-20T09:31:00.000000000+08:00', '2026-10-17T10:05:00.000000000+08:00');
INSERT INTO codex_prompt_config (id, data, created_at, updated_at) VALUES ('c4d5e6f7a8b94c0d1e2f3a4b5c6d7e8f', jsonb('{"name":"默认提示词","content":"# AGENTS.md\n\nAlways answer in Chinese.\n","is_applied":true,"sort_index":0,"created_at":"2026-05-20T09:31:00.000000000+08:00","updated_at":"2026-10-17T10:05:00.000000000+08:00"}'), '2026-05-20T09:31:00.000000000+08:00', '2026-10-17T10:05:00.000000000+08:00');
INSERT INTO claude_provider (id, data, created_at, updated_at) VALUES ('d5e6f7a8b9c04d1e2f3a4b5c6d7e8f9a', jsonb('{"name":"Claude Relay","category":"custom","settings_config":"{\"env\":{\"ANTHROPIC_BASE_URL\":\"https://claude-relay.example.com\",\"ANTHROPIC_AUTH_TOKEN\":\"sk-test-claude-1a2b3c\",\"ANTHROPIC_MODEL\":\"claude-opus-4-7\"}}","sort_index":0,"is_applied":true,"is_disabled":false,"created_at":"2026-05-20T09:31:00.000000000+08:00","updated_at":"2026-10-17T10:05:00.000000000+08:00"}'), '2026-05-20T09:31:00.000000000+08:00', '2026-10-17T10:05:00.000000000+08:00');
INSERT INTO gemini_cli_provider (id, data, created_at, updated_at) VALUES ('e6f7a8b9c0d14e2f3a4b5c6d7e8f9a0b', jsonb('{"name":"Gemini Relay","category":"custom","settings_config":"{\"env\":{\"GEMINI_API_KEY\":\"test-gemini-4d5e6f\",\"GOOGLE_GEMINI_BASE_URL\":\"https://gemini-relay.example.com\",\"GEMINI_MODEL\":\"gemini-3.1-pro-preview\"},\"config\":{}}","sort_index":0,"is_applied":false,"is_disabled":false,"created_at":"2026-05-20T09:31:00.000000000+08:00","updated_at":"2026-10-17T10:05:00.000000000+08:00"}'), '2026-05-20T09:31:00.000000000+08:00', '2026-10-17T10:05:00.000000000+08:00');
INSERT INTO opencode_favorite_provider (id, data, created_at, updated_at) VALUES ('a8b9c0d1e2f34a4b5c6d7e8f9a0b1c2d', jsonb('{"provider_id":"codex:3e5a7c9e1b3d5f7a9c1e3b5d7f9b1d3e","npm":"@ai-sdk/openai","base_url":"https://old-relay.example.com/v1","provider_config":{"npm":"@ai-sdk/openai","name":"Old Relay (deleted from Codex list)","options":{"baseURL":"https://old-relay.example.com/v1","apiKey":"sk-test-oldrelay-9e8d7c","__aiToolboxSourcePayload":{"name":"Old Relay (deleted from Codex list)","category":"custom","settingsConfig":"{\"auth\":{\"OPENAI_API_KEY\":\"sk-test-oldrelay-9e8d7c\"},\"config\":\"model = \\\"gpt-5.4\\\"\\nmodel_provider = \\\"custom\\\"\\n\\n[model_providers.custom]\\nname = \\\"OpenAI\\\"\\nwire_api = \\\"responses\\\"\\nbase_url = \\\"https://old-relay.example.com/v1\\\"\"}","notes":"deleted provider kept as backup"}},"models":{"gpt-5.4":{}}},"diagnostics":null,"created_at":"2026-05-20T09:31:00.000000000+08:00","updated_at":"2026-10-17T10:05:00.000000000+08:00"}'), '2026-05-20T09:31:00.000000000+08:00', '2026-10-17T10:05:00.000000000+08:00');
INSERT INTO opencode_favorite_provider (id, data, created_at, updated_at) VALUES ('19b0c1d2e3f44a5b6c7d8e9f0a1b2c3d', jsonb('{"provider_id":"opencode:openrouter","npm":"@openrouter/ai-sdk-provider","base_url":"","provider_config":{"npm":"@openrouter/ai-sdk-provider","name":"OpenRouter","options":{"apiKey":"sk-test-or-000"},"models":{"openai/gpt-5.5":{}}},"diagnostics":null,"created_at":"2026-05-20T09:31:00.000000000+08:00","updated_at":"2026-10-17T10:05:00.000000000+08:00"}'), '2026-05-20T09:31:00.000000000+08:00', '2026-10-17T10:05:00.000000000+08:00');
COMMIT;
PRAGMA user_version = 29;
