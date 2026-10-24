//! The request-body cap, enforced so the caller can read the refusal.
//!
//! Returning `413` to a client that is still uploading is not enough on its
//! own. When the response completes while the request body sits unread, the
//! socket is closed with data still in its receive queue, and TCP requires a
//! reset rather than an orderly FIN. A reset discards whatever the peer has
//! already buffered — including the `413` — so the caller is left with
//! `ECONNRESET` and no way to tell "your payload was too large" (shrink it)
//! from "the network died" (retry it).
//!
//! Measured against this edge before the fix: every oversize body failed this
//! way for the Rust and Python SDKs, from 2 KB over the cap to 11 MB over it.
//! `curl` and `undici` escaped only because both abandon the upload the moment
//! a response appears mid-write; a client that writes its body and then reads,
//! which is the ordinary shape, cannot.
//!
//! So an over-cap body is read and discarded before the refusal is written.
//! Both bounds on that drain are deliberate:
//!
//! - **Bytes** ([`DRAIN_BUDGET_FACTOR`]) — draining costs this side almost
//!   nothing in bandwidth, since the bytes are already on the wire whether or
//!   not they are read. What it does cost is holding a connection open for a
//!   body that will be thrown away, so a body far past the cap is not humoured:
//!   it is not a near miss, it is a client that was never going to succeed.
//! - **Time** ([`DRAIN_DEADLINE`]) — the byte budget alone bounds nothing
//!   against a client that sends those bytes one per second.
//!
//! Past either bound the drain is abandoned and the reset stands. That is the
//! pre-fix behaviour, which is the correct fallback: a caller who will not stop
//! sending gets the connection taken away.
//!
//! This handles the `Content-Length` case, which is every request the three
//! SDKs make — each serializes its body to a buffer before sending. A chunked
//! body of unknown length is still caught by `RequestBodyLimitLayer` underneath
//! (see [`crate::harden`]) and still resets, which is the lesser problem: a
//! client streaming an unknown length is writing incrementally and is far more
//! likely to notice the early response.

use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;

use crate::error::ApiError;

/// How far past the cap a body is still drained, as a multiple of the cap.
///
/// Eight covers the honest mistake — a document that came out larger than
/// expected — without turning the edge into a sink for a body that is orders of
/// magnitude over.
const DRAIN_BUDGET_FACTOR: usize = 8;

/// How long the drain may take before the reset is allowed to stand.
const DRAIN_DEADLINE: Duration = Duration::from_secs(2);

/// Reject a body whose declared length exceeds `max`, after draining it so the
/// rejection survives the connection close.
pub async fn enforce(State(max): State<usize>, request: Request, next: Next) -> Response {
    let Some(declared) = declared_length(&request) else {
        return next.run(request).await;
    };
    if declared <= max {
        return next.run(request).await;
    }

    // Only worth reading if reading it can finish. Past the budget the reset
    // is going to happen regardless, so the bytes are refused outright rather
    // than half-consumed.
    if declared <= max.saturating_mul(DRAIN_BUDGET_FACTOR) {
        let (_parts, body) = request.into_parts();
        drain(body).await;
    }

    too_large(declared, max).into_response()
}

/// The `Content-Length` a request declares, if it declares a usable one.
fn declared_length(request: &Request) -> Option<usize> {
    request
        .headers()
        .get(header::CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .parse()
        .ok()
}

/// Read and discard the body, giving up at [`DRAIN_DEADLINE`].
async fn drain(body: Body) {
    let _ = tokio::time::timeout(DRAIN_DEADLINE, async move {
        let mut body = body;
        // `frame()` rather than `collect()`: the point is to consume the bytes,
        // not to hold them. Collecting would buffer the very body the cap
        // exists to keep out of memory.
        while let Some(Ok(_frame)) = body.frame().await {}
    })
    .await;
}

/// The refusal, in the same envelope as every other error this edge returns.
///
/// Both numbers are named. A cap is configurable per deployment, so a caller
/// who is told only "too large" cannot tell whether to split the payload in two
/// or in twenty.
fn too_large(declared: usize, max: usize) -> ApiError {
    ApiError::payload_too_large(format!(
        "request body is {declared} bytes; this server accepts at most {max}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::to_bytes;
    use axum::http::StatusCode;
    use axum::routing::post;
    use tower::ServiceExt;

    const CAP: usize = 1024;

    fn app() -> Router {
        Router::new()
            .route(
                "/",
                post(|body: String| async move { body.len().to_string() }),
            )
            .layer(axum::middleware::from_fn_with_state(CAP, enforce))
    }

    async fn post_body(len: usize) -> Response {
        app()
            .oneshot(
                Request::post("/")
                    .header(header::CONTENT_LENGTH, len)
                    .body(Body::from("x".repeat(len)))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_body_at_the_cap_is_served() {
        // The cap is inclusive; an exact fit is not an edge case to lose.
        let response = post_body(CAP).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn one_byte_over_is_refused() {
        let response = post_body(CAP + 1).await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn the_refusal_names_both_sizes() {
        // Without the cap, a caller cannot tell how far to shrink; without the
        // actual size, they cannot tell which request was the problem.
        let response = post_body(CAP * 3).await;
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains(&(CAP * 3).to_string()), "{text}");
        assert!(text.contains(&CAP.to_string()), "{text}");
    }

    #[tokio::test]
    async fn the_refusal_uses_the_standard_error_envelope() {
        // `RequestBodyLimitLayer` returns a bare string body, which every SDK
        // reports as its fallback code. This is the one error a client is most
        // likely to want to branch on, so it carries a real code.
        let response = post_body(CAP + 1).await;
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["code"], "payload_too_large");
    }

    #[tokio::test]
    async fn a_near_miss_is_drained_and_a_far_miss_is_not() {
        // The drain is what lets the caller read the 413 at all, so the
        // boundary it stops at is the contract, not an implementation detail.
        let near = CAP * DRAIN_BUDGET_FACTOR;
        let far = near + 1;
        assert_eq!(
            post_body(near).await.status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(post_body(far).await.status(), StatusCode::PAYLOAD_TOO_LARGE);
        // Both refuse; the difference is only whether the body was consumed
        // first, which `oneshot` cannot observe. The sizes are asserted here so
        // that changing `DRAIN_BUDGET_FACTOR` is a deliberate act.
        assert_eq!(DRAIN_BUDGET_FACTOR, 8);
    }

    #[tokio::test]
    async fn a_request_without_content_length_is_passed_through() {
        // Chunked bodies are `RequestBodyLimitLayer`'s to catch; this layer
        // must not guess at a length it was not given.
        let response = app()
            .oneshot(Request::post("/").body(Body::from("short")).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn an_unparseable_content_length_is_passed_through() {
        // Not this layer's refusal to make: hyper rejects a malformed
        // `Content-Length` at the protocol level, and guessing here would turn
        // a protocol error into a misleading 413.
        let response = app()
            .oneshot(
                Request::post("/")
                    .header(header::CONTENT_LENGTH, "not-a-number")
                    .body(Body::from("short"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
