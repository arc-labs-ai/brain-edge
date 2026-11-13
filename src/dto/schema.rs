//! Schema verbs: get / upload / validate / replace.
//!
//! The four together are what make `PUT` coherent. Exposing the destructive
//! replace on its own would let a caller demolish a schema they had no way to
//! create through the same API, so the read and additive paths ship with it.
//!
//! `SCHEMA_LIST` is deliberately absent: it is a streaming verb, and every
//! streaming verb on this edge is either flattened or omitted rather than
//! half-exposed. Listing versions is introspection an operator does against the
//! admin listener, not something a data-plane client needs.

// The DTOs below mirror the HTTP contract one-for-one: the JSON field names are
// the API, and `tools/http_manifest.py` emits every one of them — with its type
// and serde attributes — into `contract/http-routes.json`, which the three SDK
// clients are checked against. A doc comment on each of ~312 fields would
// restate the field name; the ones that carry meaning beyond their name have
// one written below.
#![allow(missing_docs)]

use brain_db_sdk::wire::types::{
    ExtractorListRequest, ExtractorListResponseFrame, SchemaDropRequest, SchemaDropResponse,
    SchemaGetRequest, SchemaGetResponse, SchemaListRequest, SchemaListResponseFrame,
    SchemaReplaceRequest, SchemaReplaceResponse, SchemaScope, SchemaUploadRequest,
    SchemaUploadResponse, SchemaValidateRequest, SchemaValidateResponse, SchemaValidationErrorWire,
    schema_drop_target,
};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;

use super::{hex_decode, hex_encode};

/// One structured parse-or-validate diagnostic.
#[derive(Debug, Serialize)]
pub struct SchemaErrorDto {
    pub code: String,
    pub message: String,
    /// 1-based; `0` when no source position is known.
    pub line: u32,
    pub column: u32,
    pub length: u32,
    /// `0` info / `1` warning / `2` error. Always `2` in v1.
    pub severity: u8,
}

impl From<&SchemaValidationErrorWire> for SchemaErrorDto {
    fn from(e: &SchemaValidationErrorWire) -> Self {
        Self {
            code: e.code.clone(),
            message: e.message.clone(),
            line: e.line,
            column: e.column,
            length: e.length,
            severity: e.severity,
        }
    }
}

fn errors(src: &[SchemaValidationErrorWire]) -> Vec<SchemaErrorDto> {
    src.iter().map(SchemaErrorDto::from).collect()
}

/// `GET /v1/schema` response.
#[derive(Debug, Serialize)]
pub struct SchemaDto {
    pub namespace: String,
    pub schema_version: u32,
    /// Verbatim DSL text if it was uploaded as such; empty for programmatic
    /// uploads, which carry only the parsed AST.
    pub schema_document: String,
    pub uploaded_at_unix_nanos: u64,
    pub validator_version: u32,
}

impl From<SchemaGetResponse> for SchemaDto {
    fn from(r: SchemaGetResponse) -> Self {
        Self {
            namespace: r.namespace,
            schema_version: r.schema_version,
            schema_document: r.schema_document,
            uploaded_at_unix_nanos: r.uploaded_at_unix_nanos,
            validator_version: r.validator_version,
        }
    }
}

/// `POST /v1/schema` body — merge a document into the active namespace.
#[derive(Debug, Deserialize)]
pub struct SchemaUploadBody {
    pub schema_document: String,
    /// Validate and report without persisting.
    #[serde(default)]
    pub dry_run: bool,
    /// Permit a backward-incompatible change.
    #[serde(default)]
    pub allow_breaking: bool,
    /// Where this document declares: `"namespace"` (the default — the
    /// tenant's base, which every space resolves) or `"space"` (an overlay at
    /// the caller's effective space, which only that space resolves).
    /// Case-insensitive; anything else is rejected.
    ///
    /// Deliberately NOT inferred from the effective space. Nearly every
    /// request names a space, so inferring the scope from it would silently
    /// turn every ordinary namespace-wide upload into a per-space overlay
    /// invisible to the tenant's other spaces.
    #[serde(default)]
    pub scope: Option<String>,
}

