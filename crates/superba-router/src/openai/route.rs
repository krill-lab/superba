//! Which members serve a request on the OpenAI ingress, and in which order
//! their lines are tried.
//!
//! 1. The members that serve the request's protocol ([`OpenAiProtocol`]), in
//!    the ingress's order; the others are not eligible, and when none is
//!    left the router answers with each member's reason. Excluded members
//!    are also named in the request details when another member can serve it.
//! 2. With more than one: those whose model list names the model (Superba's
//!    list, else the member's own `/models`; a model-mapping entry counts),
//!    then those whose list is unknown, then the rest. The upstream decides
//!    about a model nobody lists: lists are often incomplete, and a local
//!    refusal would hide what the upstream would answer. A request waits at
//!    most [`LIST_WAIT`] for lists not read yet; a slower list counts as
//!    unknown for it and keeps loading for later requests.
//!
//! Attempt order ([`crate::claude::route::attempts`]): the conversation's
//! pinned line (session × model), that member's other lines, then the other
//! targets with their lines; open breakers skipped unless every line is open.

use std::sync::Arc;
use std::time::Duration;

use super::config::OpenAiProtocol;
use crate::claude::catalog::{Listed, get_list};
use crate::claude::route::Target;
use crate::config::join_path;
use crate::state::{Generation, Shared};
use crate::stats::hub::Tracker;
use crate::stats::meta::RequestMeta;
use http::HeaderMap;

/// How long a request waits for the model lists that decide its order.
pub const LIST_WAIT: Duration = Duration::from_millis(1500);

