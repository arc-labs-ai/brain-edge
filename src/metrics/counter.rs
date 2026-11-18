//! Atomic counters for monotonic operational totals.

use std::sync::atomic::{AtomicU64, Ordering};

/// A process-local counter whose relaxed reads are sufficient for eventual metrics snapshots.
#[derive(Debug, Default)]
pub struct Counter {
    value: AtomicU64,
}

impl Counter {
    /// Construct a counter at zero.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            value: AtomicU64::new(0),
        }
    }

    /// Increment the counter once.
    pub fn inc(&self) {
        self.value.fetch_add(1, Ordering::Relaxed);
    }

    /// Add a batch to the counter.
    pub fn add(&self, value: u64) {
        self.value.fetch_add(value, Ordering::Relaxed);
    }

    /// Read the current value.
    #[must_use]
    pub fn get(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use super::Counter;

    #[test]
    fn counter_operations() {
        let counter = Counter::new();
        assert_eq!(counter.get(), 0);
        counter.inc();
        counter.add(41);
        assert_eq!(counter.get(), 42);
    }

    #[test]
    fn counter_is_race_free() {
        let counter = Arc::new(Counter::new());
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let counter = Arc::clone(&counter);
                thread::spawn(move || {
                    for _ in 0..1_000 {
                        counter.inc();
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("counter worker");
        }
        assert_eq!(counter.get(), 8_000);
    }
}
