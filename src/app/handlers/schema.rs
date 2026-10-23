//! Schema handlers: get / upload / validate / replace.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Json;

use crate::dto::schema::{
    ListExtractorsResponseDto, ListSchemasQuery, ListSchemasResponseDto, SchemaDropQuery,
    SchemaDropResponseDto, SchemaDto, SchemaGetQuery, SchemaReplaceBody, SchemaReplaceDto,
    SchemaUploadBody, SchemaUploadDto, SchemaValidateBody, SchemaValidateDto,
    extractor_list_request,
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

/// `GET /v1/schemas` — the namespace's schema version history
/// (`SCHEMA_LIST`).
///
/// Distinct from `GET /v1/schema`, which returns one document. A caller about
/// to upload needs to see what it is adding to.
pub async fn list(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Query(query): Query<ListSchemasQuery>,
) -> Result<Json<ListSchemasResponseDto>, ApiError> {
    let req = query.to_request().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.list_schemas_frames(&req).await;
    state.record("list_schemas", &ident, Outcome::of(&out));
    let frames = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(ListSchemasResponseDto::from_frames(frames)))
}

/// `GET /v1/extractors` — the extractors registered on the shard
/// (`EXTRACTOR_LIST`).
///
/// Read-only: extraction is always-on and cannot be toggled. It is here so a
/// caller reading `extractor_id` on a statement can find out what produced it.
pub async fn extractors(
    State(state): State<EdgeState>,
    headers: HeaderMap,
) -> Result<Json<ListExtractorsResponseDto>, ApiError> {
    let req = extractor_list_request();
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.extractor_list(&req).await;
    state.record("extractor_list", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(ListExtractorsResponseDto::from(resp)))
}

/// `DELETE /v1/schema/{kind}/{name}` — drop one declared predicate or
/// relation type (`SCHEMA_DROP`).
///
/// The per-type counterpart to `PUT /v1/schema`, which replaces the whole
/// document. Existing rows on the dropped type survive as orphans — readable
/// as plain memories, no longer enriched from the typed-graph tables — which
/// is why `?force=true` is needed when any are live.
pub async fn drop(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Path((kind, name)): Path<(String, String)>,
    Query(query): Query<SchemaDropQuery>,
) -> Result<Json<SchemaDropResponseDto>, ApiError> {
    let req = query
        .to_request(&kind, &name)
        .map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let out = client.drop_schema(&req).await;
    state.record("drop_schema", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(SchemaDropResponseDto::from(resp)))
}
