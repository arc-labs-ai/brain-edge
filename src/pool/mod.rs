//! `BrainPool` — a bounded, per-credential connection-pool cache to Brain.
//!
//! Brain binds a connection's identity at handshake from the credential it
//! presents, so one shared pool cannot serve many credentials without crossing
//! isolation. `BrainPool` therefore keeps a bounded **per-credential pool
//! cache**: the first call for a credential opens a `pool_size`-wide
//! [`brain_db_sdk::Pool`] authenticated as that credential and reuses it for
//! every later call (pooled, never connect-per-request).
//!
//! The cache is held in an [`ArcSwap`] so the hot path (pool already built) is a
//! lock-free load + map lookup with a relaxed-atomic recency bump — no mutex, no
//! `.await` under a lock. The cold path builds the pool OUTSIDE any critical
//! section and publishes it via copy-on-write [`ArcSwap::rcu`].
//!
//! It is bounded two ways so a long-lived process serving many credentials
//! doesn't accumulate one `Arc<Pool>` (and `pool_size` live TCP connections) per
//! credential forever: a periodic idle sweep ([`BrainPool::sweep_idle`]) drops
//! pools unused past `idle_ttl`, and a hard cap (`max_credentials`) evicts the
//! least-recently-used entry opportunistically on insert as a burst-protection
//! backstop between sweeps.
//!
//! This is the shared wire-engine both brain-edge (self-host) and the Arc cloud
//! gateway use. Budgets and timeouts are deliberately **not** here — they are the
//! caller's concern.
//!
//! ## Two modes
//!
//! - **Per-credential** ([`BrainPool::new`]) — the self-host bearer-passthrough
//!   default described above. Each distinct wire credential gets its own pool, so
//!   Brain binds each to its own key-bound identity at handshake.
//! - **Shared / `act_as`** ([`BrainPool::shared`]) — one pool authenticated once
//!   as a trusted **service principal** (a `can_act_as` key). Every
//!   [`BrainPool::client_for`] hands back a client from that single pool
//!   regardless of the `credential` argument; the gateway then resolves each API
//!   key to a `(namespace, space_id)` and sets `act_as` per request so one
//!   connection pool serves every tenant. This mode is for the gateway only — the
//!   self-host edge doesn't resolve keys to identities, so it stays per-credential.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwap;
use brain_db_sdk::{Auth, BrainClient, BrainError, Pool};
use tokio::sync::OnceCell;
use tracing::{info, warn};

/// Configuration for a [`BrainPool`].
#[derive(Clone, Debug)]
pub struct BrainPoolConfig {
    /// The Brain database listener address every pool connects to.
    pub brain_addr: SocketAddr,
    /// Sockets per credential (the [`brain_db_sdk::Pool`] width).
    pub pool_size: usize,
    /// Hard cap on distinct cached credentials; the least-recently-used entry is
    /// evicted on insert once the cache is full.
    pub max_credentials: usize,
    /// Credentials unused longer than this are dropped by [`BrainPool::sweep_idle`].
    pub idle_ttl: Duration,
}

/// A cached credential pool plus its last-touch time, so recency can be tracked
/// without taking a lock on every hot-path hit — the atomic is bumped in place;
/// only eviction (cold path) needs to read across entries.
struct PoolEntry {
    pool: Arc<Pool>,
    last_used_secs: AtomicU64,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The key of the least-recently-used entry to evict, or `None` if the cache is
/// below `cap`. Pure over `(key, last_used_secs)` recencies so the cap/LRU
/// bookkeeping is testable without constructing a live [`Pool`].
fn lru_victim_key<'a>(
    recencies: impl Iterator<Item = (&'a str, u64)>,
    len: usize,
    cap: usize,
) -> Option<String> {
    if len < cap {
        return None;
    }
    recencies
        .min_by_key(|&(_, last_used)| last_used)
        .map(|(key, _)| key.to_string())
}

/// The keys of every entry whose last-use is strictly before `cutoff`. Pure over
/// `(key, last_used_secs)` recencies so the idle-sweep selection is testable
/// without a live [`Pool`].
fn stale_keys<'a>(recencies: impl Iterator<Item = (&'a str, u64)>, cutoff: u64) -> Vec<String> {
    recencies
        .filter(|&(_, last_used)| last_used < cutoff)
        .map(|(key, _)| key.to_string())
        .collect()
}

