//! The OpenAI ingress at run time: per member, its Responses and Chat
//! Completions base URL per line (built with each configuration generation;
//! the member's key is the router's `Authorization: Bearer` of the member).

use url::Url;

use super::config::{OpenAiEndpoint, OpenAiMember, OpenAiProtocol};
use crate::config::{RouterConfig, join_path};

/// One member's side of the ingress.
#[derive(Debug)]
pub(crate) struct MemberOpenAi {
    pub cfg: OpenAiMember,
    /// Responses base per line (`None`: not served).
    pub responses: Option<Vec<Url>>,
    /// Chat Completions base per line (`None`: not served).
    pub chat: Option<Vec<Url>>,
}

impl MemberOpenAi {
    /// The base URLs of `protocol`, one per line.
    pub fn bases(&self, protocol: OpenAiProtocol) -> Option<&[Url]> {
        match protocol {
            OpenAiProtocol::Responses => self.responses.as_deref(),
            OpenAiProtocol::Chat => self.chat.as_deref(),
        }
    }
}

/// The ingress of one generation.
#[derive(Debug)]
pub(crate) struct OpenAiRt {
    /// By member index.
    pub members: Vec<MemberOpenAi>,
    /// Member indexes in the ingress's priority order.
    pub order: Vec<usize>,
}

/// `scheme://host[:port]/` of a base URL.
pub(crate) fn origin(base: &Url) -> Url {
    let mut u = base.clone();
    u.set_path("/");
    u.set_query(None);
    u.set_fragment(None);
    u
}

fn endpoint_base(ep: &OpenAiEndpoint, line_id: &str, line_base: &Url) -> Url {
    match (ep.lines.get(line_id), &ep.path) {
        (Some(u), _) => u.clone(),
        (None, Some(path)) => join_path(&origin(line_base), path, None),
        (None, None) => line_base.clone(),
    }
}

impl OpenAiRt {
    /// Build from the configuration (validated).
    pub fn build(config: &RouterConfig) -> Self {
        let members: Vec<MemberOpenAi> = config
            .members
            .iter()
            .map(|m| {
                let cfg = config.openai_member(&m.id).unwrap_or_default();
                let bases = |ep: Option<&OpenAiEndpoint>| {
                    ep.map(|ep| {
                        m.lines
                            .iter()
                            .map(|l| endpoint_base(ep, &l.id, &l.base_url))
                            .collect::<Vec<_>>()
                    })
                };
                MemberOpenAi {
                    responses: bases(cfg.responses.as_ref()),
                    chat: bases(cfg.chat.as_ref()),
                    cfg,
                }
            })
            .collect();
        let listed = config.openai.as_ref().map(|o| o.order.as_slice());
        let mut order: Vec<usize> = listed
            .unwrap_or_default()
            .iter()
            .filter_map(|id| config.members.iter().position(|m| &m.id == id))
            .collect();
        for i in 0..members.len() {
            if !order.contains(&i) {
                order.push(i);
            }
        }
        Self { members, order }
    }

    /// Member indexes that serve `protocol`, in the ingress's order.
    pub fn serving(&self, protocol: OpenAiProtocol) -> Vec<usize> {
        self.order
            .iter()
            .copied()
            .filter(|i| self.members[*i].bases(protocol).is_some())
            .collect()
    }
}