/// `POST /v1/schema` response.
#[derive(Debug, Serialize)]
pub struct SchemaUploadDto {
    pub namespace: String,
    /// `0` means the upload was rejected — a validation failure, or `dry_run`.
    pub schema_version: u32,
    pub backward_compatible: bool,
    pub validation_errors: Vec<SchemaErrorDto>,
}

impl From<SchemaUploadResponse> for SchemaUploadDto {
    fn from(r: SchemaUploadResponse) -> Self {
        Self {
            namespace: r.namespace,
            schema_version: r.schema_version,
            backward_compatible: r.backward_compatible,
            validation_errors: errors(&r.validation_errors),
        }
    }
}

/// `POST /v1/schema/validate` body — dry run, never touches storage.
#[derive(Debug, Deserialize)]
pub struct SchemaValidateBody {
    pub schema_document: String,
    /// Which scope this operation applies at: `"namespace"` (the default —
    /// the tenant's base, which every space resolves) or `"space"` (an
    /// overlay at the caller's effective space, which only that space
    /// resolves). Case-insensitive; anything else is rejected.
    ///
    /// Deliberately NOT inferred from the effective space — see
    /// [`parse_scope`].
    #[serde(default)]
    pub scope: Option<String>,
}

/// `POST /v1/schema/validate` response.
#[derive(Debug, Serialize)]
pub struct SchemaValidateDto {
    /// Namespace parsed from the document; empty if the parse failed before
    /// reaching it.
    pub namespace: String,
    /// What the version WOULD become — `0` when validation failed.
    pub would_be_version: u32,
    pub validation_errors: Vec<SchemaErrorDto>,
}

impl From<SchemaValidateResponse> for SchemaValidateDto {
    fn from(r: SchemaValidateResponse) -> Self {
        Self {
            namespace: r.namespace,
            would_be_version: r.would_be_version,
            validation_errors: errors(&r.validation_errors),
        }
    }
}

/// `PUT /v1/schema` body — DESTRUCTIVE namespace swap.
///
/// `force_drop_existing` has no default on purpose. Brain rejects `false` with
/// `InvalidRequest`, and requiring the caller to spell it out keeps an
/// irreversible operation from being reachable by an empty body.
#[derive(Debug, Deserialize)]
pub struct SchemaReplaceBody {
    pub schema_document: String,
    pub force_drop_existing: bool,
    /// Which scope this operation applies at: `"namespace"` (the default —
    /// the tenant's base, which every space resolves) or `"space"` (an
    /// overlay at the caller's effective space, which only that space
    /// resolves). Case-insensitive; anything else is rejected.
    ///
    /// Deliberately NOT inferred from the effective space — see
    /// [`parse_scope`].
    #[serde(default)]
    pub scope: Option<String>,
}

/// `PUT /v1/schema` response.
#[derive(Debug, Serialize)]
pub struct SchemaReplaceDto {
    pub namespace: String,
    pub schema_version: u32,
    /// How many declared rows were dropped before the new schema landed. This
    /// is the number that tells a caller what the swap destroyed.
    pub dropped_count: u32,
    pub validation_errors: Vec<SchemaErrorDto>,
}

impl From<SchemaReplaceResponse> for SchemaReplaceDto {
    fn from(r: SchemaReplaceResponse) -> Self {
        Self {
            namespace: r.namespace,
            schema_version: r.schema_version,
            dropped_count: r.dropped_count,
            validation_errors: errors(&r.validation_errors),
        }
    }
}

/// `GET /v1/schema` query — namespace and version are both optional.
///
/// `version = 0` asks Brain for the active version, which is the default a
/// caller wants; an explicit version fetches a historical one.
#[derive(Debug, Deserialize)]
pub struct SchemaGetQuery {
    #[serde(default)]
    pub namespace: String,
    #[serde(default)]
    pub version: u32,
}