/// Which pooling discipline a [`BrainPool`] runs, kept as an internal enum so
/// the two modes stay cleanly separated rather than tangled behind option
/// fields. Both are honored by [`BrainPool::client_for`], [`BrainPool::sweep_idle`],
/// and [`BrainPool::probe`].
enum Mode {
    /// Self-host bearer passthrough: one pool per distinct wire credential, held
    /// in an [`ArcSwap`]-guarded LRU cache. Pools are built lazily on first use.
    PerCredential(ArcSwap<HashMap<String, Arc<PoolEntry>>>),
    /// Gateway shared / `act_as`: ONE pool authenticated as a trusted service
    /// principal (a `can_act_as` key). Every `client_for` returns a client from
    /// this pool regardless of the `credential` argument. Built lazily on first
    /// use so the constructor stays synchronous and infallible, exactly like
    /// [`BrainPool::new`]; the [`OnceCell`] serializes concurrent initializers so
    /// only one handshake set is ever opened.
    Shared {
        service_credential: String,
        pool: OnceCell<Arc<Pool>>,
    },
}

/// A bounded connection-pool cache to Brain — per-credential (self-host) or a
/// single shared service-principal pool with per-request `act_as` (gateway).
///
/// In per-credential mode the cache is keyed by the wire credential string and
/// held in an [`ArcSwap`] so the hot path (pool already built) is a lock-free
/// load + map lookup with no per-request hashing beyond the lookup itself. In
/// shared mode there is one pool for every caller. Pools are created lazily, on
/// first use.
pub struct BrainPool {
    config: BrainPoolConfig,
    mode: Mode,
}

impl std::fmt::Debug for BrainPool {
    /// Reports the mode and the cache ceiling — never a credential, and never
    /// the pooled connections themselves.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrainPool")
            .field(
                "mode",
                &match self.mode {
                    Mode::PerCredential(_) => "per-credential",
                    Mode::Shared { .. } => "shared",
                },
            )
            .field("max_credentials", &self.config.max_credentials)
            .finish_non_exhaustive()
    }
}

impl BrainPool {
    /// Build an empty per-credential pool cache (the self-host bearer default).
    /// Pools are created lazily, on first use per credential. `max_credentials`
    /// is floored at 1 so the cache can always hold at least one entry.
    #[must_use]
    pub fn new(config: BrainPoolConfig) -> Self {
        let config = BrainPoolConfig {
            max_credentials: config.max_credentials.max(1),
            ..config
        };
        Self {
            config,
            mode: Mode::PerCredential(ArcSwap::from_pointee(HashMap::new())),
        }
    }

    /// Build a shared / `act_as` pool authenticated as `service_credential` — a
    /// trusted service principal holding the `can_act_as` grant. Every
    /// [`BrainPool::client_for`] call returns a client from this ONE pool
    /// regardless of the `credential` argument, so a single connection pool can
    /// serve every tenant when the gateway sets `act_as` per request.
    ///
    /// The pool is opened lazily on first use, so this constructor is
    /// synchronous and infallible just like [`BrainPool::new`]; a bad service
    /// credential or an unreachable Brain surfaces from the first `client_for`.
    #[must_use]
    pub fn shared(config: BrainPoolConfig, service_credential: String) -> Self {
        Self {
            config,
            mode: Mode::Shared {
                service_credential,
                pool: OnceCell::new(),
            },
        }
    }

    /// Get (or lazily build) a pooled client.
    ///
    /// In per-credential mode the client is authenticated as `credential`: hot
    /// path is a lock-free load + lookup, bumping recency in place with a relaxed
    /// atomic; cold path builds the [`brain_db_sdk::Pool`] OUTSIDE any lock,
    /// publishes it via [`ArcSwap::rcu`], and LRU-evicts the least-recently-used
    /// entry first if the cache is at `max_credentials`.
    ///
    /// In shared mode `credential` is **ignored**: the returned client always
    /// comes from the single service-principal pool. The gateway then sets
    /// `act_as` per request to select the effective tenant identity.
    ///
    /// # Errors
    /// Returns the underlying [`BrainError`] if a fresh pool fails to connect or
    /// handshake.
    pub async fn client_for(&self, credential: &str) -> Result<Arc<BrainClient>, BrainError> {
        match &self.mode {
            Mode::PerCredential(pools) => self.client_for_credential(pools, credential).await,
            Mode::Shared {
                service_credential,
                pool,
            } => {
                // Ignore `credential`: one shared pool serves every caller. The
                // OnceCell serializes concurrent initializers, so at most one set
                // of handshakes is ever opened even under a cold-start burst.
                let shared = pool
                    .get_or_try_init(|| async {
                        let auth = Auth::Token(service_credential.as_bytes().to_vec());
                        Pool::connect(self.config.brain_addr, self.config.pool_size, auth)
                            .await
                            .map(Arc::new)
                            .map_err(|e| {
                                warn!(error = %e, "brain shared service pool connect failed");
                                e
                            })
                    })
                    .await?;
                // Reconnects a member Brain closed (e.g. on restart) instead
                // of handing out the dead socket forever.
                shared.get_healthy().await
            }
        }
    }

