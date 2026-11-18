//! Metrics for every round trip from the edge to Brain.

use super::counter::Counter;
use super::histogram::{ENGINE_CALL_BUCKETS_SECONDS, EdgeHistogram, HistogramSnapshot};

/// Engine call kinds in append-only label order.
#[derive(Clone, Copy, Debug)]
pub enum EngineCallKind {
    /// A cold pool handshake.
    Connect,
    /// Replacement after Brain closed a member.
    Reconnect,
    /// A readiness or operational probe.
    Probe,
}

/// Engine call kind labels.
pub const ENGINE_CALL_KIND_LABELS: &[&str] = &["connect", "reconnect", "probe"];

impl EngineCallKind {
    /// Return the label-axis index.
    #[must_use]
    pub const fn idx(self) -> usize {
        match self {
            Self::Connect => 0,
            Self::Reconnect => 1,
            Self::Probe => 2,
        }
    }
}

/// Engine call outcomes in append-only label order.
#[derive(Clone, Copy, Debug)]
pub enum EngineCallOutcome {
    /// The round trip completed successfully.
    Ok,
    /// Brain refused or otherwise failed the round trip.
    Failed,
    /// The configured deadline elapsed.
    TimedOut,
}

/// Engine call outcome labels.
pub const ENGINE_CALL_OUTCOME_LABELS: &[&str] = &["ok", "failed", "timed_out"];

impl EngineCallOutcome {
    /// Return the label-axis index.
    #[must_use]
    pub const fn idx(self) -> usize {
        match self {
            Self::Ok => 0,
            Self::Failed => 1,
            Self::TimedOut => 2,
        }
    }
}

/// Edge-to-Brain metric family.
#[derive(Debug)]
pub struct EngineMetrics {
    calls: Vec<Counter>,
    durations: Vec<EdgeHistogram>,
}

impl EngineMetrics {
    /// Construct zeroed engine metrics.
    #[must_use]
    pub fn new() -> Self {
        Self {
            calls: (0..ENGINE_CALL_KIND_LABELS.len() * ENGINE_CALL_OUTCOME_LABELS.len())
                .map(|_| Counter::new())
                .collect(),
            durations: (0..ENGINE_CALL_KIND_LABELS.len())
                .map(|_| EdgeHistogram::new(ENGINE_CALL_BUCKETS_SECONDS))
                .collect(),
        }
    }

    /// Access a kind/outcome counter.
    #[must_use]
    pub fn call(&self, kind: usize, outcome: usize) -> &Counter {
        &self.calls[kind * ENGINE_CALL_OUTCOME_LABELS.len() + outcome]
    }

    /// Access one kind's duration histogram.
    #[must_use]
    pub fn duration(&self, kind: usize) -> &EdgeHistogram {
        &self.durations[kind]
    }

    /// Copy engine metrics into plain data.
    #[must_use]
    pub fn snapshot(&self) -> EngineMetricsSnapshot {
        EngineMetricsSnapshot {
            calls: self.calls.iter().map(Counter::get).collect(),
            durations: self.durations.iter().map(EdgeHistogram::snapshot).collect(),
        }
    }
}

impl Default for EngineMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Plain-data engine snapshot.
#[derive(Clone, Debug)]
pub struct EngineMetricsSnapshot {
    /// Dense kind-by-outcome totals.
    pub calls: Vec<u64>,
    /// One duration histogram per call kind.
    pub durations: Vec<HistogramSnapshot>,
}

#[cfg(test)]
mod tests {
    use super::{EngineCallKind, EngineCallOutcome, EngineMetrics};

    #[test]
    fn snapshot_reflects_published_values() {
        let metrics = EngineMetrics::new();
        metrics
            .call(EngineCallKind::Probe.idx(), EngineCallOutcome::Ok.idx())
            .inc();
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.calls[6], 1);
    }
}
