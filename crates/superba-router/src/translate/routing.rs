//! Protocol capabilities and the single native-first routing decision used by
//! every ingress. Disabled conversions are directional.

use super::ir::Protocol;
use crate::config::{MemberKind, RouterConfig, join_path};
use crate::openai::{OpenAiEndpoint, OpenAiModel, OpenAiProtocol};
use http::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use superba_core::{CompactionSupport, Error, Result};
use url::Url;

/// One client → upstream capability switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Conversion {
    /// Client protocol.
    pub from: Protocol,
    /// Upstream protocol.
    pub to: Protocol,
}

/// All twelve conversions are on unless listed in `disabled`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TranslationConfig {
    /// Individually disabled directed conversions; native forwarding stays on.
    pub disabled: BTreeSet<Conversion>,
}

impl TranslationConfig {
    /// Whether a directed conversion is enabled.
    pub fn allows(&self, from: Protocol, to: Protocol) -> bool {
        from == to || !self.disabled.contains(&Conversion { from, to })
    }
}

/// Native Gemini endpoint and model list of one member.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeminiMember {
    /// Base ends at `/v1beta`; None means no native Gemini API.
    pub endpoint: Option<OpenAiEndpoint>,
    /// Configured model list; empty uses the member's discovery endpoint.
    pub models: Vec<OpenAiModel>,
}

/// Gemini ingress settings. Krill defaults to `/v1beta`; other providers have
/// no Gemini endpoint unless configured here.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeminiConfig {
    /// Overrides keyed by router member id.
    pub members: BTreeMap<String, GeminiMember>,
    /// Priority order; unlisted members follow in router order.
    pub order: Vec<String>,
}

impl GeminiConfig {
    /// Validate endpoint URLs and referenced members/lines.
    pub fn validate(&self, router: &RouterConfig) -> Result<()> {
        for (id, g) in &self.members {
            let member = router.member(id).ok_or_else(|| {
                Error::invalid(format!("Gemini member {id:?} is not a router member"))
            })?;
            if let Some(ep) = &g.endpoint {
                crate::openai::config::check_endpoint(id, member, ep)?;
            }
        }
        if self.order.iter().any(|id| router.member(id).is_none()) {
            return Err(Error::invalid("Gemini order names an unknown member"));
        }
        Ok(())
    }
}

impl RouterConfig {
    /// Native Gemini settings, including provider defaults.
    pub fn gemini_member(&self, id: &str) -> Option<GeminiMember> {
        let member = self.member(id)?;
        Some(
            self.gemini
                .as_ref()
                .and_then(|c| c.members.get(id))
                .cloned()
                .unwrap_or_else(|| GeminiMember {
                    endpoint: (member.kind == MemberKind::Krill)
                        .then(|| OpenAiEndpoint::at("/v1beta")),
                    models: vec![],
                }),
        )
    }

    /// True when the member has a native endpoint for this protocol.
    pub fn native_protocol(&self, id: &str, protocol: Protocol) -> bool {
        match protocol {
            Protocol::Messages => self
                .claude
                .as_ref()
                .and_then(|c| c.members.get(id))
                .is_some_and(|m| m.anthropic.is_some()),
            Protocol::Responses => self
                .openai_member(id)
                .is_some_and(|m| m.responses.is_some()),
            Protocol::Chat => self.openai_member(id).is_some_and(|m| m.chat.is_some()),
            Protocol::Gemini => self.gemini_member(id).is_some_and(|m| m.endpoint.is_some()),
        }
    }

    /// Native first, then a deterministic preference among enabled codecs.
    /// Callers keep their own model routing, failover order and stickiness.
    pub fn upstream_protocol(&self, id: &str, client: Protocol) -> Option<Protocol> {
        if self.native_protocol(id, client) {
            return Some(client);
        }
        [
            Protocol::Responses,
            Protocol::Chat,
            Protocol::Messages,
            Protocol::Gemini,
        ]
        .into_iter()
        .find(|p| self.translation.allows(client, *p) && self.native_protocol(id, *p))
    }

    /// Compaction is a native Responses capability. Conversion cannot provide it.
    pub fn compaction_support(&self, id: &str) -> CompactionSupport {
        if self.native_protocol(id, Protocol::Responses) {
            self.member(id)
                .map(|m| m.compaction)
                .unwrap_or(CompactionSupport::Unsupported)
        } else {
            CompactionSupport::Unsupported
        }
    }