    /// Per-credential `client_for` (the self-host path). Split out so the shared
    /// arm stays legible; the cache-load/RCU-insert logic is unchanged.
    async fn client_for_credential(
        &self,
        pools: &ArcSwap<HashMap<String, Arc<PoolEntry>>>,
        credential: &str,
    ) -> Result<Arc<BrainClient>, BrainError> {
        // Hot path: lock-free read. A built pool is the overwhelming common case.
        let hit = pools.load().get(credential).map(Arc::clone);
        if let Some(entry) = hit {
            entry.last_used_secs.store(now_secs(), Ordering::Relaxed);
            return entry.pool.get_healthy().await;
        }

        // Cold path: build outside any critical section so a slow handshake never
        // blocks other credentials, and no lock is held across `.await`.
        let auth = Auth::Token(credential.as_bytes().to_vec());
        let built = Pool::connect(self.config.brain_addr, self.config.pool_size, auth)
            .await
            .map_err(|e| {
                warn!(error = %e, "brain pool connect failed");
                e
            })?;
        let entry = Arc::new(PoolEntry {
            pool: Arc::new(built),
            last_used_secs: AtomicU64::new(now_secs()),
        });

        // Publish via RCU (copy-on-write + compare-and-swap retry). If a
        // concurrent task already inserted this key, leave the winner in place;
        // our freshly built pool is then dropped at end of scope. Evict the LRU
        // entry first if we're at the cap, so the map never grows past
        // max_credentials.
        pools.rcu(|current| {
            if current.contains_key(credential) {
                Arc::clone(current)
            } else {
                let mut next = HashMap::clone(current);
                if let Some(lru_key) = lru_victim_key(
                    next.iter()
                        .map(|(k, e)| (k.as_str(), e.last_used_secs.load(Ordering::Relaxed))),
                    next.len(),
                    self.config.max_credentials,
                ) {
                    next.remove(&lru_key);
                }
                next.insert(credential.to_string(), Arc::clone(&entry));
                Arc::new(next)
            }
        });

        // After the RCU the key is guaranteed present (ours or the winner's).
        let entry = pools
            .load()
            .get(credential)
            .map(Arc::clone)
            .expect("invariant: pool for credential is present after rcu insert");
        entry.pool.get_healthy().await
    }

    /// Drop credentials idle beyond `config.idle_ttl`. Called by a background
    /// sweeper, never from the request path. One RCU pass; a no-op if nothing has
    /// gone idle. In shared mode there is a single long-lived pool with no
    /// per-credential entries to sweep, so this is a no-op.
    pub fn sweep_idle(&self) {
        let Mode::PerCredential(pools) = &self.mode else {
            return;
        };
        let cutoff = now_secs().saturating_sub(self.config.idle_ttl.as_secs());
        let current = pools.load();
        let evicted = stale_keys(
            current
                .iter()
                .map(|(k, e)| (k.as_str(), e.last_used_secs.load(Ordering::Relaxed))),
            cutoff,
        );
        if evicted.is_empty() {
            return;
        }
        pools.rcu(|current| {
            let mut next = HashMap::clone(current);
            for key in &evicted {
                next.remove(key);
            }
            Arc::new(next)
        });
        info!(count = evicted.len(), "swept idle brain credential pools");
    }

    /// Cheap reachability probe used by readiness: can we open a TCP socket to
    /// the Brain listener within `timeout`? Avoids a full handshake or a
    /// credential.
    pub async fn probe(&self, timeout: Duration) -> bool {
        matches!(
            tokio::time::timeout(
                timeout,
                tokio::net::TcpStream::connect(self.config.brain_addr),
            )
            .await,
            Ok(Ok(_))
        )
    }

    /// The number of distinct credentials currently cached (test/introspection).
    /// Shared mode keeps no per-credential cache, so it always reports 0.
    #[cfg(test)]
    fn cached_len(&self) -> usize {
        match &self.mode {
            Mode::PerCredential(pools) => pools.load().len(),
            Mode::Shared { .. } => 0,
        }
    }

    /// The service credential a shared-mode pool authenticates as, or `None` in
    /// per-credential mode (test/introspection).
    #[cfg(test)]
    fn shared_service_credential(&self) -> Option<&str> {
        match &self.mode {
            Mode::Shared {
                service_credential, ..
            } => Some(service_credential),
            Mode::PerCredential(_) => None,
        }
    }

