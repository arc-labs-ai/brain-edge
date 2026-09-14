//! Memory verb handlers: encode / recall / forget.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Json;
use brain_db_sdk::wire::types::{MemoryInspectRequest, RecallScopeWire};
use brain_db_sdk::{EncodeBuilder, ForgetBuilder, RecallBuilder};

use crate::dto::memory::{
    EncodeBody, EncodeResponseDto, ForgetBody, ForgetResponseDto, MemoryInspectDto,
    MemoryListPageDto, MemoryListQuery, RecallBody, RecallResponseDto,
};
use crate::dto::parse_memory_id;
use crate::error::ApiError;
use crate::port::Outcome;
use crate::state::EdgeState;

/// `POST /v1/memories` — store a memory.
pub async fn encode(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Json(body): Json<EncodeBody>,
) -> Result<Json<EncodeResponseDto>, ApiError> {
    if body.text.trim().is_empty() {
        return Err(ApiError::bad_request("text must not be blank"));
    }
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let mut builder = EncodeBuilder::new(body.text);
    if let Some(sess) = body.session {
        builder = builder.session(sess);
    }
    if let Some(at) = body.occurred_at {
        builder = builder.occurred_at(at);
    }
    let out = client.encode(&builder.build()).await;
    state.record("encode", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(EncodeResponseDto::from(resp)))
}

/// `POST /v1/recall` — recall memories for a cue.
pub async fn recall(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Json(body): Json<RecallBody>,
) -> Result<Json<RecallResponseDto>, ApiError> {
    if body.query.trim().is_empty() {
        return Err(ApiError::bad_request("query must not be blank"));
    }
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let mut builder = RecallBuilder::new(body.query)
        .include_edges(false)
        .include_graph(false)
        .include_text(true);
    if let Some(max) = body.max_results {
        builder = builder.max_results(max);
    }
    if let Some(subject) = body.subject {
        builder = builder.subject(subject);
    }
    if let Some(scope) = body.scope.as_deref() {
        let scope = match scope.to_ascii_lowercase().as_str() {
            "space" => RecallScopeWire::Space,
            "namespace" => RecallScopeWire::Namespace,
            other => {
                return Err(ApiError::bad_request(format!(
                    "scope must be \"space\" or \"namespace\", got {other:?}"
                )))
            }
        };
        builder = builder.scope(scope);
    }
    let out = client.recall(&builder.build()).await;
    state.record("recall", &ident, Outcome::of(&out));
    let answer = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(RecallResponseDto::from(answer)))
}

/// `GET /v1/memories` — paginated enumeration of the caller's memories.
/// Not RECALL: no cue, no ranking. Walks the tenant timeline newest-first and
/// returns a page plus an opaque keyset cursor.
pub async fn list(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Query(query): Query<MemoryListQuery>,
) -> Result<Json<MemoryListPageDto>, ApiError> {
    let req = query.to_request().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.memory_list_frames(&req).await;
    state.record("memory_list", &ident, Outcome::of(&out));
    let frames = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(MemoryListPageDto::from_frames(frames)))
}

/// `GET /v1/memories/{id}/inspect` — one memory's durable write-artifact
/// bundle (embedding vector, stored record, analyzed keyword terms, write-time
/// HyPE questions, extracted graph) + its text. `found = false` for an id that
/// doesn't exist under the caller's scope.
pub async fn inspect(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<MemoryInspectDto>, ApiError> {
    let wire_id = parse_memory_id(&id).map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let req = MemoryInspectRequest {
        memory_id: wire_id.to_be_bytes(),
        act_as: None,
    };
    let out = client.memory_inspect(&req).await;
    state.record("memory_inspect", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(MemoryInspectDto::from(resp)))
}

/// `DELETE /v1/memories` — forget a memory.
pub async fn forget(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Json(body): Json<ForgetBody>,
) -> Result<Json<ForgetResponseDto>, ApiError> {
    let id = body.parse_id().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let mut builder = ForgetBuilder::new(id);
    if body.hard {
        builder = builder.hard();
    }
    let out = client.forget(&builder.build()).await;
    state.record("forget", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(ForgetResponseDto::from(resp)))
}
