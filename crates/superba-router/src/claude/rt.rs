//! The Claude Code ingress at run time: per eligible member, its Anthropic
//! base URL per line and its auth header (built with each configuration
//! generation; keys stay in marked-sensitive header values).

use http::{HeaderName, HeaderValue};
use superba_core::{Error, Result};
use url::Url;

use super::config::{AnthropicAuth, ClaudeConfig, ClaudeMember};
use crate::config::{RouterConfig, RouterSecrets};
use crate::translate::{ir::Protocol, routing};

/// One member's Claude Code side.
pub(crate) struct MemberClaude {
    pub cfg: ClaudeMember,
    /// Native protocol selected for this member.
    pub protocol: Protocol,
    /// The auth header.
    pub auth: (HeaderName, HeaderValue),
    /// Anthropic base per line (`ANTHROPIC_BASE_URL` style).
    pub base: Vec<Url>,
}

impl std::fmt::Debug for MemberClaude {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemberClaude").finish_non_exhaustive()
    }
}

/// The ingress of one generation.
#[derive(Debug)]
pub(crate) struct ClaudeRt {
    pub cfg: ClaudeConfig,
    /// By member index (`None`: the member does not serve Claude Code).
    pub members: Vec<Option<MemberClaude>>,
    /// Indexes of the eligible members in Claude Code's priority order.
    pub order: Vec<usize>,
}

impl ClaudeRt {
    /// Build from the configuration; `members` in router order.
    pub fn build(
        cfg: &ClaudeConfig,
        router: &RouterConfig,
        secrets: &RouterSecrets,
    ) -> Result<Self> {
        let members = &router.members;
        let mut out = Vec::with_capacity(members.len());
        for m in members {
            let Some(protocol) = router.upstream_protocol(&m.id, Protocol::Messages) else {
                out.push(None);
                continue;
            };
            let c = cfg.members.get(&m.id).cloned().unwrap_or_default();
            let key = secrets.key(&m.id).ok_or_else(|| Error::Secret {
                message: format!("no API key for router member {:?}", m.id),
            })?;
            let invalid = || Error::Secret {
                message: format!(
                    "the API key of router member {:?} contains invalid characters",
                    m.id
                ),
            };
            let auth = match c.auth {
                AnthropicAuth::XApiKey => {
                    let mut v = HeaderValue::from_str(key.expose()).map_err(|_| invalid())?;
                    v.set_sensitive(true);
                    (HeaderName::from_static("x-api-key"), v)
                }
                AnthropicAuth::Bearer => {
                    let mut v = HeaderValue::from_str(&format!("Bearer {}", key.expose()))
                        .map_err(|_| invalid())?;
                    v.set_sensitive(true);
                    (http::header::AUTHORIZATION, v)
                }
            };
            let member_index = out.len();
            let base = (0..m.lines.len())
                .filter_map(|line| routing::base(router, member_index, line, protocol))
                .collect();
            out.push(Some(MemberClaude {
                cfg: c,
                protocol,
                auth,
                base,
            }));
        }
        let mut order: Vec<usize> = cfg
            .order
            .iter()
            .filter_map(|id| members.iter().position(|m| &m.id == id))
            .filter(|i| out[*i].is_some())
            .collect();
        for (i, m) in out.iter().enumerate() {
            if m.is_some() && !order.contains(&i) {
                order.push(i);
            }
        }
        Ok(Self {
            cfg: cfg.clone(),
            members: out,
            order,
        })
    }

    /// The Claude Code side of member `index` (eligible members only).
    pub fn member(&self, index: usize) -> Option<&MemberClaude> {
        self.members.get(index).and_then(Option::as_ref)
    }
}
