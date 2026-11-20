//! This test must be alone in its own binary because a thread-local subscriber
//! is unreliable in a multi-test binary: tracing's max-level hint is global and
//! can be recomputed by other threads.
//! The log assertion deliberately checks that every `request_id` in the capture
//! is the same id, because a bare `contains` is satisfied by the tower-http span
//! alone and does not test the field `log_internal` emits.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use axum::routing::get;
use tower::ServiceExt;

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("capture lock")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

const MAX_BODY: usize = 64;
const TIMEOUT: Duration = Duration::from_millis(100);

fn get_req(path: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .body(Body::empty())
        .expect("build request")
}

#[test]
fn an_internal_error_uses_one_request_id_in_header_body_and_log() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let writer = Capture(captured.clone());
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(writer)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("set global subscriber");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build runtime");

    runtime.block_on(async {
        let router = brain_edge::harden(
            Router::new().route(
                "/internal",
                get(|| async { brain_edge::ApiError::internal("database detail") }),
            ),
            TIMEOUT,
            MAX_BODY,
        );
        let response = router
            .oneshot(get_req("/internal"))
            .await
            .expect("router response");
        let id = response
            .headers()
            .get("x-request-id")
            .expect("request id header")
            .to_str()
            .expect("request id is text")
            .to_owned();
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("read body");
        let body: serde_json::Value = serde_json::from_slice(&body).expect("body is json");
        assert_eq!(body["error"]["request_id"], id);

        let log =
            String::from_utf8(captured.lock().expect("capture lock").clone()).expect("log is utf8");
        let detail_line = log
            .lines()
            .find(|line| line.contains("request failed"))
            .unwrap_or_else(|| panic!("captured log has no request failed line: {log}"));
        assert!(
            detail_line.contains("detail=database detail"),
            "request failed line must contain the withheld detail: {detail_line}\nfull log: {log}"
        );

        let marker = "request_id=\"";
        let mut request_ids = Vec::new();
        let mut remaining = log.as_str();
        while let Some(start) = remaining.find(marker) {
            let value_start = start + marker.len();
            let value_end = remaining[value_start..].find('\"').map_or_else(
                || panic!("unterminated request id in captured log: {log}"),
                |offset| value_start + offset,
            );
            request_ids.push(&remaining[value_start..value_end]);
            remaining = &remaining[value_end + 1..];
        }
        assert!(
            !request_ids.is_empty(),
            "captured log must contain at least one quoted request id: {log}"
        );
        for request_id in request_ids {
            assert_eq!(
                request_id,
                id.as_str(),
                "offending request id {request_id:?}; full log: {log}"
            );
        }
    });
}
