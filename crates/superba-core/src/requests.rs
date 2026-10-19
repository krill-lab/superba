//! The request log (Usage page, `spb usage`) and the price data behind its cost
//! estimates.
//!
//! One row per proxied Responses request (HTTP `POST .../responses`,
//! `.../responses/compact`, and each WebSocket `response.create`); turns are a
//! grouping. Rows live in `<data dir>/router/usage.sqlite3`, written only by the
//! router and read read-only by Superba, merged with the model requests read
//! from Codex's rollout files ([`RequestSource`]). Only numbers are recorded:
//! request and response headers and bodies, prompts, outputs, keys and tokens
//! are never stored and never reach a log.
//!
//! Token semantics follow OpenAI: `input_tokens` includes cached and cache-write
//! tokens; `output_tokens` includes reasoning tokens. Unknown = `None`, never 0.
//!
//! Additive owner in v0.3 Phase 1: the router builder.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::codex::{ORIGINATOR_DESKTOP, ORIGINATOR_EXEC, ORIGINATOR_TUI};
use crate::i18n::LocalizedText;
use crate::ids::ProfileId;
use crate::time::Timestamp;
use crate::usage::Usd;

// ---------------------------------------------------------------------------
// Small enums
// ---------------------------------------------------------------------------

/// Where a request row comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RequestSource {
    /// The local router's request log (timing, line, transport, HTTP status).
    #[default]
    Router,
    /// Codex's rollout files (`sessions/**/rollout-*.jsonl`): model, tokens and
    /// derived times only; the router's columns are `None`.
    Rollout,
    /// Imported from cc-switch's usage history (its recorded cost and
    /// provider names; see [`crate::UsageImportReport`]).
    CcSwitch,
    /// Imported from AI Toolbox's usage history.
    AiToolbox,
}

impl RequestSource {
    /// Display text.
    pub fn text(self) -> LocalizedText {
        match self {
            RequestSource::Router => LocalizedText::new("request_source.router", "Local router"),
            RequestSource::Rollout => {
                LocalizedText::new("request_source.rollout", "Codex session files")
            }
            RequestSource::CcSwitch => {
                LocalizedText::new("request_source.ccs", "Imported from CC Switch")
            }
            RequestSource::AiToolbox => {
                LocalizedText::new("request_source.ai_toolbox", "Imported from AI Toolbox")
            }
        }
    }

    /// The tool an imported row comes from.
    pub fn imported_from(self) -> Option<crate::ImportSource> {
        match self {
            RequestSource::CcSwitch => Some(crate::ImportSource::Ccs),
            RequestSource::AiToolbox => Some(crate::ImportSource::AiToolbox),
            _ => None,
        }
    }

    /// The source of rows imported from `tool`.
    pub fn of_import(tool: crate::ImportSource) -> Self {
        match tool {
            crate::ImportSource::Ccs => RequestSource::CcSwitch,
            _ => RequestSource::AiToolbox,
        }
    }

    /// Every source, in the order the Usage page's filter lists them.
    pub const ALL: [RequestSource; 4] = [
        RequestSource::Router,
        RequestSource::Rollout,
        RequestSource::CcSwitch,
        RequestSource::AiToolbox,
    ];

    /// Stable id (`router`, `rollout`, `ccs`, `ai-toolbox`), as `spb usage
    /// --source` takes it.
    pub fn id(self) -> &'static str {
        match self {
            RequestSource::Router => "router",
            RequestSource::Rollout => "rollout",
            RequestSource::CcSwitch => "ccs",
            RequestSource::AiToolbox => "ai-toolbox",
        }
    }
}

/// How the request reached the router.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RequestTransport {
    /// HTTP (SSE response).
    #[default]
    Http,
    /// A `response.create` frame on a Responses WebSocket.
    WebSocket,
}

/// How the request body reached the upstream. The transport itself is the same
/// on both legs ([`RequestTransport`]): the router never turns an HTTP request
/// into a WebSocket or back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RequestCompression {
    /// Sent uncompressed, as Codex sent it (WebSocket frames: no per-message
    /// deflate on the relayed legs).
    #[default]
    None,
    /// Codex sent a zstd body; passed through unchanged.
    ClientZstd,
    /// The router compressed the body with zstd for this line (the line
    /// accepts zstd request bodies).
    RouterZstd,
    /// The line refused the router's zstd body (400 / 415 / 422); the request
    /// was sent again uncompressed and the line is no longer compressed for.
    ZstdRefused,
}

impl RequestCompression {
    /// True when the upstream received a zstd body.
    pub fn is_zstd(self) -> bool {
        matches!(
            self,
            RequestCompression::ClientZstd | RequestCompression::RouterZstd
        )
    }

    /// Display text.
    pub fn text(self) -> LocalizedText {
        match self {
            RequestCompression::None => {
                LocalizedText::new("request_compression.none", "Uncompressed")
            }
            RequestCompression::ClientZstd => {
                LocalizedText::new("request_compression.client_zstd", "zstd (from Codex)")
            }
            RequestCompression::RouterZstd => LocalizedText::new(
                "request_compression.router_zstd",
                "zstd (compressed by the router)",
            ),
            RequestCompression::ZstdRefused => LocalizedText::new(
                "request_compression.zstd_refused",
                "Uncompressed (zstd refused, sent again)",
            ),
        }
    }
}

