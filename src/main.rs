//! brain-edge — the self-host binary.
//!
//! Thin wrapper: load config from the environment and run the edge with the
//! self-host defaults (bearer passthrough, no metering). The reusable engine —
//! router, ports, DTOs, pool — lives in the crate library (`brain_edge`), which
//! the Arc cloud gateway embeds with its own auth + metering ports.

use brain_edge::EdgeConfig;
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "brain_edge=info,tower_http=info".into()),
        )
        .init();

    let config = EdgeConfig::from_env().map_err(|e| format!("config: {e}"))?;
    info!(
        listen = %config.listen_addr,
        brain = %config.brain_addr,
        pool_size = config.pool_size,
        "brain-edge starting",
    );
    brain_edge::run(config).await
}
