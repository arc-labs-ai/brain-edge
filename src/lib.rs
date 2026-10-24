//! brain-edge — the HTTP/JSON edge for the Brain memory database, as a library.
//!
//! Brain speaks a binary wire protocol (CBOR over TCP). This crate translates
//! the hosted `/v1/*` HTTP/JSON API to that wire protocol via the Brain SDK. It
//! is both:
//!
//! - a **binary** ([`run`]) — the self-hostable front door; point any HTTP
//!   client at it and the self-host experience matches the cloud, only the base
//!   URL differs; and
//! - a **library** — the Arc cloud gateway embeds [`app::router`] and injects
//!   its own [`CredentialResolver`] + [`MeteringSink`] (via
//!   [`EdgeState::with_ports`]) to add API-key auth and usage metering without
//!   forking the wire↔JSON translation layer.
//!
//! The two deployments share one engine: the same DTOs, the same mapping, the
//! same per-credential connection pool. They differ only in the two injected
//! ports.

pub mod app;
pub mod config;
pub mod crypto;
pub mod dto;
pub mod error;
pub mod pool;
pub mod port;
pub mod state;
pub mod wire_proxy;

pub use config::EdgeConfig;
pub use error::ApiError;
pub use pool::{BrainPool, BrainPoolConfig};
pub use port::{
    BearerResolver, CredentialResolver, MeterEvent, MeteringSink, NoopMeter, Outcome,
    ResolvedCredential, bearer_token,
};
pub use state::EdgeState;
pub use wire_proxy::{RateLimitConfig, WireProxyConfig};

/// The per-request effective-identity selector the gateway sets when running a
/// [`BrainPool::shared`] service pool: build one with `ActAs { namespace,
/// space_id }` (or the SDK request builders' `.act_as(..)`) to run each op as a
/// resolved tenant on behalf of the trusted service principal. Re-exported so a
/// gateway consuming `brain_edge` need not also name `brain_db_sdk` directly.
pub use brain_db_sdk::wire::types::ActAs;

use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderName, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::sensitive_headers::SetSensitiveRequestHeadersLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;
use tracing::{error, info};

/// The header carrying the correlation id, in and out.
const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

/// Turn a panicking handler into a `500` instead of a dropped connection.
///
/// Without this a panic aborts the task and hyper closes the socket with no
/// response: the caller sees a connection reset with no status, no body, and
/// nothing to quote in a bug report. The panic is logged, and the caller gets
/// the same JSON envelope as every other failure — carrying the request id, so
/// the two ends can be joined up afterwards.
fn panic_to_response(err: &Box<dyn std::any::Any + Send + 'static>) -> axum::response::Response {
    // `panic!` and `unwrap` produce a `&str` or a `String`; anything else is
    // opaque and there is nothing useful to extract.
    let detail: &str = err.downcast_ref::<&str>().copied().unwrap_or_else(|| {
        err.downcast_ref::<String>()
            .map_or("non-string panic payload", String::as_str)
    });
    ApiError::internal(format!("handler panicked: {detail}")).into_response()
}