/// Which Codex client sent the request (from the `originator` header).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RequestClient {
    /// The desktop app (`Codex Desktop`).
    Desktop,
    /// The interactive CLI (`codex_cli_rs`).
    Tui,
    /// `codex exec` (`codex_exec`).
    Exec,
    /// Anything else or no originator.
    #[default]
    Other,
}

impl RequestClient {
    /// Classify an `originator` header value.
    pub fn from_originator(originator: Option<&str>) -> RequestClient {
        match originator.map(str::trim) {
            Some(ORIGINATOR_DESKTOP) => RequestClient::Desktop,
            Some(ORIGINATOR_TUI) => RequestClient::Tui,
            Some(ORIGINATOR_EXEC) => RequestClient::Exec,
            _ => RequestClient::Other,
        }
    }

    /// Display text.
    pub fn text(self) -> LocalizedText {
        match self {
            RequestClient::Desktop => LocalizedText::new("request_client.desktop", "Desktop app"),
            RequestClient::Tui => LocalizedText::new("request_client.tui", "Terminal"),
            RequestClient::Exec => LocalizedText::new("request_client.exec", "codex exec"),
            RequestClient::Other => LocalizedText::new("request_client.other", "Other"),
        }
    }
}

/// The tool (harness) that sent a request. Decided by the router token it
/// presented (Superba gives each harness its own derived token), else by its
/// `User-Agent` ([`RequestHarness::from_user_agent`]).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RequestHarness {
    /// OpenAI Codex (desktop app, terminal, `codex exec`).
    #[default]
    Codex,
    /// Claude Code.
    ClaudeCode,
    /// dsh, the DeepSeek Harness.
    Dsh,
    /// opencode (OpenCode 1 or 2).
    #[serde(rename = "opencode")]
    OpenCode,
    /// pi.
    Pi,
    /// ZCode (Z.ai).
    #[serde(rename = "zcode")]
    ZCode,
    /// WorkBuddy (Tencent; China and international builds).
    #[serde(rename = "workbuddy")]
    WorkBuddy,
    /// Gemini CLI.
    Gemini,
    /// A client that could not be identified.
    Unknown,
}

impl RequestHarness {
    /// Every harness, in display order.
    pub const ALL: [RequestHarness; 9] = [
        RequestHarness::Codex,
        RequestHarness::ClaudeCode,
        RequestHarness::Dsh,
        RequestHarness::OpenCode,
        RequestHarness::Pi,
        RequestHarness::ZCode,
        RequestHarness::WorkBuddy,
        RequestHarness::Gemini,
        RequestHarness::Unknown,
    ];

    /// Stable id (`codex`, `claude-code`, `dsh`, `opencode`, `pi`, `zcode`,
    /// `workbuddy`, `unknown`), as tokens and `spb` name it. The ids of the
    /// other coding tools are the harness ids of their configuration
    /// (`spb harness`).
    pub fn id(self) -> &'static str {
        match self {
            RequestHarness::Codex => "codex",
            RequestHarness::ClaudeCode => "claude-code",
            RequestHarness::Dsh => "dsh",
            RequestHarness::OpenCode => "opencode",
            RequestHarness::Pi => "pi",
            RequestHarness::ZCode => "zcode",
            RequestHarness::WorkBuddy => "workbuddy",
            RequestHarness::Gemini => "gemini",
            RequestHarness::Unknown => "unknown",
        }
    }

    /// The harness of an [`id`](Self::id).
    pub fn from_id(id: &str) -> Option<RequestHarness> {
        Self::ALL.into_iter().find(|h| h.id() == id)
    }

    /// Display text.
    pub fn text(self) -> LocalizedText {
        match self {
            RequestHarness::Codex => LocalizedText::new("request_harness.codex", "Codex"),
            RequestHarness::ClaudeCode => {
                LocalizedText::new("request_harness.claude_code", "Claude Code")
            }
            RequestHarness::Dsh => LocalizedText::new("request_harness.dsh", "dsh"),
            RequestHarness::OpenCode => LocalizedText::new("request_harness.opencode", "opencode"),
            RequestHarness::Pi => LocalizedText::new("request_harness.pi", "pi"),
            RequestHarness::ZCode => LocalizedText::new("request_harness.zcode", "ZCode"),
            RequestHarness::WorkBuddy => {
                LocalizedText::new("request_harness.workbuddy", "WorkBuddy")
            }
            RequestHarness::Gemini => LocalizedText::new("request_harness.gemini", "Gemini"),
            RequestHarness::Unknown => LocalizedText::new("request_harness.unknown", "Other tool"),
        }
    }

    /// The harness and its version named by a `User-Agent` value (the
    /// fallback when the router token does not say):
    ///
    /// | `User-Agent` | Harness, version |
    /// |---|---|
    /// | `claude-cli/2.1.302 (external, cli)` | Claude Code 2.1.302 |
    /// | `codex_cli_rs/0.162.0 (…)`, `Codex Desktop/…` | Codex |
    /// | `deepseek-harness/0.2.1-rc.2 (+https://…)` | dsh 0.2.1-rc.2 |
    /// | `opencode/1.19.3 ai-sdk/…`, `opencode/latest/2.0.32/cli` | opencode 1.19.3, 2.0.32 |
    /// | `pi (linux 6.1.0; x64)` | pi (it sends no version) |
    /// | `ZCode/3.15.3 ai-sdk/…` | ZCode 3.15.3 |
    /// | `CLI/2.161.0 WorkBuddy/2.161.0` | WorkBuddy 2.161.0 (its engine) |
    pub fn from_user_agent(user_agent: &str) -> Option<(RequestHarness, Option<String>)> {
        let ua = user_agent.trim();
        // WorkBuddy's engine names itself after a generic first product.
        if let Some(v) = ua.split_whitespace().find_map(|t| {
            t.get(..10)
                .filter(|p| p.eq_ignore_ascii_case("workbuddy/"))
                .map(|_| &t[10..])
        }) {
            return Some((RequestHarness::WorkBuddy, clean_version(Some(v))));
        }
        let product = ua.split([' ', '(']).next()?;
        let (name, version) = match product.split_once('/') {
            Some((n, v)) => (n, Some(v)),
            None => (product, None),
        };
        let name = name.to_ascii_lowercase();
        let harness = match name.as_str() {
            "claude-cli" | "claude-code" => RequestHarness::ClaudeCode,
            "deepseek-harness" => RequestHarness::Dsh,
            "opencode" => RequestHarness::OpenCode,
            "zcode" => RequestHarness::ZCode,
            "pi" => RequestHarness::Pi,
            "gemini-cli" | "gemini" => RequestHarness::Gemini,
            n if n.starts_with("codex") => RequestHarness::Codex,
            _ => return None,
        };
        // `opencode/<channel>/<version>/<client>`: the segment that is a
        // version.
        let version = version.map(|v| match v.contains('/') {
            true => v
                .split('/')
                .find(|s| s.starts_with(|c: char| c.is_ascii_digit()))
                .unwrap_or(""),
            false => v,
        });
        Some((harness, clean_version(version)))
    }
}

