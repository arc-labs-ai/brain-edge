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

/// Longest credential used verbatim as a map key.
///
/// The key is sniffed from the customer's AUTH frame, which the proxy does not
/// and cannot validate — Brain does that, later. A frame payload may be up to
/// 16 MiB, so without this a peer could make every key enormous. Anything
/// longer is keyed by its hash instead: a credential this long is not a real
/// one, so collisions among them are not a fairness concern.
const MAX_KEY_BYTES: usize = 512;

/// Prune fully-refilled buckets once the map is at least this large.
///
/// Chosen well above any plausible tenant count so ordinary operation never
/// pays for the scan.
const PRUNE_ABOVE: usize = 4_096;

/// Hard ceiling on tracked buckets. Reached only under an attack that keeps
/// every bucket actively spending, which pruning alone cannot reclaim.
const MAX_BUCKETS: usize = 65_536;

/// A registry of per-credential token buckets. Lookups take a short, non-async
/// lock and never hold it across an `.await`, so a `std::sync::Mutex` is correct
/// here.
pub struct RateLimiters {
    config: RateLimitConfig,
    buckets: Mutex<HashMap<String, Bucket>>,
    /// Prune fully-refilled buckets once the map is at least this large.
    prune_above: usize,
    /// Hard ceiling on tracked buckets.
    ///
    /// A field rather than a constant so the tests can drive the ceiling with
    /// a handful of inserts. Exercising a 65 536-entry limit directly took 47
    /// seconds and starved the other tests of CPU, which made a neighbouring
    /// timing assertion fail — a test that slow is one nobody runs.
    max_buckets: usize,
}

impl std::fmt::Debug for RateLimiters {
    /// Reports the configuration and how many buckets are live — never the
    /// credentials, which are the map's keys.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let live = self.buckets.lock().ok().map(|b| b.len());
        f.debug_struct("RateLimiters")
            .field("config", &self.config)
            .field("live_buckets", &live)
            .finish_non_exhaustive()
    }
}

