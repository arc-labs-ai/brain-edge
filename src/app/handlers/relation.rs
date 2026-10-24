//! Relation handlers: get / list, and the authoring half — create /
//! supersede / tombstone.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;

use crate::app::names::entity_names;
use crate::dto::relation::{
    CreateRelationBody, CreateRelationResponseDto, GetRelationQuery, ListRelationsQuery,
    ListRelationsResponseDto, RelationDetailDto, RelationSide, SupersedeRelationBody,
    SupersedeRelationResponseDto, TombstoneRelationQuery, TombstoneRelationResponseDto,
    get_request_from, split_from_frames, split_to_frames,
};
use crate::error::ApiError;
use crate::port::Outcome;
use crate::state::EdgeState;

/// `GET /v1/relations/{id}` — fetch one relation by id.
pub async fn get(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<GetRelationQuery>,
) -> Result<Json<RelationDetailDto>, ApiError> {
    let mut req = get_request_from(&id, &query).map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let out = client.get_relation(&req).await;
    state.record("get_relation", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;

    let names = entity_names(&client, RelationDetailDto::entity_ids(&resp.relation)).await;
    let mut dto = RelationDetailDto::from(resp.relation.clone());
    dto.apply_names(&resp.relation, &names);
    Ok(Json(dto))
}

/// `GET /v1/entities/{id}/relations` — enumerate the relations touching an
/// entity, in a chosen direction (`from`/outgoing or `to`/incoming).
pub async fn list(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<ListRelationsQuery>,
) -> Result<Json<ListRelationsResponseDto>, ApiError> {
    let side = query.side().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    // The streamed frames rather than the flattened views: the resume cursor
    // only exists on a frame, and both directions need it — a cursor threaded
    // into one verb leaves the other a dead end that still looks paginated.
    let (views, next_cursor) = match side {
        RelationSide::From => {
            let mut req = query.to_from_request(&id).map_err(ApiError::bad_request)?;
            ident.scope(&mut req);
            let out = client.list_relations_from_frames(&req).await;
            state.record("list_relations", &ident, Outcome::of(&out));
            split_from_frames(out.map_err(|e| ApiError::from_brain(&e))?)
        }
        RelationSide::To => {
            let mut req = query.to_to_request(&id).map_err(ApiError::bad_request)?;
            ident.scope(&mut req);
            let out = client.list_relations_to_frames(&req).await;
            state.record("list_relations", &ident, Outcome::of(&out));
            split_to_frames(out.map_err(|e| ApiError::from_brain(&e))?)
        }
    };
    let names = entity_names(
        &client,
        views.iter().flat_map(RelationDetailDto::entity_ids),
    )
    .await;
    Ok(Json(ListRelationsResponseDto::from_views_named(
        views,
        &names,
        next_cursor,
    )))
}

/// `POST /v1/relations` — assert a typed edge (`RELATION_CREATE`).
pub async fn create(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Json(body): Json<CreateRelationBody>,
) -> Result<(StatusCode, Json<CreateRelationResponseDto>), ApiError> {
    let mut req = body.to_request().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let out = client.create_relation(&req).await;
    state.record("create_relation", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok((
        StatusCode::CREATED,
        Json(CreateRelationResponseDto::from(resp)),
    ))
}

/// `POST /v1/relations/{id}/supersede` — replace an edge with a revised one
/// (`RELATION_SUPERSEDE`).
///
/// The replacement is a whole edge, and it REPLACES the property blob — which
/// is why `GET /v1/relations/{id}` returns `properties`, so a revision that
/// only meant to change validity can carry the old ones back.
pub async fn supersede(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<SupersedeRelationBody>,
) -> Result<(StatusCode, Json<SupersedeRelationResponseDto>), ApiError> {
    let mut req = body.to_request(&id).map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let out = client.supersede_relation(&req).await;
    state.record("supersede_relation", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok((
        StatusCode::CREATED,
        Json(SupersedeRelationResponseDto::from(resp)),
    ))
}

/// `DELETE /v1/relations/{id}` — retire an edge (`RELATION_TOMBSTONE`).
pub async fn tombstone(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<TombstoneRelationQuery>,
) -> Result<Json<TombstoneRelationResponseDto>, ApiError> {
    let mut req = query.to_request(&id).map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let out = client.tombstone_relation(&req).await;
    state.record("tombstone_relation", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(TombstoneRelationResponseDto::from(resp)))
}