/// A version from a `User-Agent`: trimmed, non-empty, at most 64 characters.
fn clean_version(v: Option<&str>) -> Option<String> {
    v.map(str::trim)
        .filter(|v| !v.is_empty() && v.len() <= 64)
        .map(str::to_owned)
}

/// The API protocol a tool spoke to the router.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RequestProtocol {
    /// OpenAI Responses (`/responses`, HTTP or WebSocket).
    #[default]
    Responses,
    /// OpenAI Chat Completions (`/chat/completions`).
    ChatCompletions,
    /// Anthropic Messages (`/v1/messages`).
    Messages,
    /// Gemini v1beta generateContent.
    Gemini,
}

impl RequestProtocol {
    /// Display text.
    pub fn text(self) -> LocalizedText {
        match self {
            RequestProtocol::Responses => {
                LocalizedText::new("request_protocol.responses", "Responses API")
            }
            RequestProtocol::ChatCompletions => {
                LocalizedText::new("request_protocol.chat_completions", "Chat Completions")
            }
            RequestProtocol::Gemini => LocalizedText::new("request_protocol.gemini", "Gemini"),
            RequestProtocol::Messages => {
                LocalizedText::new("request_protocol.messages", "Anthropic Messages")
            }
        }
    }
}

/// Which agent of a session sent a request.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AgentKind {
    /// The main conversation.
    #[default]
    Main,
    /// A subagent (Codex sub-agent thread, Claude Code Task agent).
    Subagent,
    /// Background work: titles, memory, compaction, side questions.
    Background,
}

impl AgentKind {
    /// Display text.
    pub fn text(self) -> LocalizedText {
        match self {
            AgentKind::Main => LocalizedText::new("agent_kind.main", "Main"),
            AgentKind::Subagent => LocalizedText::new("agent_kind.subagent", "Subagent"),
            AgentKind::Background => LocalizedText::new("agent_kind.background", "Background"),
        }
    }
}

/// `request_kind` of Codex's turn metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RequestKind {
    /// A user turn (or a tool-loop request of one).
    Turn,
    /// Connection prewarm (`generate: false`).
    Prewarm,
    /// Remote compaction.
    Compaction,
    /// Memory extraction.
    Memory,
    /// Any other value.
    #[serde(other)]
    Other,
}

