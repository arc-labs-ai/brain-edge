//! Memory-graph edge handlers: link / unlink.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::Json;
use brain_db_sdk::new_id;
use brain_db_sdk::wire::types::{LinkRequest, UnlinkRequest};

use crate::dto::graph::{
    GraphFetchQuery, GraphPageDto, LinkBody, LinkResponseDto, UnlinkBody, UnlinkResponseDto,
};
use crate::error::ApiError;
use crate::port::Outcome;
use crate::state::EdgeState;

/// `GET /v1/graph` — paginated export of the caller's typed graph as nodes +
/// edges. Not RECALL: no cue, no ranking. Paginates the subject-anchored
/// statement index and derives the entity set from traversal.
pub async fn fetch(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Query(query): Query<GraphFetchQuery>,
) -> Result<Json<GraphPageDto>, ApiError> {
    let req = query.to_request().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.graph_fetch_frames(&req).await;
    state.record("graph_fetch", &ident, Outcome::of(&out));
    let frames = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(GraphPageDto::from_frames(frames)))
}

/// `POST /v1/links` — create/overwrite a directed edge between two memories.
pub async fn link(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Json(body): Json<LinkBody>,
) -> Result<Json<LinkResponseDto>, ApiError> {
    let (source, target, kind, weight) = body.parts().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let req = LinkRequest {
        source,
        target,
        kind,
        weight,
        request_id: new_id(),
        txn_id: None,
        act_as: None,
    };
    let out = client.link(&req).await;
    state.record("link", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(LinkResponseDto::from(resp)))
}

/// `DELETE /v1/links` — remove a directed edge (idempotent).
pub async fn unlink(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Json(body): Json<UnlinkBody>,
) -> Result<Json<UnlinkResponseDto>, ApiError> {
    let (source, target, kind) = body.parts().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let req = UnlinkRequest {
        source,
        target,
        kind,
        request_id: new_id(),
        txn_id: None,
        act_as: None,
    };
    let out = client.unlink(&req).await;
    state.record("unlink", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(UnlinkResponseDto::from(resp)))
}