    /// Claude's routed eligibility includes enabled protocol conversion.
    pub fn claude_eligibility(
        &self,
        id: &str,
    ) -> std::result::Result<(), superba_core::ClaudeIneligible> {
        self.upstream_protocol(id, Protocol::Messages)
            .map(|_| ())
            .ok_or(superba_core::ClaudeIneligible::NoAnthropicEndpoint)
    }
}

/// Native base URL for one member/line/protocol. It never resolves through a
/// second conversion, so conversions cannot loop.
pub(crate) fn base(
    config: &RouterConfig,
    member: usize,
    line: usize,
    protocol: Protocol,
) -> Option<Url> {
    let m = config.members.get(member)?;
    let l = m.lines.get(line)?;
    if protocol == Protocol::Messages {
        let ep = config
            .claude
            .as_ref()?
            .members
            .get(&m.id)?
            .anthropic
            .as_ref()?;
        return Some(ep.lines.get(&l.id).cloned().unwrap_or_else(|| {
            join_path(&crate::openai::rt::origin(&l.base_url), &ep.path, None)
        }));
    }
    let ep = match protocol {
        Protocol::Responses => config.openai_member(&m.id)?.responses?,
        Protocol::Chat => config.openai_member(&m.id)?.chat?,
        Protocol::Gemini => config.gemini_member(&m.id)?.endpoint?,
        Protocol::Messages => unreachable!(),
    };
    Some(crate::openai::rt::endpoint_base(&ep, &l.id, &l.base_url))
}

/// Request URL; Gemini uses the model and streaming mode in the path.
pub(crate) fn url(base: &Url, protocol: Protocol, model: &str, stream: bool) -> Url {
    match protocol {
        Protocol::Messages => join_path(base, "/v1/messages", None),
        Protocol::Responses => join_path(base, "/responses", None),
        Protocol::Chat => join_path(base, "/chat/completions", None),
        Protocol::Gemini => {
            let mut u = base.clone();
            if let Ok(mut segments) = u.path_segments_mut() {
                segments.pop_if_empty().push("models").push(&format!(
                    "{}:{}",
                    model.trim_start_matches("models/"),
                    if stream {
                        "streamGenerateContent"
                    } else {
                        "generateContent"
                    }
                ));
            }
            u.set_query(if stream { Some("alt=sse") } else { None });
            u
        }
    }
}

/// Target protocol headers. Credentials from the client never cross this
/// boundary; the member's marked-sensitive key is used in the native scheme.
pub(crate) fn headers(
    src: &HeaderMap,
    g: &crate::state::Generation,
    member: usize,
    protocol: Protocol,
) -> HeaderMap {
    let m = &g.members[member];
    let mut h = crate::forward::request_headers(src, false, true, &m.auth);
    for name in [
        "anthropic-version",
        "anthropic-beta",
        "x-goog-api-key",
        "content-encoding",
        "accept-encoding",
        "content-length",
        "sec-websocket-key",
        "sec-websocket-version",
        "sec-websocket-extensions",
        "sec-websocket-protocol",
    ] {
        h.remove(name);
    }
    h.insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    let key = crate::token::bearer(m.auth.as_bytes()).and_then(|k| HeaderValue::from_bytes(k).ok());
    if protocol == Protocol::Messages {
        h.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        let bearer = g
            .config
            .claude
            .as_ref()
            .and_then(|c| c.members.get(&m.cfg.id))
            .is_some_and(|c| c.auth == crate::claude::AnthropicAuth::Bearer);
        if !bearer && let Some(mut key) = key {
            key.set_sensitive(true);
            h.remove(http::header::AUTHORIZATION);
            h.insert("x-api-key", key);
        }
    } else if protocol == Protocol::Gemini
        && let Some(mut key) = key
    {
        key.set_sensitive(true);
        h.remove(http::header::AUTHORIZATION);
        h.insert("x-goog-api-key", key);
    }
    h
}

impl From<OpenAiProtocol> for Protocol {
    fn from(p: OpenAiProtocol) -> Self {
        match p {
            OpenAiProtocol::Responses => Self::Responses,
            OpenAiProtocol::Chat => Self::Chat,
        }
    }
}

/// Remove URL context from the Gemini codec envelope before sending its body.
/// Other protocols encode that context in their JSON bodies.
pub(crate) fn wire_body(protocol: Protocol, body: bytes::Bytes) -> bytes::Bytes {
    if protocol != Protocol::Gemini {
        return body;
    }
    let Ok(mut v) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return body;
    };
    if let Some(o) = v.as_object_mut() {
        o.remove("model");
        o.remove("stream");
    }
    bytes::Bytes::from(v.to_string())
}
