//! Configuration of the Claude Code ingress (`config.json` → `claude`).
//!
//! Members are the router's members (same ids, lines, keys, health and
//! request log); this section says which of them serve Claude Code and which
//! models go where. Requests are passed through to each member's Anthropic
//! Messages endpoint (`<origin><path>`, what one would set as
//! `ANTHROPIC_BASE_URL`; `/v1/messages` is appended). A member without one is
//! not eligible ([`ClaudeIneligible::NoAnthropicEndpoint`]): there is no
//! protocol conversion yet.
//!
//! - **Routes**: requested model (exact or `*` glob) → targets in failover
//!   order (member, upstream model name). Without a matching route a model
//!   goes to the members whose model list names it, in member order (the list
//!   Superba wrote, else the member's own Anthropic `/v1/models`).
//! - **Aliases**: Claude Code's own ids (`claude-haiku-*` for titles and
//!   other background calls) → the model that serves them, applied first.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use superba_core::{ClaudeIneligible, Error, Result};
use url::Url;

use crate::config::{RouterConfig, check_upstream_url};

/// How the key goes to an Anthropic endpoint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnthropicAuth {
    /// `x-api-key: <key>` (Anthropic's own scheme).
    #[default]
    XApiKey,
    /// `Authorization: Bearer <key>`.
    Bearer,
}

/// Which `anthropic-beta` values reach the member.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "values", rename_all = "snake_case")]
pub enum BetaPolicy {
    /// All of them (Krill and other full Anthropic relays).
    #[default]
    Pass,
    /// None (the header is removed).
    Drop,
    /// Only these.
    Allow(Vec<String>),
    /// All but these.
    Deny(Vec<String>),
}

impl BetaPolicy {
    /// The header value after the policy (`None`: no header).
    pub fn filter(&self, value: &str) -> Option<String> {
        let keep = |b: &str| match self {
            BetaPolicy::Pass => true,
            BetaPolicy::Drop => false,
            BetaPolicy::Allow(list) => list.iter().any(|x| x == b),
            BetaPolicy::Deny(list) => !list.iter().any(|x| x == b),
        };
        let kept: Vec<&str> = value
            .split(',')
            .map(str::trim)
            .filter(|b| !b.is_empty() && keep(b))
            .collect();
        (!kept.is_empty()).then(|| kept.join(","))
    }
}

/// A member's Anthropic endpoint: a path below each line's origin, or full
/// base URLs per line.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Endpoint {
    /// Path below each line's origin (`/anthropic`; empty = the origin).
    pub path: String,
    /// Full base URLs of single lines, by line id (they override `path`).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub lines: BTreeMap<String, Url>,
}

/// One model a member serves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeModel {
    /// Model id.
    pub id: String,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// How one member serves Claude Code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClaudeMember {
    /// Its Anthropic Messages endpoint (`None`: not eligible).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anthropic: Option<Endpoint>,
    /// How its key is sent.
    pub auth: AnthropicAuth,
    /// `anthropic-beta` values forwarded to it.
    pub betas: BetaPolicy,
    /// Top-level request fields removed before the request is sent.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub drop_fields: Vec<String>,
    /// The endpoint takes `role: "system"` messages inside `messages`; when
    /// false they are sent as user text.
    pub system_messages: bool,
    /// `/v1/messages/count_tokens` upstream: `None` tries it and estimates
    /// locally when it is missing; `false` always estimates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count_tokens: Option<bool>,
    /// Models it serves (Superba's model list); empty = the router reads the
    /// member's own `/v1/models` ([`crate::claude::catalog`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ClaudeModel>,
    /// It serves only Claude Code: Codex requests never go to it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub claude_only: bool,
}

impl Default for ClaudeMember {
    fn default() -> Self {
        Self {
            anthropic: None,
            auth: AnthropicAuth::XApiKey,
            betas: BetaPolicy::Pass,
            drop_fields: Vec::new(),
            system_messages: true,
            count_tokens: None,
            models: Vec::new(),
            claude_only: false,
        }
    }
}

impl ClaudeMember {
    /// Whether it can serve Claude Code, and why not.
    pub fn eligibility(&self) -> std::result::Result<(), ClaudeIneligible> {
        match self.anthropic {
            Some(_) => Ok(()),
            None => Err(ClaudeIneligible::NoAnthropicEndpoint),
        }
    }
}

