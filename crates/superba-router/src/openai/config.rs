//! Configuration of the OpenAI ingress (`config.json` → `openai`).
//!
//! Members are the router's members (same ids, lines, keys, health and
//! request log). Each serves the ingress's protocols at an endpoint of its
//! own, the base URL a tool would be given (`…/v1`; the ingress appends
//! `/responses` or `/chat/completions`):
//!
//! | Member | Responses | Chat Completions |
//! |---|---|---|
//! | Krill | `<origin>/codex/v1` | `<origin>/codex/v1` |
//! | compatible site | its line base URL (what Codex uses) | none until declared |
//! | reserved for Claude Code | none until declared | none until declared |
//!
//! Without an `openai` section every member serves these defaults; a member
//! listed in [`OpenAiConfig::members`] serves exactly what its entry says. A
//! member without the protocol of a request is not eligible
//! ([`OpenAiIneligible`]) unless an enabled IR conversion can serve it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use superba_core::{Error, OpenAiIneligible, RequestProtocol, Result};
use url::Url;

use crate::config::{MemberConfig, MemberKind, RouterConfig, check_upstream_url};

/// The Krill relay's OpenAI-compatible path below a line's origin.
pub const KRILL_OPENAI_PATH: &str = "/codex/v1";

/// The protocols of the ingress.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenAiProtocol {
    /// OpenAI Responses (`POST /openai/v1/responses`).
    Responses,
    /// OpenAI Chat Completions (`POST /openai/v1/chat/completions`).
    Chat,
}

impl OpenAiProtocol {
    /// The protocol of the request log.
    pub fn request_protocol(self) -> RequestProtocol {
        match self {
            OpenAiProtocol::Responses => RequestProtocol::Responses,
            OpenAiProtocol::Chat => RequestProtocol::ChatCompletions,
        }
    }

    /// The path appended to a member's base URL.
    pub fn path(self) -> &'static str {
        match self {
            OpenAiProtocol::Responses => "/responses",
            OpenAiProtocol::Chat => "/chat/completions",
        }
    }

    /// Why a member without this protocol cannot serve it.
    pub fn ineligible(self) -> OpenAiIneligible {
        match self {
            OpenAiProtocol::Responses => OpenAiIneligible::NoResponsesEndpoint,
            OpenAiProtocol::Chat => OpenAiIneligible::NoChatEndpoint,
        }
    }
}

/// Where a member serves one protocol: a path below each line's origin, each
/// line's own base URL, or full base URLs of single lines.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenAiEndpoint {
    /// Path below each line's origin (`/codex/v1`); `None` = each line's own
    /// base URL (the Responses base Codex uses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Full base URLs of single lines, by line id (they override `path`).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub lines: BTreeMap<String, Url>,
}

impl OpenAiEndpoint {
    /// Each line's own base URL.
    pub fn line_base() -> Self {
        Self::default()
    }

    /// `path` below each line's origin.
    pub fn at(path: &str) -> Self {
        Self {
            path: Some(path.to_owned()),
            lines: BTreeMap::new(),
        }
    }
}

/// One model a member serves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAiModel {
    /// Model id.
    pub id: String,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// How one member serves the OpenAI ingress.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenAiMember {
    /// Its native Responses endpoint; absent endpoints may use conversion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub responses: Option<OpenAiEndpoint>,
    /// Its native Chat Completions endpoint; absent endpoints may use conversion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat: Option<OpenAiEndpoint>,
    /// Top-level request fields removed before a request goes to it (a strict
    /// gateway that refuses fields some tools send, such as ZCode's
    /// `thinking` / `enable_thinking` / `reasoning` on Chat Completions).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub drop_fields: Vec<String>,
    /// Models it serves (Superba's list); empty = the router reads the
    /// member's own `/models` when it has to choose between members.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<OpenAiModel>,
}

impl OpenAiMember {
    /// What a member serves when the configuration does not say (module
    /// docs): `serves_codex` is false for members reserved for Claude Code.
    pub fn default_for(member: &MemberConfig, serves_codex: bool) -> Self {
        if !serves_codex {
            return Self::default();
        }
        match member.kind {
            MemberKind::Krill => Self {
                responses: Some(OpenAiEndpoint::at(KRILL_OPENAI_PATH)),
                chat: Some(OpenAiEndpoint::at(KRILL_OPENAI_PATH)),
                ..Self::default()
            },
            MemberKind::Compatible => Self {
                responses: Some(OpenAiEndpoint::line_base()),
                ..Self::default()
            },
            // The official login serves Codex only (the OpenAI ingress
            // leaves it out, RouterConfig::upstream_protocol refuses other
            // clients).
            MemberKind::Official => Self {
                responses: Some(OpenAiEndpoint::line_base()),
                ..Self::default()
            },
        }
    }