impl SchemaGetQuery {
    #[must_use]
    pub fn to_request(&self) -> SchemaGetRequest {
        SchemaGetRequest {
            namespace: self.namespace.clone(),
            version: self.version,
            act_as: None,
        }
    }
}

/// Parse the free-text `scope` field the schema verbs accept.
///
/// Fallible on purpose: an unrecognised value is refused rather than
/// silently narrowed to the namespace base, which would apply to the shared
/// base an operation the caller meant to keep to one space.
///
/// Absent means the namespace base. The scope is NEVER inferred from the
/// caller's effective space: nearly every request names a space, so
/// inferring it would turn every ordinary namespace-wide operation into a
/// per-space one invisible to the tenant's other spaces.
///
/// # Errors
/// [`ApiError::bad_request`] for anything but `"namespace"` or `"space"`.
pub fn parse_scope(scope: Option<&str>) -> Result<SchemaScope, ApiError> {
    match scope {
        None => Ok(SchemaScope::Namespace),
        Some(s) => match s.to_ascii_lowercase().as_str() {
            "namespace" => Ok(SchemaScope::Namespace),
            "space" => Ok(SchemaScope::Space),
            other => Err(ApiError::bad_request(format!(
                "scope must be \"namespace\" or \"space\", got {other:?}"
            ))),
        },
    }
}

impl SchemaUploadBody {
    /// Build the wire request (mints a fresh `request_id`).
    ///
    /// Fallible because `scope` arrives as free text: an unrecognised value is
    /// refused here rather than silently narrowed to the namespace base, which
    /// would publish into the shared base a document the caller meant to keep
    /// to one space.
    pub fn to_request(self) -> Result<SchemaUploadRequest, ApiError> {
        let scope = parse_scope(self.scope.as_deref())?;
        Ok(SchemaUploadRequest {
            schema_document: self.schema_document,
            dry_run: self.dry_run,
            allow_breaking: self.allow_breaking,
            request_id: brain_db_sdk::new_id(),
            act_as: None,
            scope,
        })
    }
}

impl SchemaValidateBody {
    /// Build the wire request.
    ///
    /// Fallible now that a scope is carried: VALIDATE predicts UPLOAD, so it
    /// resolves references against the scope the upload would land in, and an
    /// unrecognised scope must be refused rather than narrowed.
    ///
    /// # Errors
    /// [`ApiError::bad_request`] for an unrecognised `scope`.
    pub fn to_request(self) -> Result<SchemaValidateRequest, ApiError> {
        let scope = parse_scope(self.scope.as_deref())?;
        Ok(SchemaValidateRequest {
            schema_document: self.schema_document,
            act_as: None,
            scope,
        })
    }
}

impl SchemaReplaceBody {
    /// Build the wire request (mints a fresh `request_id`).
    ///
    /// Rejects `force_drop_existing = false` here rather than forwarding it:
    /// Brain answers that with `InvalidRequest`, and a 400 naming the field is
    /// a far better answer than a relayed protocol error.
    pub fn to_request(self) -> Result<SchemaReplaceRequest, ApiError> {
        if !self.force_drop_existing {
            return Err(ApiError::bad_request(
                "force_drop_existing must be true — PUT /v1/schema drops every declared row in \
                 the scope before the new document lands",
            ));
        }
        let scope = parse_scope(self.scope.as_deref())?;
        Ok(SchemaReplaceRequest {
            schema_document: self.schema_document,
            force_drop_existing: true,
            request_id: brain_db_sdk::new_id(),
            act_as: None,
            scope,
        })
    }
}

#[cfg(test)]
mod schema_tests {
    use super::*;
    use brain_db_sdk::wire::types::SchemaUploadResponse;