    /// Whether the shared-mode service pool has been lazily built yet
    /// (test/introspection). Always `None` in per-credential mode.
    #[cfg(test)]
    fn shared_pool_initialized(&self) -> Option<bool> {
        match &self.mode {
            Mode::Shared { pool, .. } => Some(pool.initialized()),
            Mode::PerCredential(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(max_credentials: usize, idle_ttl: Duration) -> BrainPoolConfig {
        BrainPoolConfig {
            brain_addr: "127.0.0.1:1".parse().expect("addr"),
            pool_size: 1,
            max_credentials,
            idle_ttl,
        }
    }

    #[test]
    fn max_credentials_is_floored_at_one() {
        let pool = BrainPool::new(test_config(0, Duration::from_secs(900)));
        assert_eq!(pool.config.max_credentials, 1);
    }

    #[test]
    fn a_fresh_pool_caches_nothing() {
        let pool = BrainPool::new(test_config(8, Duration::from_secs(900)));
        assert_eq!(pool.cached_len(), 0);
    }

    // The cap/LRU and idle-sweep bookkeeping is exercised against the pure
    // selection helpers, which is exactly the logic `client_for` (cold path) and
    // `sweep_idle` drive — hermetic, no live Brain, no `Pool` construction.

    #[test]
    fn lru_victim_is_none_below_the_cap() {
        let recencies = [("a", 100u64), ("b", 200)];
        assert_eq!(
            lru_victim_key(recencies.iter().copied(), recencies.len(), 4),
            None
        );
    }

    #[test]
    fn lru_victim_picks_the_stalest_at_the_cap() {
        // Three candidates, cap 2: at/over the cap, evict the least-recently-used.
        let recencies = [("a", 100u64), ("b", 300), ("c", 200)];
        assert_eq!(
            lru_victim_key(recencies.iter().copied(), recencies.len(), 2),
            Some("a".to_string()),
            "stalest entry ('a', oldest last-used) is the eviction victim",
        );
    }

    #[test]
    fn stale_keys_selects_only_entries_past_the_cutoff() {
        // cutoff 250: 100 and 200 are stale; 300 and 400 are fresh.
        let recencies = [
            ("old1", 100u64),
            ("fresh1", 300),
            ("old2", 200),
            ("fresh2", 400),
        ];
        let mut stale = stale_keys(recencies.iter().copied(), 250);
        stale.sort();
        assert_eq!(stale, vec!["old1".to_string(), "old2".to_string()]);
    }

    #[test]
    fn stale_keys_is_empty_when_all_are_fresh() {
        let recencies = [("a", 300u64), ("b", 400)];
        assert!(stale_keys(recencies.iter().copied(), 250).is_empty());
    }

    #[test]
    fn sweep_idle_on_an_empty_cache_is_a_noop() {
        let pool = BrainPool::new(test_config(8, Duration::from_secs(100)));
        pool.sweep_idle();
        assert_eq!(pool.cached_len(), 0);
    }

    #[tokio::test]
    async fn probe_a_dead_address_is_false() {
        // Port 1 on loopback is not listening; the probe fails fast.
        let pool = BrainPool::new(test_config(8, Duration::from_secs(900)));
        assert!(!pool.probe(Duration::from_millis(200)).await);
    }

    #[test]
    fn shared_mode_records_its_service_credential_and_no_percredential_cache() {
        let pool = BrainPool::shared(
            test_config(8, Duration::from_secs(900)),
            "service-key".to_string(),
        );
        assert_eq!(pool.shared_service_credential(), Some("service-key"));
        assert_eq!(
            pool.cached_len(),
            0,
            "shared mode keeps no per-credential cache"
        );
        assert_eq!(pool.shared_pool_initialized(), Some(false), "built lazily");
    }

    #[test]
    fn new_is_per_credential_not_shared() {
        let pool = BrainPool::new(test_config(8, Duration::from_secs(900)));
        assert_eq!(pool.shared_service_credential(), None);
        assert_eq!(pool.shared_pool_initialized(), None);
    }

    #[tokio::test]
    async fn shared_mode_ignores_the_credential_argument() {
        // Dead address: both calls fail to connect, but the point is that shared
        // mode routes EVERY credential to the ONE service-principal pool and never
        // spins up a per-credential entry. Hermetic — no live Brain.
        let pool = BrainPool::shared(
            test_config(8, Duration::from_secs(900)),
            "service-key".to_string(),
        );
        let a = pool.client_for("tenant-a").await;
        let b = pool.client_for("tenant-b").await;
        assert!(a.is_err(), "dead addr: connect fails");
        assert!(b.is_err(), "dead addr: connect fails");
        // Regardless of the (differing) credential args, no per-credential cache
        // ever forms — both were served by the single shared pool.
        assert_eq!(pool.cached_len(), 0);
        assert_eq!(pool.shared_service_credential(), Some("service-key"));
    }

    #[test]
    fn sweep_idle_in_shared_mode_is_a_noop() {
        let pool = BrainPool::shared(
            test_config(8, Duration::from_secs(1)),
            "service-key".to_string(),
        );
        pool.sweep_idle();
        assert_eq!(pool.cached_len(), 0);
    }
}