/// One target of a route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeTarget {
    /// Member id.
    pub member: String,
    /// Model name sent upstream (default: the routed model).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Model → targets in failover order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeRoute {
    /// Requested model: exact, or a pattern with `*` (any characters).
    pub model: String,
    /// Targets in failover order.
    pub targets: Vec<ClaudeTarget>,
}

/// A model id the client sends that another model serves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelAlias {
    /// Requested model: exact or a `*` pattern (`claude-haiku-*`).
    pub from: String,
    /// The model that serves it.
    pub to: String,
}

/// The Claude Code ingress.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClaudeConfig {
    /// Members that serve Claude Code, by member id (each one of
    /// [`RouterConfig::members`]; their priority is that order).
    pub members: BTreeMap<String, ClaudeMember>,
    /// Model routes, first match wins.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<ClaudeRoute>,
    /// Aliases, first match wins, applied before the routes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<ModelAlias>,
}

/// `pattern` matches `model` (`*` = any characters, case-insensitive).
pub fn matches(pattern: &str, model: &str) -> bool {
    let p = pattern.to_ascii_lowercase();
    let m = model.to_ascii_lowercase();
    let parts: Vec<&str> = p.split('*').collect();
    if parts.len() == 1 {
        return p == m;
    }
    let mut rest = m.as_str();
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(part) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else {
            match rest.find(part) {
                Some(at) => rest = &rest[at + part.len()..],
                None => return false,
            }
        }
    }
    true
}

impl ClaudeConfig {
    /// Check the section against the router configuration.
    pub fn validate(&self, router: &RouterConfig) -> Result<()> {
        for (id, m) in &self.members {
            let Some(member) = router.member(id) else {
                return Err(Error::invalid(format!(
                    "Claude Code member {id:?} is not a router member"
                )));
            };
            if let Some(ep) = &m.anthropic {
                if !(ep.path.is_empty()
                    || (ep.path.starts_with('/') && !ep.path.contains(['?', '#'])))
                {
                    return Err(Error::invalid(format!(
                        "Claude Code member {id:?}: endpoint path {:?} must be empty or start with '/'",
                        ep.path
                    )));
                }
                for (line, url) in &ep.lines {
                    if !member.lines.iter().any(|l| &l.id == line) {
                        return Err(Error::invalid(format!(
                            "Claude Code member {id:?}: no line {line:?}"
                        )));
                    }
                    check_upstream_url(url)?;
                }
            }
        }
        for r in &self.routes {
            if r.model.trim().is_empty() || r.targets.is_empty() {
                return Err(Error::invalid(
                    "a Claude Code route needs a model and at least one target",
                ));
            }
            for t in &r.targets {
                let Some(m) = self.members.get(&t.member) else {
                    return Err(Error::invalid(format!(
                        "Claude Code route {:?}: {:?} is not a Claude Code member",
                        r.model, t.member
                    )));
                };
                if m.eligibility().is_err() {
                    return Err(Error::invalid(format!(
                        "Claude Code route {:?}: member {:?} has no Anthropic endpoint",
                        r.model, t.member
                    )));
                }
                if t.model.as_deref().is_some_and(|m| m.trim().is_empty()) {
                    return Err(Error::invalid(format!(
                        "Claude Code route {:?}: empty upstream model",
                        r.model
                    )));
                }
            }
        }
        for a in &self.aliases {
            if a.from.trim().is_empty() || a.to.trim().is_empty() {
                return Err(Error::invalid("a Claude Code alias needs both model names"));
            }
        }
        Ok(())
    }

    /// The member is reserved for Claude Code.
    pub fn claude_only(&self, member: &str) -> bool {
        self.members.get(member).is_some_and(|m| m.claude_only)
    }

    /// Whether router member `id` can serve Claude Code, and why not.
    pub fn eligibility(&self, id: &str) -> std::result::Result<(), ClaudeIneligible> {
        match self.members.get(id) {
            Some(m) => m.eligibility(),
            None => Err(ClaudeIneligible::NoAnthropicEndpoint),
        }
    }
}
