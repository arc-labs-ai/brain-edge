//! HTTP error type — one JSON error envelope, mapped from Brain's taxonomy.
//!
//! A `5xx` body deliberately carries no internal detail. `BrainError` renders
//! frame-codec faults, protocol violations and version mismatches into their
//! `Display` text, and forwarding that to an HTTP caller tells them about the
//! edge's conversation with its database rather than about their request. The
//! detail carries the request's own correlation id, returned in `x-request-id`
//! and `error.request_id`, and logged on the request's line.

use axum::Json;
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use brain_db_sdk::BrainError;
use brain_db_sdk::wire::types::{ErrorCategoryWire, ErrorCodeWire};
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
    retry_after_ms: Option<u32>,
    field_errors: Vec<FieldError>,
    engine_code: Option<&'static str>,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: ErrorInner<'a>,
}

#[derive(Serialize)]
struct ErrorInner<'a> {
    code: &'a str,
    message: &'a str,
    /// The engine's own code for this condition, when the caller can act on it.
    /// `code` above stays the stable class a client switches on; this names the
    /// exact refusal, so an idempotency conflict and a cardinality violation are
    /// told apart without parsing the message.
    #[serde(skip_serializing_if = "Option::is_none")]
    engine_code: Option<&'a str>,
    /// Per-field refusals, keyed by the request field the engine named.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    errors: &'a Vec<FieldError>,
    /// Present only when detail was withheld. The request's id is also on the
    /// response header and its request log line, so a caller can name the
    /// request an operator has to find.
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<&'a str>,
}

/// One request field the engine refused, in the `errors` array of an error body.
///
/// The shape is the platform's: the cloud gateway serves RFC 9457 problem
/// documents whose `errors[]` carries exactly these three members, and all
/// three SDKs' HTTP clients already read that array. A self-hosted edge
/// answering a different shape would make handling depend on the deployment.
#[derive(Debug, Serialize)]
pub struct FieldError {
    /// The request field the engine named.
    pub field: String,
    /// The engine's code for the rule that refused it.
    pub code: String,
    /// Caller-facing detail for this field.
    pub message: String,
}

tokio::task_local! {
    /// The correlation id for the request being served.
    static REQUEST_ID: String;
}

/// Carry the request's correlation id for the duration of the request, so an
/// error body and the log line beside it quote the id the caller was handed.
pub async fn capture_request_id(req: Request, next: Next) -> axum::response::Response {
    let id = req
        .headers()
        .get(crate::REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    match id {
        Some(id) => REQUEST_ID.scope(id, next.run(req)).await,
        None => next.run(req).await,
    }
}

fn current_request_id() -> Option<String> {
    REQUEST_ID.try_with(String::clone).ok()
}

fn retry_after_secs(ms: u32) -> u32 {
    ms.div_ceil(1000).max(1)
}

fn engine_code_slug(code: ErrorCodeWire) -> Option<&'static str> {
    Some(match code {
        ErrorCodeWire::InvalidArgument => "invalid_argument",
        ErrorCodeWire::BadSessionId => "bad_session_id",
        ErrorCodeWire::PredicateNotInSchema => "predicate_not_in_schema",
        ErrorCodeWire::RelationTypeNotInSchema => "relation_type_not_in_schema",
        ErrorCodeWire::MemoryNotFound => "memory_not_found",
        ErrorCodeWire::ContextNotFound => "context_not_found",
        ErrorCodeWire::SubscriptionNotFound => "subscription_not_found",
        ErrorCodeWire::SnapshotNotFound => "snapshot_not_found",
        ErrorCodeWire::TxnNotFound => "txn_not_found",
        ErrorCodeWire::IdempotencyConflict => "idempotency_conflict",
        ErrorCodeWire::TransactionConflict => "transaction_conflict",
        ErrorCodeWire::TransactionTimeout => "transaction_timeout",
        ErrorCodeWire::StreamIdInUse => "stream_id_in_use",
        ErrorCodeWire::SubscriptionLsnTooOld => "subscription_lsn_too_old",
        ErrorCodeWire::CardinalityViolation => "cardinality_violation",
        ErrorCodeWire::LlmCredentialMissing => "llm_credential_missing",
        ErrorCodeWire::OutOfSlots => "out_of_slots",
        ErrorCodeWire::OutOfDisk => "out_of_disk",
        ErrorCodeWire::OutOfMemory => "out_of_memory",
        ErrorCodeWire::RateLimited => "rate_limited",
        ErrorCodeWire::StreamLimitExceeded => "stream_limit_exceeded",
        ErrorCodeWire::TransactionLimitExceeded => "transaction_limit_exceeded",
        ErrorCodeWire::TransactionTooLarge => "transaction_too_large",
        ErrorCodeWire::Unauthenticated => "unauthenticated",
        ErrorCodeWire::NotAuthenticated => "not_authenticated",
        ErrorCodeWire::SessionExpired => "session_expired",
        ErrorCodeWire::PermissionDenied => "permission_denied",
        ErrorCodeWire::AdminPermissionRequired => "admin_permission_required",
        ErrorCodeWire::ActAsDenied => "act_as_denied",
        _ => return None,
    })
}

