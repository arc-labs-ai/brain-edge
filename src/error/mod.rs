//! HTTP error type — one JSON error envelope, mapped from Brain's taxonomy.
//!
//! A `5xx` body deliberately carries no internal detail. `BrainError` renders
//! frame-codec faults, protocol violations and version mismatches into their
//! `Display` text, and forwarding that to an HTTP caller tells them about the
//! edge's conversation with its database rather than about their request. The
//! detail is logged with the request id instead, so an operator can still find
//! it — see [`ApiError::log_internal`].

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
    /// What the caller is told.
    message: String,
    /// What the caller is NOT told: the underlying failure, logged instead.
    ///
    /// `Some` only for statuses that describe a fault on this side. A `4xx`
    /// message is about the caller's own request and is safe to return.
    internal: Option<String>,
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
            internal: None,
        }
    }

    /// A `400` for a malformed request the edge itself rejects.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "bad_request",
            message: message.into(),
            internal: None,
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
                // The OS error text describes the edge's own connectivity, not
                // the caller's request.
                message: "the memory engine is unavailable; retry shortly".to_string(),
                internal: Some(format!("brain unavailable: {err}")),
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
                // Brain's own message is about THIS caller's request, so it is
                // returned as-is for the statuses that describe the request.
                // The 5xx arms are the edge reporting its own trouble.
                let internal = if status.is_server_error() {
                    Some(format!("brain {category:?}: {message}"))
                } else {
                    None
                };
                Self {
                    status,
                    code,
                    message: if internal.is_some() {
                        "the memory engine returned an error".to_string()
                    } else {
                        message.clone()
                    },
                    internal,
                }
            }
            // Frame-codec faults, protocol violations, version mismatches: all
            // describe the edge's conversation with Brain, none describe the
            // caller's request.
            other => Self {
                status: StatusCode::BAD_GATEWAY,
                code: "engine.error",
                message: "the memory engine returned an error".to_string(),
                internal: Some(format!("brain error: {other}")),
            },
        }
    }
}

impl ApiError {
    /// A `500` for a fault the edge itself could not classify.
    #[must_use]
    pub fn internal(detail: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal",
            message: "internal error".to_string(),
            internal: Some(detail.into()),
        }
    }

    /// Emit the withheld detail to the log, where the request id ties it back
    /// to the response the caller saw. Called once, on the way out.
    fn log_internal(&self) {
        if let Some(detail) = &self.internal {
            tracing::warn!(
                status = self.status.as_u16(),
                code = self.code,
                detail = %detail,
                "request failed"
            );
        }
    }

    /// The HTTP status this error renders as (test/introspection).
    #[cfg(test)]
    #[must_use]
    pub const fn status(&self) -> StatusCode {
        self.status
    }

    /// The message the caller is told (test/introspection).
    #[cfg(test)]
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        self.log_internal();
        let body = ErrorBody {
            error: ErrorInner {
                code: self.code,
                message: &self.message,
            },
        };
        (self.status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brain_db_sdk::wire::types::{ErrorCodeWire, ErrorResponse};

    fn server(category: ErrorCategoryWire, message: &str) -> BrainError {
        BrainError::from_server(ErrorResponse {
            code: ErrorCodeWire::PermissionDenied,
            category,
            message: message.to_string(),
            details: None,
            retry_after_ms: None,
        })
    }

    #[test]
    fn a_4xx_returns_the_engine_message_because_it_is_about_this_request() {
        // Validation / not-found / conflict describe what the caller asked for,
        // so withholding the detail would only make the API harder to use.
        let err = ApiError::from_brain(&server(ErrorCategoryWire::Validation, "k must be 1..=100"));
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert_eq!(err.message(), "k must be 1..=100");
        assert!(err.internal.is_none(), "nothing to withhold on a 4xx");
    }

    #[test]
    fn a_5xx_withholds_the_engine_message() {
        // `Internal` is the engine reporting its own trouble. Forwarding that
        // text tells the caller about the edge's conversation with its
        // database rather than about their request.
        let err = ApiError::from_brain(&server(
            ErrorCategoryWire::Internal,
            "shard 7 wal replay failed at lsn 918273",
        ));
        assert_eq!(err.status(), StatusCode::BAD_GATEWAY);
        assert!(
            !err.message().contains("shard 7") && !err.message().contains("lsn"),
            "internal state must not reach the caller, got: {}",
            err.message()
        );
        // Withheld, not discarded: it is logged with the request id.
        assert!(
            err.internal
                .as_deref()
                .unwrap_or_default()
                .contains("wal replay failed"),
            "the detail must still be available to the log"
        );
    }

    #[test]
    fn a_transport_fault_withholds_the_os_error() {
        // The OS text describes the edge's own connectivity to its backend.
        let err = ApiError::from_brain(&BrainError::Io(std::io::Error::from(
            std::io::ErrorKind::ConnectionRefused,
        )));
        assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            !err.message().to_lowercase().contains("refused"),
            "the OS error must not reach the caller, got: {}",
            err.message()
        );
        assert!(err.internal.is_some(), "and must still reach the log");
    }

    #[test]
    fn a_protocol_fault_withholds_the_protocol_detail() {
        // Frame-codec faults and version mismatches are entirely about the
        // edge↔Brain link; a caller can do nothing with them.
        let err = ApiError::from_brain(&BrainError::Protocol(
            "expected WELCOME on stream 0, got 0x00FF".to_string(),
        ));
        assert_eq!(err.status(), StatusCode::BAD_GATEWAY);
        assert!(
            !err.message().contains("WELCOME") && !err.message().contains("0x00FF"),
            "protocol internals must not reach the caller, got: {}",
            err.message()
        );
        assert!(err.internal.is_some());
    }

    #[test]
    fn every_5xx_arm_withholds_something_and_every_4xx_arm_does_not() {
        // Exhaustive over the category table, so a new arm cannot be added
        // without a decision being made about what the caller is told.
        let categories = [
            ErrorCategoryWire::Protocol,
            ErrorCategoryWire::Authentication,
            ErrorCategoryWire::Authorization,
            ErrorCategoryWire::Validation,
            ErrorCategoryWire::NotFound,
            ErrorCategoryWire::Conflict,
            ErrorCategoryWire::ResourceExhausted,
            ErrorCategoryWire::Internal,
            ErrorCategoryWire::Unavailable,
        ];
        for category in categories {
            let err = ApiError::from_brain(&server(category, "engine detail"));
            assert_eq!(
                err.status().is_server_error(),
                err.internal.is_some(),
                "{category:?}: a 5xx must withhold, a 4xx must not"
            );
        }
    }
}
