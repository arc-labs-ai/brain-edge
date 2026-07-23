//! Statement-graph handlers: get / list.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Json;

use crate::dto::statement::{
    GetStatementQuery, ListStatementsQuery, ListStatementsResponseDto, StatementDetailDto,
    get_request_from,
};
use crate::error::ApiError;
use crate::port::Outcome;
use crate::state::EdgeState;

/// `GET /v1/statements/{id}` — fetch one statement by id.
pub async fn get(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<GetStatementQuery>,
) -> Result<Json<StatementDetailDto>, ApiError> {
    let req = get_request_from(&id, &query).map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.get_statement(&req).await;
    state.record("get_statement", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(StatementDetailDto::from(resp.statement)))
}

/// `GET /v1/statements` — enumerate statements in the caller's graph.
pub async fn list(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Query(query): Query<ListStatementsQuery>,
) -> Result<Json<ListStatementsResponseDto>, ApiError> {
    let req = query.to_request().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.list_statements(&req).await;
    state.record("list_statements", &ident, Outcome::of(&out));
    let views = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(ListStatementsResponseDto::from_views(views)))
}
