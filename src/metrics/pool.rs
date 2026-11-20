//! Connection-pool and credential-cache metric families.

use super::counter::Counter;
use super::gauge::Gauge;
use super::histogram::{EdgeHistogram, HistogramSnapshot, REQUEST_DURATION_BUCKETS_SECONDS};

/// Acquisition outcomes in append-only label order.
#[derive(Clone, Copy, Debug)]
pub enum AcquireOutcome {
    /// Existing initialized pool served the request.
    Cached,
    /// This task built the pool.
    Built,
    /// Pool acquisition failed.
    Failed,
}

/// Acquisition outcome labels.
pub const ACQUIRE_OUTCOME_LABELS: &[&str] = &["cached", "built", "failed"];

impl AcquireOutcome {
    /// Return the label-axis index.
    #[must_use]
    pub const fn idx(self) -> usize {
        match self {
            Self::Cached => 0,
            Self::Built => 1,
            Self::Failed => 2,
        }
    }
}

/// Cache eviction reasons in append-only label order.
#[derive(Clone, Copy, Debug)]
pub enum EvictionReason {
    /// The configured credential cap evicted an entry.
    Capacity,
    /// Idle maintenance evicted an entry.
    Idle,
}

/// Eviction reason labels.
pub const EVICTION_REASON_LABELS: &[&str] = &["capacity", "idle"];

impl EvictionReason {
    /// Return the label-axis index.
    #[must_use]
    pub const fn idx(self) -> usize {
        match self {
            Self::Capacity => 0,
            Self::Idle => 1,
        }
    }
}

/// Pool and credential-cache metric family.
#[derive(Debug)]
pub struct PoolMetrics {
    acquisitions: Vec<Counter>,
    acquire_duration: EdgeHistogram,
    init_collapsed: Counter,
    cache_size: Gauge,
    evictions: Vec<Counter>,
}

impl PoolMetrics {
    /// Construct zeroed pool metrics.
    #[must_use]
    pub fn new() -> Self {
        Self {
            acquisitions: (0..ACQUIRE_OUTCOME_LABELS.len())
                .map(|_| Counter::new())
                .collect(),
            acquire_duration: EdgeHistogram::new(REQUEST_DURATION_BUCKETS_SECONDS),
            init_collapsed: Counter::new(),
            cache_size: Gauge::new(),
            evictions: (0..EVICTION_REASON_LABELS.len())
                .map(|_| Counter::new())
                .collect(),
        }
    }

    /// Access an acquisition outcome counter.
    #[must_use]
    pub fn acquisition(&self, outcome: usize) -> &Counter {
        &self.acquisitions[outcome]
    }
    /// Access whole-acquisition latency.
    #[must_use]
    pub fn acquire_duration(&self) -> &EdgeHistogram {
        &self.acquire_duration
    }
    /// Access the collapsed initializer counter.
    #[must_use]
    pub fn init_collapsed(&self) -> &Counter {
        &self.init_collapsed
    }
    /// Access the published cache size gauge.
    #[must_use]
    pub fn cache_size(&self) -> &Gauge {
        &self.cache_size
    }
    /// Access an eviction counter.
    #[must_use]
    pub fn eviction(&self, reason: usize) -> &Counter {
        &self.evictions[reason]
    }

    /// Copy pool metrics into plain data.
    #[must_use]
    pub fn snapshot(&self) -> PoolMetricsSnapshot {
        PoolMetricsSnapshot {
            acquisitions: self.acquisitions.iter().map(Counter::get).collect(),
            acquire_duration: self.acquire_duration.snapshot(),
            init_collapsed: self.init_collapsed.get(),
            cache_size: self.cache_size.get(),
            evictions: self.evictions.iter().map(Counter::get).collect(),
        }
    }
}

impl Default for PoolMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Plain-data pool snapshot.
#[derive(Clone, Debug)]
pub struct PoolMetricsSnapshot {
    /// Acquisition totals by outcome.
    pub acquisitions: Vec<u64>,
    /// Whole-acquisition latency histogram.
    pub acquire_duration: HistogramSnapshot,
    /// Requests that waited on another initializer.
    pub init_collapsed: u64,
    /// Last published distinct-credential count.
    pub cache_size: u64,
    /// Eviction totals by reason.
    pub evictions: Vec<u64>,
}

#[cfg(test)]
mod tests {
    use super::{AcquireOutcome, PoolMetrics};

    #[test]
    fn snapshot_reflects_published_values() {
        let metrics = PoolMetrics::new();
        metrics.acquisition(AcquireOutcome::Built.idx()).inc();
        metrics.cache_size().set(3);
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.acquisitions[1], 1);
        assert_eq!(snapshot.cache_size, 3);
    }
}