    fn sample_error() -> SchemaValidationErrorWire {
        SchemaValidationErrorWire {
            code: "E001".into(),
            message: "undeclared type".into(),
            line: 3,
            column: 5,
            length: 7,
            severity: 2,
        }
    }

    #[test]
    fn get_query_forwards_namespace_and_version() {
        let req = SchemaGetQuery {
            namespace: "people".into(),
            version: 4,
        }
        .to_request();
        assert_eq!(req.namespace, "people");
        assert_eq!(req.version, 4);
    }

    #[test]
    fn upload_and_validate_bodies_build_requests() {
        let up = SchemaUploadBody {
            schema_document: "entity P {}".into(),
            dry_run: true,
            allow_breaking: true,
            scope: None,
        }
        .to_request()
        .expect("no scope is the namespace base");
        assert_eq!(up.schema_document, "entity P {}");
        assert!(up.dry_run);
        assert!(up.allow_breaking);
        assert_ne!(up.request_id, [0u8; 16]); // a fresh id was minted
        // An omitted scope declares at the namespace base. It is NOT inferred
        // from the effective space, which almost every request names.
        assert_eq!(up.scope, SchemaScope::Namespace);

        let val = SchemaValidateBody {
            schema_document: "entity Q {}".into(),
            scope: None,
        }
        .to_request()
        .expect("no scope is the namespace base");
        assert_eq!(val.schema_document, "entity Q {}");
        assert_eq!(val.scope, SchemaScope::Namespace);
    }

    #[test]
    fn upload_scope_is_parsed_case_insensitively() {
        for (text, want) in [
            ("space", SchemaScope::Space),
            ("SPACE", SchemaScope::Space),
            ("Namespace", SchemaScope::Namespace),
            ("namespace", SchemaScope::Namespace),
        ] {
            let req = SchemaUploadBody {
                schema_document: "entity P {}".into(),
                dry_run: false,
                allow_breaking: false,
                scope: Some(text.into()),
            }
            .to_request()
            .unwrap_or_else(|_| panic!("{text} is a valid scope"));
            assert_eq!(req.scope, want, "scope {text:?}");
        }
    }

    #[test]
    fn an_unrecognised_upload_scope_is_refused_not_narrowed() {
        // Narrowing an unknown value to the base would publish into the
        // shared namespace a document the caller meant to keep to one space.
        let err = SchemaUploadBody {
            schema_document: "entity P {}".into(),
            dry_run: false,
            allow_breaking: false,
            scope: Some("tenant".into()),
        }
        .to_request()
        .expect_err("an unknown scope must be refused");
        assert!(
            format!("{err:?}").contains("tenant"),
            "the error names the value it rejected: {err:?}"
        );
    }

    #[test]
    fn replace_body_requires_force_drop_existing() {
        assert!(
            SchemaReplaceBody {
                schema_document: "entity P {}".into(),
                force_drop_existing: false,
                scope: None,
            }
            .to_request()
            .is_err()
        );
        let req = SchemaReplaceBody {
            schema_document: "entity P {}".into(),
            force_drop_existing: true,
            scope: None,
        }
        .to_request()
        .unwrap();
        assert!(req.force_drop_existing);
        assert_eq!(req.schema_document, "entity P {}");
    }

    #[test]
    fn error_dto_maps_position_and_severity() {
        let dto = SchemaErrorDto::from(&sample_error());
        assert_eq!(dto.code, "E001");
        assert_eq!(dto.message, "undeclared type");
        assert_eq!((dto.line, dto.column, dto.length), (3, 5, 7));
        assert_eq!(dto.severity, 2);
    }

    #[test]
    fn get_response_maps_and_drops_source_blob() {
        let dto = SchemaDto::from(SchemaGetResponse {
            namespace: "people".into(),
            schema_version: 2,
            schema_document: "entity Person {}".into(),
            source_blob: vec![1, 2, 3], // AST blob is not surfaced
            uploaded_at_unix_nanos: 99,
            validator_version: 1,
        });
        assert_eq!(dto.namespace, "people");
        assert_eq!(dto.schema_version, 2);
        assert_eq!(dto.schema_document, "entity Person {}");
        assert_eq!(dto.uploaded_at_unix_nanos, 99);
        assert_eq!(dto.validator_version, 1);
    }

