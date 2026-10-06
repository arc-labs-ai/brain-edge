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
    SchemaGetRequest, SchemaGetResponse, SchemaReplaceRequest, SchemaReplaceResponse,
    SchemaUploadRequest, SchemaUploadResponse, SchemaValidateRequest, SchemaValidateResponse,
    SchemaValidationErrorWire,
};
use serde::{Deserialize, Serialize};

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

impl SchemaUploadBody {
    /// Build the wire request (mints a fresh `request_id`).
    #[must_use]
    pub fn to_request(self) -> SchemaUploadRequest {
        SchemaUploadRequest {
            schema_document: self.schema_document,
            dry_run: self.dry_run,
            allow_breaking: self.allow_breaking,
            request_id: brain_db_sdk::new_id(),
            act_as: None,
        }
    }
}

impl SchemaValidateBody {
    #[must_use]
    pub fn to_request(self) -> SchemaValidateRequest {
        SchemaValidateRequest {
            schema_document: self.schema_document,
            act_as: None,
        }
    }
}

impl SchemaReplaceBody {
    /// Build the wire request (mints a fresh `request_id`).
    ///
    /// Rejects `force_drop_existing = false` here rather than forwarding it:
    /// Brain answers that with `InvalidRequest`, and a 400 naming the field is
    /// a far better answer than a relayed protocol error.
    pub fn to_request(self) -> Result<SchemaReplaceRequest, String> {
        if !self.force_drop_existing {
            return Err(
                "force_drop_existing must be true — PUT /v1/schema drops every declared row in \
                 the namespace before the new document lands"
                    .into(),
            );
        }
        Ok(SchemaReplaceRequest {
            schema_document: self.schema_document,
            force_drop_existing: true,
            request_id: brain_db_sdk::new_id(),
            act_as: None,
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
        }
        .to_request();
        assert_eq!(up.schema_document, "entity P {}");
        assert!(up.dry_run);
        assert!(up.allow_breaking);
        assert_ne!(up.request_id, [0u8; 16]); // a fresh id was minted

        let val = SchemaValidateBody {
            schema_document: "entity Q {}".into(),
        }
        .to_request();
        assert_eq!(val.schema_document, "entity Q {}");
    }

    #[test]
    fn replace_body_requires_force_drop_existing() {
        assert!(
            SchemaReplaceBody {
                schema_document: "entity P {}".into(),
                force_drop_existing: false,
            }
            .to_request()
            .is_err()
        );
        let req = SchemaReplaceBody {
            schema_document: "entity P {}".into(),
            force_drop_existing: true,
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