    /// Its endpoint for `protocol`.
    pub fn endpoint(&self, protocol: OpenAiProtocol) -> Option<&OpenAiEndpoint> {
        match protocol {
            OpenAiProtocol::Responses => self.responses.as_ref(),
            OpenAiProtocol::Chat => self.chat.as_ref(),
        }
    }

    /// Whether it serves `protocol`, and why not.
    pub fn eligibility(
        &self,
        protocol: OpenAiProtocol,
    ) -> std::result::Result<(), OpenAiIneligible> {
        match self.endpoint(protocol) {
            Some(_) => Ok(()),
            None => Err(protocol.ineligible()),
        }
    }
}

/// The OpenAI ingress.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenAiConfig {
    /// Members whose service differs from the defaults, by member id (each
    /// one of [`RouterConfig::members`]).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub members: BTreeMap<String, OpenAiMember>,
    /// Priority of the members on this ingress (member ids; members left out
    /// follow in [`RouterConfig::members`] order).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<String>,
}

pub(crate) fn check_endpoint(id: &str, member: &MemberConfig, ep: &OpenAiEndpoint) -> Result<()> {
    if let Some(path) = &ep.path
        && !(path.starts_with('/') && !path.contains(['?', '#']))
    {
        return Err(Error::invalid(format!(
            "OpenAI ingress member {id:?}: endpoint path {path:?} must start with '/'"
        )));
    }
    for (line, url) in &ep.lines {
        if !member.lines.iter().any(|l| &l.id == line) {
            return Err(Error::invalid(format!(
                "OpenAI ingress member {id:?}: no line {line:?}"
            )));
        }
        check_upstream_url(url)?;
    }
    Ok(())
}

impl OpenAiConfig {
    /// Check the section against the router configuration.
    pub fn validate(&self, router: &RouterConfig) -> Result<()> {
        for (id, m) in &self.members {
            let Some(member) = router.member(id) else {
                return Err(Error::invalid(format!(
                    "OpenAI ingress member {id:?} is not a router member"
                )));
            };
            for ep in [&m.responses, &m.chat].into_iter().flatten() {
                check_endpoint(id, member, ep)?;
            }
            if m.drop_fields.iter().any(|f| f.trim().is_empty()) {
                return Err(Error::invalid(format!(
                    "OpenAI ingress member {id:?}: empty field name to drop"
                )));
            }
        }
        for id in &self.order {
            if router.member(id).is_none() {
                return Err(Error::invalid(format!(
                    "OpenAI ingress order names {id:?}, not a router member"
                )));
            }
        }
        Ok(())
    }

    /// Drop everything that names a member not in `ids`.
    pub fn retain_members(&mut self, ids: &[&str]) {
        self.members.retain(|id, _| ids.contains(&id.as_str()));
        self.order.retain(|id| ids.contains(&id.as_str()));
    }
}

impl RouterConfig {
    /// How member `id` serves the OpenAI ingress (its entry, else the
    /// defaults); `None` for an unknown member.
    pub fn openai_member(&self, id: &str) -> Option<OpenAiMember> {
        let member = self.member(id)?;
        if member.kind == MemberKind::Official {
            return Some(OpenAiMember::default_for(member, self.serves_codex(id)));
        }
        Some(match self.openai.as_ref().and_then(|o| o.members.get(id)) {
            Some(m) => m.clone(),
            None => OpenAiMember::default_for(member, self.serves_codex(id)),
        })
    }

    /// Whether member `id` serves `protocol` on the OpenAI ingress, and why
    /// not (the reason the router and tool pages show).
    pub fn openai_eligibility(
        &self,
        id: &str,
        protocol: OpenAiProtocol,
    ) -> std::result::Result<(), OpenAiIneligible> {
        if self
            .member(id)
            .is_some_and(|m| m.kind == MemberKind::Official)
        {
            return Err(protocol.ineligible());
        }
        self.upstream_protocol(id, protocol.into())
            .map(|_| ())
            .ok_or(protocol.ineligible())
    }
}