    #[test]
    fn upload_response_maps_errors_and_compat() {
        let dto = SchemaUploadDto::from(SchemaUploadResponse {
            namespace: "people".into(),
            schema_version: 0, // rejected
            validation_errors: vec![sample_error()],
            backward_compatible: false,
            migration_summary_blob: vec![],
        });
        assert_eq!(dto.namespace, "people");
        assert_eq!(dto.schema_version, 0);
        assert!(!dto.backward_compatible);
        assert_eq!(dto.validation_errors.len(), 1);
        assert_eq!(dto.validation_errors[0].code, "E001");
    }

    #[test]
    fn validate_response_maps_would_be_version_and_errors() {
        let dto = SchemaValidateDto::from(SchemaValidateResponse {
            namespace: "people".into(),
            would_be_version: 3,
            validation_errors: vec![],
        });
        assert_eq!(dto.namespace, "people");
        assert_eq!(dto.would_be_version, 3);
        assert!(dto.validation_errors.is_empty());
    }

    #[test]
    fn replace_response_surfaces_dropped_count() {
        let dto = SchemaReplaceDto::from(SchemaReplaceResponse {
            namespace: "people".into(),
            schema_version: 5,
            dropped_count: 12,
            validation_errors: vec![sample_error()],
        });
        assert_eq!(dto.namespace, "people");
        assert_eq!(dto.schema_version, 5);
        assert_eq!(dto.dropped_count, 12);
        assert_eq!(dto.validation_errors.len(), 1);
    }
}

// --- Introspection ----------------------------------------------------------
//
// What schema versions exist, and which extractors are registered. Both are
// read-only: a caller tuning a schema needs to see the version history it is
// adding to, and a caller reading extracted statements needs to know what
// produced them.

