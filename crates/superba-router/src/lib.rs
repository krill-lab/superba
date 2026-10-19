//! # superba-router
//!
//! Superba's opt-in local smart router (v2 decision 6): a per-user background
//! service on `127.0.0.1` that Codex is pointed at in API-key mode. It forwards
//! each request to one of its members — Krill lines and custom OpenAI-compatible
//! sites, each with its own API key — and fails over across lines and members
//! before the first response byte. Remote hosts and the official ChatGPT login are
//! never routed through it.
//!
//! ## Contract with Codex (router mode, verified in the router spike)
//!
//! | File | Value |
//! |---|---|
//! | `config.toml` | `model_provider = "openai"`, `openai_base_url = "http://127.0.0.1:<port>/backend-api/codex"`, no `chatgpt_base_url` |
//! | `auth.json` | `{"auth_mode":"apikey","OPENAI_API_KEY":"<router token>"}` |
//!
//! The router requires the router token and replaces `Authorization` with the
//! chosen member's own key; Codex's credential never reaches a member, and a
//! Krill key never reaches a custom site. PAT is not offered in router mode
//! (`chatgpt_base_url` must be https while PAT is in use).
//!
//! ## Behaviour
//!
//! - **Stickiness** ([`sticky`]): by the `session-id` header (then `thread-id`,
//!   `x-client-request-id`) with a 2 h idle TTL; the member changes only on
//!   failover and stays changed.
//! - **Failover** ([`select`], `proxy`): one attempt per candidate, only before
//!   any byte reached Codex, only on connect/TLS errors, resets before headers,
//!   502/503/504 or an edge interception. Never on 4xx (401/403/429 included) or
//!   500. No time-to-first-byte timeout. A broken stream aborts the downstream
//!   response (Codex re-sends) and marks the line suspect.
//! - **Streaming**: SSE and every other body pass through unbuffered; request
//!   bodies are forwarded byte for byte (`zstd` included) unless a member maps
//!   the model name ([`model_map`]) or the line accepts zstd and Codex sent the
//!   body uncompressed.
//! - **Transports per line** ([`transport`]): Responses request bodies are
//!   zstd-compressed for lines Superba's probe found to accept zstd, sent again
//!   uncompressed (and remembered) when a line refuses one; WebSocket
//!   handshakes skip lines that answered 426. HTTP turns are never upgraded to
//!   an upstream WebSocket (see [`transport`] for why).
//! - **WebSocket**: passthrough pinned per connection; failover only at the
//!   handshake; 426 locally for members without WebSocket.
//! - **Remote compaction** ([`compaction`]): a compacted conversation goes
//!   only to the member that made its `compaction` item (no failover to
//!   members that cannot read it); compaction requests and compacted
//!   conversations of unknown origin skip members marked as not supporting
//!   remote compaction. Bodies pass through untouched.
//! - **Ciphertexts another provider made** ([`cipher`]): a request a member
//!   refuses with `invalid_encrypted_content` (reasoning, sub-agent messages,
//!   encrypted tool outputs made elsewhere) is sent again once without exactly
//!   what it cannot read, and later requests to it leave those items out
//!   before they leave.
//! - **Health** ([`health`]): passive breakers with exponential backoff plus
//!   unauthenticated probes of half-open lines.
//! - **Local budget** ([`limits`]): bounded concurrent requests / WebSocket
//!   sessions and inbound connections sized from the open-file limit, a capped
//!   upstream pool; a local shortage (every admission in use, or "too many open
//!   files") is answered 503 `local_router_busy` and never counted against a
//!   line.
//! - **Admin API** (`/_superba/v1/*`, per-instance token in `runtime.json`):
//!   status and metrics ([`status`]), reload, probe, shutdown; [`client`] calls it.
//! - **Request log and stats line** ([`stats`], [`store`]; only while the
//!   configuration has a `stats` section): one row per Responses request in
//!   `usage.sqlite3` (numbers only, never headers or bodies), the
//!   optional reply stats line, and the strip of earlier stats lines from every
//!   outgoing request. WebSockets are then relayed frame by frame (no
//!   per-message deflate) so `response.create` frames and events can be read.
//! - **Claude Code** ([`claude`]): Anthropic Messages under `/claude/v1/*` on
//!   the same port, passed through to members with an Anthropic endpoint, with
//!   the same failover, stickiness, health, limits, request log and stats line.
//! - **The other coding tools** ([`openai`]): OpenAI Responses and Chat
//!   Completions under `/openai/v1/*` on the same port (dsh, opencode, pi,
//!   ZCode, WorkBuddy), passed through to the members that serve the
//!   protocol, with the same machinery; each tool has its own harness token
//!   ([`harness`]).
//! - **Logging**: never keys, tokens, bodies, thread ids or query strings.
//!
//! ## Files
//!
//! See [`paths::RouterPaths`]: `config.json` (no secret), `secrets.json` (0600:
//! router token + member keys), `runtime.json` (0600: pid, port, admin token).
//!
//! ## Diagnostics
//!
//! [`doctor`] gathers the program, service, configuration, port and log facts
//! behind `spb router doctor`; a start that fails names the service manager's
//! view and the last log lines instead of a bare timeout.

#![warn(missing_docs)]

pub mod cipher;
pub mod claude;
pub mod client;
pub mod compaction;
pub mod config;
pub mod doctor;
pub mod firewall;
pub mod harness;
pub mod health;
pub mod limits;
pub mod manager;
pub mod members;
pub mod model_map;
pub mod openai;
pub mod outage;
pub mod paths;
pub mod port;
pub mod runtime;
pub mod select;
pub mod service;
pub mod stats;
pub mod status;
pub mod sticky;
pub mod store;
pub mod token;
pub mod transport;
pub mod translate;
pub mod watchdog;

mod admin;
mod forward;
mod probe;
mod proxy;
mod server;
mod state;
mod ws;
mod ws_relay;

pub use admin::{ADMIN_PREFIX, VERSION};
pub use claude::ClaudeConfig;
pub use config::{
    LineConfig, MemberConfig, MemberKind, ROUTER_BASE_PATH, RouterConfig, RouterSecrets, Tuning,
};
pub use forward::ROUTE_HEADER;
pub use health::LineState;
pub use openai::OpenAiConfig;
pub use paths::RouterPaths;
pub use runtime::RuntimeInfo;
pub use server::{RouterServer, ShutdownHandle};
pub use stats::{PriceConfig, StatsConfig, StatsLineConfig};
pub use status::{
    HealthReply, LocalProblem, LocalStatus, RouteEvent, RouteEventKind, RouterStatus,
};
pub use store::Reader;