fn exhausted_status_and_code(wire_code: ErrorCodeWire) -> (StatusCode, &'static str) {
    match wire_code {
        ErrorCodeWire::RateLimited => (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
        ErrorCodeWire::OutOfSlots | ErrorCodeWire::OutOfDisk | ErrorCodeWire::OutOfMemory => {
            (StatusCode::SERVICE_UNAVAILABLE, "engine.unavailable")
        }
        // Every other exhaustion verdict is a ceiling that belongs to the
        // caller: the named quota codes — stream count, transaction count,
        // transaction size — and any exhaustion code this build cannot name.
        // A 429 says the ceiling is yours and will not move for a retry,
        // where a 503 would promise capacity coming back; guessing retryable
        // for a condition we cannot name is how a client spins forever, the
        // same rule as Unknown and OperatorIntervention. Which ceiling it was
        // still reaches the caller as `engine_code`, from the slug table, for
        // every code that build knows.
        _ => (StatusCode::TOO_MANY_REQUESTS, "quota.exceeded"),
    }
}

fn status_and_code(
    category: ErrorCategoryWire,
    wire_code: ErrorCodeWire,
) -> (StatusCode, &'static str) {
    match category {
        ErrorCategoryWire::Authentication => (StatusCode::UNAUTHORIZED, "unauthorized"),
        ErrorCategoryWire::Authorization => (StatusCode::FORBIDDEN, "forbidden"),
        ErrorCategoryWire::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "engine.unavailable"),
        ErrorCategoryWire::ResourceExhausted => exhausted_status_and_code(wire_code),
        ErrorCategoryWire::Validation => (StatusCode::BAD_REQUEST, "bad_request"),
        ErrorCategoryWire::NotFound => (StatusCode::NOT_FOUND, "not_found"),
        ErrorCategoryWire::Conflict => (StatusCode::CONFLICT, "conflict"),
        // The deployment itself is broken and no retry can clear
        // it; an operator has to repair it first. A 503 would be
        // worse than unhelpful here — it invites exactly the retry
        // that can never succeed — so the status says "upstream
        // fault" and the code carries the part a client acts on.
        ErrorCategoryWire::OperatorIntervention => {
            (StatusCode::BAD_GATEWAY, "engine.operator_intervention")
        }
        // A category this build does not know, which means Brain is
        // newer than this edge. Unclassifiable is not retryable:
        // guessing a retry policy for a condition we cannot name is
        // how a client spins against a permanent failure.
        ErrorCategoryWire::Unknown => (StatusCode::BAD_GATEWAY, "engine.error"),
        ErrorCategoryWire::Protocol | ErrorCategoryWire::Internal => {
            (StatusCode::BAD_GATEWAY, "engine.error")
        }
    }
}

