use serde::Deserialize;
use serde_json::{Value, json};

use super::*;

fn local(method: Method, path: &str) -> Route {
    route(&method, path).unwrap_or_else(|| panic!("{method} {path} must be answered locally"))
}

fn body(method: Method, path: &str, query: Option<&str>) -> Value {
    match local(method.clone(), path).answer {
        Answer::Json(make) => make(query),
        other => panic!("{method} {path}: expected JSON, got {other:?}"),
    }
}

fn get(path: &str) -> Value {
    body(Method::GET, path, None)
}

#[test]
fn model_traffic_below_the_codex_prefix_goes_upstream() {
    for rest in [
        "/responses",
        "/responses/",
        "/responses/compact",
        "/models",
        "/models/gpt-6",
        "/memories/trace_summarize",
        "/images/generations",
        "/images/edits",
        "/realtime",
        "/realtime/calls",
        "/alpha/search",
        "/alpha/notes/v2/read_file",
        "/alpha/history/v2/read_item",
        "/guardian",
    ] {
        assert!(upstream(rest), "{rest}");
        let path = format!("/backend-api/codex{rest}");
        assert!(route(&Method::POST, &path).is_none(), "{path}");
        assert!(route(&Method::GET, &path).is_none(), "{path}");
    }
    for rest in [
        "",
        "/",
        "/analytics-events/events",
        "/alpha/notes/v2/thread_hint",
        "/alpha",
        "/telemetry",
        "/responsesx",
        "/modelsx",
        "/wham/usage",
    ] {
        assert!(!upstream(rest), "{rest}");
    }
}

#[test]
fn other_ingresses_and_unknown_paths_are_left_to_the_router() {
    for path in [
        "/v1/responses",
        "/v1/models",
        "/v1/user-auth-credential/whoami",
        "/models",
        "/claude/v1/messages",
        "/openai/v1/responses",
        "/gemini/v1beta/models",
        "/_superba/v1/status",
        "/elsewhere",
        "/backend-apix/wham/usage",
        "/oauth/token",
    ] {
        assert!(route(&Method::GET, path).is_none(), "{path}");
        assert!(route(&Method::POST, path).is_none(), "{path}");
    }
}

#[test]
fn labels_name_routes_never_ids() {
    let cases = [
        (
            Method::GET,
            "/backend-api/ps/plugins/installed",
            "ps/plugins/installed",
            false,
        ),
        (
            Method::GET,
            "/backend-api/ps/plugins/installed/",
            "ps/plugins/installed",
            false,
        ),
        (
            Method::GET,
            "/backend-api/ps/plugins/plugin_asdk_app_69a1d78e",
            "ps/plugins/{id}",
            true,
        ),
        (
            Method::POST,
            "/backend-api/ps/plugins/plugin_x/install",
            "ps/plugins/{id}",
            true,
        ),
        (
            Method::GET,
            "/backend-api/ps/plugins/suggested/codex",
            "ps/plugins/suggested/*",
            false,
        ),
        (Method::POST, "/backend-api/ps/mcp", "ps/mcp", false),
        (
            Method::GET,
            "/backend-api/wham/tasks/task_123",
            "wham/*",
            true,
        ),
        (
            Method::GET,
            "/backend-api/wham/remote/control/server/refresh",
            "wham/*",
            true,
        ),
        (Method::POST, "/backend-api/wham/usage", "wham/*", true),
        (
            Method::GET,
            "/backend-api/wham/usage/plan_limit_history",
            "wham/usage/plan_limit_history",
            true,
        ),
        (
            Method::GET,
            "/backend-api/settings/is_adult",
            "settings/is_adult",
            true,
        ),
        (
            Method::GET,
            "/backend-api/accounts/abc/settings",
            "accounts/*",
            true,
        ),
        (
            Method::GET,
            "/backend-api/accounts/check/v4-2023-04-27",
            "accounts/check/*",
            false,
        ),
        (Method::POST, "/backend-api/files", "files/*", true),
        (Method::GET, "/backend-api/somewhere/else", "other", true),
        (Method::GET, "/backend-api", "other", true),
        (
            Method::POST,
            "/backend-api/codex/analytics-events/events",
            "codex/analytics-events/events",
            false,
        ),
        (
            Method::POST,
            "/backend-api/codex/alpha/notes/v2/thread_hint",
            "codex/alpha/notes/v2/thread_hint",
            false,
        ),
        (
            Method::GET,
            "/backend-api/codex/wham/usage",
            "codex/*",
            true,
        ),
        (Method::GET, "/backend-api/codex", "codex/*", true),
        // The same calls without the /backend-api prefix.
        (Method::GET, "/wham/usage", "wham/usage", false),
        (Method::GET, "/me", "me", false),
        (Method::GET, "/profiles/me/page", "profiles/me/page", false),
        (Method::GET, "/settings/voices", "settings/*", true),
    ];
    for (method, path, label, not_found) in cases {
        let r = local(method.clone(), path);
        assert_eq!(r.label, label, "{method} {path}");
        assert_eq!(r.not_found(), not_found, "{method} {path}");
    }
}

