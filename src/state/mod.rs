//! Shared state: the [`BrainPool`] connection cache to Brain plus the injected
//! [`CredentialResolver`] + [`MeteringSink`] ports.
//!
//! The resolver turns an incoming request into a Brain wire credential; the
//! [`BrainPool`] reuses one handshake per credential so we don't re-connect on
//! every request; the metering sink observes each op's outcome. Self-host uses
//! the bearer/no-op defaults ([`EdgeState::new`]); the gateway injects its own
//! ports ([`EdgeState::with_ports`]).

use std::sync::Arc;
use std::time::Duration;

use axum::http::HeaderMap;
use brain_db_sdk::BrainClient;

use crate::config::EdgeConfig;
use crate::error::ApiError;
use crate::pool::{BrainPool, BrainPoolConfig};
use crate::port::{
    BearerResolver, CredentialResolver, MeterEvent, MeteringSink, NoopMeter, Outcome,
    ResolvedCredential,
};

/// Cheap-to-clone application state.
#[derive(Clone)]
pub struct EdgeState {
    inner: Arc<Inner>,
}

struct Inner {
    pool: BrainPool,
    resolver: Arc<dyn CredentialResolver>,
    meter: Arc<dyn MeteringSink>,
}

impl EdgeState {
    /// The self-host default: bearer passthrough, no metering.
    #[must_use]
    pub fn new(config: EdgeConfig) -> Self {
        Self::with_ports(config, Arc::new(BearerResolver), Arc::new(NoopMeter))
    }

    /// Build state with an injected resolver + metering sink (the hosted gateway
    /// path). The gateway passes an API-key resolver and its analytics sink.
    #[must_use]
    pub fn with_ports(
        config: EdgeConfig,
        resolver: Arc<dyn CredentialResolver>,
        meter: Arc<dyn MeteringSink>,
    ) -> Self {
        let pool = BrainPool::new(BrainPoolConfig {
            brain_addr: config.brain_addr,
            pool_size: config.pool_size,
            max_credentials: config.max_credentials,
            idle_ttl: Duration::from_secs(config.idle_ttl_secs),
        });
        Self {
            inner: Arc::new(Inner {
                pool,
                resolver,
                meter,
            }),
        }
    }

    /// Resolve a request to a Brain credential via the injected resolver.
    ///
    /// # Errors
    /// Propagates the resolver's `401`/`403` when no valid credential is present.
    pub async fn resolve(&self, headers: &HeaderMap) -> Result<ResolvedCredential, ApiError> {
        self.inner.resolver.resolve(headers).await
    }

    /// Record one op's outcome via the injected sink (fire-and-forget).
    pub fn record(&self, op: &str, ident: &ResolvedCredential, outcome: Outcome) {
        self.inner.meter.record(&MeterEvent {
            tenant: ident.tenant.as_deref(),
            op,
            outcome,
        });
    }

    /// A Brain client authenticated as `credential`, from the [`BrainPool`]'s
    /// cached (or freshly opened) per-credential pool.
    ///
    /// # Errors
    /// Returns an engine error if the pool fails to open a connection.
    pub async fn client_for(&self, credential: &str) -> Result<Arc<BrainClient>, ApiError> {
        self.inner
            .pool
            .client_for(credential)
            .await
            .map_err(|e| ApiError::from_brain(&e))
    }

    /// The shared Brain connection pool (for the background idle sweeper and
    /// readiness probing).
    #[must_use]
    pub fn pool(&self) -> &BrainPool {
        &self.inner.pool
    }

    /// The injected metering sink, shared. The wire proxy reuses it so wire-path
    /// ops meter through the same sink as the HTTP data plane.
    #[must_use]
    pub fn meter(&self) -> Arc<dyn MeteringSink> {
        Arc::clone(&self.inner.meter)
    }
}