/// `GET /v1/schemas` query — the namespace's schema version history.
#[derive(Debug, Default, Deserialize)]
pub struct ListSchemasQuery {
    /// Page size.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Opaque keyset cursor from the previous page, lowercase hex.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Default page size for `GET /v1/schemas`.
const DEFAULT_SCHEMA_PAGE: u32 = 100;
/// Ceiling on that page size.
const MAX_SCHEMA_PAGE: u32 = 1000;

impl ListSchemasQuery {
    /// Build the wire request for `namespace`, which the handler reads from
    /// the credential Brain resolved — never from the request.
    ///
    /// # Errors
    /// The cursor is not hex.
    pub fn to_request(&self, namespace: &str) -> Result<SchemaListRequest, String> {
        Ok(SchemaListRequest {
            namespace: namespace.to_owned(),
            limit: self
                .limit
                .filter(|n| *n > 0)
                .map_or(DEFAULT_SCHEMA_PAGE, |n| n.min(MAX_SCHEMA_PAGE)),
            cursor: match self.cursor.as_deref() {
                Some(c) => hex_decode(c)?,
                None => Vec::new(),
            },
            act_as: None,
        })
    }
}

/// One schema version.
#[derive(Debug, Serialize)]
pub struct SchemaVersionDto {
    pub schema_version: u32,
    pub uploaded_at_unix_nanos: u64,
    /// Which validator accepted it — a document that passed an older
    /// validator is not necessarily one the current validator would accept.
    pub validator_version: u32,
    /// Whether the original document text was retained and can be fetched.
    pub has_source_text: bool,
}

/// `GET /v1/schemas` response.
#[derive(Debug, Serialize)]
pub struct ListSchemasResponseDto {
    pub namespace: String,
    pub items: Vec<SchemaVersionDto>,
    /// How many versions exist in total, across every page.
    pub total: u32,
    /// Cursor for the next page, or `null` on the last one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

impl ListSchemasResponseDto {
    /// Fold the streamed frames into one page.
    #[must_use]
    pub fn from_frames(frames: Vec<SchemaListResponseFrame>) -> Self {
        let namespace = frames
            .first()
            .map(|f| f.namespace.clone())
            .unwrap_or_default();
        let total = frames.first().map_or(0, |f| f.total);
        // The LAST frame's cursor: an earlier one would replay rows this page
        // already returned.
        let next_cursor = frames
            .last()
            .filter(|f| !f.next_cursor.is_empty())
            .map(|f| hex_encode(&f.next_cursor));
        Self {
            namespace,
            items: frames
                .into_iter()
                .flat_map(|f| f.items)
                .map(|i| SchemaVersionDto {
                    schema_version: i.schema_version,
                    uploaded_at_unix_nanos: i.uploaded_at_unix_nanos,
                    validator_version: i.validator_version,
                    has_source_text: i.has_source_text,
                })
                .collect(),
            total,
            next_cursor,
        }
    }
}

/// One registered extractor.
#[derive(Debug, Serialize)]
pub struct ExtractorDto {
    /// The id statements carry as `extractor_id`.
    pub extractor_id: u32,
    pub namespace: String,
    pub name: String,
    /// `0` = pattern, `1` = classifier, `2` = llm.
    pub kind: u8,
    pub schema_version: u32,
    pub created_at_unix_nanos: u64,
}

/// `GET /v1/extractors` response.
///
/// Read-only: extraction is always-on and cannot be toggled, so there is
/// nothing here to write.
#[derive(Debug, Serialize)]
pub struct ListExtractorsResponseDto {
    pub items: Vec<ExtractorDto>,
    pub total: u32,
}

impl From<ExtractorListResponseFrame> for ListExtractorsResponseDto {
    fn from(f: ExtractorListResponseFrame) -> Self {
        Self {
            total: f.total,
            items: f
                .items
                .into_iter()
                .map(|i| ExtractorDto {
                    extractor_id: i.extractor_id,
                    namespace: i.namespace,
                    name: i.name,
                    kind: i.kind,
                    schema_version: i.schema_version,
                    created_at_unix_nanos: i.created_at_unix_nanos,
                })
                .collect(),
        }
    }
}

/// The wire request listing the shard's extractors. It takes no arguments —
/// every registered extractor comes back — and is a struct rather than a unit
/// type only so the CBOR encoding matches every other request body.
#[must_use]
pub fn extractor_list_request() -> ExtractorListRequest {
    ExtractorListRequest { act_as: None }
}

// --- Drop -------------------------------------------------------------------

/// `DELETE /v1/schema/{kind}/{name}` query.
#[derive(Debug, Default, Deserialize)]
pub struct SchemaDropQuery {
    /// Drop even while live rows still use the type.
    ///
    /// Required only when there ARE live rows: without it such a drop is
    /// rejected and mutates nothing, while a type nothing uses drops freely.
    /// So this is not a "yes I'm sure" flag — it is specifically consent to
    /// orphan existing rows, which survive as plain memories no longer
    /// enriched from the typed-graph tables.
    #[serde(default)]
    pub force: bool,
    /// Which scope to narrow: `"namespace"` (the default — the tenant's base)
    /// or `"space"` (the caller's effective space). Case-insensitive.
    ///
    /// A narrowing reaches exactly the declarations the named scope made, so
    /// dropping at the base leaves every space's overlay standing and
    /// dropping in a space leaves the tenant's base standing. Never inferred
    /// from the effective space — see [`parse_scope`].
    #[serde(default)]
    pub scope: Option<String>,
}

/// Map the path segment to the wire's target discriminant.
///
/// Entity types are deliberately absent, matching the wire: they are global
/// in the v1 storage model, so dropping one would race rows in other
/// namespaces that reference the same shared type.
fn drop_target(raw: &str) -> Result<u8, String> {
    Ok(match raw.trim() {
        "predicate" => schema_drop_target::PREDICATE,
        "relation_type" => schema_drop_target::RELATION_TYPE,
        other => {
            return Err(format!(
                "`{other}` is not a droppable kind — use `predicate` or `relation_type`"
            ));
        }
    })
}

impl SchemaDropQuery {
    /// Build the wire request for `kind`/`name` in `namespace`.
    ///
    /// The namespace is sent explicitly and is NOT optional: `SCHEMA_DROP`
    /// requires it to equal the caller's own and rejects anything else —
    /// including the empty string — with `Unauthorized`. The handler reads it
    /// from the credential Brain resolved, so it is still never taken from
    /// the request.
    ///
    /// # Errors
    /// An unknown kind, or a blank name.
    pub fn to_request(
        &self,
        namespace: &str,
        kind: &str,
        name: &str,
    ) -> Result<SchemaDropRequest, ApiError> {
        let target_name = name.trim().to_owned();
        if target_name.is_empty() {
            return Err(ApiError::bad_request("name must not be blank"));
        }
        let scope = parse_scope(self.scope.as_deref())?;
        Ok(SchemaDropRequest {
            namespace: namespace.to_owned(),
            target_kind: drop_target(kind).map_err(ApiError::bad_request)?,
            target_name,
            force: self.force,
            request_id: brain_db_sdk::new_id(),
            act_as: None,
            scope,
        })
    }
}

/// `DELETE /v1/schema/{kind}/{name}` response.
#[derive(Debug, Serialize)]
pub struct SchemaDropResponseDto {
    pub namespace: String,
    /// The new active version, or `0` when nothing was dropped.
    pub schema_version: u32,
    pub target_kind: u8,
    pub target_name: String,
    /// `false` with `live_rows > 0` means the type is still in use and
    /// `force` was not set — nothing was changed.
    pub dropped: bool,
    /// Live rows on the type. After a forced drop these are the rows now
    /// orphaned.
    pub live_rows: u32,
    pub validation_errors: Vec<SchemaErrorDto>,
}

impl From<SchemaDropResponse> for SchemaDropResponseDto {
    fn from(r: SchemaDropResponse) -> Self {
        Self {
            namespace: r.namespace,
            schema_version: r.schema_version,
            target_kind: r.target_kind,
            target_name: r.target_name,
            dropped: r.dropped,
            live_rows: r.live_rows,
            validation_errors: errors(&r.validation_errors),
        }
    }
}

#[cfg(test)]
mod schema_introspection_tests {
    use super::*;

