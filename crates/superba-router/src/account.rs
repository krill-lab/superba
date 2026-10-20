//! The local token represents the router, not any member's upstream account.
//! This stable identity keeps workspace discovery independent of failover.
//! These identity endpoints require the router token; the other ChatGPT
//! backend calls are answered by [`crate::local_backend`] with the same
//! identity.

use bytes::Bytes;
use http::{Method, Response, StatusCode};
use serde_json::json;

use crate::forward::{full, router_error};
use crate::state::RespBody;

/// User and account id of the local identity.
pub(crate) const ACCOUNT: &str = "superba-local-router";
/// Display name of the local identity.
pub(crate) const DISPLAY_NAME: &str = "Superba";

pub(crate) fn handles(path: &str) -> bool {
    matches!(
        path,
        "/v1/user-auth-credential/whoami"
            | "/backend-api/wham/accounts/check"
            | "/backend-api/wham/settings/user"
            | "/backend-api/settings/user"
            | "/wham/settings/user"
    )
}

/// Counter label of an identity endpoint ([`crate::local_backend::note`]).
pub(crate) fn label(path: &str) -> &'static str {
    match path {
        "/v1/user-auth-credential/whoami" => "v1/user-auth-credential/whoami",
        "/backend-api/wham/accounts/check" => "wham/accounts/check",
        "/backend-api/settings/user" => "settings/user",
        _ => "wham/settings/user",
    }
}

pub(crate) fn response(method: &Method, path: &str) -> Response<RespBody> {
    if method != Method::GET {
        return router_error(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "use GET for router account metadata",
        );
    }
    let body = match path {
        "/v1/user-auth-credential/whoami" => json!({
            "email": null,
            "chatgpt_user_id": ACCOUNT,
            "chatgpt_account_id": ACCOUNT,
            "chatgpt_plan_type": "unknown",
            "chatgpt_account_is_fedramp": false
        }),
        "/backend-api/wham/accounts/check" => json!({
            "accounts": [{
                "id": ACCOUNT,
                "name": DISPLAY_NAME,
                "plan_type": "unknown",
                "structure": "personal",
                "workspace_backend_origin": "NO_CONSTRAINT",
                "account_routing_override": "NO_CONSTRAINT"
            }],
            "account_ordering": [ACCOUNT]
        }),
        _ => json!({"commit_attribution_enabled": false}),
    };
    Response::builder()
        .header("content-type", "application/json")
        .header("cache-control", "no-store")
        .body(full(Bytes::from(body.to_string())))
        .expect("static response headers")
}
