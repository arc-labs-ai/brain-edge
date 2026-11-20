//! Schema handlers: get / upload / validate / replace.

use std::time::Instant;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Json;

use crate::app::extract::{ApiJson, ApiQuery};
use crate::dto::schema::{
    ListExtractorsResponseDto, ListSchemasQuery, ListSchemasResponseDto, SchemaDropQuery,
    SchemaDropResponseDto, SchemaDto, SchemaErrorDto, SchemaGetQuery, SchemaReplaceBody,
    SchemaReplaceDto, SchemaUploadBody, SchemaUploadDto, SchemaValidateBody, SchemaValidateDto,
    extractor_list_request,
};
use crate::error::ApiError;
use crate::port::{Op, Outcome};
use crate::state::EdgeState;

/// The severity Brain marks a blocking validation error with.
const SEVERITY_ERROR: u8 = 2;

/// Refuse a write whose document did not become the active schema.
///
/// Brain reports a rejected document inside a SUCCESSFUL wire response: the
/// reply carries `schema_version: 0` and the parse errors, and the call itself
/// did not fail. Handed straight to the caller that is `200 OK` for a request
/// which stored nothing — so every client that branches on the status, which
/// is all three SDKs and the console, reads a schema that does not exist as
/// live, and the next typed-graph call fails for a reason the caller has no
/// way to connect to the upload it watched succeed.
///
/// `dry_run` is the one case where version `0` is the correct outcome: the
/// caller asked for the document to be checked and not stored, so a clean
/// dry run is a success.
fn ensure_stored(
    schema_version: u32,
    errors: &[SchemaErrorDto],
    dry_run: bool,
) -> Result<(), ApiError> {
    let blocking = errors
        .iter()
        .filter(|e| e.severity >= SEVERITY_ERROR)
        .count();
    if blocking > 0 {
        let first = errors
            .iter()
            .find(|e| e.severity >= SEVERITY_ERROR)
            .map_or_else(String::new, |e| e.message.clone());
        let plural = if blocking == 1 { "error" } else { "errors" };
        return Err(ApiError::schema_rejected(format!(
            "schema not stored: {blocking} validation {plural}; first: {first}"
        )));
    }
    if !dry_run && schema_version == 0 {
        // No validation error and still no version: the document parsed but
        // the change was refused. The reachable case is a breaking change
        // without `allow_breaking`, which is a conflict with what the
        // namespace already declares rather than a fault in the document.
        return Err(ApiError::conflict(
            "schema not stored: the document conflicts with the namespace's active schema; \
             set allow_breaking to accept a breaking change",
        ));
    }
    Ok(())
}

/// `GET /v1/schema` — the active schema for a namespace, or a historical
/// version when `?version=` is given.
pub async fn get(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    ApiQuery(query): ApiQuery<SchemaGetQuery>,
) -> Result<Json<SchemaDto>, ApiError> {
    let mut req = query.to_request();
    let ident = state.resolve(&headers).await?;
    state.admit(Op::GetSchema, &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.get_schema(&req).await;
    state.record(Op::GetSchema, &ident, Outcome::of(&out), started.elapsed());
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(SchemaDto::from(resp)))
}

