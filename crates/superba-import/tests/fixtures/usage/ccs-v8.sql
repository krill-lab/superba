-- Fixture: an older cc-switch usage table (before input_token_semantics, pricing_model and
-- data_source; daily totals not yet introduced), as created by cc-switch's
-- create_tables for the proxy feature (schema.rs, proxy_request_logs without the later
-- columns). Applied on top of ../ccs/schema-v1.sql. Rows are invented.

CREATE TABLE IF NOT EXISTS proxy_request_logs (
            request_id TEXT PRIMARY KEY, provider_id TEXT NOT NULL, app_type TEXT NOT NULL, model TEXT NOT NULL,
            input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
            cache_read_tokens INTEGER NOT NULL DEFAULT 0, cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
            input_cost_usd TEXT NOT NULL DEFAULT '0', output_cost_usd TEXT NOT NULL DEFAULT '0',
            cache_read_cost_usd TEXT NOT NULL DEFAULT '0', cache_creation_cost_usd TEXT NOT NULL DEFAULT '0',
            total_cost_usd TEXT NOT NULL DEFAULT '0', latency_ms INTEGER NOT NULL,
            status_code INTEGER NOT NULL, error_message TEXT, created_at INTEGER NOT NULL
        );

INSERT INTO proxy_request_logs (request_id, provider_id, app_type, model, input_tokens, output_tokens,
    cache_read_tokens, cache_creation_tokens, total_cost_usd, latency_ms, status_code, created_at) VALUES
  ('old-1', '3f6a9b2c-2222-4d1e-8f00-000000000002', 'codex', 'gpt-5.5', 2000, 100, 1500, 0, '0.0021', 3000, 200, 1791619200),
  ('old-2', 'claude-p', 'claude', 'claude-x', 10, 1, 0, 0, '0.0001', 100, 200, 1791619300);