/// Outcome of one request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RequestStatus {
    /// `response.completed` (or a successful non-streaming answer).
    #[default]
    Ok,
    /// An HTTP error status from the upstream or the router itself.
    Http {
        /// Status code.
        status: u16,
        /// Short error code (`router_unavailable`, upstream `error.code`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
    /// `response.failed`.
    Failed {
        /// `error.code`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
    /// `response.incomplete`.
    Incomplete {
        /// `incomplete_details.reason`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// The upstream broke the stream.
    Broken,
    /// Codex went away before the end (Stop, turn aborted).
    Cancelled,
}

impl RequestStatus {
    /// True for every outcome except [`RequestStatus::Ok`].
    pub fn is_error(&self) -> bool {
        !matches!(self, RequestStatus::Ok)
    }

    /// Display text.
    pub fn text(&self) -> LocalizedText {
        match self {
            RequestStatus::Ok => LocalizedText::new("request_status.ok", "OK"),
            RequestStatus::Http { status, code } => {
                LocalizedText::new("request_status.http", "HTTP {status} {code}")
                    .arg("status", status)
                    .arg("code", code.as_deref().unwrap_or(""))
            }
            RequestStatus::Failed { code } => {
                LocalizedText::new("request_status.failed", "Failed {code}")
                    .arg("code", code.as_deref().unwrap_or(""))
            }
            RequestStatus::Incomplete { reason } => {
                LocalizedText::new("request_status.incomplete", "Incomplete {reason}")
                    .arg("reason", reason.as_deref().unwrap_or(""))
            }
            RequestStatus::Broken => LocalizedText::new("request_status.broken", "Stream broken"),
            RequestStatus::Cancelled => LocalizedText::new("request_status.cancelled", "Cancelled"),
        }
    }
}

// ---------------------------------------------------------------------------
// Rows, queries, pages
// ---------------------------------------------------------------------------

/// One recorded request (spike RS 1.4 columns plus the detail fields).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestRow {
    /// Row id (increasing).
    pub id: i64,
    /// When the router received the request (`t0`).
    pub started_at: Timestamp,
    /// The body's `prompt_cache_key` (one per Codex thread; `<thread id>` or
    /// `<source>:<parent thread id>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    /// Thread id from the turn metadata / session headers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// Turn id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    /// Router member (profile) that answered.
    pub member_id: ProfileId,
    /// Its display name.
    #[serde(default)]
    pub member_name: String,
    /// Line that answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_id: Option<String>,
    /// Its display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_name: Option<String>,
    /// Host of the line that answered (`api.example.com`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_host: Option<String>,
    /// Failover attempts before the answering line.
    #[serde(default)]
    pub failovers: u32,
    /// Transport.
    #[serde(default)]
    pub transport: RequestTransport,
    /// Raw `originator` header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub originator: Option<String>,
    /// Client derived from the originator.
    #[serde(default)]
    pub client: RequestClient,
    /// Turn metadata `request_kind`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_kind: Option<RequestKind>,
    /// Turn metadata `thread_source` (`user`, `thread_title`, `subagent`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_source: Option<String>,
    /// Requested model (Codex's name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Model sent upstream when the member maps it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_model: Option<String>,
    /// `response.model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    /// Requested `service_tier`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier_requested: Option<String>,
    /// `response.service_tier`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier_answered: Option<String>,
    /// Outcome.
    #[serde(default)]
    pub status: RequestStatus,
    /// HTTP status of the answer, when one was received.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    /// Redacted short error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// `usage.input_tokens` (includes cached and cache-write tokens).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// `input_tokens_details.cached_tokens`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u64>,
    /// `input_tokens_details.cache_write_tokens` (or a normalised equivalent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    /// `usage.output_tokens` (includes reasoning).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// `output_tokens_details.reasoning_tokens`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    /// `t0` -> response headers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttfb_ms: Option<u64>,
    /// `t0` -> first output (首字).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_ms: Option<u64>,
    /// `t0` -> end (完成时间).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Output tokens per second x 1000 (`None` below the noise thresholds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tps_milli: Option<u64>,
    /// Estimated cost (`None` = no price for the model, or no usage).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<Usd>,
    /// What the cost was computed with: the list price's revision, then a
    /// space and its source page when it has one, or `override` for a price
    /// set on the Models page (`superba_models::price::PriceBasis`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_rev: Option<String>,
    /// The stats line was added to this response.
    #[serde(default)]
    pub injected: bool,
    /// Stats lines removed from the request.
    #[serde(default)]
    pub stripped: u32,
    /// Upstream request id (`x-request-id`, Anthropic `request-id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_request_id: Option<String>,
    /// The upstream's id of the answer (`resp_…`, Anthropic `msg_…`): with
    /// the request id what tools' own transcripts keep, so imported rows can
    /// be matched with the router's. Ids only, never content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    /// Where the row comes from (router log or Codex rollout).
    #[serde(default)]
    pub source: RequestSource,
    /// The Codex host that sent the request: `None` = this computer,
    /// `wsl:<distro>` = a WSL distribution (router rows, user bug 45; rollout
    /// rows of a distribution name it the same way). The host of every
    /// harness, not only Codex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_host: Option<String>,
    /// The tool that sent the request.
    #[serde(default)]
    pub harness: RequestHarness,
    /// Its version (from its `User-Agent`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_version: Option<String>,
    /// The protocol the tool spoke to the router.
    #[serde(default)]
    pub protocol: RequestProtocol,
    /// The model the tool asked for. [`RequestRow::model`] is the model the
    /// router served it with: they differ when the router maps a tool's own
    /// model id (Claude Code's `claude-haiku-4-5` served by `gpt-5.4-mini`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<String>,
    /// The tool's session id (Codex `session-id`, Claude Code
    /// `X-Claude-Code-Session-Id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Main conversation, subagent or background work, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_kind: Option<AgentKind>,
}

impl RequestRow {
    /// The client, with the Codex host it ran on when that is not this computer
    /// ("Terminal · WSL: Ubuntu", user bug 45). Rows of another harness name
    /// that harness ("Claude Code").
    pub fn client_text(&self) -> LocalizedText {
        let client = match self.harness {
            RequestHarness::Codex => self.client.text(),
            other => other.text(),
        };
        match self
            .codex_host
            .as_deref()
            .and_then(crate::wsl::distro_of_router_source)
        {
            Some(distro) => LocalizedText::new("request_host.client_on_host", "{client} · {host}")
                .arg_text("client", client)
                .arg_text("host", crate::target::wsl_title(distro)),
            None => client,
        }
    }

    /// Non-cached input: `input - cached - cache_write` (`None` without usage).
    pub fn fresh_input_tokens(&self) -> Option<u64> {
        let input = self.input_tokens?;
        Some(
            input
                .saturating_sub(self.cached_tokens.unwrap_or(0))
                .saturating_sub(self.cache_write_tokens.unwrap_or(0)),
        )
    }

    /// The short session key shown when no title is known (first 8 characters of
    /// the prompt cache key, else of the thread id).
    pub fn short_key(&self) -> Option<String> {
        self.prompt_cache_key
            .as_deref()
            .or(self.thread_id.as_deref())
            .map(|k| k.chars().take(8).collect())
    }

    /// The provider group key ([`RequestGrouping::Provider`]): the router member
    /// (the provider's profile) of the row; `None` for a request that never
    /// reached a member.
    pub fn provider_key(&self) -> Option<&str> {
        Some(self.member_id.as_str()).filter(|m| !m.is_empty())
    }

    /// The line of the row as the log shows it: its line name, else the line id.
    /// Rows that failed before any line answered keep the name of the last line
    /// tried, so they stay under that line.
    pub fn line_label(&self) -> Option<&str> {
        self.line_name
            .as_deref()
            .filter(|l| !l.is_empty())
            .or(self.line_id.as_deref().filter(|l| !l.is_empty()))
    }

    /// The row belongs to the provider group `key`: a provider key, or a
    /// provider · line key from [`provider_line_key`] (an empty line: the
    /// provider's rows without a known line, such as rows from Codex rollouts).
    pub fn in_provider_group(&self, key: &str) -> bool {
        let (provider, line) = split_provider_key(key);
        self.provider_key() == Some(provider)
            && line.is_none_or(|l| self.line_label().unwrap_or("") == l)
    }
}

/// Separator of a provider · line group key (`<provider>\u{1f}<line>`; the unit
/// separator never occurs in member ids or line names).
pub const PROVIDER_LINE_SEPARATOR: char = '\u{1f}';

/// The group key of one line of a provider (a sub-entry of
/// [`RequestGrouping::Provider`], [`RequestGroup::lines`] and
/// [`StatsBucket::lines`]); an empty `line` selects the provider's rows
/// without a known line.
pub fn provider_line_key(provider: &str, line: &str) -> String {
    format!("{provider}{PROVIDER_LINE_SEPARATOR}{line}")
}

/// `(provider, line)` of a provider group key (`line` is `None` for a whole
/// provider).
pub fn split_provider_key(key: &str) -> (&str, Option<&str>) {
    match key.split_once(PROVIDER_LINE_SEPARATOR) {
        Some((provider, line)) => (provider, Some(line)),
        None => (key, None),
    }
}

/// Which status rows a query returns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RequestStatusFilter {
    /// Every row.
    #[default]
    All,
    /// Only [`RequestStatus::Ok`].
    Ok,
    /// Everything else.
    Errors,
    /// Requests that failed over to another member or line at least once.
    Failover,
}

