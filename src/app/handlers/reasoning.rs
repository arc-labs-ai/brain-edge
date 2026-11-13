//! Reasoning handlers: plan / reason.

use std::time::Instant;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Json;

use crate::app::extract::ApiJson;
use crate::dto::reasoning::{PlanBody, PlanResponseDto, ReasonBody, ReasonResponseDto};
use crate::error::ApiError;
use crate::port::Outcome;
use crate::state::EdgeState;

/// `POST /v1/plan` — plan a path from a start state to a goal state.
pub async fn plan(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<PlanBody>,
) -> Result<Json<PlanResponseDto>, ApiError> {
    let mut req = body.to_request().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    state.admit("plan", &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;
    let started = Instant::now();
    // THE FRAMES, not the flattened steps. `plan()` concatenates every frame's
    // steps and discards the rest, which throws away the terminal status —
    // the only thing that says whether an empty path was unreachable, out of
    // budget or cancelled — along with the trace the request may have asked
    // for. The steps are still flattened, one layer up.
    let out = client.plan_frames(&req).await;
    state.record("plan", &ident, Outcome::of(&out), started.elapsed());
    let frames = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(PlanResponseDto::from_frames(frames)))
}

/// `POST /v1/reason` — infer over the graph from an observation.
pub async fn reason(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<ReasonBody>,
) -> Result<Json<ReasonResponseDto>, ApiError> {
    let mut req = body.to_request().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    state.admit("reason", &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;
    let started = Instant::now();
    // See the note in `plan`: the terminal status and the trace ride the final
    // frame, and the flattening client hands back neither.
    let out = client.reason_frames(&req).await;
    state.record("reason", &ident, Outcome::of(&out), started.elapsed());
    let frames = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(ReasonResponseDto::from_frames(frames)))
}
