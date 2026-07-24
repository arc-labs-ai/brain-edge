//! Per-credential token-bucket rate limiting for the wire proxy.
//!
//! One bucket per sniffed credential, shared across every wire connection that
//! authenticates with the same credential. A bucket refills continuously at
//! `refill_per_sec` up to `capacity` (the burst ceiling); each admitted op frame
//! spends one token. `capacity == 0` disables limiting entirely (the default), so
//! a self-host deployment opts in explicitly.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

/// Token-bucket parameters. Deploy-time knobs, uniform with the rest of the edge
/// config (see [`crate::EdgeConfig`]).
#[derive(Clone, Copy, Debug)]
pub struct RateLimitConfig {
    /// Bucket capacity — the maximum burst of ops admitted back-to-back. `0`
    /// disables rate limiting (every op is admitted).
    pub capacity: u32,
    /// Tokens refilled per second (the sustained op rate once the burst is spent).
    pub refill_per_sec: u32,
}

impl RateLimitConfig {
    /// Whether limiting is off (capacity `0`), in which case [`RateLimiters::allow`]
    /// never rejects and never allocates a bucket.
    #[must_use]
    pub fn disabled(&self) -> bool {
        self.capacity == 0
    }
}

/// A single credential's bucket: current token count and the last-refill instant.
struct Bucket {
    tokens: f64,
    last: Instant,
}

/// A registry of per-credential token buckets. Lookups take a short, non-async
/// lock and never hold it across an `.await`, so a `std::sync::Mutex` is correct
/// here.
pub struct RateLimiters {
    config: RateLimitConfig,
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl RateLimiters {
    /// Build an empty registry with the given config.
    #[must_use]
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Admit one op for `key` (the sniffed credential), spending a token. Returns
    /// `true` if a token was available, `false` if the bucket is empty (the op
    /// should be rejected with a wire ERROR). Always `true` when limiting is off.
    pub fn allow(&self, key: &str) -> bool {
        if self.config.disabled() {
            return true;
        }
        let cap = f64::from(self.config.capacity);
        let refill = f64::from(self.config.refill_per_sec);
        let now = Instant::now();

        let mut buckets = self.buckets.lock().expect("rate-limit mutex poisoned");
        let bucket = buckets.entry(key.to_string()).or_insert(Bucket {
            tokens: cap,
            last: now,
        });
        // Continuous refill since the last touch, capped at the burst ceiling.
        let elapsed = now.duration_since(bucket.last).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * refill).min(cap);
        bucket.last = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Number of distinct credentials with a live bucket (test/introspection).
    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.buckets.lock().expect("rate-limit mutex poisoned").len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_always_admits_and_allocates_nothing() {
        let rl = RateLimiters::new(RateLimitConfig {
            capacity: 0,
            refill_per_sec: 0,
        });
        for _ in 0..1000 {
            assert!(rl.allow("k"));
        }
        assert_eq!(rl.tracked(), 0, "disabled never builds a bucket");
    }

    #[test]
    fn spends_the_burst_then_rejects() {
        // Capacity 3, no refill: exactly three ops admitted, the fourth rejected.
        let rl = RateLimiters::new(RateLimitConfig {
            capacity: 3,
            refill_per_sec: 0,
        });
        assert!(rl.allow("k"));
        assert!(rl.allow("k"));
        assert!(rl.allow("k"));
        assert!(!rl.allow("k"), "burst is spent");
    }

    #[test]
    fn buckets_are_per_credential() {
        let rl = RateLimiters::new(RateLimitConfig {
            capacity: 1,
            refill_per_sec: 0,
        });
        assert!(rl.allow("a"));
        assert!(!rl.allow("a"), "a's single token is spent");
        assert!(rl.allow("b"), "b has its own bucket");
        assert_eq!(rl.tracked(), 2);
    }

    #[test]
    fn refills_over_time() {
        // Capacity 1, refill 1000/s: after spending the token, ~2ms restores > 1.
        let rl = RateLimiters::new(RateLimitConfig {
            capacity: 1,
            refill_per_sec: 1000,
        });
        assert!(rl.allow("k"));
        assert!(!rl.allow("k"));
        std::thread::sleep(std::time::Duration::from_millis(3));
        assert!(rl.allow("k"), "token refilled after the sleep");
    }
}