/// Grouping of the source list (按会话 / 按供应商).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RequestGrouping {
    /// By session key (`prompt_cache_key`, else thread id).
    #[default]
    Session,
    /// By provider ([`RequestRow::provider_key`]: the member profile), with the
    /// provider's lines in [`RequestGroup::lines`]. A group key is a provider
    /// key or a [`provider_line_key`].
    Provider,
}

/// Sortable columns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RequestSortKey {
    /// Start time.
    #[default]
    Time,
    /// Prompt cache key.
    CacheKey,
    /// Session (title resolved by the caller; sorts by key here).
    Session,
    /// Status.
    Status,
    /// Member, then line.
    Provider,
    /// Non-cached input.
    Input,
    /// Output.
    Output,
    /// Cache read.
    CacheRead,
    /// Cache write.
    CacheWrite,
    /// First token.
    FirstToken,
    /// Duration.
    Duration,
    /// Tokens per second.
    Tps,
    /// Cost.
    Cost,
}

/// Sort order of a query.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestSort {
    /// Column.
    #[serde(default)]
    pub key: RequestSortKey,
    /// Descending (newest / largest first).
    #[serde(default = "descending")]
    pub descending: bool,
}

fn descending() -> bool {
    true
}

impl Default for RequestSort {
    fn default() -> Self {
        Self {
            key: RequestSortKey::Time,
            descending: true,
        }
    }
}

/// A request log query.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RequestQuery {
    /// Rows started at or after this time.
    pub from: Option<Timestamp>,
    /// Rows started before this time.
    pub to: Option<Timestamp>,
    /// Only this member.
    pub member: Option<ProfileId>,
    /// Only this line id.
    pub line: Option<String>,
    /// Only this model (requested name).
    pub model: Option<String>,
    /// Status filter.
    pub status: RequestStatusFilter,
    /// Only this client.
    pub client: Option<RequestClient>,
    /// Only rows from this source (router log, Codex rollouts, an import).
    pub source: Option<RequestSource>,
    /// Grouping of [`RequestPage::groups`].
    pub group_by: RequestGrouping,
    /// Only the rows of this group key (of `group_by`).
    pub group: Option<String>,
    /// Free text over cache key, member, line, model, status and upstream request
    /// id (titles are matched by the caller).
    pub search: Option<String>,
    /// Only rows with a larger id (live updates).
    pub after_id: Option<i64>,
    /// Page size (0 = the default, [`RequestQuery::DEFAULT_LIMIT`]).
    pub limit: u32,
    /// Sort order.
    pub sort: RequestSort,
}

impl RequestQuery {
    /// Default page size.
    pub const DEFAULT_LIMIT: u32 = 500;
}

