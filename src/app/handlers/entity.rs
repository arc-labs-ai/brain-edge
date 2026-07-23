//! Entity-graph handlers: create / resolve / get / list / traverse.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Json;

use crate::dto::entity::{
    CreateEntityBody, CreateEntityResponseDto, EntityDetailDto, ListEntitiesQuery,
    ListEntitiesResponseDto, ResolveEntityBody, ResolveEntityResponseDto, TraverseBody,
    TraverseResponseDto, get_request_from_id,
};
use crate::error::ApiError;
use crate::port::Outcome;
use crate::state::EdgeState;

/// `POST /v1/entities` — create a typed entity.
pub async fn create(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Json(body): Json<CreateEntityBody>,
) -> Result<Json<CreateEntityResponseDto>, ApiError> {
    let req = body.to_request().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.create_entity(&req).await;
    state.record("create_entity", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(CreateEntityResponseDto::from(resp)))
}

/// `POST /v1/entities/resolve` — resolve a candidate name to an entity.
pub async fn resolve(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Json(body): Json<ResolveEntityBody>,
) -> Result<Json<ResolveEntityResponseDto>, ApiError> {
    let req = body.to_request().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.resolve_entity(&req).await;
    state.record("resolve_entity", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(ResolveEntityResponseDto::from(resp)))
}

/// `GET /v1/entities` — enumerate entities in the caller's graph.
pub async fn list(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Query(query): Query<ListEntitiesQuery>,
) -> Result<Json<ListEntitiesResponseDto>, ApiError> {
    let req = query.to_request();
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.list_entities(&req).await;
    state.record("list_entities", &ident, Outcome::of(&out));
    let items = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(ListEntitiesResponseDto::from_items(items)))
}

/// `GET /v1/entities/{id}` — fetch one entity by id.
pub async fn get(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<EntityDetailDto>, ApiError> {
    let req = get_request_from_id(&id).map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.get_entity(&req).await;
    state.record("get_entity", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(EntityDetailDto::from(resp.entity)))
}

/// `POST /v1/entities/{id}/traverse` — walk the relation graph from an anchor
/// entity.
pub async fn traverse(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<TraverseBody>,
) -> Result<Json<TraverseResponseDto>, ApiError> {
    let req = body.to_request(&id).map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.traverse_relations_frames(&req).await;
    state.record("traverse", &ident, Outcome::of(&out));
    let frames = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(TraverseResponseDto::from_frames(frames)))
}
