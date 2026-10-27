//! Line order helpers shared by Overview, Providers and the router page.

use superba_core::{Line, LineId, Profile, Site};

use crate::pages::{Action, Data};

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

/// The line of `profile` Codex talks to in direct mode: the applied line while
/// `profile` is the applied provider; `None` in router mode (the router picks
/// per request) or for another provider.
pub fn applied_line(data: &Data, profile: &Profile) -> Option<LineId> {
    if let Some((exit, line)) = data.router_fixed_exit() {
        return (exit.id == profile.id).then_some(line);
    }
    if data.router_enabled() {
        return None;
    }
    let active = data.active_state()?;
    if active.profile_id.as_ref() != Some(&profile.id) {
        return None;
    }
    active.line_id.clone()
}

/// What moving entry `from` to `to` of `ids` (the lines of `profile` in their
/// order) asks for (user bug 105). In direct mode Codex talks to one line, so
/// for the provider in use a move that puts another line than the applied one
/// first is the switch to that line: the same [`super::switch_action`] as the
/// Overview's line popover (hot-switch countdown and confirmation; the switch
/// saves the order when it is applied). Every other move saves the order: the
/// line a switch to the provider tries first and the router's failover order,
/// which a running router reloads at once (rline). `None` when nothing moves.
pub fn reorder_action(
    data: &Data,
    profile: &Profile,
    ids: Vec<LineId>,
    from: usize,
    to: usize,
) -> Option<Action> {
    if data.router_enabled() && !data.router_failover_enabled() {
        return None;
    }
    let order = moved(ids.clone(), from, to);
    if order == ids {
        return None;
    }
    if let Some(applied) = applied_line(data, profile)
        && order.first() != ids.first()
        && let Some(first) = order.first()
        && first != &applied
    {
        return Some(super::switch_action(data, profile, first));
    }
    let mut changed = profile.clone();
    changed.set_line_priority(order);
    Some(Action::SaveProfile {
        profile: Box::new(changed),
        key: None,
    })
}

/// What disabling or enabling `line` of `profile` asks for (user bug 108).
/// Enabling, and disabling a line Codex does not use now, save the profile
/// (a running router reloads its members at once). Disabling the line Codex
/// uses in direct mode is [`LineToggle::Confirm`]: the caller asks, then
/// [`disable_and_switch`]. `None` when the change is refused (the last
/// enabled line, a line of another site).
pub fn toggle_action(
    data: &Data,
    profile: &Profile,
    site: &Site,
    line: &LineId,
    disable: bool,
) -> Option<LineToggle> {
    let mut changed = profile.clone();
    changed.set_line_disabled(site, line, disable).ok()?;
    if disable && applied_line(data, profile).as_ref() == Some(line) {
        let next = changed.enabled_line_ids(site).into_iter().next()?;
        return Some(LineToggle::Confirm { next });
    }
    Some(LineToggle::Save(Action::SaveProfile {
        profile: Box::new(changed),
        key: None,
    }))
}

/// The answer of [`toggle_action`].
#[derive(Debug)]
pub enum LineToggle {
    /// Save the changed profile.
    Save(Action),
    /// The line is the one in use (direct mode): confirm, then switch to `next`.
    Confirm {
        /// The first enabled line after the change.
        next: LineId,
    },
}

/// The confirmed disable of the line in use: save the profile with `line`
/// disabled, then the usual switch to the first enabled line (hot-switch
/// countdown and confirmation). Empty when the change is refused.
pub fn disable_and_switch(profile: &Profile, site: &Site, line: &LineId) -> Vec<Action> {
    let mut changed = profile.clone();
    if changed.set_line_disabled(site, line, true).is_err() {
        return Vec::new();
    }
    let Some(next) = changed.enabled_line_ids(site).into_iter().next() else {
        return Vec::new();
    };
    vec![
        Action::SaveProfile {
            profile: Box::new(changed),
            key: None,
        },
        Action::Switch {
            profile: profile.id.clone(),
            line: Some(next),
            auth: None,
        },
    ]
}

/// True when `line` is the only enabled line of `profile` on `site` (it cannot
/// be disabled).
pub fn last_enabled(profile: &Profile, site: &Site, line: &LineId) -> bool {
    !profile.is_line_disabled(line)
        && site
            .lines
            .iter()
            .all(|l| &l.id == line || profile.is_line_disabled(&l.id))
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