impl Default for RequestQuery {
    fn default() -> Self {
        Self {
            from: None,
            to: None,
            member: None,
            line: None,
            model: None,
            status: RequestStatusFilter::All,
            client: None,
            source: None,
            group_by: RequestGrouping::Session,
            group: None,
            search: None,
            after_id: None,
            limit: 0,
            sort: RequestSort::default(),
        }
    }
}

/// Totals over a set of rows (the pinned totals row and the statistics).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RequestTotals {
    /// Rows.
    pub requests: u64,
    /// Rows with [`RequestStatus::Ok`].
    pub ok: u64,
    /// Other rows.
    pub errors: u64,
    /// Sum of `input_tokens` (incl. cached).
    pub input_tokens: u64,
    /// Sum of cached tokens.
    pub cached_tokens: u64,
    /// Sum of cache-write tokens.
    pub cache_write_tokens: u64,
    /// Sum of output tokens.
    pub output_tokens: u64,
    /// Sum of known costs.
    pub cost: Usd,
    /// Rows with usage but no price.
    pub unpriced: u64,
    /// Rows with a first-token time.
    pub first_token_rows: u64,
    /// Sum of their first-token times (ms).
    pub first_token_ms_sum: u64,
    /// Output tokens of rows with a TPS value (the weight of [`Self::tps_milli`]).
    pub tps_output_tokens: u64,
    /// Sum of `output_tokens × tps_milli` over those rows.
    pub tps_weighted_milli: u64,
}

impl RequestTotals {
    /// Average first token in ms over the rows that have one.
    pub fn avg_first_token_ms(&self) -> Option<u64> {
        (self.first_token_rows > 0).then(|| self.first_token_ms_sum / self.first_token_rows)
    }

    /// Output-weighted tokens per second (×1000) over the rows with a TPS value.
    pub fn tps_milli(&self) -> Option<u64> {
        (self.tps_output_tokens > 0).then(|| self.tps_weighted_milli / self.tps_output_tokens)
    }

    /// Add one row's timing to the sums (first token, TPS weight).
    pub fn add_timing(
        &mut self,
        first_token_ms: Option<u64>,
        tps_milli: Option<u64>,
        output_tokens: Option<u64>,
    ) {
        if let Some(ms) = first_token_ms {
            self.first_token_rows += 1;
            self.first_token_ms_sum = self.first_token_ms_sum.saturating_add(ms);
        }
        if let (Some(t), Some(o)) = (tps_milli, output_tokens) {
            self.tps_output_tokens = self.tps_output_tokens.saturating_add(o);
            self.tps_weighted_milli = self.tps_weighted_milli.saturating_add(o.saturating_mul(t));
        }
    }
}

/// One entry of the source list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestGroup {
    /// Group key (session key, provider key or provider · line key).
    pub key: String,
    /// Label (the newest row's provider or line name; for sessions the caller
    /// resolves titles).
    pub label: String,
    /// Rows in the query range.
    pub count: u64,
    /// Newest row's start.
    pub last: Timestamp,
    /// Upstream host of the newest row (provider groups and their lines).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_host: Option<String>,
    /// The provider's lines, most requests first (provider groups only; rows
    /// without a line count only for the provider).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<RequestGroup>,
}

/// Result of a request log query.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestPage {
    /// Rows in query order.
    #[serde(default)]
    pub rows: Vec<RequestRow>,
    /// Totals over every row the filters match (not only this page).
    #[serde(default)]
    pub totals: RequestTotals,
    /// Source list entries for `group_by` (ignoring `group`).
    #[serde(default)]
    pub groups: Vec<RequestGroup>,
    /// More rows match than were returned.
    #[serde(default)]
    pub truncated: bool,
    /// Recording is on (else the page shows why nothing new arrives).
    #[serde(default)]
    pub recording: bool,
}

/// Phase of a request in flight.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum InflightPhase {
    /// Waiting for response headers.
    #[default]
    Waiting,
    /// Streaming events.
    Streaming,
    /// Holding the end of the final answer for the stats line.
    Holding,
}

/// A request still running (活动请求), from the router's admin API.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InflightRequest {
    /// Router-local id.
    pub id: u64,
    /// Start.
    pub started_at: Timestamp,
    /// Member.
    pub member_id: ProfileId,
    /// Member name.
    #[serde(default)]
    pub member_name: String,
    /// Line name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_name: Option<String>,
    /// Requested model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Prompt cache key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    /// Client.
    #[serde(default)]
    pub client: RequestClient,
    /// Transport.
    #[serde(default)]
    pub transport: RequestTransport,
    /// Phase.
    #[serde(default)]
    pub phase: InflightPhase,
    /// Output deltas so far.
    #[serde(default)]
    pub output_deltas: u64,
    /// Time to the first output delta, once one arrived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_ms: Option<u64>,
    /// Elapsed time when reported.
    #[serde(default)]
    pub elapsed_ms: u64,
}

