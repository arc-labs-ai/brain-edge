//! Relation-graph handlers: get one / list by direction.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Json;

use crate::dto::relation::{
    GetRelationQuery, ListRelationsQuery, ListRelationsResponseDto, RelationDetailDto, RelationSide,
    get_request_from,
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
    let req = get_request_from(&id, &query).map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.get_relation(&req).await;
    state.record("get_relation", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(RelationDetailDto::from(resp.relation)))
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

    let views = match side {
        RelationSide::From => {
            let req = query.to_from_request(&id).map_err(ApiError::bad_request)?;
            let out = client.list_relations_from(&req).await;
            state.record("list_relations", &ident, Outcome::of(&out));
            out.map_err(|e| ApiError::from_brain(&e))?
        }
        RelationSide::To => {
            let req = query.to_to_request(&id).map_err(ApiError::bad_request)?;
            let out = client.list_relations_to(&req).await;
            state.record("list_relations", &ident, Outcome::of(&out));
            out.map_err(|e| ApiError::from_brain(&e))?
        }
    };
    Ok(Json(ListRelationsResponseDto::from_views(views)))
}