/// Apply the transport-level protections every deployment needs.
///
/// Panic recovery, correlation ids, header redaction, request tracing, a
/// request timeout, and a request-body cap. [`run`] applies these;
/// [`app::router`] deliberately does not, because a host application merges
/// that router under its own middleware stack.
///
/// This exists so an embedder does not have to rediscover the list. The body
/// cap is the one most easily missed: [`EdgeConfig::max_body_bytes`] is a
/// visible, validated knob, so a gateway author can reasonably assume the
/// router already honours it. It does not — only this function does.
///
/// # Arguments
///
/// * `request_timeout` — a stalled Brain must not hang a request forever; past
///   this the request is cut with an explicit `408`.
/// * `max_body_bytes` — an oversized body must not be buffered into memory.
pub fn harden(router: Router, request_timeout: Duration, max_body_bytes: usize) -> Router {
    router
        // A panic must not take the connection down silently. Innermost, so it
        // wraps the handler rather than the middleware around it.
        .layer(CatchPanicLayer::custom(|e| panic_to_response(&e)))
        // Redact credential headers so they never reach the log sink, then trace
        // every request. Order matters: mark-sensitive is outermost so it wraps
        // the trace layer's view of the headers.
        .layer(TraceLayer::new_for_http())
        .layer(SetSensitiveRequestHeadersLayer::new([
            header::AUTHORIZATION,
            HeaderName::from_static("x-api-key"),
        ]))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            request_timeout,
        ))
        // The cap, twice over, and the order is the point. The tower-http
        // layer is the backstop: it is the only one of the two that can catch
        // a chunked body, whose length is not known until it has been read.
        .layer(RequestBodyLimitLayer::new(max_body_bytes))
        // Outside it, the `Content-Length` case — every request the SDKs make
        // — refused only after the body has been drained, so the 413 survives
        // the connection close. See the module docs for why that is needed.
        .layer(axum::middleware::from_fn_with_state(
            max_body_bytes,
            app::middleware::body_limit::enforce,
        ))
        // Correlation, outermost so every response carries it — including the
        // ones the layers above generate without reaching a handler (413 from
        // the body cap, 408 from the timeout). A caller-supplied
        // `x-request-id` is kept, so an id assigned upstream survives the hop.
        .layer(PropagateRequestIdLayer::new(REQUEST_ID_HEADER))
        .layer(SetRequestIdLayer::new(REQUEST_ID_HEADER, MakeRequestUuid))
}

/// Run the self-host edge: build default state (bearer passthrough, no
/// metering), bind the listener, and serve until shutdown.
///
/// The hosted gateway does not call this — it builds its own [`EdgeState`] with
/// [`EdgeState::with_ports`], merges [`app::router`] under its middleware, and
/// serves itself.
///
/// # Errors
/// Fails if the listen address can't be bound or the server exits with an error.
pub async fn run(config: EdgeConfig) -> Result<(), Box<dyn std::error::Error>> {
    let listen_addr = config.listen_addr;
    let request_timeout = Duration::from_secs(config.request_timeout_secs);
    let max_body_bytes = config.max_body_bytes;
    // Wire-proxy settings, captured before `config` is moved into the state.
    let wire_listen_addr = config.wire_listen_addr;
    let brain_addr = config.brain_addr;
    let wire_rate = RateLimitConfig {
        capacity: config.wire_rate_capacity,
        refill_per_sec: config.wire_rate_refill_per_sec,
    };
    // BYOK: install the key ring when one is configured. Absent, the edge runs
    // exactly as before and refuses to store provider keys at all — it never
    // falls back to holding them in the clear.
    //
    // The self-host default store is process-local, so a restart drops every
    // registered key. That is said out loud at startup rather than discovered
    // when writes begin failing; a durable store is injected by the gateway.
    let keyring = crypto::keyring_from_env().map_err(|e| format!("llm kek: {e}"))?;
    let state = EdgeState::new(config);
    let state = if let Some(ring) = keyring {
        {
            tracing::info!(
                kek_id = ring.current_kek_id(),
                "BYOK enabled: provider keys sealed at the edge",
            );
            state.with_llm_credentials(byok_store()?, std::sync::Arc::new(ring))
        }
    } else {
        tracing::info!(
            "BYOK disabled: set {} to enable tenant provider keys",
            crypto::KEK_SPEC_ENV,
        );
        state
    };

    // The transparent wire-protocol proxy is a second listener alongside HTTP.
    // Opt-in (only when an address is configured); it reuses the state's metering
    // sink so wire-path ops meter through the same sink as the HTTP data plane.
    if let Some(wire_addr) = wire_listen_addr {
        let meter = state.meter();
        let wcfg = WireProxyConfig {
            wire_listen_addr: wire_addr,
            brain_addr,
            rate: wire_rate,
        };
        tokio::spawn(async move {
            if let Err(e) = wire_proxy::serve(wcfg, meter).await {
                error!(error = %e, "wire proxy listener exited");
            }
        });
    }

    // Drop credential pools that have gone idle past their TTL. Runs off the
    // request path so a long-lived self-host process serving many credentials
    // doesn't hold live sockets forever between bursts.
    {
        let sweeper_state = state.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(60));
            ticker.tick().await; // consume the immediate first tick
            loop {
                ticker.tick().await;
                sweeper_state.pool().sweep_idle();
            }
        });
    }

    // The library router is the data plane (`/v1/*`) only, so a host app can
    // merge it without colliding on health routes; the binary owns its health.
    // `/health/live` is a static liveness signal (the process is up);
    // `/health/ready` actually probes Brain so an orchestrator doesn't route
    // traffic here before the database it proxies to is reachable.
    let health = axum::Router::new()
        .route("/health/live", get(|| async { StatusCode::OK }))
        .route("/health/ready", get(readiness))
        .with_state(state.clone());

    // One binary, two deployable planes. Default `Both` so a self-host
    // operator never has to think about it; production splits them to keep
    // tenants' provider keys out of the process carrying memory traffic.
    let plane = app::Plane::from_env().map_err(|e| format!("{}: {e}", app::PLANE_ENV))?;
    tracing::info!(?plane, "serving plane");

    let app = harden(
        app::router_for(state, plane).merge(health),
        request_timeout,
        max_body_bytes,
    );

    let listener = tokio::net::TcpListener::bind(listen_addr).await?;
    info!(addr = %listen_addr, "listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    info!("shutdown complete");
    Ok(())
}

