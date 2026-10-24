//! Reasoning handlers: plan / reason.

use std::time::Instant;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Json;

use crate::dto::reasoning::{PlanBody, PlanResponseDto, ReasonBody, ReasonResponseDto};
use crate::error::ApiError;
use crate::port::Outcome;
use crate::state::EdgeState;

/// `POST /v1/plan` — plan a path from a start state to a goal state.
pub async fn plan(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Json(body): Json<PlanBody>,
) -> Result<Json<PlanResponseDto>, ApiError> {
    let mut req = body.to_request().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;
    let started = Instant::now();
    let out = client.plan(&req).await;
    state.record("plan", &ident, Outcome::of(&out), started.elapsed());
    let steps = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(PlanResponseDto::from(steps)))
}

/// `POST /v1/reason` — infer over the graph from an observation.
pub async fn reason(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Json(body): Json<ReasonBody>,
) -> Result<Json<ReasonResponseDto>, ApiError> {
    let mut req = body.to_request().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;
    let started = Instant::now();
    let out = client.reason(&req).await;
    state.record("reason", &ident, Outcome::of(&out), started.elapsed());
    let steps = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(ReasonResponseDto::from(steps)))
}
