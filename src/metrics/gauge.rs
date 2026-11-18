//! Saturating atomic gauges for current process occupancy.

use std::sync::atomic::{AtomicU64, Ordering};

/// A non-negative gauge. Relaxed ordering keeps metric publication off the critical path.
#[derive(Debug, Default)]
pub struct Gauge {
    value: AtomicU64,
}

impl Gauge {
    /// Construct a gauge at zero.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            value: AtomicU64::new(0),
        }
    }

    /// Increment the gauge.
    pub fn inc(&self) {
        self.value.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement the gauge, stopping at zero so an unbalanced path cannot poison dashboards.
    pub fn dec(&self) {
        let mut current = self.value.load(Ordering::Relaxed);
        loop {
            let next = current.saturating_sub(1);
            match self.value.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }

    /// Replace the gauge value.
    pub fn set(&self, value: u64) {
        self.value.store(value, Ordering::Relaxed);
    }

    /// Read the current value.
    #[must_use]
    pub fn get(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::Gauge;

    #[test]
    fn gauge_operations() {
        let gauge = Gauge::new();
        gauge.inc();
        gauge.inc();
        gauge.dec();
        gauge.set(7);
        assert_eq!(gauge.get(), 7);
    }

    #[test]
    fn decrement_saturates_at_zero() {
        let gauge = Gauge::new();
        gauge.dec();
        assert_eq!(gauge.get(), 0);
    }
}
