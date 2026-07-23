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
pub mod dto;
pub mod error;
pub mod pool;
pub mod port;
pub mod state;

pub use config::EdgeConfig;
pub use error::ApiError;
pub use pool::{BrainPool, BrainPoolConfig};
pub use port::{
    bearer_token, BearerResolver, CredentialResolver, MeterEvent, MeteringSink, NoopMeter, Outcome,
    ResolvedCredential,
};
pub use state::EdgeState;

/// The per-request effective-identity selector the gateway sets when running a
/// [`BrainPool::shared`] service pool: build one with `ActAs { namespace,
/// agent_id }` (or the SDK request builders' `.act_as(..)`) to run each op as a
/// resolved tenant on behalf of the trusted service principal. Re-exported so a
/// gateway consuming `brain_edge` need not also name `brain_db_sdk` directly.
pub use brain_db_sdk::wire::types::ActAs;

use std::time::Duration;

use axum::extract::State;
use axum::http::{header, HeaderName, StatusCode};
use axum::routing::get;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::sensitive_headers::SetSensitiveRequestHeadersLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;
use tracing::info;

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
    let state = EdgeState::new(config);

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

    let app = app::router(state)
        .merge(health)
        // Redact credential headers so they never reach the log sink, then trace
        // every request. Order matters: mark-sensitive is outermost so it wraps
        // the trace layer's view of the headers.
        .layer(TraceLayer::new_for_http())
        .layer(SetSensitiveRequestHeadersLayer::new([
            header::AUTHORIZATION,
            HeaderName::from_static("x-api-key"),
        ]))
        // A stalled Brain must not hang a request forever; cut it at the timeout
        // with an explicit 408.
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            request_timeout,
        ))
        // Cap request bodies so a hostile/oversized payload can't exhaust memory.
        .layer(RequestBodyLimitLayer::new(max_body_bytes));

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