/// Timing marks of one request, in ms after `t0` (the router received the
/// request; for WebSocket the `response.create` frame). Each mark is taken from
/// the monotonic clock when the router processed that item, never inferred from
/// read boundaries: headers and the first event that arrive in one read get the
/// same (or nearly the same) time, a valid 0 ms phase.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RequestTiming {
    /// The request was sent upstream (the answering attempt started; for
    /// WebSocket the frame was forwarded). `None` when not reached.
    pub sent_ms: Option<u64>,
    /// Response headers (`None` for WebSocket requests: their socket was opened
    /// earlier).
    pub headers_ms: Option<u64>,
    /// First Responses event (SSE event or WebSocket frame with a `type`;
    /// comments and keep-alives do not count).
    pub first_event_ms: Option<u64>,
    /// First output (首字): first output delta, or an output item that arrived
    /// whole.
    pub first_token_ms: Option<u64>,
    /// Last output delta.
    pub last_delta_ms: Option<u64>,
    /// End: `response.completed` / `failed` / `incomplete`, the end of the
    /// stream, an error or the client leaving.
    pub end_ms: Option<u64>,
}

/// Shortest first-event → end window that gets a TPS value: an answer whose
/// events arrived in one read (or a few ms apart) would divide by ~0.
pub const TPS_MIN_WINDOW_MS: u64 = 200;
/// Fewest output tokens that get a TPS value.
pub const TPS_MIN_OUTPUT: u64 = 10;

/// Output tokens per second × 1000 from the first event to the end (both in ms
/// after `t0`): the TPS of the request log, the stats line and the GUI. `None`
/// when the window is shorter than [`TPS_MIN_WINDOW_MS`] (zero, or out of
/// order) or fewer than [`TPS_MIN_OUTPUT`] tokens were output.
pub fn tps_milli(output_tokens: u64, first_event_ms: u64, end_ms: u64) -> Option<u64> {
    let window = end_ms.checked_sub(first_event_ms)?;
    if window < TPS_MIN_WINDOW_MS || output_tokens < TPS_MIN_OUTPUT {
        return None;
    }
    u64::try_from(u128::from(output_tokens) * 1_000_000 / u128::from(window)).ok()
}

impl RequestTiming {
    /// The marks in order (sent, headers, first event, first token, last delta,
    /// end), each raised to the latest known mark before it, so no phase is
    /// negative; equal marks (0 ms phases) stay. The marks are taken
    /// independently (response headers, the event stream, the token parser), so
    /// they can come out of order; the router stores normalized marks.
    pub fn normalized(&self) -> RequestTiming {
        let mut floor = 0u64;
        let mut next = |v: Option<u64>| {
            v.map(|ms| {
                floor = floor.max(ms);
                floor
            })
        };
        RequestTiming {
            sent_ms: next(self.sent_ms),
            headers_ms: next(self.headers_ms),
            first_event_ms: next(self.first_event_ms),
            first_token_ms: next(self.first_token_ms),
            last_delta_ms: next(self.last_delta_ms),
            end_ms: next(self.end_ms),
        }
    }

    /// The TPS window (first event → end) in ms, when both are known.
    pub fn tps_window_ms(&self) -> Option<u64> {
        let t = self.normalized();
        Some(t.end_ms? - t.first_event_ms?)
    }
}

/// One candidate tried for a request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestAttempt {
    /// Member.
    pub member_id: ProfileId,
    /// Line name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_name: Option<String>,
    /// Upstream host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_host: Option<String>,
    /// HTTP status, when headers arrived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Redacted outcome (`connect error`, `502`, `answered`).
    #[serde(default)]
    pub outcome: String,
    /// Time spent on this attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    /// How the request body went to this line (HTTP; `None` for WebSocket
    /// frames). The answering attempt's value is how the request reached the
    /// upstream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compression: Option<RequestCompression>,
}

/// Everything about one request (the detail pane).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestDetail {
    /// The row.
    pub row: RequestRow,
    /// Timing marks.
    #[serde(default)]
    pub timing: RequestTiming,
    /// Candidates tried, in order.
    #[serde(default)]
    pub attempts: Vec<RequestAttempt>,
    /// Redacted router log lines of this request.
    #[serde(default)]
    pub log: Vec<String>,
}

/// Grouping of [`RequestStats`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StatsGroupBy {
    /// Requested model.
    #[default]
    Model,
    /// Provider ([`RequestRow::provider_key`]), with its lines in
    /// [`StatsBucket::lines`].
    Provider,
    /// Local calendar day.
    Day,
    /// Session key.
    Session,
}

/// A statistics query.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatsQuery {
    /// Range start.
    pub from: Option<Timestamp>,
    /// Range end.
    pub to: Option<Timestamp>,
    /// Grouping.
    pub by: StatsGroupBy,
    /// Only this member.
    pub member: Option<ProfileId>,
    /// UTC offset of the local day in minutes (for [`StatsGroupBy::Day`] and the
    /// daily bars).
    pub utc_offset_minutes: i32,
    /// Only rows from this source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<RequestSource>,
}

/// One aggregate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatsBucket {
    /// Key (model, provider key or provider · line key, `YYYY-MM-DD`, session
    /// key).
    pub key: String,
    /// Label (for a provider the newest row's name, for a line its name).
    pub label: String,
    /// Totals.
    pub totals: RequestTotals,
    /// The provider's lines, largest cost first ([`StatsGroupBy::Provider`]
    /// only; rows without a line count only for the provider).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<StatsBucket>,
}

/// Grouped aggregates (统计 tab, `spb router stats`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestStats {
    /// Buckets of the query's grouping, largest cost first.
    #[serde(default)]
    pub buckets: Vec<StatsBucket>,
    /// Per local day, oldest first (daily bars).
    #[serde(default)]
    pub daily: Vec<StatsBucket>,
    /// Over the whole range.
    #[serde(default)]
    pub totals: RequestTotals,
}

