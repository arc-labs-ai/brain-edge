//! Request extractors that refuse in this API's own error envelope.
//!
//! EVERY REFUSAL LOOKS THE SAME OR THE ENVELOPE IS NOT AN ENVELOPE. A caller
//! parses one shape — `{"error":{code,message}}` — and axum's own `Json` and
//! `Query` extractors do not produce it: they reject with `text/plain`, so a
//! request that was merely missing a field came back as a bare sentence while
//! every other refusal on the same route came back as JSON. A client that
//! parsed the body saw a parse failure where the server had been perfectly
//! clear, and the error it reported was its own, not the one it was sent.
//!
//! ```text
//! POST /v1/plan  {"goal":{…}}         422 text/plain   "Failed to deserialize…"
//! POST /v1/plan  {"start":{},"goal":…} 400 application/json  {"error":{…}}
//! ```
//!
//! Both of those refuse the same request for the same reason. Only the second
//! one could be read.
//!
//! The wrappers below delegate to axum and map the rejection, so the
//! extraction behaviour is unchanged and only the refusal is rewritten. They
//! are the extractors handlers use; plain `Json` and `Query` are not.

use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{FromRequest, FromRequestParts, Query, Request};
use axum::http::request::Parts;
use serde::de::DeserializeOwned;

use crate::error::ApiError;

/// A JSON body, refused in this API's error envelope.
///
/// Used exactly like `axum::Json` in a handler argument.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    Json<T>: FromRequest<S, Rejection = JsonRejection>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(v)| Self(v))
            .map_err(|e| ApiError::invalid_input(e.status(), body_message(&e)))
    }
}

/// A query string, refused in this API's error envelope.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApiQuery<T>(pub T);

impl<S, T> FromRequestParts<S> for ApiQuery<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(v)| Self(v))
            .map_err(|e: QueryRejection| {
                ApiError::invalid_input(e.status(), format!("invalid query string: {}", cause(&e)))
            })
    }
}

/// What was wrong with the body, in the caller's terms.
///
/// Axum's own text names the Rust type it failed to build — "into the target
/// type" — which is this server's business and not the caller's. The variant
/// says which of the three failures happened; the detail after it is axum's,
/// because it names the field or the byte offset and that is the useful part.
fn body_message(rejection: &JsonRejection) -> String {
    let detail = cause(rejection);
    match rejection {
        JsonRejection::JsonDataError(_) => format!("malformed request body: {detail}"),
        JsonRejection::JsonSyntaxError(_) => format!("request body is not valid JSON: {detail}"),
        JsonRejection::MissingJsonContentType(_) => {
            "request body must be sent as application/json".to_string()
        }
        _ => format!("request body could not be read: {detail}"),
    }
}

/// The innermost reason, with axum's framing stripped.
///
/// A rejection's `Display` is a sentence wrapping its source; the source is
/// serde's own message, which names the field. Taking the source where there
/// is one keeps the part a caller can act on and drops the part about this
/// server's types.
fn cause(e: &(dyn std::error::Error + 'static)) -> String {
    let mut current: &(dyn std::error::Error + 'static) = e;
    while let Some(source) = current.source() {
        current = source;
    }
    current.to_string()
}

#[cfg(test)]
mod extract_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request as HttpRequest, StatusCode, header};
    use axum::response::IntoResponse;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    struct Shape {
        start: String,
    }

    async fn refuse(body: &'static str, content_type: Option<&str>) -> (StatusCode, String) {
        let mut req = HttpRequest::builder().method("POST").uri("/");
        if let Some(ct) = content_type {
            req = req.header(header::CONTENT_TYPE, ct);
        }
        let req = req.body(Body::from(body)).unwrap();
        let err = ApiJson::<Shape>::from_request(req, &())
            .await
            .err()
            .unwrap();
        // Read both off the RESPONSE, which is what a caller actually gets.
        let resp = err.into_response();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    /// The whole point: a refusal a client can parse.
    #[tokio::test]
    async fn a_missing_field_refuses_in_the_error_envelope() {
        let (status, body) = refuse(r#"{"goal":"x"}"#, Some("application/json")).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let v: serde_json::Value = serde_json::from_str(&body).expect("the body IS json");
        assert_eq!(v["error"]["code"], "bad_request");
        // The caller is told which field, and not which Rust type.
        let message = v["error"]["message"].as_str().unwrap();
        assert!(message.contains("start"), "names the field: {message}");
        assert!(
            !message.contains("target type"),
            "does not name this server's types: {message}"
        );
    }

    /// Syntax and schema are different failures and keep different statuses —
    /// one is "this is not JSON", the other is "this is not the right JSON".
    #[tokio::test]
    async fn a_syntax_error_and_a_schema_error_stay_apart() {
        let (syntax, body) = refuse("{not json", Some("application/json")).await;
        assert_eq!(syntax, StatusCode::BAD_REQUEST);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not valid JSON"),
            "{body}"
        );
    }

    /// A body sent without the JSON content type is its own failure, and
    /// telling the caller to fix the field would send them after the wrong
    /// thing.
    #[tokio::test]
    async fn a_missing_content_type_says_so() {
        let (status, body) = refuse(r#"{"start":"x"}"#, None).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap()
                .contains("application/json"),
            "{body}"
        );
    }

    /// A body that IS the right shape still extracts, unchanged.
    #[tokio::test]
    async fn a_valid_body_is_extracted_as_before() {
        let req = HttpRequest::builder()
            .method("POST")
            .uri("/")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"start":"here"}"#))
            .unwrap();
        let ApiJson(shape) = ApiJson::<Shape>::from_request(req, &()).await.unwrap();
        assert_eq!(shape.start, "here");
    }
}
