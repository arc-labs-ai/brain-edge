//! Query-plan introspection: explain / trace.
//!
//! Explain is free — Brain plans the query and returns the plan without
//! running it. Trace is not: it runs the query and reports what happened.
//! Two verbs rather than one with a flag, because the difference is whether
//! work is done.

use std::time::Instant;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Json;

use crate::dto::query::{QueryBody, QueryExplainResponseDto, QueryTraceResponseDto};
use crate::error::ApiError;
use crate::port::Outcome;
use crate::state::EdgeState;

/// `POST /v1/query/explain` — the plan Brain would run, and its estimated
/// cost (`QUERY_EXPLAIN`). Does not execute the query.
pub async fn explain(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Json(body): Json<QueryBody>,
) -> Result<Json<QueryExplainResponseDto>, ApiError> {
    let mut req = body.to_explain().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.query_explain(&req).await;
    state.record("query_explain", &ident, Outcome::of(&out), started.elapsed());
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(QueryExplainResponseDto::from(resp)))
}

/// `POST /v1/query/trace` — run the query and report what happened, with the
/// measured latency (`QUERY_TRACE`).
pub async fn trace(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Json(body): Json<QueryBody>,
) -> Result<Json<QueryTraceResponseDto>, ApiError> {
    let mut req = body.to_trace().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.query_trace(&req).await;
    state.record("query_trace", &ident, Outcome::of(&out), started.elapsed());
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(QueryTraceResponseDto::from(resp)))
}
