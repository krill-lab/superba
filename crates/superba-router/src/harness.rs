//! How a coding tool reaches the local router: the ingress of the protocol it
//! speaks and its own harness token ([`crate::token::harness_token`]), so the
//! router records its requests as that tool's.
//!
//! | Protocol | Ingress | The tool appends |
//! |---|---|---|
//! | OpenAI Responses, Chat Completions | `http://127.0.0.1:<port>/openai/v1` ([`crate::openai`]) | `/responses`, `/chat/completions`, `/models` |
//! | Anthropic Messages | `http://127.0.0.1:<port>/claude` ([`crate::claude`]) | `/v1/messages` |
//!
//! The switch service writes [`HarnessRoute::base_url`] and
//! [`HarnessRoute::token`] into a tool's configuration in routed mode
//! (Claude Code: `superba_switch::claude::claude_router_route` does the
//! same for `/claude`). The harness writers name a tool by its harness id,
//! which is [`RequestHarness::id`] (`RequestHarness::from_id`).

use std::net::SocketAddr;

use superba_core::{RequestHarness, RequestProtocol, Secret};

/// A tool's way into the router.
#[derive(Clone, Debug)]
pub struct HarnessRoute {
    /// The router's own base URL (`http://127.0.0.1:<port>`), for callers
    /// that append the ingress path themselves.
    pub root: String,
    /// The ingress base URL of the protocol (`…/openai/v1`, `…/claude`).
    pub base_url: String,
    /// The tool's own token (bearer or `x-api-key`).
    pub token: Secret,
}

/// The ingress base URL of `protocol` on a router listening on `listen`.
pub fn ingress_url(listen: SocketAddr, protocol: RequestProtocol) -> String {
    match protocol {
        RequestProtocol::Messages => crate::claude::base_url(listen),
        _ => crate::openai::base_url(listen),
    }
}

/// The route of `harness` speaking `protocol` on a router listening on
/// `listen` whose router token is `router_token`; `host` as for
/// [`crate::token::harness_token`] (`None` = this computer, else
/// `wsl:<distro>`). `None` for an invalid host.
pub fn route(
    listen: SocketAddr,
    router_token: &[u8],
    harness: RequestHarness,
    protocol: RequestProtocol,
    host: Option<&str>,
) -> Option<HarnessRoute> {
    let token = crate::token::harness_token(router_token, harness, host)?;
    Some(HarnessRoute {
        root: format!("http://{listen}"),
        base_url: ingress_url(listen, protocol),
        token: Secret::new(token),
    })
}