/// `POST /v1/schema` — merge a document into the namespace's active schema.
///
/// Additive and versioned. A document the validator refuses is answered
/// `422 schema.rejected` and nothing is stored; a document that parses but
/// breaks what the namespace already declares is `409` unless `allow_breaking`
/// is set. A clean `dry_run` is `200` with `schema_version: 0`, because being
/// checked and not stored is what the caller asked for.
pub async fn upload(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<SchemaUploadBody>,
) -> Result<Json<SchemaUploadDto>, ApiError> {
    let mut req = body.to_request()?;
    let ident = state.resolve(&headers).await?;
    state.admit(Op::UploadSchema, &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.upload_schema(&req).await;
    state.record(
        Op::UploadSchema,
        &ident,
        Outcome::of(&out),
        started.elapsed(),
    );
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    let dto = SchemaUploadDto::from(resp);
    ensure_stored(dto.schema_version, &dto.validation_errors, req.dry_run)?;
    Ok(Json(dto))
}

/// `POST /v1/schema/validate` — parse and check a document without persisting.
pub async fn validate(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<SchemaValidateBody>,
) -> Result<Json<SchemaValidateDto>, ApiError> {
    let mut req = body.to_request()?;
    let ident = state.resolve(&headers).await?;
    state.admit(Op::ValidateSchema, &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.validate_schema(&req).await;
    state.record(
        Op::ValidateSchema,
        &ident,
        Outcome::of(&out),
        started.elapsed(),
    );
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
    ApiJson(body): ApiJson<SchemaReplaceBody>,
) -> Result<Json<SchemaReplaceDto>, ApiError> {
    let mut req = body.to_request()?;
    let ident = state.resolve(&headers).await?;
    state.admit(Op::ReplaceSchema, &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.replace_schema(&req).await;
    state.record(
        Op::ReplaceSchema,
        &ident,
        Outcome::of(&out),
        started.elapsed(),
    );
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    let dto = SchemaReplaceDto::from(resp);
    // PUT has no dry-run mode: it either replaced the schema or it did not.
    ensure_stored(dto.schema_version, &dto.validation_errors, false)?;
    Ok(Json(dto))
}

/// `GET /v1/schemas` — the namespace's schema version history
/// (`SCHEMA_LIST`).
///
/// Distinct from `GET /v1/schema`, which returns one document. A caller about
/// to upload needs to see what it is adding to.
pub async fn list(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    ApiQuery(query): ApiQuery<ListSchemasQuery>,
) -> Result<Json<ListSchemasResponseDto>, ApiError> {
    let ident = state.resolve(&headers).await?;
    state.admit(Op::ListSchemas, &ident).await?;
    let client = state.client_for(&ident.credential).await?;
    // The namespace the request runs in, not one the caller asked for.
    // Via `ident`, not `connection()`: under the hosted gateway the connection
    // is a shared service principal and its namespace is the gateway's.
    let mut req = query
        .to_request(ident.namespace(client.connection()))
        .map_err(ApiError::bad_request)?;
    ident.scope(&mut req);

    let started = Instant::now();
    let out = client.list_schemas_frames(&req).await;
    state.record(
        Op::ListSchemas,
        &ident,
        Outcome::of(&out),
        started.elapsed(),
    );
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
    let mut req = extractor_list_request();
    let ident = state.resolve(&headers).await?;
    state.admit(Op::ExtractorList, &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.extractor_list(&req).await;
    state.record(
        Op::ExtractorList,
        &ident,
        Outcome::of(&out),
        started.elapsed(),
    );
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
    ApiQuery(query): ApiQuery<SchemaDropQuery>,
) -> Result<Json<SchemaDropResponseDto>, ApiError> {
    let ident = state.resolve(&headers).await?;
    state.admit(Op::DropSchema, &ident).await?;
    let client = state.client_for(&ident.credential).await?;
    // See `list` for why the namespace comes from `ident`.
    let mut req = query.to_request(ident.namespace(client.connection()), &kind, &name)?;
    ident.scope(&mut req);

    let started = Instant::now();
    let out = client.drop_schema(&req).await;
    state.record(Op::DropSchema, &ident, Outcome::of(&out), started.elapsed());
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(SchemaDropResponseDto::from(resp)))
}

#[cfg(test)]
mod stored_tests {
    use super::{SEVERITY_ERROR, ensure_stored};
    use crate::dto::schema::SchemaErrorDto;
    use axum::http::StatusCode;

    fn err(message: &str, severity: u8) -> SchemaErrorDto {
        SchemaErrorDto {
            code: "Syntax".to_string(),
            message: message.to_string(),
            line: 1,
            column: 1,
            length: 0,
            severity,
        }
    }

    #[test]
    fn a_document_that_did_not_parse_is_not_a_success() {
        // The shape this guard exists for: Brain answers the CALL successfully
        // while refusing the DOCUMENT, so the only signal is in the body.
        let e = ensure_stored(
            0,
            &[err("syntax error at 1:1: expected schema", SEVERITY_ERROR)],
            false,
        )
        .expect_err("a document that stored nothing must not be a 2xx");
        assert_eq!(e.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn the_message_names_the_count_and_the_first_error() {
        // One envelope field has to carry enough to act on: how many problems
        // there are, and where the first one is.
        let e = ensure_stored(
            0,
            &[
                err("syntax error at 4:2: expected attribute", SEVERITY_ERROR),
                err("unknown type Widgt", SEVERITY_ERROR),
            ],
            false,
        )
        .expect_err("two blocking errors must refuse");
        let m = e.message();
        assert!(m.contains('2'), "names the count: {m}");
        assert!(m.contains("errors"), "pluralised: {m}");
        assert!(m.contains("4:2"), "names the first error's position: {m}");
    }

    #[test]
    fn one_error_is_not_pluralised() {
        let e = ensure_stored(0, &[err("syntax error at 1:1: bad", SEVERITY_ERROR)], false)
            .expect_err("must refuse");
        let m = e.message();
        assert!(m.contains("1 validation error;"), "{m}");
    }

    #[test]
    fn a_clean_dry_run_is_a_success() {
        // Version 0 is the CORRECT outcome here: the caller asked for the
        // document to be checked and not stored.
        ensure_stored(0, &[], true).expect("a clean dry run is what was asked for");
    }

    #[test]
    fn a_dry_run_still_reports_a_broken_document() {
        // Not storing it was requested; failing to parse it was not.
        let e = ensure_stored(0, &[err("syntax error at 1:1: bad", SEVERITY_ERROR)], true)
            .expect_err("a dry run of a broken document is still a refusal");
        assert_eq!(e.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn a_parsed_document_refused_for_another_reason_is_a_conflict() {
        // No validation error and still no version: the document is fine, the
        // change is not acceptable against what the namespace already has.
        let e = ensure_stored(0, &[], false).expect_err("version 0 is not a success");
        assert_eq!(e.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn a_stored_document_passes() {
        ensure_stored(1, &[], false).expect("version 1 means it landed");
    }

    #[test]
    fn a_non_blocking_note_does_not_refuse_a_stored_document() {
        // Severities below error are advisory. A warning on a schema that DID
        // land must not be turned into a refusal.
        ensure_stored(3, &[err("deprecated attribute form", 1)], false)
            .expect("a warning on a stored schema is not a refusal");
    }
}
