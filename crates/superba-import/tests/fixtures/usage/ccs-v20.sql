-- Fixture: the usage tables of cc-switch.db at schema v20 (cc-switch HEAD 77c6dc7,
-- 2026-10-18), DDL copied from src-tauri/src/database/schema.rs (create_tables_on_conn),
-- applied on top of ../ccs/schema-v20.sql (providers). Rows are invented:
--   req-total   codex proxy, input_token_semantics 1 (input includes cache reads/writes)
--   req-fresh   codex proxy, semantics 2 (fresh input), cost 0 with tokens (no price)
--   req-failed  codex proxy, HTTP 502
--   req-claude  claude proxy (another app: counted, not imported)
--   codex_session:thread-v1:<T-ON-DISK>:0   from Codex's session files, thread still on disk
--   codex_session:thread-v1:<T-GONE>:0      from Codex's session files, thread deleted since
-- Daily totals (pruned days): a codex proxy day, a codex session day, a claude day.

CREATE TABLE IF NOT EXISTS proxy_request_logs (
            request_id TEXT PRIMARY KEY, provider_id TEXT NOT NULL, app_type TEXT NOT NULL, model TEXT NOT NULL,
            request_model TEXT,
            pricing_model TEXT,
            input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
            cache_read_tokens INTEGER NOT NULL DEFAULT 0, cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
            input_token_semantics INTEGER NOT NULL DEFAULT 0,
            input_cost_usd TEXT NOT NULL DEFAULT '0', output_cost_usd TEXT NOT NULL DEFAULT '0',
            cache_read_cost_usd TEXT NOT NULL DEFAULT '0', cache_creation_cost_usd TEXT NOT NULL DEFAULT '0',
            total_cost_usd TEXT NOT NULL DEFAULT '0', latency_ms INTEGER NOT NULL, first_token_ms INTEGER,
            duration_ms INTEGER, status_code INTEGER NOT NULL, error_message TEXT, session_id TEXT,
            provider_type TEXT, is_streaming INTEGER NOT NULL DEFAULT 0,
            cost_multiplier TEXT NOT NULL DEFAULT '1.0', created_at INTEGER NOT NULL,
            data_source TEXT NOT NULL DEFAULT 'proxy'
        );

CREATE TABLE IF NOT EXISTS usage_daily_rollups (
                date TEXT NOT NULL,
                app_type TEXT NOT NULL,
                provider_id TEXT NOT NULL,
                model TEXT NOT NULL,
                request_model TEXT NOT NULL DEFAULT '',
                pricing_model TEXT NOT NULL DEFAULT '',
                request_count INTEGER NOT NULL DEFAULT 0,
                success_count INTEGER NOT NULL DEFAULT 0,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
                input_token_semantics INTEGER NOT NULL DEFAULT 0,
                total_cost_usd TEXT NOT NULL DEFAULT '0',
                avg_latency_ms INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (date, app_type, provider_id, model, request_model, pricing_model)
            );

INSERT INTO proxy_request_logs (request_id, provider_id, app_type, model, request_model, pricing_model,
    input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, input_token_semantics,
    total_cost_usd, latency_ms, first_token_ms, duration_ms, status_code, error_message, session_id,
    is_streaming, created_at, data_source) VALUES
  ('req-total', '3f6a9b2c-2222-4d1e-8f00-000000000002', 'codex', 'gpt-6.1-sol', 'gpt-6.1-sol', 'gpt-6.1-sol',
    12000, 800, 9000, 0, 1, '0.031250', 4000, 900, 4000, 200, NULL, '019a7c00-0000-7000-8000-00000000aaa1',
    1, 1791619200, 'proxy'),
  ('req-fresh', 'universal-codex-u-test-01', 'codex', 'gpt-5.5', 'gpt-5.5', '',
    3000, 200, 1000, 500, 2, '0', 2500, NULL, 2500, 200, NULL, NULL,
    1, 1791619500, 'proxy'),
  ('req-failed', 'universal-codex-u-test-01', 'codex', 'gpt-5.5', 'gpt-5.5', '',
    0, 0, 0, 0, 1, '0', 300, NULL, NULL, 502, 'upstream said: secret prompt text', NULL,
    1, 1791619600, 'proxy'),
  ('req-claude', 'claude-p', 'claude', 'claude-x', 'claude-x', 'claude-x',
    100, 10, 0, 0, 2, '0.002', 900, NULL, 900, 200, NULL, NULL,
    1, 1791619700, 'proxy'),
  ('codex_session:thread-v1:019a7c00-0000-7000-8000-0000000d15c0:0', '_codex_session', 'codex', 'gpt-6.1-sol', 'gpt-6.1-sol', NULL,
    5000, 100, 4000, 0, 1, '0.004', 1200, 300, NULL, 200, NULL, '019a7c00-0000-7000-8000-0000000d15c0',
    1, 1791709200, 'codex_session'),
  ('codex_session:thread-v1:019a7c00-0000-7000-8000-00000000901e:0', '_codex_session', 'codex', 'gpt-6.1-sol', 'gpt-6.1-sol', NULL,
    7000, 300, 6000, 0, 1, '0.006', 1500, NULL, NULL, 200, NULL, '019a7c00-0000-7000-8000-00000000901e',
    1, 1791709300, 'codex_session');

INSERT INTO usage_daily_rollups (date, app_type, provider_id, model, request_model, pricing_model,
    request_count, success_count, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
    input_token_semantics, total_cost_usd, avg_latency_ms) VALUES
  ('2026-08-01', 'codex', '3f6a9b2c-2222-4d1e-8f00-000000000002', 'gpt-6.1-sol', 'gpt-6.1-sol', 'gpt-6.1-sol', 40, 39, 400000, 20000, 300000, 0, 1, '1.250000', 3000),
  ('2026-08-01', 'codex', '_codex_session', 'gpt-6.1-sol', '', '', 12, 12, 90000, 3000, 80000, 0, 1, '0.120000', 0),
  ('2026-08-02', 'claude', 'claude-p', 'claude-x', '', '', 5, 5, 500, 50, 0, 0, 2, '0.010000', 800);
