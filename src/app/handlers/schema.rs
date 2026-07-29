//! Schema handlers: get / upload / validate / replace.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::Json;

use crate::dto::schema::{
    SchemaDto, SchemaGetQuery, SchemaReplaceBody, SchemaReplaceDto, SchemaUploadBody,
    SchemaUploadDto, SchemaValidateBody, SchemaValidateDto,
};
use crate::error::ApiError;
use crate::port::Outcome;
use crate::state::EdgeState;

/// `GET /v1/schema` — the active schema for a namespace, or a historical
/// version when `?version=` is given.
pub async fn get(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Query(query): Query<SchemaGetQuery>,
) -> Result<Json<SchemaDto>, ApiError> {
    let req = query.to_request();
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.get_schema(&req).await;
    state.record("get_schema", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(SchemaDto::from(resp)))
}

/// `POST /v1/schema` — merge a document into the namespace's active schema.
///
/// Additive and versioned. A `schema_version` of `0` in the reply means the
/// upload was rejected — either validation failed, or `dry_run` was set.
pub async fn upload(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Json(body): Json<SchemaUploadBody>,
) -> Result<Json<SchemaUploadDto>, ApiError> {
    let req = body.to_request();
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.upload_schema(&req).await;
    state.record("upload_schema", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(SchemaUploadDto::from(resp)))
}

/// `POST /v1/schema/validate` — parse and check a document without persisting.
pub async fn validate(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Json(body): Json<SchemaValidateBody>,
) -> Result<Json<SchemaValidateDto>, ApiError> {
    let req = body.to_request();
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.validate_schema(&req).await;
    state.record("validate_schema", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(SchemaValidateDto::from(resp)))
}

/// `PUT /v1/schema` — DESTRUCTIVE namespace swap.
///
/// Drops every declared row in the namespace before the new document lands.
/// Entities whose type disappears survive as orphans: readable as plain
/// memories, no longer enriched from the typed-graph tables. `dropped_count`
/// in the reply is how many rows went.
///
/// PUT rather than POST because the semantics really are replace-the-resource,
/// and the method difference is the clearest signal available that this is not
/// the additive path.
pub async fn replace(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Json(body): Json<SchemaReplaceBody>,
) -> Result<Json<SchemaReplaceDto>, ApiError> {
    let req = body.to_request().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.replace_schema(&req).await;
    state.record("replace_schema", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(SchemaReplaceDto::from(resp)))
}