impl RateLimiters {
    /// Build an empty registry with the given config.
    #[must_use]
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            buckets: Mutex::new(HashMap::new()),
            prune_above: PRUNE_ABOVE,
            max_buckets: MAX_BUCKETS,
        }
    }

    /// Same, with the memory bounds overridden so a test can reach them.
    #[cfg(test)]
    fn with_bounds(config: RateLimitConfig, prune_above: usize, max_buckets: usize) -> Self {
        Self {
            config,
            buckets: Mutex::new(HashMap::new()),
            prune_above,
            max_buckets,
        }
    }

    /// Admit one op for `key` (the sniffed credential), spending a token. Returns
    /// `true` if a token was available, `false` if the bucket is empty (the op
    /// should be rejected with a wire ERROR). Always `true` when limiting is off.
    ///
    /// # Panics
    ///
    /// If the bucket mutex is poisoned — which means another thread panicked
    /// while holding it, and the limiter's accounting is no longer trustworthy.
    pub fn allow(&self, key: &str) -> bool {
        if self.config.disabled() {
            return true;
        }
        let cap = f64::from(self.config.capacity);
        let refill = f64::from(self.config.refill_per_sec);
        let now = Instant::now();
        let key = Self::bucket_key(key);

        let mut buckets = self.buckets.lock().expect("rate-limit mutex poisoned");

        // The map is keyed by an *unvalidated* credential: the proxy sniffs it
        // from the customer's AUTH frame and forwards that frame for Brain to
        // accept or reject. A peer whose credential Brain will refuse still
        // reaches this line, so without a bound it can insert a bucket per
        // connection attempt and grow the map without limit — turning the
        // defence against request floods into a memory-exhaustion vector.
        if buckets.len() >= self.prune_above {
            // A bucket refilled to capacity is indistinguishable from one that
            // does not exist: a fresh bucket starts at exactly `cap`. Dropping
            // those is therefore lossless, not an approximation — no caller can
            // observe the difference.
            buckets.retain(|_, b| {
                let refilled = now
                    .duration_since(b.last)
                    .as_secs_f64()
                    .mul_add(refill, b.tokens)
                    .min(cap);
                refilled < cap
            });
        }
        if buckets.len() >= self.max_buckets && !buckets.contains_key(&key) {
            // Every bucket is actively spending and pruning reclaimed nothing.
            // Shedding a new credential is the right failure here: admitting it
            // means unbounded growth, and the alternative — evicting someone
            // else's bucket — hands an attacker a way to reset any tenant's
            // limit.
            return false;
        }

        let bucket = buckets.entry(key).or_insert(Bucket {
            tokens: cap,
            last: now,
        });
        // Continuous refill since the last touch, capped at the burst ceiling.
        let elapsed = now.duration_since(bucket.last).as_secs_f64();
        bucket.tokens = elapsed.mul_add(refill, bucket.tokens).min(cap);
        bucket.last = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// The map key for a sniffed credential, bounded in length.
    ///
    /// Real credentials are far under [`MAX_KEY_BYTES`] and pass through
    /// unchanged, so buckets stay per-credential exactly as before.
    fn bucket_key(key: &str) -> String {
        use std::hash::{Hash, Hasher};

        if key.len() <= MAX_KEY_BYTES {
            return key.to_string();
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut h);
        format!("oversize-{:016x}", h.finish())
    }

    /// Longest stored key in bytes (test/introspection).
    #[cfg(test)]
    fn longest_key(&self) -> usize {
        self.buckets
            .lock()
            .expect("rate-limit mutex poisoned")
            .keys()
            .map(String::len)
            .max()
            .unwrap_or(0)
    }

    /// Number of distinct credentials with a live bucket (test/introspection).
    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.buckets
            .lock()
            .expect("rate-limit mutex poisoned")
            .len()
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

    // -- memory bounds ----------------------------------------------------
    //
    // The bucket key is an UNVALIDATED credential, sniffed from the customer's
    // AUTH frame. Brain decides whether it is real; the proxy inserts a bucket
    // regardless, and the entry outlives the connection. Without the bounds
    // below, enabling rate limiting — the defence against request floods —
    // opens a memory-exhaustion vector against a peer that never authenticates.

    #[test]
    fn an_oversize_credential_does_not_become_an_oversize_key() {
        // A frame payload may be 16 MiB, so an unvalidated credential can be
        // too. Storing it verbatim as a map key is what makes that expensive.
        let rl = RateLimiters::new(RateLimitConfig {
            capacity: 1,
            refill_per_sec: 0,
        });
        let huge = "z".repeat(1_000_000);
        assert!(rl.allow(&huge));
        assert_eq!(rl.tracked(), 1);
        assert!(
            rl.longest_key() < MAX_KEY_BYTES + 64,
            "an oversize credential must not be stored verbatim"
        );
        // Still a real bucket: the second op is refused, so bounding the key
        // did not disable limiting for this credential.
        assert!(!rl.allow(&huge), "the bounded key still keys a real bucket");
    }

    #[test]
    fn distinct_credentials_cannot_grow_the_map_without_bound() {
        // Each attacker bucket is spent once and then refills to full, at which
        // point it carries no information and is reclaimable. Bounds are small
        // here so the test reaches them in milliseconds.
        let rl = RateLimiters::with_bounds(
            RateLimitConfig {
                capacity: 1,
                refill_per_sec: 1_000_000,
            },
            16,
            1_024,
        );
        for i in 0..2_000 {
            assert!(rl.allow(&format!("attacker-{i}")));
        }
        assert!(
            rl.tracked() <= 16,
            "map grew to {} entries; pruning is not reclaiming",
            rl.tracked()
        );
    }

    #[test]
    fn pruning_a_full_bucket_is_lossless() {
        // The eviction rule is sound only because a bucket refilled to capacity
        // is indistinguishable from one that does not exist. Assert exactly
        // that: spend the whole burst, wait for a full refill, and confirm the
        // burst is available again — whether or not the entry was reclaimed.
        //
        // 100 tokens/s means one token per 10ms, so the spend loop below cannot
        // accidentally refill mid-flight the way a 1_000_000/s rate would.
        let rl = RateLimiters::with_bounds(
            RateLimitConfig {
                capacity: 2,
                refill_per_sec: 100,
            },
            1,
            1_024,
        );
        assert!(rl.allow("k"));
        assert!(rl.allow("k"));
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(rl.allow("k"), "first token back after a full refill");
        assert!(
            rl.allow("k"),
            "and the second — the whole burst, as if fresh"
        );
    }

    #[test]
    fn a_spending_flood_is_shed_rather_than_grown() {
        // No refill, so no bucket ever becomes prunable — the case pruning
        // cannot handle. The hard ceiling is what bounds it.
        let rl = RateLimiters::with_bounds(
            RateLimitConfig {
                capacity: 4,
                refill_per_sec: 0,
            },
            1_000_000, // pruning effectively off; isolate the ceiling
            32,
        );
        for i in 0..500 {
            rl.allow(&format!("attacker-{i}"));
        }
        assert_eq!(rl.tracked(), 32, "the ceiling is what stops the growth");
    }

    #[test]
    fn refills_over_time() {
        // Capacity 1 at 100 tokens/s: one token per 10ms.
        //
        // This used to use 1000/s, so the refusal below only held if the two
        // calls landed within a millisecond of each other. Under a loaded test
        // runner they did not, and it failed intermittently — a flake that
        // looks like a rate-limiter bug. 10ms is far longer than two calls can
        // take, and 50ms is comfortably more than one refill.
        let rl = RateLimiters::new(RateLimitConfig {
            capacity: 1,
            refill_per_sec: 100,
        });
        assert!(rl.allow("k"));
        assert!(!rl.allow("k"), "token spent, and 10ms have not passed");
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(rl.allow("k"), "token refilled after the sleep");
    }
}
