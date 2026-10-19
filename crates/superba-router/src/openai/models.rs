//! `GET /openai/v1/models[/<id>]`: the models the ingress's members serve, in
//! the OpenAI list shape (`{"object": "list", "data": [{id, object: "model",
//! created, owned_by}]}`): each member's list in the ingress's order
//! (Superba's, else fetched), each id once, `owned_by` the first member that
//! lists it.

use std::sync::Arc;

use bytes::Bytes;
use http::header::CONTENT_TYPE;
use http::{HeaderValue, Response, StatusCode};
use serde_json::{Value, json};

use super::route;
use crate::forward::{full, router_error};
use crate::state::{RespBody, Shared};

fn entry(id: &str, owner: &str) -> Value {
    json!({"id": id, "object": "model", "created": 0, "owned_by": owner})
}

fn json_response(body: &Value) -> Response<RespBody> {
    let mut resp = Response::new(full(Bytes::from(body.to_string())));
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    resp
}

/// The list.
pub(super) async fn list(shared: &Arc<Shared>) -> Response<RespBody> {
    let generation = shared.current();
    let models = route::all_models(shared, &generation).await;
    let data: Vec<Value> = models
        .iter()
        .map(|(m, member)| entry(&m.id, &generation.members[*member].cfg.name))
        .collect();
    json_response(&json!({"object": "list", "data": data}))
}

/// One model.
pub(super) async fn one(shared: &Arc<Shared>, id: &str) -> Response<RespBody> {
    let generation = shared.current();
    match route::all_models(shared, &generation)
        .await
        .into_iter()
        .find(|(m, _)| m.id == id)
    {
        Some((m, member)) => json_response(&entry(&m.id, &generation.members[member].cfg.name)),
        None => router_error(
            StatusCode::NOT_FOUND,
            "model_not_found",
            &format!("model {id:?} is not served by the local router"),
        ),
    }
}