#[test]
fn every_shaped_endpoint_answers_json() {
    for (verb, path, answer) in EXACT {
        if let Answer::Json(make) = answer {
            let method = if *verb == Verb::Post {
                Method::POST
            } else {
                Method::GET
            };
            assert!(route(&method, &format!("/backend-api{path}")).is_some());
            let v = make(None);
            assert!(v.is_object() || v.is_array(), "{path}: {v}");
            serde_json::from_str::<Value>(&v.to_string()).unwrap();
        }
    }
    for (_, prefix, _, answer) in PREFIXED {
        if let Answer::Json(make) = answer {
            assert!(make(None).is_object(), "{prefix}");
        }
    }
}

/// Codex's `RateLimitStatusPayload` (codex-backend-openapi-models): windows
/// are i32 when present, `plan_type` one of its names (`unknown` included).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct RateLimitStatus {
    user_id: String,
    account_id: String,
    email: Option<String>,
    plan_type: String,
    rate_limit: Option<RateLimitDetails>,
    credits: Option<Value>,
    spend_control: Option<Value>,
    additional_rate_limits: Option<Vec<Value>>,
    rate_limit_reached_type: Option<Value>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct RateLimitDetails {
    allowed: bool,
    limit_reached: bool,
    primary_window: Option<Window>,
    secondary_window: Option<Window>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct Window {
    used_percent: i32,
    limit_window_seconds: i32,
    reset_after_seconds: i32,
    reset_at: i32,
}

#[test]
fn usage_is_the_local_identity_without_limits() {
    let v = get("/backend-api/wham/usage");
    let usage: RateLimitStatus = serde_json::from_value(v.clone()).unwrap();
    assert_eq!(usage.user_id, crate::account::ACCOUNT);
    assert_eq!(usage.account_id, crate::account::ACCOUNT);
    assert_eq!(usage.plan_type, "unknown");
    let limits = usage.rate_limit.unwrap();
    assert!(limits.allowed && !limits.limit_reached);
    assert!(limits.primary_window.is_none() && limits.secondary_window.is_none());
    assert!(v["credits"].is_null());
    assert_eq!(get("/wham/usage"), v, "same answer without /backend-api");
}

#[test]
fn usage_and_analytics_pages_get_empty_data() {
    for path in [
        "/backend-api/wham/usage/credit-usage-events",
        "/backend-api/wham/analytics/daily-workspace-usage-counts",
        "/backend-api/wham/analytics/daily-plugin-usage-metrics",
        "/backend-api/wham/analytics/daily-skill-usage-metrics",
        "/backend-api/wham/analytics/daily-code-review-metrics",
    ] {
        assert_eq!(get(path), json!({"data": []}), "{path}");
    }
    for path in [
        "/backend-api/wham/usage/daily-token-usage-breakdown",
        "/backend-api/wham/usage/daily-workspace-user-token-usage-breakdown",
    ] {
        let v = get(path);
        assert_eq!(v["data"], json!([]), "{path}");
        let ts = v["data_freshness_ts"].as_str().unwrap();
        assert!(
            ts.len() == 24 && ts.ends_with('Z') && ts.as_bytes()[10] == b'T',
            "{ts}"
        );
    }
    let v2 = body(
        Method::POST,
        "/backend-api/wham/usage/thread_usage/query_v2",
        None,
    );
    assert_eq!(v2["threads"], json!([]));
    assert!(v2["data_as_of"].as_str().unwrap().ends_with('Z'));
    let v1 = body(
        Method::POST,
        "/backend-api/wham/usage/thread_usage/query",
        None,
    );
    assert_eq!(v1, json!({"threads": []}));
    let events = body(
        Method::POST,
        "/backend-api/wham/analytics-events/events",
        None,
    );
    assert_eq!(events, json!({}));
}

#[test]
fn credit_usage_echoes_a_plain_balance_unit() {
    let path = "/backend-api/wham/usage/daily-workspace-user-credit-usage";
    let unit = |q: Option<&str>| body(Method::GET, path, q)["unit"].clone();
    assert_eq!(unit(None), "credit");
    assert_eq!(
        unit(Some("start_date=2026-10-10&breakdown=product")),
        "credit"
    );
    assert_eq!(unit(Some("balance_unit=usd&breakdown=product")), "usd");
    assert_eq!(unit(Some("balance_unit=%3Cb%3E")), "credit");
    assert_eq!(unit(Some("balance_unit=")), "credit");
    let v = body(Method::GET, path, None);
    assert_eq!(v["series"], json!([]));
    assert_eq!(v["data"], json!([]));
}

/// Codex's plugin service responses (core-plugins `remote.rs`).
#[derive(Deserialize)]
#[allow(dead_code)]
struct PluginList {
    plugins: Vec<Value>,
    pagination: Pagination,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct Pagination {
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct Recommended {
    enabled: bool,
    plugins: Vec<Value>,
}

/// Codex's connector directory page (connectors `lib.rs`).
#[derive(Deserialize)]
#[allow(dead_code)]
struct DirectoryList {
    apps: Vec<Value>,
    next_token: Option<String>,
}

#[test]
fn plugin_service_lists_nothing() {
    for path in [
        "/backend-api/ps/plugins/installed",
        "/backend-api/ps/plugins/list",
        "/backend-api/ps/plugins/search",
        "/backend-api/ps/plugins/workspace/shared",
        "/backend-api/ps/plugins/workspace/created",
    ] {
        let list: PluginList = serde_json::from_value(get(path)).unwrap();
        assert!(list.plugins.is_empty() && list.pagination.next_page_token.is_none());
    }
    let suggested: Recommended =
        serde_json::from_value(get("/backend-api/ps/plugins/suggested/codex")).unwrap();
    assert!(!suggested.enabled && suggested.plugins.is_empty());
    let featured: Vec<String> =
        serde_json::from_value(get("/backend-api/plugins/featured")).unwrap();
    assert!(featured.is_empty());
    for path in [
        "/backend-api/connectors/directory/list",
        "/backend-api/connectors/directory/list_workspace",
    ] {
        let page: DirectoryList = serde_json::from_value(get(path)).unwrap();
        assert!(page.apps.is_empty() && page.next_token.is_none());
    }
}

#[test]
fn identity_pages_use_the_local_identity() {
    let me = get("/backend-api/me");
    assert_eq!(me["id"], crate::account::ACCOUNT);
    assert_eq!(me["name"], crate::account::DISPLAY_NAME);
    let profile = get("/backend-api/wham/profiles/me");
    assert_eq!(profile["profile"]["username"], "superba");
    let page = get("/backend-api/profiles/me/page");
    assert_eq!(page["is_self"], true);
    assert!(page["page"]["stats"].is_null());
    assert_eq!(page["page"]["visibility"]["value"], "private");
    for bucket in [
        "daily_usage_buckets",
        "weekly_usage_buckets",
        "cumulative_daily_usage_buckets",
    ] {
        assert_eq!(page["page"]["activity_graph"][bucket], json!([]));
    }
    assert_eq!(get("/backend-api/profiles/me"), page);
    assert_eq!(
        get("/backend-api/profiles/me/page/pin_options"),
        json!({"items": [], "next_cursor": null, "pinned_ids": []})
    );
    let check = get("/backend-api/accounts/check/v4-2023-04-27");
    assert_eq!(check["account_ordering"], json!([crate::account::ACCOUNT]));
    for key in [crate::account::ACCOUNT, "default"] {
        let account = &check["accounts"][key]["account"];
        for field in [
            "account_id",
            "account_user_id",
            "account_user_role",
            "structure",
            "plan_type",
            "is_zdr",
        ] {
            assert!(!account[field].is_null(), "{key}.{field}");
        }
        assert_eq!(account["account_id"], crate::account::ACCOUNT);
        assert_eq!(account["plan_type"], "unknown");
    }
}

#[test]
fn configuration_and_notes_hint_are_empty() {
    assert_eq!(
        get("/backend-api/wham/config/bundle"),
        json!({"config_toml": null, "requirements_toml": null})
    );
    assert_eq!(
        get("/backend-api/wham/workspace-messages"),
        json!({"messages": []})
    );
    assert_eq!(
        body(
            Method::POST,
            "/backend-api/codex/alpha/notes/v2/thread_hint",
            None
        ),
        json!({"text": ""})
    );
    assert_eq!(
        body(
            Method::POST,
            "/backend-api/codex/analytics-events/events",
            None
        ),
        json!({})
    );
}

#[test]
fn mcp_server_initializes_and_lists_nothing() {
    assert!(matches!(
        local(Method::POST, "/backend-api/ps/mcp").answer,
        Answer::Mcp
    ));
    assert!(matches!(
        local(Method::GET, "/backend-api/ps/mcp").answer,
        Answer::Mcp
    ));
    let init = mcp_reply(&json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
        "params": {"protocolVersion": "2025-03-26", "capabilities": {},
                   "clientInfo": {"name": "codex-mcp-client", "version": "0.163.0"}}}))
    .unwrap();
    assert_eq!(init["id"], 0);
    assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
    assert_eq!(
        init["result"]["capabilities"]["tools"]["listChanged"],
        false
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "superba-local-router");
    let newer = mcp_reply(&json!({"jsonrpc": "2.0", "id": "a", "method": "initialize",
        "params": {"protocolVersion": "2099-01-01"}}))
    .unwrap();
    assert_eq!(newer["id"], "a");
    assert_eq!(newer["result"]["protocolVersion"], "2025-06-18");
    let tools = mcp_reply(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap();
    assert_eq!(
        tools,
        json!({"jsonrpc": "2.0", "id": 1, "result": {"tools": []}})
    );
    let ping = mcp_reply(&json!({"jsonrpc": "2.0", "id": 2, "method": "ping"})).unwrap();
    assert_eq!(ping["result"], json!({}));
    let unknown = mcp_reply(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call"})).unwrap();
    assert_eq!(unknown["error"]["code"], -32601);
    assert_eq!(unknown["id"], 3);
    // Notifications and responses are accepted without a reply.
    assert!(mcp_reply(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none());
    assert!(mcp_reply(&json!({"jsonrpc": "2.0", "id": 4, "result": {}})).is_none());
}
