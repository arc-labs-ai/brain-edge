//! The transport protections in [`brain_edge::harden`], asserted end to end.
//!
//! These are the layers a deployment gets from `run` and an embedder has to
//! apply itself. Every one of them is invisible when it works and expensive
//! when it silently does not, so each is exercised through the real router with
//! `ServiceExt::oneshot` rather than trusted to be wired up.
//!
//! `tests/ports.rs` covers the commercial seam (who is consulted, what is
//! metered). This file covers what happens to a request that goes wrong.

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use tower::ServiceExt;

/// The body cap, timeout and id header used throughout. Small enough to reach.
const MAX_BODY: usize = 64;
const TIMEOUT: Duration = Duration::from_millis(100);

/// A router with one route per failure mode, wrapped in the real `harden`.
fn hardened() -> Router {
    let inner = Router::new()
        .route("/ok", get(|| async { "ok" }))
        .route(
            "/panic",
            get(|| async {
                panic!("deliberate panic from a handler");
                #[allow(unreachable_code)]
                ""
            }),
        )
        .route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                "never"
            }),
        )
        .route(
            "/echo",
            axum::routing::post(|body: String| async move { body }),
        );
    brain_edge::harden(inner, TIMEOUT, MAX_BODY)
}

async fn send(req: Request<Body>) -> axum::http::Response<Body> {
    hardened().oneshot(req).await.expect("router response")
}

fn get_req(path: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .body(Body::empty())
        .expect("build request")
}

async fn body_string(resp: axum::http::Response<Body>) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).into_owned()
}

// --- panic recovery --------------------------------------------------------

#[tokio::test]
async fn a_panicking_handler_becomes_a_500_rather_than_a_dropped_connection() {
    // Without `CatchPanicLayer` the task aborts and hyper closes the socket
    // with no response at all: the caller sees a connection reset, with no
    // status and no body to quote in a bug report.
    let resp = send(get_req("/panic")).await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let body = body_string(resp).await;
    assert!(
        body.contains("\"code\":\"internal\""),
        "a panic must render the same JSON envelope as every other failure, got: {body}"
    );
    // The panic message is for the log, not for the caller.
    assert!(
        !body.contains("deliberate panic"),
        "the panic payload must not reach the caller, got: {body}"
    );
}

#[tokio::test]
async fn a_panic_does_not_take_the_service_down() {
    // The point of recovering is that the next request still works.
    let router = hardened();
    let panicked = router
        .clone()
        .oneshot(get_req("/panic"))
        .await
        .expect("panic response");
    assert_eq!(panicked.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let after = router.oneshot(get_req("/ok")).await.expect("next response");
    assert_eq!(after.status(), StatusCode::OK);
}

// --- correlation id --------------------------------------------------------

#[tokio::test]
async fn every_response_carries_a_request_id() {
    let resp = send(get_req("/ok")).await;
    let id = resp
        .headers()
        .get("x-request-id")
        .expect("x-request-id must be set on every response");
    assert!(!id.is_empty(), "the id must not be blank");
}

#[tokio::test]
async fn a_caller_supplied_request_id_survives_the_hop() {
    // An id assigned by a gateway or load balancer upstream has to be kept, or
    // the two halves of a trace cannot be joined.
    let req = Request::builder()
        .uri("/ok")
        .header("x-request-id", "upstream-abc-123")
        .body(Body::empty())
        .expect("build request");
    let resp = send(req).await;
    assert_eq!(
        resp.headers().get("x-request-id").expect("id echoed"),
        "upstream-abc-123"
    );
}

#[tokio::test]
async fn a_failure_generated_by_the_middleware_still_carries_an_id() {
    // The 413 below never reaches a handler — it is produced by the body-limit
    // layer. Correlation is least useful if it only covers the easy path, so
    // the id layer is outermost precisely for this case.
    let req = Request::builder()
        .method("POST")
        .uri("/echo")
        .body(Body::from("x".repeat(MAX_BODY * 4)))
        .expect("build request");
    let resp = send(req).await;
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(
        resp.headers().contains_key("x-request-id"),
        "a middleware-generated failure must be correlatable too"
    );
}

#[tokio::test]
async fn a_panic_response_carries_an_id() {
    let resp = send(get_req("/panic")).await;
    assert!(
        resp.headers().contains_key("x-request-id"),
        "the one failure an operator most needs to find in the log"
    );
}

// --- body cap --------------------------------------------------------------

#[tokio::test]
async fn a_body_over_the_cap_is_refused() {
    let req = Request::builder()
        .method("POST")
        .uri("/echo")
        .body(Body::from("x".repeat(MAX_BODY + 1)))
        .expect("build request");
    assert_eq!(send(req).await.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn a_body_at_the_cap_is_accepted() {
    // Asserting the boundary from both sides keeps the cap from being off by
    // one, which would reject legitimate requests at exactly the documented
    // limit.
    let req = Request::builder()
        .method("POST")
        .uri("/echo")
        .body(Body::from("x".repeat(MAX_BODY)))
        .expect("build request");
    assert_eq!(send(req).await.status(), StatusCode::OK);
}

// --- timeout ---------------------------------------------------------------

#[tokio::test]
async fn a_stalled_handler_is_cut_at_the_timeout() {
    // A stalled Brain must not hold a connection open indefinitely; the caller
    // gets an explicit 408 rather than waiting on a socket that never answers.
    let resp = send(get_req("/slow")).await;
    assert_eq!(resp.status(), StatusCode::REQUEST_TIMEOUT);
}
