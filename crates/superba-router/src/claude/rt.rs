//! The Claude Code ingress at run time: per eligible member, its Anthropic
//! base URL per line and its auth header (built with each configuration
//! generation; keys stay in marked-sensitive header values).

use http::{HeaderName, HeaderValue};
use superba_core::{Error, Result};
use url::Url;

use super::config::{AnthropicAuth, ClaudeConfig, ClaudeMember, Endpoint};
use crate::config::{MemberConfig, RouterSecrets, join_path};

/// One member's Claude Code side.
pub(crate) struct MemberClaude {
    pub cfg: ClaudeMember,
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
}

/// `scheme://host[:port]/` of a base URL.
fn origin(base: &Url) -> Url {
    let mut u = base.clone();
    u.set_path("/");
    u.set_query(None);
    u.set_fragment(None);
    u
}

fn endpoint_base(ep: &Endpoint, line_id: &str, line_base: &Url) -> Url {
    match ep.lines.get(line_id) {
        Some(u) => u.clone(),
        None => join_path(&origin(line_base), &ep.path, None),
    }
}

impl ClaudeRt {
    /// Build from the configuration; `members` in router order.
    pub fn build(
        cfg: &ClaudeConfig,
        members: &[MemberConfig],
        secrets: &RouterSecrets,
    ) -> Result<Self> {
        let mut out = Vec::with_capacity(members.len());
        for m in members {
            let Some((c, ep)) = cfg
                .members
                .get(&m.id)
                .and_then(|c| c.anthropic.as_ref().map(|ep| (c, ep)))
            else {
                out.push(None);
                continue;
            };
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
            let base = m
                .lines
                .iter()
                .map(|l| endpoint_base(ep, &l.id, &l.base_url))
                .collect();
            out.push(Some(MemberClaude {
                cfg: c.clone(),
                auth,
                base,
            }));
        }
        Ok(Self {
            cfg: cfg.clone(),
            members: out,
        })
    }

    /// The Claude Code side of member `index` (eligible members only).
    pub fn member(&self, index: usize) -> Option<&MemberClaude> {
        self.members.get(index).and_then(Option::as_ref)
    }
}