    fn frame(cursor: Vec<u8>, versions: &[u32]) -> SchemaListResponseFrame {
        SchemaListResponseFrame {
            namespace: "mirror".into(),
            items: versions
                .iter()
                .map(|v| brain_db_sdk::wire::types::SchemaListItemWire {
                    schema_version: *v,
                    uploaded_at_unix_nanos: 1,
                    validator_version: 2,
                    has_source_text: true,
                })
                .collect(),
            total: 5,
            next_cursor: cursor,
            is_final: false,
        }
    }

    #[test]
    fn list_defaults_clamps_and_decodes_the_cursor() {
        let q = ListSchemasQuery::default().to_request("mirror").unwrap();
        assert_eq!(q.limit, DEFAULT_SCHEMA_PAGE);
        assert_eq!(q.cursor, Vec::<u8>::new());
        assert_eq!(
            q.namespace, "mirror",
            "the namespace comes from the resolved credential, never the query"
        );

        let q = ListSchemasQuery {
            limit: Some(99_999),
            cursor: Some("0a0b".into()),
        }
        .to_request("mirror")
        .unwrap();
        assert_eq!(q.limit, MAX_SCHEMA_PAGE);
        assert_eq!(q.cursor, vec![0x0a, 0x0b]);

        assert!(
            ListSchemasQuery {
                limit: None,
                cursor: Some("nothex".into()),
            }
            .to_request("mirror")
            .is_err()
        );
    }