fn field_errors(
    details: Option<&brain_db_sdk::wire::types::ErrorDetails>,
    message: &str,
    wire_code: ErrorCodeWire,
    fallback_code: &'static str,
) -> Vec<FieldError> {
    details
        .and_then(|details| details.field.as_ref().map(|field| (field, details)))
        .map(|(field, details)| {
            let mut detail = message.to_string();
            if details.expected.is_some() || details.actual.is_some() {
                detail.push_str(" (expected ");
                detail.push_str(details.expected.as_deref().unwrap_or("-"));
                detail.push_str(", actual ");
                detail.push_str(details.actual.as_deref().unwrap_or("-"));
                detail.push(')');
            }
            vec![FieldError {
                field: field.clone(),
                code: engine_code_slug(wire_code)
                    .unwrap_or(fallback_code)
                    .to_string(),
                message: detail,
            }]
        })
        .unwrap_or_default()
}

fn from_brain_server(
    category: ErrorCategoryWire,
    wire_code: ErrorCodeWire,
    message: &str,
    details: Option<&brain_db_sdk::wire::types::ErrorDetails>,
) -> ApiError {
    let (status, code) = status_and_code(category, wire_code);
    // Brain's own message is about THIS caller's request, so it is
    // returned as-is for the statuses that describe the request.
    // The 5xx arms are the edge reporting its own trouble.
    let internal = if status.is_server_error() {
        Some(format!("brain {category:?}: {message}"))
    } else {
        None
    };
    // The message, exact code and field detail describe the
    // caller-facing refusal only when there is no withheld detail;
    // a 5xx keeps all three with the edge↔engine conversation.
    let engine_code = if internal.is_none() {
        engine_code_slug(wire_code)
    } else {
        None
    };
    let field_errors = if internal.is_none() {
        field_errors(details, message, wire_code, code)
    } else {
        Vec::new()
    };
    ApiError {
        status,
        code,
        message: if internal.is_some() {
            "the memory engine returned an error".to_string()
        } else {
            message.to_string()
        },
        internal,
        retry_after_ms: None,
        field_errors,
        engine_code,
    }
}

