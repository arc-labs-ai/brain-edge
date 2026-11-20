//! Process-wide edge observability for HTTP, pool, cache, and Brain latency.
//!
//! The registry is process-global because the pool's RCU cold path and the HTTP middleware
//! publish into the same families without sharing an `EdgeState` handle. This also mirrors
//! Brain's process-wide degradation family; construction is only a few hundred atomics.

// One module per metric family, plus the primitives they are built from and the
// exposition that renders them. All public: a family's label consts and its
// exposition helpers are part of how an operator reads the output, and the
// re-exports below are a convenience over them rather than a smaller surface.
pub mod counter;
pub mod engine;
pub mod format;
pub mod gauge;
pub mod histogram;
pub mod pool;
pub mod request;

pub use counter::Counter;
pub use engine::{
    ENGINE_CALL_KIND_LABELS, ENGINE_CALL_OUTCOME_LABELS, EngineCallKind, EngineCallOutcome,
    EngineMetrics, EngineMetricsSnapshot,
};
pub use gauge::Gauge;
pub use histogram::{BucketSnapshot, EdgeHistogram, HistogramSnapshot};
pub use pool::{AcquireOutcome, EvictionReason, PoolMetrics, PoolMetricsSnapshot};
pub use request::{RequestMetrics, RequestMetricsSnapshot, StatusClass};

use std::sync::LazyLock;

/// All edge metric families.
#[derive(Debug)]
pub struct EdgeMetrics {
    /// HTTP request metrics.
    pub request: RequestMetrics,
    /// Pool and cache metrics.
    pub pool: PoolMetrics,
    /// Brain round-trip metrics.
    pub engine: EngineMetrics,
}

impl EdgeMetrics {
    /// Construct zeroed edge metrics.
    #[must_use]
    pub fn new() -> Self {
        Self {
            request: RequestMetrics::new(),
            pool: PoolMetrics::new(),
            engine: EngineMetrics::new(),
        }
    }

    /// Copy all families into plain data.
    #[must_use]
    pub fn snapshot(&self) -> EdgeMetricsSnapshot {
        EdgeMetricsSnapshot {
            request: self.request.snapshot(),
            pool: self.pool.snapshot(),
            engine: self.engine.snapshot(),
        }
    }
}

impl Default for EdgeMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Plain-data snapshot suitable for exposition.
#[derive(Clone, Debug)]
pub struct EdgeMetricsSnapshot {
    /// HTTP request snapshot.
    pub request: RequestMetricsSnapshot,
    /// Pool/cache snapshot.
    pub pool: PoolMetricsSnapshot,
    /// Engine snapshot.
    pub engine: EngineMetricsSnapshot,
}

static GLOBAL: LazyLock<EdgeMetrics> = LazyLock::new(EdgeMetrics::new);

/// Return the process-global registry shared by middleware and pool cold paths.
#[must_use]
pub fn global() -> &'static EdgeMetrics {
    &GLOBAL
}

/// Build the opt-in `/metrics` router so embedders can choose a non-conflicting operational path.
///
/// It is separate from `app::router_for` because an embedding host commonly serves its own
/// `/metrics`, and merging duplicate routes panics; the host must opt in and choose the path.
#[must_use]
pub fn metrics_router() -> axum::Router {
    axum::Router::new().route(
        "/metrics",
        axum::routing::get(|| async {
            (
                [(
                    axum::http::header::CONTENT_TYPE,
                    "text/plain; version=0.0.4; charset=utf-8",
                )],
                format::format(&global().snapshot()),
            )
        }),
    )
}
