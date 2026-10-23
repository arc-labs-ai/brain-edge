//! Statement-graph handlers: get / list, and the authoring half —
//! create / supersede / tombstone / retract / history.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::http::HeaderMap;
use axum::response::Json;

use crate::dto::statement::{
    CreateStatementBody, CreateStatementResponseDto, GetStatementQuery, ListStatementsQuery,
    ListStatementsResponseDto, RetractStatementResponseDto, StatementDetailDto,
    StatementHistoryQuery, StatementHistoryResponseDto, StatementRetireQuery,
    SupersedeStatementBody, SupersedeStatementResponseDto, TombstoneStatementResponseDto,
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

/// `POST /v1/statements` — assert a claim (`STATEMENT_CREATE`).
///
/// Brain may recognise the claim as a revision of an existing one and
/// supersede it automatically; `auto_superseded` says when it did, so a
/// caller is not left to discover that its "new" statement replaced something.
pub async fn create(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Json(body): Json<CreateStatementBody>,
) -> Result<(StatusCode, Json<CreateStatementResponseDto>), ApiError> {
    let req = body.to_request().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.create_statement(&req).await;
    state.record("create_statement", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok((
        StatusCode::CREATED,
        Json(CreateStatementResponseDto::from(resp)),
    ))
}

/// `POST /v1/statements/{id}/supersede` — replace a claim with a revised one
/// (`STATEMENT_SUPERSEDE`), keeping both and the link between them.
pub async fn supersede(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<SupersedeStatementBody>,
) -> Result<(StatusCode, Json<SupersedeStatementResponseDto>), ApiError> {
    let req = body.to_request(&id).map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.supersede_statement(&req).await;
    state.record("supersede_statement", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok((
        StatusCode::CREATED,
        Json(SupersedeStatementResponseDto::from(resp)),
    ))
}

/// `DELETE /v1/statements/{id}` — retire a claim (`STATEMENT_TOMBSTONE`).
///
/// The row stays and reads stop returning it, which is what keeps anything
/// that referenced it interpretable. For withdrawing the content itself, see
/// `retract`.
pub async fn tombstone(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<StatementRetireQuery>,
) -> Result<Json<TombstoneStatementResponseDto>, ApiError> {
    let req = query.to_tombstone(&id).map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.tombstone_statement(&req).await;
    state.record("tombstone_statement", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(TombstoneStatementResponseDto::from(resp)))
}

/// `POST /v1/statements/{id}/retract` — withdraw a claim
/// (`STATEMENT_RETRACT`).
///
/// Stronger than a tombstone: the content is scheduled to be zeroed, and the
/// response says when. A separate verb rather than a flag on DELETE, because
/// the two differ in whether the text survives — not a detail to hide in a
/// query parameter.
pub async fn retract(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<StatementRetireQuery>,
) -> Result<Json<RetractStatementResponseDto>, ApiError> {
    let req = query.to_retract(&id).map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.retract_statement(&req).await;
    state.record("retract_statement", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(RetractStatementResponseDto::from(resp)))
}

/// `GET /v1/statements/{id}/history` — every version of one claim
/// (`STATEMENT_HISTORY`), oldest first.
pub async fn history(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<StatementHistoryQuery>,
) -> Result<Json<StatementHistoryResponseDto>, ApiError> {
    let req = query.to_request(&id).map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.statement_history_frames(&req).await;
    state.record("statement_history", &ident, Outcome::of(&out));
    let frames = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(StatementHistoryResponseDto::from_frames(frames)))
}