impl ApiError {
    /// A `401` for a missing/blank credential.
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message: message.into(),
            internal: None,
            retry_after_ms: None,
            field_errors: Vec::new(),
            engine_code: None,
        }
    }

    /// A `429` for a tenant that has spent its plan's allowance.
    ///
    /// Same status as a rate limit, a different code, and the distinction
    /// matters to the caller: a rate limit says "slower" and clears in
    /// seconds, this says "you have used the month" and clears by upgrading or
    /// waiting for the period. A client that cannot tell them apart backs off
    /// and retries forever against a ceiling that will not move.
    pub fn quota_exceeded(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "quota.exceeded",
            message: message.into(),
            internal: None,
            retry_after_ms: None,
            field_errors: Vec::new(),
            engine_code: None,
        }
    }

    /// A `429` for a caller the engine is throttling.
    ///
    /// A different code from `quota.exceeded` and the distinction is the point:
    /// this one clears in seconds and a backoff is the right response, while a
    /// spent plan allowance clears only by upgrading or waiting out the period.
    pub fn rate_limited(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "rate_limited",
            message: message.into(),
            internal: None,
            retry_after_ms: None,
            field_errors: Vec::new(),
            engine_code: None,
        }
    }

    /// A `403` for a credential that is valid but not permitted here.
    ///
    /// Distinct from `401`, which says the caller is unknown. Both exist
    /// because a host's resolver can reach either verdict and collapsing them
    /// tells a tenant to go fix the wrong thing.
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "forbidden",
            message: message.into(),
            internal: None,
            retry_after_ms: None,
            field_errors: Vec::new(),
            engine_code: None,
        }
    }

    /// A `503` for a dependency that is down but expected back.
    ///
    /// The identity store is the case this exists for: a host's resolver that
    /// cannot reach its key store must say so honestly, because answering
    /// `401` would tell a tenant their key is bad and send them to rotate a
    /// key that is fine. Retryable, and in every SDK's retry list.
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "unavailable",
            message: message.into(),
            internal: None,
            retry_after_ms: None,
            field_errors: Vec::new(),
            engine_code: None,
        }
    }

    /// A `501` for a capability this edge was not configured with.
    ///
    /// Used when BYOK is requested but no key-encryption key is set: the edge
    /// declines rather than falling back to storing a provider key in the
    /// clear. `501` rather than `503` because it will not start working on
    /// retry — it needs configuration.
    pub fn not_implemented(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_IMPLEMENTED,
            code: "not_implemented",
            message: message.into(),
            internal: None,
            retry_after_ms: None,
            field_errors: Vec::new(),
            engine_code: None,
        }
    }

    /// A `409` for a request that conflicts with the tenant's current state.
    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "conflict",
            message: message.into(),
            internal: None,
            retry_after_ms: None,
            field_errors: Vec::new(),
            engine_code: None,
        }
    }

    /// A `502` when an upstream the edge depends on could not be reached.
    ///
    /// The detail is deliberately thin. An upstream error can embed the
    /// request URL and its headers, and a header on a credential-bearing call
    /// carries a tenant's provider key.
    pub fn bad_gateway(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            code: "upstream.error",
            message: message.into(),
            internal: None,
            retry_after_ms: None,
            field_errors: Vec::new(),
            engine_code: None,
        }
    }

    /// A `409 llm_credential.missing` — this namespace has no provider key.
    ///
    /// Shares the code the data plane returns for the same condition (see
    /// `from_brain`), so one code means one thing whichever surface produced
    /// it, and a client's handling works against both.
    pub fn llm_credential_missing(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "llm_credential.missing",
            message: message.into(),
            internal: None,
            retry_after_ms: None,
            field_errors: Vec::new(),
            engine_code: None,
        }
    }

    /// A `404` for a resource this tenant has not created.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            message: message.into(),
            internal: None,
            retry_after_ms: None,
            field_errors: Vec::new(),
            engine_code: None,
        }
    }

    /// A `413` for a body past this deployment's cap.
    ///
    /// The message names the actual size and the cap, because the cap is
    /// configurable: told only "too large", a caller cannot tell whether to
    /// split the payload in two or in twenty.
    pub fn payload_too_large(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: "payload_too_large",
            message: message.into(),
            internal: None,
            retry_after_ms: None,
            field_errors: Vec::new(),
            engine_code: None,
        }
    }

    /// A `422` for a schema document the validator refused.
    ///
    /// Its own code rather than `bad_request` because the request's SHAPE was
    /// fine — the document's content is what has to change — and a client
    /// branches on that difference.
    ///
    /// This envelope carries one message, so it names the error count and the
    /// first error with its position. `POST /v1/schema/validate` returns the
    /// full per-line list and is the endpoint to drive an editor from.
    pub fn schema_rejected(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "schema.rejected",
            message: message.into(),
            internal: None,
            retry_after_ms: None,
            field_errors: Vec::new(),
            engine_code: None,
        }
    }

    /// A `400` for a malformed request the edge itself rejects.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "bad_request",
            message: message.into(),
            internal: None,
            retry_after_ms: None,
            field_errors: Vec::new(),
            engine_code: None,
        }
    }

    /// A refused request body or query string, at the status the extractor
    /// chose.
    ///
    /// The status is NOT normalised: axum separates a body that is not JSON
    /// (`400`) from one that is JSON but not this shape (`422`) from one sent
    /// without a JSON content type (`415`), and those are three different
    /// things for the caller to fix. Only the envelope is made uniform.
    pub fn invalid_input(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            code: "bad_request",
            message: message.into(),
            internal: None,
            retry_after_ms: None,
            field_errors: Vec::new(),
            engine_code: None,
        }
    }

    /// Map a Brain client error to the matching HTTP status. Transport faults and
    /// unavailable/exhausted verdicts are `503`; auth verdicts `401`/`403`;
    /// validation/not-found/conflict map directly; anything else is `502`.
    pub fn from_brain(err: &BrainError) -> Self {
        let hint = match err {
            BrainError::Server { retry_after_ms, .. } => *retry_after_ms,
            _ => None,
        };
        let mapped = match err {
            BrainError::Io(_) | BrainError::Closed | BrainError::Timeout(_) => Self {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "engine.unavailable",
                // The OS error text describes the edge's own connectivity, not
                // the caller's request.
                message: "the memory engine is unavailable; retry shortly".to_string(),
                internal: Some(format!("brain unavailable: {err}")),
                retry_after_ms: None,
                field_errors: Vec::new(),
                engine_code: None,
            },
            // BYOK credential verdicts are matched on the CODE before the
            // category fan-out below, because the category alone cannot carry
            // them: `Conflict` is also an ordinary idempotency conflict, and
            // telling an operator to register a provider key they already hold
            // is worse than a generic error.
            BrainError::Server {
                code: ErrorCodeWire::LlmCredentialMissing,
                message,
                ..
            } => Self {
                // 409, deliberately NOT 503. The SDK retry lists are
                // `(0, 503)` — a 503 here would make every client retry
                // forever against a tenant that simply has not registered a
                // key. This condition cannot self-heal, so it must be a 4xx.
                status: StatusCode::CONFLICT,
                code: "llm_credential.missing",
                message: message.clone(),
                internal: None,
                retry_after_ms: None,
                field_errors: Vec::new(),
                engine_code: Some("llm_credential_missing"),
            },
            BrainError::Server {
                code: ErrorCodeWire::LlmCredentialUnavailable,
                message,
                ..
            } => Self {
                // 503 so clients DO retry: the key exists and resolving it
                // briefly failed. Collapsing this into the 409 above would
                // permanently fail writes for a healthy tenant.
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "llm_credential.unavailable",
                message: message.clone(),
                internal: None,
                retry_after_ms: None,
                field_errors: Vec::new(),
                engine_code: None,
            },
            BrainError::Server {
                code: wire_code,
                category,
                message,
                response,
                ..
            } => from_brain_server(*category, *wire_code, message, response.details.as_ref()),
            // Frame-codec faults, protocol violations, version mismatches: all
            // describe the edge's conversation with Brain, none describe the
            // caller's request.
            other => Self {
                status: StatusCode::BAD_GATEWAY,
                code: "engine.error",
                message: "the memory engine returned an error".to_string(),
                internal: Some(format!("brain error: {other}")),
                retry_after_ms: None,
                field_errors: Vec::new(),
                engine_code: None,
            },
        };
        mapped.with_retry_after_ms(hint)
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
            retry_after_ms: None,
            field_errors: Vec::new(),
            engine_code: None,
        }
    }

    /// Attach the engine's backoff hint to this response.
    #[must_use]
    pub fn with_retry_after_ms(mut self, ms: Option<u32>) -> Self {
        self.retry_after_ms = ms;
        self
    }

    /// Emit the withheld detail to the log, where the request id ties it back
    /// to the response the caller saw. Called once, on the way out.
    fn log_internal(&self, request_id: Option<&str>) {
        if let Some(detail) = &self.internal {
            tracing::warn!(
                status = self.status.as_u16(),
                code = self.code,
                request_id = request_id.unwrap_or("-"),
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

/// The caller-facing message, and only that.
///
/// `internal` is deliberately not printed: it carries the detail that is
/// withheld from a response, and a host that logs an error it got back from
/// the edge should not be the way that detail escapes.
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let request_id = current_request_id();
        let retry_after_ms = self.retry_after_ms;
        let body_request_id = self.internal.as_ref().and(request_id.as_deref());
        self.log_internal(request_id.as_deref());
        let body = ErrorBody {
            error: ErrorInner {
                code: self.code,
                message: &self.message,
                engine_code: self.engine_code,
                errors: &self.field_errors,
                request_id: body_request_id,
            },
        };
        let mut response = (self.status, Json(body)).into_response();
        if let Some(ms) = retry_after_ms {
            let secs = retry_after_secs(ms);
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(secs));
        }
        response
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

    fn server_with_code(
        code: ErrorCodeWire,
        category: ErrorCategoryWire,
        message: &str,
    ) -> BrainError {
        BrainError::from_server(ErrorResponse {
            code,
            category,
            message: message.to_string(),
            details: None,
            retry_after_ms: None,
        })
    }

    fn server_with_details(
        code: ErrorCodeWire,
        category: ErrorCategoryWire,
        message: &str,
        details: brain_db_sdk::wire::types::ErrorDetails,
        retry_after_ms: Option<u32>,
    ) -> BrainError {
        BrainError::from_server(ErrorResponse {
            code,
            category,
            message: message.to_string(),
            details: Some(details),
            retry_after_ms,
        })
    }

    #[test]
    fn a_missing_llm_credential_is_a_409_not_a_503() {
        // The status choice is the contract. Every SDK's retry list is
        // `(0, 503)`, so mapping this to 503 would make clients retry forever
        // against a tenant that has simply not registered a provider key.
        let err = ApiError::from_brain(&server_with_code(
            ErrorCodeWire::LlmCredentialMissing,
            ErrorCategoryWire::Conflict,
            "no provider key registered for acme",
        ));
        assert_eq!(err.status(), StatusCode::CONFLICT);
        assert_eq!(err.code, "llm_credential.missing");
        assert_eq!(err.message(), "no provider key registered for acme");
        assert!(
            err.internal.is_none(),
            "this is about the caller's workspace, not the edge's own trouble"
        );
    }

    #[test]
    fn a_transient_credential_failure_is_a_503_so_clients_retry() {
        let err = ApiError::from_brain(&server_with_code(
            ErrorCodeWire::LlmCredentialUnavailable,
            ErrorCategoryWire::Unavailable,
            "credential store briefly unreachable",
        ));
        assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.code, "llm_credential.unavailable");
    }

    #[test]
    fn an_ordinary_conflict_is_not_reported_as_a_missing_credential() {
        // `Conflict` also covers idempotency conflicts. Matching on category
        // alone would tell operators to configure a key they already have.
        let err = ApiError::from_brain(&server_with_code(
            ErrorCodeWire::IdempotencyConflict,
            ErrorCategoryWire::Conflict,
            "same request id, different body",
        ));
        assert_eq!(err.status(), StatusCode::CONFLICT);
        assert_eq!(err.code, "conflict");
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

    #[test]
    fn resource_exhaustion_distinguishes_rate_limit_from_capacity() {
        let limited = ApiError::from_brain(&server_with_code(
            ErrorCodeWire::RateLimited,
            ErrorCategoryWire::ResourceExhausted,
            "slow down",
        ));
        assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(limited.code, "rate_limited");
        assert_eq!(limited.message(), "slow down");
        assert!(limited.internal.is_none());

        let capacity = ApiError::from_brain(&server_with_code(
            ErrorCodeWire::OutOfDisk,
            ErrorCategoryWire::ResourceExhausted,
            "disk full",
        ));
        assert_eq!(capacity.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(capacity.code, "engine.unavailable");
        assert!(capacity.internal.is_some());
    }

    #[test]
    fn retry_after_conversion_rounds_up_and_never_returns_zero() {
        assert_eq!(retry_after_secs(1), 1);
        assert_eq!(retry_after_secs(1000), 1);
        assert_eq!(retry_after_secs(1001), 2);
        assert_eq!(retry_after_secs(0), 1);
    }

    #[tokio::test]
    async fn details_and_retry_hint_reach_the_error_response() {
        let err = ApiError::from_brain(&server_with_details(
            ErrorCodeWire::InvalidArgument,
            ErrorCategoryWire::Validation,
            "text has the wrong length",
            brain_db_sdk::wire::types::ErrorDetails {
                field: Some("text".into()),
                expected: Some("384".into()),
                actual: Some("768".into()),
            },
            Some(1500),
        ));
        assert_eq!(err.retry_after_ms, Some(1500));
        let response = err.into_response();
        assert_eq!(response.headers()[header::RETRY_AFTER], "2");
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("read body");
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("body is json");
        assert_eq!(body["error"]["errors"][0]["field"], "text");
        assert_eq!(body["error"]["errors"][0]["code"], "invalid_argument");
        assert_eq!(body["error"]["engine_code"], "invalid_argument");
    }
}