/// A refusal: status, error code, message.
pub(crate) type Refusal = (u16, &'static str, String);

/// The stickiness key of a request (its session and model).
pub(crate) fn sticky_key(meta: &RequestMeta) -> Option<String> {
    let session = meta.session_id.as_deref()?;
    Some(format!(
        "openai:{session}:{}",
        meta.model.as_deref().unwrap_or("")
    ))
}

/// The member's model list: Superba's, else its own `/models`.
pub(crate) async fn list_of(
    shared: &Shared,
    generation: &Arc<Generation>,
    member: usize,
) -> Option<Arc<Vec<Listed>>> {
    let o = &generation.openai.members[member];
    if !o.cfg.models.is_empty() {
        return Some(Arc::new(
            o.cfg
                .models
                .iter()
                .map(|m| Listed {
                    id: m.id.clone(),
                    display_name: m.display_name.clone(),
                })
                .collect(),
        ));
    }
    let id = &generation.members[member].cfg.id;
    if let Some(models) = generation
        .config
        .claude
        .as_ref()
        .and_then(|c| c.members.get(id))
        .map(|m| &m.models)
        .filter(|m| !m.is_empty())
    {
        return Some(Arc::new(
            models
                .iter()
                .map(|m| Listed {
                    id: m.id.clone(),
                    display_name: m.display_name.clone(),
                })
                .collect(),
        ));
    }
    if let Some(g) = generation
        .config
        .gemini_member(id)
        .filter(|g| !g.models.is_empty())
    {
        return Some(Arc::new(
            g.models
                .iter()
                .map(|m| Listed {
                    id: m.id.clone(),
                    display_name: m.display_name.clone(),
                })
                .collect(),
        ));
    }
    shared
        .openai
        .catalog
        .lists_via(generation, member, "OpenAI", || fetch(generation, member))
        .await
}

/// `GET <base>/models` of member `member` (its Responses base, else its Chat
/// base; the first line that answers).
async fn fetch(generation: &Generation, member: usize) -> Result<Vec<Listed>, String> {
    use crate::translate::{ir::Protocol, routing};
    let id = &generation.members[member].cfg.id;
    let protocol = Protocol::ALL
        .into_iter()
        .find(|p| generation.config.native_protocol(id, *p))
        .ok_or("no native protocol")?;
    let bases: Vec<_> = (0..generation.members[member].lines.len())
        .filter_map(|line| routing::base(&generation.config, member, line, protocol))
        .collect();
    let headers = routing::headers(&HeaderMap::new(), generation, member, protocol);
    let path = if protocol == Protocol::Messages {
        "/v1/models"
    } else {
        "/models"
    };
    let mut last_error = String::from("no line");
    for base in &bases {
        match get_list(generation, &join_path(base, path, None), &headers).await {
            Ok(list) => return Ok(list),
            Err(e) => last_error = e,
        }
    }
    Err(last_error)
}

/// The lists of `members`, those not there within `wait` as unknown (their
/// reads go on and fill the cache).
async fn lists_within(
    shared: &Arc<Shared>,
    generation: &Arc<Generation>,
    members: &[usize],
    wait: Duration,
) -> Vec<Option<Arc<Vec<Listed>>>> {
    let deadline = tokio::time::Instant::now() + wait;
    let reads: Vec<_> = members
        .iter()
        .map(|m| {
            let (s, g, m) = (shared.clone(), generation.clone(), *m);
            tokio::spawn(async move { list_of(&s, &g, m).await })
        })
        .collect();
    let mut out = Vec::with_capacity(reads.len());
    for read in reads {
        out.push(match tokio::time::timeout_at(deadline, read).await {
            Ok(Ok(list)) => list,
            _ => None,
        });
    }
    out
}

/// Every model the ingress's members list, each with the first member that
/// lists it (the `/models` answer).
pub(crate) async fn all_models(
    shared: &Shared,
    generation: &Arc<Generation>,
    bound: Option<&str>,
) -> Vec<(Listed, usize)> {
    let members: Vec<usize> = generation
        .openai
        .order
        .iter()
        .copied()
        .filter(|m| {
            bound.is_none_or(|id| generation.members[*m].cfg.id == id)
                && (generation
                    .config
                    .upstream_protocol(
                        &generation.members[*m].cfg.id,
                        crate::translate::ir::Protocol::Responses,
                    )
                    .is_some()
                    || generation
                        .config
                        .upstream_protocol(
                            &generation.members[*m].cfg.id,
                            crate::translate::ir::Protocol::Chat,
                        )
                        .is_some())
        })
        .collect();
    let lists =
        futures_util::future::join_all(members.iter().map(|m| list_of(shared, generation, *m)))
            .await;
    let mut out: Vec<(Listed, usize)> = Vec::new();
    for (member, list) in members.into_iter().zip(lists) {
        for l in list.iter().flat_map(|l| l.iter()) {
            match out.iter_mut().find(|(x, _)| x.id == l.id) {
                Some((x, _)) if x.display_name.is_none() => {
                    x.display_name.clone_from(&l.display_name)
                }
                Some(_) => {}
                None => out.push((l.clone(), member)),
            }
        }
    }
    out
}

/// Why no member serves `protocol`: each member's reason.
fn nobody(generation: &Generation, protocol: OpenAiProtocol) -> String {
    let reason = protocol.ineligible().text().render_fallback();
    let names: Vec<String> = generation
        .members
        .iter()
        .map(|m| format!("{}: {reason}", m.cfg.name))
        .collect();
    format!(
        "no provider of the local router serves {} ({})",
        protocol.request_protocol().text().render_fallback(),
        names.join("; ")
    )
}

/// Explain every protocol exclusion even when some members remain eligible.
/// Only member labels and capability reasons are recorded, never bodies.
pub(crate) fn note_excluded(
    generation: &Generation,
    protocol: OpenAiProtocol,
    tracker: Option<&mut Tracker>,
) {
    let Some(tracker) = tracker else { return };
    let reason = protocol.ineligible().text().render_fallback();
    let excluded: Vec<_> = generation
        .openai
        .order
        .iter()
        .filter(|i| {
            generation
                .config
                .upstream_protocol(&generation.members[**i].cfg.id, protocol.into())
                .is_none()
        })
        .map(|i| {
            let member = &generation.members[*i].cfg;
            format!("{} ({}): {reason}", member.name, member.id)
        })
        .collect();
    if !excluded.is_empty() {
        // One entry retains every reason within the detail log's entry limit.
        tracker.note(format!("excluded providers: {}", excluded.join("; ")));
    }
}

/// The targets of a request for `model` (`None`: the body names none)
/// speaking `protocol`.
pub(crate) async fn resolve(
    shared: &Arc<Shared>,
    generation: &Arc<Generation>,
    protocol: OpenAiProtocol,
    model: Option<&str>,
    bound: Option<&str>,
) -> Result<Vec<Target>, Refusal> {
    let serving: Vec<usize> = generation
        .openai
        .order
        .iter()
        .copied()
        .filter(|i| {
            bound.is_none_or(|id| generation.members[*i].cfg.id == id)
                && generation
                    .config
                    .upstream_protocol(&generation.members[*i].cfg.id, protocol.into())
                    .is_some()
        })
        .collect();
    if serving.is_empty() {
        return Err((404, "no_provider", nobody(generation, protocol)));
    }
    let target = |member: usize| Target {
        member,
        model: model.unwrap_or_default().to_owned(),
    };
    let Some(model) = model.filter(|_| serving.len() > 1) else {
        return Ok(serving.into_iter().map(target).collect());
    };
    let lists = lists_within(shared, generation, &serving, LIST_WAIT).await;
    let (mut listing, mut unknown, mut rest) = (Vec::new(), Vec::new(), Vec::new());
    for (member, list) in serving.into_iter().zip(lists) {
        let mapped = generation.members[member].cfg.model_map.get(model);
        match list {
            _ if mapped.is_some() => listing.push(target(member)),
            Some(l) if l.iter().any(|m| m.id == model) => listing.push(target(member)),
            Some(_) => rest.push(target(member)),
            None => unknown.push(target(member)),
        }
    }
    listing.append(&mut unknown);
    listing.append(&mut rest);
    Ok(listing)
}
