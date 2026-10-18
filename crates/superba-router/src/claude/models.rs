//! `GET /claude/v1/models[/<id>]`: the models the ingress serves, in the
//! Anthropic list shape (`data[]` of `{type: "model", id, display_name,
//! created_at}`, `has_more`, `first_id`, `last_id`): the routes' exact model
//! names, then each Claude Code member's merged model list in member order
//! (Superba's, else fetched), each id once.

use std::collections::HashSet;
use std::sync::Arc;

use http::{Response, StatusCode};
use serde_json::{Value, json};

use super::error::{self, Kind, json_response};
use super::route;
use crate::state::{RespBody, Shared};

/// `created_at` of every entry (the router has no release dates).
const CREATED_AT: &str = "2025-01-01T00:00:00Z";

/// Every model with its display name.
async fn all(shared: &Arc<Shared>) -> Vec<(String, Option<String>)> {
    let generation = shared.current();
    let Some(claude) = generation.claude.as_ref() else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for r in &claude.cfg.routes {
        if !r.model.contains('*') && seen.insert(r.model.clone()) {
            out.push((r.model.clone(), None));
        }
    }
    let members: Vec<usize> = (0..generation.members.len())
        .filter(|m| claude.member(*m).is_some())
        .collect();
    let lists = futures_util::future::join_all(
        members
            .iter()
            .map(|m| route::lists_of(shared, &generation, *m)),
    )
    .await;
    for lists in lists.into_iter().flatten() {
        for m in lists.iter() {
            if seen.insert(m.id.clone()) {
                out.push((m.id.clone(), m.display_name.clone()));
            } else if let Some(e) = out.iter_mut().find(|(id, d)| id == &m.id && d.is_none()) {
                e.1.clone_from(&m.display_name);
            }
        }
    }
    out
}

fn entry(id: &str, display: Option<&str>) -> Value {
    json!({
        "type": "model",
        "id": id,
        "display_name": display.unwrap_or(id),
        "created_at": CREATED_AT,
    })
}

/// The list (`limit`, `after_id` honoured).
pub(super) async fn list(shared: &Arc<Shared>, query: Option<&str>) -> Response<RespBody> {
    let models = all(shared).await;
    let mut limit = models.len().max(1);
    let mut after: Option<String> = None;
    for (k, v) in url::form_urlencoded::parse(query.unwrap_or_default().as_bytes()) {
        match k.as_ref() {
            "limit" => limit = v.parse().unwrap_or(limit).clamp(1, 1000),
            "after_id" => after = Some(v.into_owned()),
            _ => {}
        }
    }
    let start = after
        .and_then(|a| models.iter().position(|(id, _)| *id == a).map(|i| i + 1))
        .unwrap_or(0);
    let page: Vec<&(String, Option<String>)> = models.iter().skip(start).take(limit).collect();
    let has_more = start + page.len() < models.len();
    let data: Vec<Value> = page.iter().map(|(id, d)| entry(id, d.as_deref())).collect();
    json_response(
        StatusCode::OK,
        &json!({
            "data": data,
            "has_more": has_more,
            "first_id": page.first().map(|(id, _)| id),
            "last_id": page.last().map(|(id, _)| id),
        }),
    )
}

/// One model.
pub(super) async fn one(shared: &Arc<Shared>, id: &str) -> Response<RespBody> {
    match all(shared).await.into_iter().find(|(m, _)| m == id) {
        Some((m, d)) => json_response(StatusCode::OK, &entry(&m, d.as_deref())),
        None => error::local(
            StatusCode::NOT_FOUND,
            Kind::NotFound,
            &format!("model {id:?} is not served by the local router"),
        ),
    }
}