    #[test]
    fn list_folds_frames_and_takes_the_last_cursor() {
        let page = ListSchemasResponseDto::from_frames(vec![
            frame(vec![0x01], &[1, 2]),
            frame(vec![0x02], &[3]),
        ]);
        assert_eq!(page.namespace, "mirror");
        assert_eq!(page.items.len(), 3);
        assert_eq!(page.total, 5);
        assert_eq!(page.next_cursor.as_deref(), Some("02"));
        assert!(page.items[0].has_source_text);
    }

    #[test]
    fn an_empty_listing_has_no_cursor() {
        let page = ListSchemasResponseDto::from_frames(vec![]);
        assert!(page.next_cursor.is_none());
        assert!(page.items.is_empty());
    }

    #[test]
    fn extractor_list_takes_no_filters() {
        // The wire request is an empty map; it is a struct only so the
        // encoding matches every other request body.
        let _ = extractor_list_request();
        let dto = ListExtractorsResponseDto::from(ExtractorListResponseFrame {
            items: vec![brain_db_sdk::wire::types::ExtractorListItem {
                extractor_id: 7,
                namespace: "mirror".into(),
                name: "llm".into(),
                kind: 2,
                schema_version: 1,
                created_at_unix_nanos: 3,
            }],
            total: 1,
            is_final: true,
        });
        assert_eq!(dto.total, 1);
        assert_eq!(dto.items[0].extractor_id, 7);
        assert_eq!(dto.items[0].kind, 2, "2 = llm");
    }

    #[test]
    fn drop_maps_the_kind_segment_and_rejects_anything_else() {
        let q = SchemaDropQuery {
            force: true,
            scope: None,
        };
        let req = q.to_request("mirror", "predicate", "works_at").unwrap();
        assert_eq!(req.target_kind, schema_drop_target::PREDICATE);
        assert_eq!(req.target_name, "works_at");
        assert!(req.force);
        assert_eq!(
            req.namespace, "mirror",
            "SCHEMA_DROP requires the caller's own namespace and rejects an \
             empty one with Unauthorized — caught live against real Brain"
        );

        assert_eq!(
            SchemaDropQuery::default()
                .to_request("mirror", "relation_type", "employs")
                .unwrap()
                .target_kind,
            schema_drop_target::RELATION_TYPE
        );

        // Entity types are deliberately not droppable: they are global in the
        // v1 storage model, so dropping one would race other namespaces.
        let err = SchemaDropQuery::default()
            .to_request("mirror", "entity_type", "Person")
            .unwrap_err();
        assert!(
            format!("{err:?}").contains("predicate"),
            "the error names what IS allowed: {err:?}"
        );

        assert!(
            SchemaDropQuery::default()
                .to_request("mirror", "predicate", "  ")
                .is_err()
        );
    }

    #[test]
    fn drop_defaults_to_refusing_to_orphan_live_rows() {
        let req = SchemaDropQuery::default()
            .to_request("mirror", "predicate", "p")
            .unwrap();
        assert!(
            !req.force,
            "force is consent to orphan rows, so it is never the default"
        );
    }

    #[test]
    fn a_refused_drop_reports_why_rather_than_erroring() {
        let dto = SchemaDropResponseDto::from(SchemaDropResponse {
            namespace: "mirror".into(),
            schema_version: 0,
            target_kind: 0,
            target_name: "works_at".into(),
            dropped: false,
            live_rows: 12,
            validation_errors: vec![],
        });
        // `dropped: false` with `live_rows > 0` is the "still in use, and you
        // did not say force" answer — a caller can act on that.
        assert!(!dto.dropped);
        assert_eq!(dto.live_rows, 12);
        assert_eq!(dto.schema_version, 0, "nothing was bumped");
    }
}
