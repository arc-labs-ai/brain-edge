//! Entity-graph handlers: create / resolve / get / list / traverse, and the
//! authoring half — update / rename / merge / unmerge / tombstone.

use std::time::Instant;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Json;

use crate::app::extract::{ApiJson, ApiQuery};
use crate::dto::entity::{
    CreateEntityBody, CreateEntityResponseDto, EntityDetailDto, ListEntitiesQuery,
    ListEntitiesResponseDto, ListEntityTypesResponseDto, MergeEntityBody, MergeEntityResponseDto,
    RenameEntityBody, ResolveEntityBody, ResolveEntityResponseDto, TombstoneEntityQuery,
    TombstoneEntityResponseDto, TraverseBody, TraverseResponseDto, UnmergeEntityResponseDto,
    UpdateEntityBody, get_request_from_id, tombstone_request, unmerge_request,
};
use brain_db_sdk::wire::types::EntityTypeListRequest;

use crate::error::ApiError;
use crate::port::Outcome;
use crate::state::EdgeState;

/// `POST /v1/entities` — create a typed entity.
pub async fn create(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<CreateEntityBody>,
) -> Result<Json<CreateEntityResponseDto>, ApiError> {
    let mut req = body.to_request().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    state.admit("create_entity", &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.create_entity(&req).await;
    state.record(
        "create_entity",
        &ident,
        Outcome::of(&out),
        started.elapsed(),
    );
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(CreateEntityResponseDto::from(resp)))
}

/// `POST /v1/entities/resolve` — resolve a candidate name to an entity.
pub async fn resolve(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<ResolveEntityBody>,
) -> Result<Json<ResolveEntityResponseDto>, ApiError> {
    let mut req = body.to_request().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    state.admit("resolve_entity", &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.resolve_entity(&req).await;
    state.record(
        "resolve_entity",
        &ident,
        Outcome::of(&out),
        started.elapsed(),
    );
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(ResolveEntityResponseDto::from(resp)))
}

/// `GET /v1/entities` — enumerate entities in the caller's graph.
pub async fn list(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    ApiQuery(query): ApiQuery<ListEntitiesQuery>,
) -> Result<Json<ListEntitiesResponseDto>, ApiError> {
    let mut req = query.to_request().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    state.admit("list_entities", &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    // The streamed frames rather than the flattened items: the resume cursor
    // only exists on a frame, and without it a page past the first is
    // unreachable.
    let started = Instant::now();
    let out = client.list_entities_frames(&req).await;
    state.record(
        "list_entities",
        &ident,
        Outcome::of(&out),
        started.elapsed(),
    );
    let frames = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(ListEntitiesResponseDto::from_frames(frames)))
}

/// `GET /v1/entities/{id}` — fetch one entity by id.
pub async fn get(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<EntityDetailDto>, ApiError> {
    let mut req = get_request_from_id(&id).map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    state.admit("get_entity", &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.get_entity(&req).await;
    state.record("get_entity", &ident, Outcome::of(&out), started.elapsed());
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(EntityDetailDto::from_get_response(resp)))
}

/// `POST /v1/entities/{id}/traverse` — walk the relation graph from an anchor
/// entity.
pub async fn traverse(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<TraverseBody>,
) -> Result<Json<TraverseResponseDto>, ApiError> {
    let mut req = body.to_request(&id).map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    state.admit("traverse", &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.traverse_relations_frames(&req).await;
    state.record("traverse", &ident, Outcome::of(&out), started.elapsed());
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
    ApiJson(body): ApiJson<UpdateEntityBody>,
) -> Result<Json<EntityDetailDto>, ApiError> {
    let mut req = body.to_request(&id).map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    state.admit("update_entity", &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.update_entity(&req).await;
    state.record(
        "update_entity",
        &ident,
        Outcome::of(&out),
        started.elapsed(),
    );
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(EntityDetailDto::from(resp.entity)))
}

/// `POST /v1/entities/{id}/rename` — change the canonical name
/// (`ENTITY_RENAME`), keeping the old one as an alias unless told otherwise.
pub async fn rename(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<RenameEntityBody>,
) -> Result<Json<EntityDetailDto>, ApiError> {
    let mut req = body.to_request(&id).map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    state.admit("rename_entity", &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.rename_entity(&req).await;
    state.record(
        "rename_entity",
        &ident,
        Outcome::of(&out),
        started.elapsed(),
    );
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
    ApiJson(body): ApiJson<MergeEntityBody>,
) -> Result<Json<MergeEntityResponseDto>, ApiError> {
    let mut req = body.to_request(&id).map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    state.admit("merge_entities", &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.merge_entities(&req).await;
    state.record(
        "merge_entities",
        &ident,
        Outcome::of(&out),
        started.elapsed(),
    );
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
    let mut req = unmerge_request(&id).map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    state.admit("unmerge_entity", &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.unmerge_entity(&req).await;
    state.record(
        "unmerge_entity",
        &ident,
        Outcome::of(&out),
        started.elapsed(),
    );
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
    ApiQuery(query): ApiQuery<TombstoneEntityQuery>,
) -> Result<Json<TombstoneEntityResponseDto>, ApiError> {
    let mut req = tombstone_request(&id, &query).map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    state.admit("tombstone_entity", &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.tombstone_entity(&req).await;
    state.record(
        "tombstone_entity",
        &ident,
        Outcome::of(&out),
        started.elapsed(),
    );
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(TombstoneEntityResponseDto::from(resp)))
}

/// `GET /v1/entity-types` — the entity types this caller can use, with the
/// ids every other entity route takes (`ENTITY_TYPE_LIST`).
///
/// Read-only. It exists because `entity_type_id` is a number the registry
/// assigns and nothing else reported it: a schema document names its types
/// without ids, and a type the extractor coined appears in no document, so a
/// caller holding `acme:Device` from a recalled memory had no way to reach
/// the id `POST /v1/entities` and `GET /v1/entities` require.
pub async fn entity_types(
    State(state): State<EdgeState>,
    headers: HeaderMap,
) -> Result<Json<ListEntityTypesResponseDto>, ApiError> {
    let mut req = EntityTypeListRequest::default();
    let ident = state.resolve(&headers).await?;
    state.admit("entity_type_list", &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.entity_type_list(&req).await;
    state.record(
        "entity_type_list",
        &ident,
        Outcome::of(&out),
        started.elapsed(),
    );
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(ListEntityTypesResponseDto::from(resp)))
}
