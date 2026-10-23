//! Entity-graph handlers: create / resolve / get / list / traverse, and the
//! authoring half — update / rename / merge / unmerge / tombstone.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Json;

use crate::dto::entity::{
    CreateEntityBody, CreateEntityResponseDto, EntityDetailDto, ListEntitiesQuery,
    ListEntitiesResponseDto, MergeEntityBody, MergeEntityResponseDto, RenameEntityBody,
    ResolveEntityBody, ResolveEntityResponseDto, TombstoneEntityQuery, TombstoneEntityResponseDto,
    TraverseBody, TraverseResponseDto, UnmergeEntityResponseDto, UpdateEntityBody,
    get_request_from_id, tombstone_request, unmerge_request,
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
    Ok(Json(EntityDetailDto::from_get_response(resp)))
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

/// `PUT /v1/entities/{id}` — replace an entity's name, aliases and attribute
/// blob (`ENTITY_UPDATE`).
///
/// PUT because the wire op is a wholesale replace: an omitted `aliases` or
/// `attributes` clears it. `GET /v1/entities/{id}` returns both, so a
/// read-modify-write round-trips.
pub async fn update(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<UpdateEntityBody>,
) -> Result<Json<EntityDetailDto>, ApiError> {
    let req = body.to_request(&id).map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.update_entity(&req).await;
    state.record("update_entity", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(EntityDetailDto::from(resp.entity)))
}

/// `POST /v1/entities/{id}/rename` — change the canonical name
/// (`ENTITY_RENAME`), keeping the old one as an alias unless told otherwise.
pub async fn rename(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<RenameEntityBody>,
) -> Result<Json<EntityDetailDto>, ApiError> {
    let req = body.to_request(&id).map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.rename_entity(&req).await;
    state.record("rename_entity", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(EntityDetailDto::from(resp.entity)))
}

/// `POST /v1/entities/{id}/merge` — fold another entity into this one
/// (`ENTITY_MERGE`). The path id is the survivor.
///
/// Reversible for `grace_period_seconds`, which the response carries so a
/// caller knows how long it has rather than having to know the deployment's
/// configuration.
pub async fn merge(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<MergeEntityBody>,
) -> Result<Json<MergeEntityResponseDto>, ApiError> {
    let req = body.to_request(&id).map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.merge_entities(&req).await;
    state.record("merge_entities", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(MergeEntityResponseDto::from(resp)))
}

/// `POST /v1/entities/{id}/unmerge` — undo a merge (`ENTITY_UNMERGE`). The
/// path id is the entity that was merged away.
pub async fn unmerge(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<UnmergeEntityResponseDto>, ApiError> {
    let req = unmerge_request(&id).map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.unmerge_entity(&req).await;
    state.record("unmerge_entity", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(UnmergeEntityResponseDto::from(resp)))
}

/// `DELETE /v1/entities/{id}` — tombstone an entity (`ENTITY_TOMBSTONE`).
///
/// A tombstone, not an erasure: the row stays and reads stop resolving to it,
/// which is what keeps statements that referenced it interpretable.
pub async fn tombstone(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<TombstoneEntityQuery>,
) -> Result<Json<TombstoneEntityResponseDto>, ApiError> {
    let req = tombstone_request(&id, &query).map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.tombstone_entity(&req).await;
    state.record("tombstone_entity", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(TombstoneEntityResponseDto::from(resp)))
}
