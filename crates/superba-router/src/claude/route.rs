//! Which members serve a Claude Code request, with which model name, and in
//! which order their lines are tried.
//!
//! 1. Aliases map Claude Code's own ids first (`claude-haiku-*` → a model).
//! 2. The first route whose pattern matches gives the targets.
//! 3. Otherwise: the eligible members whose model list names the model, in
//!    member order, then those whose list is unknown (it could not be read).
//!
//! Attempt order: the conversation's pinned line (session × model), that
//! member's other lines, then the other targets with their lines; open
//! breakers skipped unless every line is open (then the one reopening first).

use std::sync::Arc;
use std::time::Instant;

use super::catalog::Listed;
use super::config::matches;
use super::error::Kind;
use crate::select::Candidate;
use crate::state::{Generation, Shared, lock};
use crate::sticky::Pin;

/// One way to serve the request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    /// Member index.
    pub member: usize,
    /// Model name sent upstream.
    pub model: String,
}

/// The routing decision for a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Resolution {
    /// The model that serves the request (after aliases).
    pub model: String,
    /// Targets in failover order.
    pub targets: Vec<Target>,
}

/// A refusal: status, error kind, message.
pub(crate) type Refusal = (u16, Kind, String);

/// The member's model list: Superba's (configuration) or its own.
pub(crate) async fn lists_of(
    shared: &Shared,
    generation: &Arc<Generation>,
    member: usize,
) -> Option<Arc<Vec<Listed>>> {
    let c = generation.claude.as_ref()?.member(member)?;
    if !c.cfg.models.is_empty() {
        return Some(Arc::new(
            c.cfg
                .models
                .iter()
                .map(|m| Listed {
                    id: m.id.clone(),
                    display_name: m.display_name.clone(),
                })
                .collect(),
        ));
    }
    shared.claude.catalog.lists(generation, member).await
}

/// The routed model for `requested` (aliases applied).
pub(crate) fn alias(generation: &Generation, requested: &str) -> String {
    generation
        .claude
        .as_ref()
        .and_then(|c| c.cfg.aliases.iter().find(|a| matches(&a.from, requested)))
        .map(|a| a.to.clone())
        .unwrap_or_else(|| requested.to_owned())
}

/// Decide the targets for `requested`.
pub(crate) async fn resolve(
    shared: &Shared,
    generation: &Arc<Generation>,
    requested: &str,
) -> Result<Resolution, Refusal> {
    let Some(claude) = generation.claude.as_ref() else {
        return Err((
            404,
            Kind::NotFound,
            "Claude Code is not set up in the local router".into(),
        ));
    };
    let model = alias(generation, requested);
    if let Some(route) = claude.cfg.routes.iter().find(|r| matches(&r.model, &model)) {
        let targets = route
            .targets
            .iter()
            .filter_map(|t| {
                let member = generation
                    .members
                    .iter()
                    .position(|m| m.cfg.id == t.member)?;
                claude.member(member)?;
                Some(Target {
                    member,
                    model: t.model.clone().unwrap_or_else(|| model.clone()),
                })
            })
            .collect();
        return Ok(Resolution { model, targets });
    }
    let members: Vec<usize> = (0..generation.members.len())
        .filter(|m| claude.member(*m).is_some())
        .collect();
    // Every member's list at once (a slow provider delays only itself).
    let lists =
        futures_util::future::join_all(members.iter().map(|m| lists_of(shared, generation, *m)))
            .await;
    let mut listing = Vec::new();
    let mut unknown = Vec::new();
    for (member, list) in members.into_iter().zip(lists) {
        let target = Target {
            member,
            model: model.clone(),
        };
        match list {
            Some(l) if l.iter().any(|m| m.id == model) => listing.push(target),
            Some(_) => {}
            None => unknown.push(target),
        }
    }
    listing.extend(unknown);
    if listing.is_empty() {
        return Err((
            404,
            Kind::NotFound,
            format!(
                "model {model:?} is not available through the local router: no Claude Code provider lists it and no route names it"
            ),
        ));
    }
    Ok(Resolution {
        model,
        targets: listing,
    })
}

/// One attempt: a target on one of its member's lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Attempt {
    /// Target index.
    pub target: usize,
    /// Member and line.
    pub c: Candidate,
}

/// The attempt order (see the module docs).
pub(crate) fn attempts(
    generation: &Generation,
    targets: &[Target],
    pin: Option<Pin>,
    now: Instant,
) -> Vec<Attempt> {
    let lines = |m: usize| generation.members[m].lines.len();
    let mut order = Vec::new();
    let pinned = pin.and_then(|p| {
        let t = targets.iter().position(|t| t.member == p.member)?;
        (p.line < lines(p.member)).then_some((t, p))
    });
    if let Some((t, p)) = pinned {
        for line in std::iter::once(p.line).chain((0..lines(p.member)).filter(|l| *l != p.line)) {
            order.push(Attempt {
                target: t,
                c: Candidate {
                    member: p.member,
                    line,
                },
            });
        }
    }
    for (t, target) in targets.iter().enumerate() {
        if pinned.is_some_and(|(pt, _)| pt == t) {
            continue;
        }
        for line in 0..lines(target.member) {
            order.push(Attempt {
                target: t,
                c: Candidate {
                    member: target.member,
                    line,
                },
            });
        }
    }
    let health = |a: &Attempt| lock(&generation.members[a.c.member].lines[a.c.line].health);
    let available: Vec<Attempt> = order
        .iter()
        .copied()
        .filter(|a| health(a).is_available(now))
        .collect();
    if !available.is_empty() {
        return available;
    }
    order
        .into_iter()
        .min_by_key(|a| health(a).open_until())
        .into_iter()
        .collect()
}
