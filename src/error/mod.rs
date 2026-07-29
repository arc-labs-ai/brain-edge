//! HTTP error type — one JSON error envelope, mapped from Brain's taxonomy.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use brain_db_sdk::BrainError;
use brain_db_sdk::wire::types::ErrorCategoryWire;
use serde::Serialize;

/// A structured API error. Serializes to `{ "error": { code, message } }` with
/// the matching HTTP status.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: ErrorInner<'a>,
}

#[derive(Serialize)]
struct ErrorInner<'a> {
    code: &'a str,
    message: &'a str,
}

impl ApiError {
    /// A `401` for a missing/blank credential.
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message: message.into(),
        }
    }

    /// A `400` for a malformed request the edge itself rejects.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "bad_request",
            message: message.into(),
        }
    }

    /// Map a Brain client error to the matching HTTP status. Transport faults and
    /// unavailable/exhausted verdicts are `503`; auth verdicts `401`/`403`;
    /// validation/not-found/conflict map directly; anything else is `502`.
    pub fn from_brain(err: &BrainError) -> Self {
        match err {
            BrainError::Io(_) | BrainError::Closed | BrainError::Timeout(_) => Self {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "engine.unavailable",
                message: format!("brain unavailable: {err}"),
            },
            BrainError::Server {
                category, message, ..
            } => {
                let (status, code) = match category {
                    ErrorCategoryWire::Authentication => (StatusCode::UNAUTHORIZED, "unauthorized"),
                    ErrorCategoryWire::Authorization => (StatusCode::FORBIDDEN, "forbidden"),
                    ErrorCategoryWire::Unavailable | ErrorCategoryWire::ResourceExhausted => {
                        (StatusCode::SERVICE_UNAVAILABLE, "engine.unavailable")
                    }
                    ErrorCategoryWire::Validation => (StatusCode::BAD_REQUEST, "bad_request"),
                    ErrorCategoryWire::NotFound => (StatusCode::NOT_FOUND, "not_found"),
                    ErrorCategoryWire::Conflict => (StatusCode::CONFLICT, "conflict"),
                    ErrorCategoryWire::Protocol | ErrorCategoryWire::Internal => {
                        (StatusCode::BAD_GATEWAY, "engine.error")
                    }
                };
                Self {
                    status,
                    code,
                    message: message.clone(),
                }
            }
            other => Self {
                status: StatusCode::BAD_GATEWAY,
                code: "engine.error",
                message: format!("brain error: {other}"),
            },
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            error: ErrorInner {
                code: self.code,
                message: &self.message,
            },
        };
        (self.status, Json(body)).into_response()
    }
}
