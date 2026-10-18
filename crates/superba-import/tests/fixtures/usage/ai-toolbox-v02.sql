-- Fixture: the first AI Toolbox usage tables (schema v2, migrations.rs
-- create_proxy_gateway_usage_tables before any later column). Rows are invented.

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
            upstream_status_code INTEGER
        );

CREATE TABLE IF NOT EXISTS usage_daily_rollups (
            date TEXT NOT NULL,
            app_type TEXT NOT NULL,
            provider_id TEXT NOT NULL,
            model TEXT NOT NULL,
            request_count INTEGER NOT NULL DEFAULT 0,
            success_count INTEGER NOT NULL DEFAULT 0,
            input_tokens INTEGER NOT NULL DEFAULT 0,
            output_tokens INTEGER NOT NULL DEFAULT 0,
            cache_read_tokens INTEGER NOT NULL DEFAULT 0,
            cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
            total_cost_usd TEXT NOT NULL DEFAULT '0',
            avg_latency_ms INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (date, app_type, provider_id, model)
        );

INSERT INTO proxy_request_logs (request_id, provider_id, app_type, model, input_tokens, output_tokens,
    cache_read_tokens, cache_creation_tokens, total_cost_usd, latency_ms, duration_ms, status_code,
    created_at) VALUES
  ('gw-old', '4a1d2c3b5e6f47a8b9c0d1e2f3a4b5c6', 'codex', 'gpt-5.5', 1000, 100, 3000, 0, '0.0050', 1000, 1000, 200, 1791619200);
INSERT INTO usage_daily_rollups (date, app_type, provider_id, model, request_count, success_count,
    input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, total_cost_usd) VALUES
  ('2026-07-30', 'codex', '4a1d2c3b5e6f47a8b9c0d1e2f3a4b5c6', 'gpt-5.5', 3, 3, 3000, 300, 9000, 0, '0.015');
