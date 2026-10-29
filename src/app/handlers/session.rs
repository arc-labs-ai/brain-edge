//! Session-lifecycle handlers: provision / list / erase.
//!
//! Scoped to the caller's effective space like the space verbs, but a session
//! has an id of its own, so create and delete name one.

use std::time::Instant;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;

use crate::dto::session::{
    CreateSessionBody, DeleteSessionQuery, ListSessionsQuery, ListSessionsResponseDto,
    SessionErasedDto, SessionProvisionDto, delete_session_request,
};
use crate::error::ApiError;
use crate::port::Outcome;
use crate::state::EdgeState;

/// `POST /v1/sessions` — provision a session in the caller's space
/// (`SESSION_CREATE`). Idempotent on the client-chosen id; `201` on a real
/// create, `200` otherwise.
pub async fn create(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Json(body): Json<CreateSessionBody>,
) -> Result<(StatusCode, Json<SessionProvisionDto>), ApiError> {
    let mut req = body.to_request();
    let ident = state.resolve(&headers).await?;
    state.admit("create_session", &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.create_session(&req).await;
    state.record(
        "create_session",
        &ident,
        Outcome::of(&out),
        started.elapsed(),
    );
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    let status = if resp.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(SessionProvisionDto::from(resp))))
}

/// `GET /v1/sessions` — the sessions in the caller's space (`SESSION_LIST`).
pub async fn list(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Query(query): Query<ListSessionsQuery>,
) -> Result<Json<ListSessionsResponseDto>, ApiError> {
    let mut req = query.to_request();
    let ident = state.resolve(&headers).await?;
    state.admit("list_sessions", &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.list_sessions(&req).await;
    state.record(
        "list_sessions",
        &ident,
        Outcome::of(&out),
        started.elapsed(),
    );
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(ListSessionsResponseDto::from(resp)))
}

/// `DELETE /v1/sessions/{id}` — erase a session (`SESSION_DELETE`).
///
/// Soft by default; `?hard=true` is the irreversible form, which has to be
/// asked for by name rather than being what a bare DELETE does.
pub async fn delete(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Path(id): Path<u64>,
    Query(query): Query<DeleteSessionQuery>,
) -> Result<Json<SessionErasedDto>, ApiError> {
    let mut req = delete_session_request(id, &query);
    let ident = state.resolve(&headers).await?;
    state.admit("delete_session", &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.delete_session(&req).await;
    state.record(
        "delete_session",
        &ident,
        Outcome::of(&out),
        started.elapsed(),
    );
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(SessionErasedDto::from(resp)))
}