/// Readiness: 200 only when a TCP socket to Brain opens within the probe budget,
/// else 503. Lets an orchestrator gate traffic on the downstream being live.
async fn readiness(State(state): State<EdgeState>) -> StatusCode {
    if state.pool().probe(Duration::from_secs(2)).await {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

/// Resolve when the process receives SIGINT (Ctrl-C) or SIGTERM (the signal an
/// orchestrator sends on rollout/scale-down), so in-flight requests drain
/// instead of being severed mid-response.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => info!("SIGINT received, shutting down"),
        () = terminate => info!("SIGTERM received, shutting down"),
    }
}

/// Env var pointing at a durable BYOK credential file.
pub const CREDENTIAL_STORE_ENV: &str = "BRAIN_EDGE_LLM_CREDENTIAL_FILE";

/// Pick the credential store for this process.
///
/// A path gives a durable, file-backed store; without one the store is
/// in-memory and every registered key is lost on restart — which then fails
/// the whole write path until each tenant re-registers.
///
/// The in-memory case stays available because requiring a path to try BYOK
/// would be obnoxious, but it warns loudly rather than letting an operator
/// discover the property during an incident.
fn byok_store() -> Result<std::sync::Arc<dyn port::LlmCredentialStore>, Box<dyn std::error::Error>>
{
    match std::env::var(CREDENTIAL_STORE_ENV) {
        Ok(path) if !path.trim().is_empty() => {
            let store = port::FileLlmCredentialStore::open(path.trim())
                .map_err(|e| format!("{CREDENTIAL_STORE_ENV}: {e}"))?;
            tracing::info!(path = %path.trim(), "BYOK credential store: file-backed");
            Ok(std::sync::Arc::new(store))
        }
        _ => {
            tracing::warn!(
                "BYOK credential store is IN-MEMORY: registered provider keys are lost on \
                 restart, and writes then fail until every tenant re-registers. Set {} to a \
                 file path, or inject a durable LlmCredentialStore.",
                CREDENTIAL_STORE_ENV,
            );
            Ok(std::sync::Arc::new(
                port::InMemoryLlmCredentialStore::default(),
            ))
        }
    }
}
