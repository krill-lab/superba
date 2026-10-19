//! Line order helpers shared by Overview, Providers and the router page.

use superba_core::{Line, LineId, Profile, Site};

/// Lines of `site` in the profile's priority order; lines the profile does not list
/// follow in the site's order.
pub fn ordered_lines(site: &Site, profile: Option<&Profile>) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    if let Some(p) = profile {
        for id in p.ordered_line_ids(site) {
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

/// Human-readable priority, distinct from the latest observed upstream.
pub fn priority_text(tr: &crate::i18n::Tr, names: impl IntoIterator<Item = String>) -> String {
    names
        .into_iter()
        .enumerate()
        .map(|(i, name)| {
            if i == 0 {
                tr.f("router.preference.first", &[("line", &name)])
            } else {
                name
            }
        })
        .collect::<Vec<_>>()
        .join(" → ")
}

/// Latest routed line, including why a backup was used. The surrounding UI
/// explains that simultaneous conversations can use different lines.
pub fn route_text(
    tr: &crate::i18n::Tr,
    route: &superba_core::RouterLineRoute,
    name: impl Fn(&str) -> String,
) -> String {
    let line = name(&route.line_id);
    let failed = route
        .failed_lines
        .iter()
        .map(|id| name(id))
        .collect::<Vec<_>>()
        .join("、");
    if failed.is_empty() {
        tr.f("router.preference.current", &[("line", &line)])
    } else {
        tr.f(
            "router.preference.failed_over",
            &[("line", &line), ("failed", &failed)],
        )
    }
}