/// What [`crate::SwitchService::router_clear`] deletes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClearRequest {
    /// Only rows started before this time (`None` = everything).
    pub before: Option<Timestamp>,
}

/// Result of a clear.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClearReport {
    /// Rows deleted.
    pub rows: u64,
}

/// Recording state reported by the router's admin status.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RouterStatsStatus {
    /// Rows are being recorded.
    pub recording: bool,
    /// Rows written since the router started.
    pub rows_written: u64,
    /// Rows dropped because the writer fell behind.
    pub dropped: u64,
    /// Size of `usage.sqlite3`.
    pub db_bytes: Option<u64>,
}

// ---------------------------------------------------------------------------
// Prices
// ---------------------------------------------------------------------------

/// A non-negative multiplier with six decimals (parts per million), so settings
/// that hold one stay `Eq` and exact. Serialized as a JSON number (`0.15`, `2`);
/// accepts a number or a numeric string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Multiplier(u64);

impl Multiplier {
    /// 1x.
    pub const ONE: Multiplier = Multiplier(1_000_000);

    /// From parts per million.
    pub const fn from_ppm(ppm: u64) -> Self {
        Multiplier(ppm)
    }

    /// Parts per million.
    pub const fn ppm(self) -> u64 {
        self.0
    }

    /// From a float; `None` for negative, NaN, infinite or absurd values.
    pub fn from_f64(x: f64) -> Option<Self> {
        (x.is_finite() && (0.0..1e9).contains(&x)).then(|| Multiplier((x * 1e6).round() as u64))
    }

    /// As a float (display and serialization).
    pub fn as_f64(self) -> f64 {
        self.0 as f64 / 1e6
    }

    /// `usd * self`, rounded to the micro-dollar.
    pub fn apply(self, usd: Usd) -> Usd {
        let v = i128::from(usd.micros()) * i128::from(self.0) / 1_000_000;
        Usd::from_micros(v.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64)
    }
}

impl Default for Multiplier {
    fn default() -> Self {
        Multiplier::ONE
    }
}

impl fmt::Display for Multiplier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let int = self.0 / 1_000_000;
        let frac = self.0 % 1_000_000;
        if frac == 0 {
            write!(f, "{int}")
        } else {
            let s = format!("{frac:06}");
            write!(f, "{int}.{}", s.trim_end_matches('0'))
        }
    }
}

impl Serialize for Multiplier {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_f64(self.as_f64())
    }
}

impl<'de> Deserialize<'de> for Multiplier {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Multiplier;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a non-negative multiplier")
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Multiplier, E> {
                Multiplier::from_f64(v).ok_or_else(|| E::custom("multiplier out of range"))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Multiplier, E> {
                self.visit_f64(v as f64)
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Multiplier, E> {
                self.visit_f64(v as f64)
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Multiplier, E> {
                let x: f64 = v
                    .trim()
                    .parse()
                    .map_err(|_| E::custom("not a decimal multiplier"))?;
                self.visit_f64(x)
            }
        }
        d.deserialize_any(V)
    }
}

/// Prices above a prompt size (models priced by context length).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LongContextPrice {
    /// Applies when `input_tokens` is above this.
    pub above_input_tokens: u64,
    /// Input, USD per 1M tokens.
    pub input: Usd,
    /// Cached input (default = `input`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input: Option<Usd>,
    /// Cache write (default = this band's `input`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<Usd>,
    /// Output.
    pub output: Usd,
}

/// Absolute prices of one service tier in USD per 1M tokens, copied from the
/// tier's own table on the public price page (OpenAI's tier prices are not
/// always a uniform multiple of Standard).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierPrice {
    /// Input.
    pub input: Usd,
    /// Cached input (default = `input`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input: Option<Usd>,
    /// Cache write (default = `input`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<Usd>,
    /// Output.
    pub output: Usd,
    /// Long-context bands of this tier, ascending by threshold. A tier without
    /// bands has no long-context price: a request in one of the entry's
    /// Standard bands is then priced at that Standard band.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub long_context: Vec<LongContextPrice>,
    /// Public price page of this tier when it differs from the entry's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Prices of one model in USD per 1M tokens (from public price pages only).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceEntry {
    /// Input.
    pub input: Usd,
    /// Cached input (default = `input`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input: Option<Usd>,
    /// Cache write (default = `input`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<Usd>,
    /// Output.
    pub output: Usd,
    /// Long-context tiers, ascending by threshold.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub long_context: Vec<LongContextPrice>,
    /// Multipliers by the response's `service_tier` (`priority`, `flex`, ...),
    /// for tiers without absolute [`PriceEntry::tier_prices`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tiers: BTreeMap<String, Multiplier>,
    /// Absolute prices by service tier (`priority` = Fast, `flex`,
    /// `ultrafast`); they win over [`PriceEntry::tiers`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tier_prices: BTreeMap<String, TierPrice>,
    /// Public price page the entry comes from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The date the prices were read from `source` (`YYYY-MM-DD`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
}

/// A price table: model slug -> prices.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceTable {
    /// Revision of the table (stored with each computed cost).
    #[serde(default)]
    pub rev: String,
    /// Entries by model slug.
    #[serde(default)]
    pub entries: BTreeMap<String, PriceEntry>,
}

impl PriceTable {
    /// The entry of `model`.
    pub fn get(&self, model: &str) -> Option<&PriceEntry> {
        self.entries.get(model)
    }
}
