//! Space-lifecycle handlers: provision / list / erase.
//!
//! None of them takes a path parameter. All three act on the caller's
//! effective space, which Brain resolves from `act_as` before its handler runs
//! and never takes from the client — a credential cannot address a space it is
//! not bound to, so there is nothing for a path to name.

use std::time::Instant;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;

use crate::dto::space::{
    CreateSpaceBody, ListSpacesQuery, ListSpacesResponseDto, SpaceErasedDto, SpaceProvisionDto,
    delete_space_request,
};
use crate::error::ApiError;
use crate::port::Outcome;
use crate::state::EdgeState;

/// `POST /v1/spaces` — provision the caller's space (`SPACE_CREATE`).
///
/// Idempotent: `201` when this call created it, `200` when it was already
/// there. `created` says which in both cases, because the rest of the body is
/// identical either way.
pub async fn create(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    body: Option<Json<CreateSpaceBody>>,
) -> Result<(StatusCode, Json<SpaceProvisionDto>), ApiError> {
    // The body is optional: there is nothing required in it, and making a
    // provision call carry `{}` to succeed is friction for no gain.
    let body = body.map_or_else(CreateSpaceBody::default, |Json(b)| b);
    let mut req = body.to_request();
    let ident = state.resolve(&headers).await?;
    state.admit("create_space", &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.create_space(&req).await;
    state.record("create_space", &ident, Outcome::of(&out), started.elapsed());
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    let status = if resp.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(SpaceProvisionDto::from(resp))))
}

/// `GET /v1/spaces` — the caller's namespace's spaces (`SPACE_LIST`).
pub async fn list(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Query(query): Query<ListSpacesQuery>,
) -> Result<Json<ListSpacesResponseDto>, ApiError> {
    let mut req = query.to_request();
    let ident = state.resolve(&headers).await?;
    state.admit("list_spaces", &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.list_spaces(&req).await;
    state.record("list_spaces", &ident, Outcome::of(&out), started.elapsed());
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(ListSpacesResponseDto::from(resp)))
}

/// `DELETE /v1/spaces` — erase the caller's space and every row under it
/// (`SPACE_DELETE`). Hard, immediate erasure, not a soft delete.
pub async fn delete(
    State(state): State<EdgeState>,
    headers: HeaderMap,
) -> Result<Json<SpaceErasedDto>, ApiError> {
    let mut req = delete_space_request();
    let ident = state.resolve(&headers).await?;
    state.admit("delete_space", &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.delete_space(&req).await;
    state.record("delete_space", &ident, Outcome::of(&out), started.elapsed());
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(SpaceErasedDto::from(resp)))
}
