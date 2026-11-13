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
    RecallSignals, ResolvedCredential,
};

/// Cheap-to-clone application state.
#[derive(Clone)]
pub struct EdgeState {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for EdgeState {
    /// The ports are trait objects and the pool holds live connections, so this
    /// prints the shape rather than the contents.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EdgeState")
            .field("pool", &self.inner.pool)
            .finish_non_exhaustive()
    }
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
    pub fn record(
        &self,
        op: &str,
        ident: &ResolvedCredential,
        outcome: Outcome,
        latency: Duration,
    ) {
        self.record_recall(op, ident, outcome, Some(latency), None, None);
    }

    /// Ask the injected sink whether this op may run.
    ///
    /// Handlers call this between resolving the caller and reaching Brain, so
    /// a refusal costs nothing but the resolve.
    ///
    /// # Errors
    /// Whatever the sink decides to refuse with — a plan ceiling, typically.
    pub async fn admit(&self, op: &str, ident: &ResolvedCredential) -> Result<(), ApiError> {
        self.inner.meter.admit(op, ident).await
    }

    /// Whether the injected sink wants RECALL's pipeline trace on every call.
    #[must_use]
    pub fn wants_recall_trace(&self) -> bool {
        self.inner.meter.wants_recall_trace()
    }

    /// Record an op that never reached Brain, so has no round trip to report.
    pub fn record_untimed(&self, op: &str, ident: &ResolvedCredential, outcome: Outcome) {
        self.record_recall(op, ident, outcome, None, None, None);
    }

    /// Record one op, carrying RECALL's answer-quality signals.
    ///
    /// Split from [`Self::record`] so the other fifty-odd call sites do not
    /// each pass a `None` that means nothing for them.
    pub fn record_recall(
        &self,
        op: &str,
        ident: &ResolvedCredential,
        outcome: Outcome,
        latency: Option<Duration>,
        recall: Option<RecallSignals>,
        trace: Option<&str>,
    ) {
        self.inner.meter.record(&MeterEvent {
            tenant: ident.tenant.as_deref(),
            act_as: ident.act_as.as_ref(),
            op,
            outcome,
            latency,
            recall,
            trace,
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
