//! Line order helpers shared by Overview, Providers and the router page.

use superba_core::{Line, LineId, Profile, Site};

/// Lines of `site` in the profile's priority order; lines the profile does not list
/// follow in the site's order.
pub fn ordered_lines(site: &Site, profile: Option<&Profile>) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    if let Some(p) = profile {
        for id in p.line_priority() {
            if let Some(l) = site.line(&id) {
                out.push(l.clone());
            }
        }
    }
    if out.is_empty()
        && let Some(default) = site.default_line()
    {
        out.push(default.clone());
    }
    for l in &site.lines {
        if !out.iter().any(|o| o.id == l.id) {
            out.push(l.clone());
        }
    }
    out
}

/// Move entry `from` to `to` in `ids`.
pub fn moved(mut ids: Vec<LineId>, from: usize, to: usize) -> Vec<LineId> {
    if from < ids.len() && to < ids.len() && from != to {
        let id = ids.remove(from);
        ids.insert(to, id);
    }
    ids
}
