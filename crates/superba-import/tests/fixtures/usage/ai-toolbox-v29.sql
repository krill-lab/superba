-- Fixture: the usage tables of ai-toolbox.db at schema v29 (AI Toolbox HEAD 2be684a,
-- 2026-10-18): create_proxy_gateway_usage_tables (migrations.rs, v2) plus every column
-- added through v29 and the v29 rebuild of usage_daily_rollups. Applied on top of
-- ../ai-toolbox/schema-v29.sql (providers). Rows are invented:
--   gw-1        codex proxy, fresh input (cache reads/writes separate), completed stream
--   gw-models   codex GET /models (usage_request_count 0: not a model request)
--   gw-failed   codex proxy, stream failed
--   gw-claude   claude proxy (another app)
--   SESSION:codex:session:<T-ON-DISK>:1  from Codex's session files, thread still on disk
-- Daily totals: a codex proxy day (two projects), a codex session day.

CREATE TABLE IF NOT EXISTS proxy_request_logs (
            request_id TEXT PRIMARY KEY NOT NULL,
            provider_id TEXT NOT NULL,
            app_type TEXT NOT NULL,
            model TEXT NOT NULL,
            request_model TEXT,
            input_tokens INTEGER NOT NULL DEFAULT 0,
            output_tokens INTEGER NOT NULL DEFAULT 0,
            cache_read_tokens INTEGER NOT NULL DEFAULT 0,
            cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
            input_cost_usd TEXT NOT NULL DEFAULT '0',
            output_cost_usd TEXT NOT NULL DEFAULT '0',
            cache_read_cost_usd TEXT NOT NULL DEFAULT '0',
            cache_creation_cost_usd TEXT NOT NULL DEFAULT '0',
            total_cost_usd TEXT NOT NULL DEFAULT '0',
            latency_ms INTEGER NOT NULL DEFAULT 0,
            first_token_ms INTEGER,
            duration_ms INTEGER,
            status_code INTEGER NOT NULL DEFAULT 0,
            error_message TEXT,
            session_id TEXT,
            provider_type TEXT,
            is_streaming INTEGER NOT NULL DEFAULT 0,
            cost_multiplier TEXT NOT NULL DEFAULT '1.0',
            pricing_model_source TEXT NOT NULL DEFAULT 'upstream',
            created_at INTEGER NOT NULL,
            data_source TEXT NOT NULL DEFAULT 'proxy',
            detail_file TEXT,
            detail_offset INTEGER,
            route_name TEXT,
            method TEXT,
            path TEXT,
            upstream_status_code INTEGER,
            stream_outcome TEXT,
            error_category TEXT,
            attempt_count INTEGER NOT NULL DEFAULT 1,
            total_attempt_count INTEGER NOT NULL DEFAULT 1,
            reasoning_effort TEXT,
            usage_metadata BLOB,
            usage_request_count INTEGER NOT NULL DEFAULT 1,
            extra_tokens INTEGER NOT NULL DEFAULT 0,
            transport TEXT NOT NULL DEFAULT 'http',
            request_kind TEXT NOT NULL DEFAULT 'request',
            service_tier TEXT,
            service_tier_multiplier TEXT,
            compaction_mode TEXT,
            project_path TEXT,
            credits REAL
        );

CREATE TABLE usage_daily_rollups (
            date TEXT NOT NULL,
            app_type TEXT NOT NULL,
            provider_id TEXT NOT NULL,
            model TEXT NOT NULL,
            project_path TEXT NOT NULL DEFAULT '',
            request_count INTEGER NOT NULL DEFAULT 0,
            success_count INTEGER NOT NULL DEFAULT 0,
            input_tokens INTEGER NOT NULL DEFAULT 0,
            output_tokens INTEGER NOT NULL DEFAULT 0,
            cache_read_tokens INTEGER NOT NULL DEFAULT 0,
            cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
            total_cost_usd TEXT NOT NULL DEFAULT '0',
            avg_latency_ms INTEGER NOT NULL DEFAULT 0,
            latency_sample_count INTEGER,
            extra_tokens INTEGER NOT NULL DEFAULT 0,
            credits REAL NOT NULL DEFAULT 0,
            PRIMARY KEY (date, app_type, provider_id, model, project_path)
        );

INSERT INTO proxy_request_logs (request_id, provider_id, app_type, model, request_model,
    input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, total_cost_usd,
    latency_ms, first_token_ms, duration_ms, status_code, error_message, session_id, created_at,
    data_source, method, path, stream_outcome, usage_request_count, request_kind, service_tier) VALUES
  ('gw-1', '4a1d2c3b5e6f47a8b9c0d1e2f3a4b5c6', 'codex', 'gpt-6.1-sol', 'gpt-6.1-sol',
    3000, 700, 9000, 0, '0.028100', 850, 850, 5200, 200, NULL, '019a7c00-0000-7000-8000-00000000bbb1', 1791619200,
    'proxy', 'POST', '/openai/v1/responses', 'completed', 1, 'request', 'priority'),
  ('gw-models', '4a1d2c3b5e6f47a8b9c0d1e2f3a4b5c6', 'codex', 'unknown', NULL,
    0, 0, 0, 0, '0', 40, NULL, 40, 200, NULL, NULL, 1791619210,
    'proxy', 'GET', '/openai/v1/models', NULL, 0, 'request', NULL),
  ('gw-failed', '4a1d2c3b5e6f47a8b9c0d1e2f3a4b5c6', 'codex', 'gpt-6.1-sol', 'gpt-6.1-sol',
    100, 0, 0, 0, '0.000125', 900, NULL, 900, 200, 'a prompt fragment that must not be imported', NULL, 1791619300,
    'proxy', 'POST', '/openai/v1/responses', 'failed', 1, 'request', NULL),
  ('gw-claude', 'claude-provider', 'claude', 'claude-x', NULL,
    10, 1, 0, 0, '0.0001', 100, NULL, 100, 200, NULL, NULL, 1791619400,
    'proxy', 'POST', '/v1/messages', 'completed', 1, 'request', NULL),
  ('SESSION:codex:session:019a7c00-0000-7000-8000-0000000d15c0:1', 'session', 'codex', 'gpt-6.1-sol', NULL,
    1000, 50, 4000, 0, '0.003', 0, NULL, NULL, 200, NULL, '019a7c00-0000-7000-8000-0000000d15c0', 1791709200,
    'session', NULL, NULL, NULL, 1, 'request', NULL);

INSERT INTO usage_daily_rollups (date, app_type, provider_id, model, project_path, request_count,
    success_count, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
    total_cost_usd) VALUES
  ('2026-08-03', 'codex', '4a1d2c3b5e6f47a8b9c0d1e2f3a4b5c6', 'gpt-6.1-sol', '/work/a', 10, 10, 20000, 4000, 80000, 0, '0.400000'),
  ('2026-08-03', 'codex', '4a1d2c3b5e6f47a8b9c0d1e2f3a4b5c6', 'gpt-6.1-sol', '/work/b', 5, 4, 10000, 2000, 40000, 0, '0'),
  ('2026-08-03', 'codex', 'session', 'gpt-6.1-sol', '', 7, 7, 7000, 700, 21000, 0, '0.070000');
