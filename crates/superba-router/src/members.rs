//! Router members built from Superba sites, so callers never assemble upstream
//! URLs by hand.
//!
//! | Site kind | Line base URL | Probe | WebSocket |
//! |---|---|---|---|
//! | Krill | `<origin>/backend-api/codex` (or `/codex/v1`) | `<origin>/v1/models` (401 = alive) | passthrough; Krill answers 426 per account when off |
//! | Compatible | `<origin><path>` with the path of the profile's base URL (e.g. `/v1`) | `<base>/models` | refused locally (426), Codex uses HTTP |
//! | Official | refused: the official ChatGPT login is never routed | | |

use superba_core::{CodexPath, Error, LineId, Result, Site, SiteKind};
use url::Url;

use crate::config::{LineConfig, MemberConfig, MemberKind, check_upstream_url};

/// Turn a site or line id into a router id (`[a-z0-9._-]`, at most 64 bytes).
pub fn router_id(raw: &str) -> String {
    let mut id: String = raw
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    id.truncate(64);
    if id.is_empty() { "member".into() } else { id }
}

/// Which path below each line origin the member serves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemberPath {
    /// A Krill relay path (`/backend-api/codex` or `/codex/v1`).
    Codex(CodexPath),
    /// A compatible site's API path, e.g. `/v1` (the path of the profile's
    /// `base_url_override`); empty for the origin itself.
    Custom(String),
}

impl MemberPath {
    /// The path of a compatible site's full base URL (`https://api.x.com/v1` → `/v1`).
    pub fn of_base_url(base: &Url) -> Self {
        MemberPath::Custom(base.path().trim_end_matches('/').to_owned())
    }

    fn suffix(&self) -> &str {
        match self {
            MemberPath::Codex(p) => p.suffix(),
            MemberPath::Custom(p) => p.as_str(),
        }
    }
}

/// A member for `site`: its lines in `line_order` (empty = the site's order;
/// otherwise only the listed lines, in that order), each at `path`.
pub fn member_from_site(
    site: &Site,
    line_order: &[LineId],
    path: &MemberPath,
) -> Result<MemberConfig> {
    let kind = MemberKind::for_site(site.kind)?;
    if let MemberPath::Custom(p) = path
        && !(p.is_empty() || (p.starts_with('/') && !p.contains(['?', '#'])))
    {
        return Err(Error::invalid(format!(
            "member path {p:?} must be empty or start with '/'"
        )));
    }
    let lines: Vec<_> = if line_order.is_empty() {
        site.lines.iter().collect()
    } else {
        line_order
            .iter()
            .map(|id| {
                site.line(id).ok_or_else(|| Error::NotFound {
                    what: format!("line {id} of site {}", site.id),
                })
            })
            .collect::<Result<_>>()?
    };
    if lines.is_empty() {
        return Err(Error::invalid(format!("site {} has no line", site.id)));
    }
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        let base_url = line.origin.join(path.suffix());
        check_upstream_url(&base_url)?;
        let probe_url = match site.kind {
            SiteKind::Krill => Some(line.origin.join(&site.endpoints.models_unauth)),
            _ => None,
        };
        out.push(LineConfig {
            id: router_id(line.id.as_str()),
            name: line.name.fallback.clone(),
            base_url,
            probe_url,
            zstd: None,
            websocket: None,
        });
    }
    Ok(MemberConfig {
        id: router_id(site.id.as_str()),
        name: site.name.fallback.clone(),
        kind,
        lines: out,
        websocket: kind == MemberKind::Krill,
        model_map: Default::default(),
    })
}
