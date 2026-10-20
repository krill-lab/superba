//! Model namespaces constrain eligibility before failover or stickiness.

use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use http::{HeaderMap, Response, StatusCode};
use serde_json::Value;

use super::{Target, cache, snapshot};
use crate::compaction::Route;
use crate::forward::router_error;
use crate::select::{Candidate, Topology, candidates};
use crate::state::{Generation, RespBody, View, lock};
use crate::sticky::Pin;

#[derive(Clone)]
pub(crate) struct Selection {
    pub targets: Vec<Target>,
}

impl Selection {
    pub(crate) fn standard(&self, member: usize) -> bool {
        self.targets
            .iter()
            .any(|t| t.member == member && t.standard)
    }

    pub(crate) fn base(&self, g: &Generation, c: Candidate) -> Option<url::Url> {
        self.standard(c.member)
            .then(|| cache::standard_base(g, c.member, c.line))
    }

    pub(crate) fn candidates(
        &self,
        g: &Generation,
        route: &Route,
        pin: Option<Pin>,
        now: Instant,
    ) -> Vec<Candidate> {
        struct Selected<'a> {
            view: View<'a>,
            selection: &'a Selection,
        }
        impl Topology for Selected<'_> {
            fn members(&self) -> usize {
                self.view.members()
            }
            fn lines(&self, member: usize) -> usize {
                self.view.lines(member)
            }
            fn eligible(&self, member: usize) -> bool {
                self.view.eligible(member)
                    && self.selection.targets.iter().any(|t| t.member == member)
            }
            fn available(&self, member: usize, line: usize, now: Instant) -> bool {
                self.view.available(member, line, now)
            }
            fn reopens_at(&self, member: usize, line: usize) -> Option<Instant> {
                self.view.reopens_at(member, line)
            }
        }
        candidates(
            &Selected {
                view: View {
                    generation: g,
                    websocket_only: false,
                    route,
                },
                selection: self,
            },
            pin,
            now,
        )
    }
}

pub(crate) async fn select(
    g: &Arc<Generation>,
    headers: &mut HeaderMap,
    body: Bytes,
    limit: usize,
) -> Result<(Bytes, Option<Selection>), Box<Response<RespBody>>> {
    // Read all supported encodings before inspecting the model; an opaque body
    // must never bypass the namespace boundary.
    let (body, _, _) = crate::openai::prepare_responses(headers, body, limit)?;
    let mut request: Value = serde_json::from_slice(&body).map_err(|_| {
        Box::new(router_error(
            StatusCode::BAD_REQUEST,
            "invalid_json",
            "expected a JSON Responses request",
        ))
    })?;
    let Some(model) = request["model"].as_str().map(str::to_owned) else {
        return Ok((body, None));
    };
    let snap = { lock(&g.merged_catalog.latest).clone() };
    let snap = match snap {
        Some(s) => s,
        None => snapshot(g, "client_version=0.163.0").await,
    };
    let (bare, pinned) = namespace(g, &snap, &model);
    if pinned.as_ref().is_some_and(Vec::is_empty) {
        return Err(Box::new(router_error(
            StatusCode::NOT_FOUND,
            "unknown_provider",
            "unknown provider namespace",
        )));
    }
    // An alias has its own single target; a bare id fails over in member order.
    let mut targets = snap
        .routes
        .get(&model)
        .cloned()
        .or_else(|| snap.routes.get(bare).cloned())
        .unwrap_or_default();
    if let Some(members) = &pinned {
        targets.retain(|t| members.contains(&t.member));
    }
    let known = snap.all.contains_key(bare) || snap.all.contains_key(&model);
    if targets.is_empty() {
        if known {
            return Err(Box::new(router_error(
                StatusCode::CONFLICT,
                "model_not_verified",
                "model is pending, failed its tool probe, or has no enabled protocol route; test it on the Models page",
            )));
        }
        match &pinned {
            // A model the pinned provider does not list goes to it unchanged.
            Some(members) => {
                targets = members
                    .iter()
                    .map(|member| Target {
                        member: *member,
                        upstream: bare.into(),
                        standard: false,
                    })
                    .collect();
            }
            None => return Ok((body, None)),
        }
    }
    let upstream = targets.first().map(|t| t.upstream.as_str()).unwrap_or(bare);
    request["model"] = Value::String(upstream.into());
    let bytes = serde_json::to_vec(&request).expect("request JSON");
    Ok((Bytes::from(bytes), Some(Selection { targets })))
}

/// The requested name and the members it is pinned to. A real upstream id
/// (slashes included) is taken as it is; `slug/id` pins the member with that
/// slug; the historical Krill spellings pin Krill members only. Any other
/// prefix pins nothing (an empty list: unknown namespace).
fn namespace<'a>(
    g: &Generation,
    snap: &super::Snapshot,
    model: &'a str,
) -> (&'a str, Option<Vec<usize>>) {
    let Some((prefix, bare)) = model.split_once('/') else {
        return (model, None);
    };
    let Some(config) = &g.config.catalog else {
        return (model, None);
    };
    if snap
        .all
        .get(model)
        .is_some_and(|row| row.sources.iter().any(|p| p.upstream_id == model))
    {
        return (model, None);
    }
    let slugs = config.slugs(&g.config);
    let matching: Vec<_> = g
        .members
        .iter()
        .enumerate()
        .filter(|(_, m)| slugs.get(&m.cfg.id).is_some_and(|s| s == prefix))
        .map(|(i, _)| i)
        .collect();
    if !matching.is_empty() {
        return (bare, Some(matching));
    }
    if matches!(prefix, "krill" | "krill-usage" | "krill-balance") {
        let krill: Vec<_> = g
            .members
            .iter()
            .enumerate()
            .filter(|(_, m)| m.cfg.kind == crate::MemberKind::Krill)
            .map(|(i, _)| i)
            .collect();
        let specific: Vec<_> = krill
            .iter()
            .copied()
            .filter(|i| {
                let m = &g.members[*i].cfg;
                let label = format!("{} {}", m.id, m.name).to_ascii_lowercase();
                match prefix {
                    "krill-usage" => label.contains("usage") || label.contains("用量"),
                    "krill-balance" => label.contains("balance") || label.contains("余额"),
                    _ => true,
                }
            })
            .collect();
        return (
            bare,
            Some(if specific.is_empty() { krill } else { specific }),
        );
    }
    // Some upstreams use slash-containing ids that no list named yet; an
    // unknown prefix is never sent to every provider.
    if snap.routes.contains_key(model) {
        (model, None)
    } else {
        (bare, Some(Vec::new()))
    }
}
