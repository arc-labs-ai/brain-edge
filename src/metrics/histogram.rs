//! Fixed-bucket latency histograms with cumulative Prometheus snapshots.

use std::sync::atomic::{AtomicU64, Ordering};

/// Bounds cover a cache-warm identity request through the default 30-second request timeout.
pub const REQUEST_DURATION_BUCKETS_SECONDS: &[f64] = &[
    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];

/// Bounds cover fast handshakes and probes through the same 30-second operational ceiling.
pub const ENGINE_CALL_BUCKETS_SECONDS: &[f64] = REQUEST_DURATION_BUCKETS_SECONDS;

/// A fixed-bucket histogram whose observations are cheap relaxed atomic increments.
#[derive(Debug)]
pub struct EdgeHistogram {
    bounds: &'static [f64],
    counts: Vec<AtomicU64>,
    sum_micros: AtomicU64,
    count: AtomicU64,
}

impl EdgeHistogram {
    /// Construct an empty histogram using sorted static bounds.
    #[must_use]
    pub fn new(bounds: &'static [f64]) -> Self {
        let mut counts = Vec::with_capacity(bounds.len() + 1);
        for _ in 0..=bounds.len() {
            counts.push(AtomicU64::new(0));
        }
        Self {
            bounds,
            counts,
            sum_micros: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }

    /// Record one observation; negative and non-finite values are treated as zero.
    pub fn observe(&self, value: f64) {
        let value = if value.is_finite() {
            value.max(0.0)
        } else {
            0.0
        };
        // The value is clamped nonnegative and finite first, so this local scaling conversion is safe.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_precision_loss,
            clippy::cast_sign_loss
        )]
        let scaled = (value * 1_000_000.0).min(u64::MAX as f64) as u64;
        self.sum_micros.fetch_add(scaled, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
        for (index, bound) in self.bounds.iter().enumerate() {
            if value <= *bound {
                self.counts[index].fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        self.counts[self.bounds.len()].fetch_add(1, Ordering::Relaxed);
    }

    /// Return cumulative bucket counts and the total sum/count.
    #[must_use]
    pub fn snapshot(&self) -> HistogramSnapshot {
        let mut buckets = Vec::with_capacity(self.counts.len());
        let mut cumulative_count = 0;
        for (index, count) in self.counts.iter().enumerate() {
            cumulative_count += count.load(Ordering::Relaxed);
            buckets.push(BucketSnapshot {
                le: self.bounds.get(index).copied(),
                cumulative_count,
            });
        }
        HistogramSnapshot {
            buckets,
            // The stored integer is a microsecond-scaled finite sum.
            #[allow(clippy::cast_precision_loss)]
            sum: self.sum_micros.load(Ordering::Relaxed) as f64 / 1_000_000.0,
            count: self.count.load(Ordering::Relaxed),
        }
    }
}

/// Plain-data histogram snapshot for exposition.
#[derive(Clone, Debug)]
pub struct HistogramSnapshot {
    /// Cumulative buckets, including a trailing `+Inf` bucket.
    pub buckets: Vec<BucketSnapshot>,
    /// Sum of observations in seconds.
    pub sum: f64,
    /// Number of observations.
    pub count: u64,
}

/// One cumulative histogram bucket.
#[derive(Clone, Copy, Debug)]
pub struct BucketSnapshot {
    /// Upper bound, or `None` for the trailing `+Inf` bucket.
    pub le: Option<f64>,
    /// Number of observations at or below this bound.
    pub cumulative_count: u64,
}

#[cfg(test)]
mod tests {
    use super::{EdgeHistogram, REQUEST_DURATION_BUCKETS_SECONDS};

    #[test]
    fn buckets_are_cumulative_and_overflow_is_inf() {
        let histogram = EdgeHistogram::new(REQUEST_DURATION_BUCKETS_SECONDS);
        histogram.observe(0.006);
        histogram.observe(100.0);
        histogram.observe(-1.0);
        let snapshot = histogram.snapshot();
        assert_eq!(snapshot.count, 3);
        assert!(
            snapshot
                .buckets
                .windows(2)
                .all(|pair| { pair[0].cumulative_count <= pair[1].cumulative_count })
        );
        assert_eq!(snapshot.buckets.last().and_then(|bucket| bucket.le), None);
        assert_eq!(snapshot.buckets[2].cumulative_count, 2);
    }
}
