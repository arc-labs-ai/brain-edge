//! Shared state: the [`BrainPool`] connection cache to Brain plus the injected
//! [`CredentialResolver`] + [`MeteringSink`] ports.
//!
//! The resolver turns an incoming request into a Brain wire credential; the
//! [`BrainPool`] reuses one handshake per credential so we don't re-connect on
//! every request; the metering sink observes each op's outcome. Self-host uses
//! the bearer/no-op defaults ([`EdgeState::new`]); the gateway injects its own
//! ports ([`EdgeState::with_ports`]).

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::http::HeaderMap;
use brain_db_sdk::BrainClient;

use crate::config::EdgeConfig;
use crate::crypto::{KeyRing, SealedSecret, SecretBinding};
use crate::error::ApiError;
use crate::pool::{BrainPool, BrainPoolConfig};
use crate::port::{
    BearerResolver, CredentialResolver, LlmCredentialStore, MeterEvent, MeteringSink, NoopMeter,
    Outcome, RecallSignals, ResolvedCredential,
};
use zeroize::Zeroizing;

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
    /// BYOK: where sealed credentials live and the keys that seal them.
    ///
    /// Unset by default (self-host) and settable once at startup. A
    /// `OnceLock` rather than a field because the pair is all-or-nothing — a
    /// store without a keyring could only hold plaintext, which is the exact
    /// outcome this module exists to prevent — and because installing it must
    /// not require `Inner: Clone`.
    byok: OnceLock<Byok>,
}

/// The BYOK pair. Both or neither.
struct Byok {
    store: Arc<dyn LlmCredentialStore>,
    keyring: Arc<KeyRing>,
}

impl EdgeState {
    /// The self-host default: bearer passthrough, no metering, no BYOK.
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
                byok: OnceLock::new(),
            }),
        }
    }

    /// Install the BYOK pieces: where sealed credentials live, and the keys
    /// that seal them.
    ///
    /// Separate from [`Self::with_ports`] so the existing two-port constructor
    /// keeps working unchanged and BYOK is opt-in. A deployment that never
    /// calls this has no KEK, so [`Self::seal_llm_key`] refuses — the edge
    /// will not fall back to storing a provider key in the clear.
    #[must_use]
    pub fn with_llm_credentials(
        self,
        store: Arc<dyn LlmCredentialStore>,
        keyring: Arc<KeyRing>,
    ) -> Self {
        // Set-once: a second call is a wiring bug, not a reconfiguration, and
        // silently swapping the keyring under live ciphertext would strand it.
        if self.inner.byok.set(Byok { store, keyring }).is_err() {
            tracing::warn!("BYOK already installed on this EdgeState; ignoring repeat install");
        }
        self
    }

    /// True when a KEK is configured, i.e. BYOK is usable on this edge.
    #[must_use]
    pub fn byok_enabled(&self) -> bool {
        self.inner.byok.get().is_some()
    }

    /// Seal a plaintext provider key for `namespace`/`provider`.
    ///
    /// The plaintext is borrowed and never stored; only the returned
    /// [`SealedSecret`] crosses the store boundary.
    ///
    /// # Errors
    /// `501` when no KEK is configured — storing the key would mean writing it
    /// in the clear, so the edge declines instead. `500` if sealing fails.
    pub fn seal_llm_key(
        &self,
        plaintext: &str,
        binding: SecretBinding<'_>,
    ) -> Result<SealedSecret, ApiError> {
        let keyring = self.inner.byok.get().map(|b| &b.keyring).ok_or_else(|| {
            ApiError::not_implemented(
                "BYOK is not configured on this edge: no key-encryption key is set, \
                 so a provider key cannot be stored",
            )
        })?;
        keyring
            .seal(plaintext.as_bytes(), binding)
            .map_err(|e| ApiError::internal(format!("seal provider key: {e}")))
    }

    /// Open a sealed provider key. The plaintext is wiped when dropped.
    ///
    /// # Errors
    /// `501` when no KEK is configured; `500` when the ciphertext does not open
    /// (wrong key, tampering, or a binding that does not match).
    pub fn open_llm_key(
        &self,
        sealed: &SealedSecret,
        binding: SecretBinding<'_>,
    ) -> Result<Zeroizing<Vec<u8>>, ApiError> {
        let keyring = self
            .inner
            .byok
            .get()
            .map(|b| &b.keyring)
            .ok_or_else(|| ApiError::not_implemented("BYOK is not configured on this edge"))?;
        keyring
            .open(sealed, binding)
            .map_err(|e| ApiError::internal(format!("open provider key: {e}")))
    }

    /// The sealed-credential store, when BYOK is configured.
    ///
    /// `None` without a keyring: a store with nothing to seal with could only
    /// ever hold plaintext, so there is deliberately no way to reach one.
    #[must_use]
    pub fn llm_credentials(&self) -> Option<&Arc<dyn LlmCredentialStore>> {
        self.inner.byok.get().map(|b| &b.store)
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

#[cfg(test)]
mod byok_state_tests {
    use super::*;
    use crate::port::{InMemoryLlmCredentialStore, StoredLlmCredential};

    const KEK: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    fn config() -> EdgeConfig {
        // Explicit rather than `..Default::default()`: EdgeConfig has no
        // Default, and these tests never open a socket or a Brain connection.
        EdgeConfig {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            brain_addr: "127.0.0.1:1".parse().unwrap(),
            pool_size: 1,
            max_credentials: 1,
            idle_ttl_secs: 1,
            request_timeout_secs: 1,
            max_body_bytes: 1024,
            wire_listen_addr: None,
            wire_rate_capacity: 0,
            wire_rate_refill_per_sec: 0,
        }
    }

    fn binding<'a>() -> SecretBinding<'a> {
        SecretBinding {
            namespace: "acme",
            provider: "openai",
        }
    }

    #[test]
    fn byok_is_off_by_default() {
        let st = EdgeState::new(config());
        assert!(!st.byok_enabled());
        assert!(
            st.llm_credentials().is_none(),
            "a store with no keyring could only hold plaintext, so it must be unreachable"
        );
    }

    #[test]
    fn without_a_kek_sealing_is_refused_rather_than_stored_in_the_clear() {
        // The central safety property of this whole design: a misconfigured
        // edge declines to hold the key at all. There is deliberately no
        // plaintext fallback path to reach.
        let st = EdgeState::new(config());
        let err = st.seal_llm_key("sk-live-secret", binding()).unwrap_err();
        assert_eq!(err.status(), axum::http::StatusCode::NOT_IMPLEMENTED);
        assert!(
            !format!("{err:?}").contains("sk-live-secret"),
            "the rejected key must not appear in the error"
        );
    }

    #[test]
    fn installing_byok_enables_seal_and_open() {
        let keyring = Arc::new(KeyRing::from_spec(&format!("k1:{KEK}"), "k1").unwrap());
        let st = EdgeState::new(config())
            .with_llm_credentials(Arc::new(InMemoryLlmCredentialStore::default()), keyring);

        assert!(st.byok_enabled());
        assert!(st.llm_credentials().is_some());

        let sealed = st.seal_llm_key("sk-live-secret", binding()).unwrap();
        assert_eq!(
            &*st.open_llm_key(&sealed, binding()).unwrap(),
            b"sk-live-secret"
        );
    }

    #[test]
    fn a_sealed_key_does_not_open_under_another_tenant_via_state() {
        // End-to-end at the state layer, not just the crypto layer: the
        // binding must survive the wiring.
        let keyring = Arc::new(KeyRing::from_spec(&format!("k1:{KEK}"), "k1").unwrap());
        let st = EdgeState::new(config())
            .with_llm_credentials(Arc::new(InMemoryLlmCredentialStore::default()), keyring);

        let sealed = st.seal_llm_key("acme-key", binding()).unwrap();
        let other = SecretBinding {
            namespace: "globex",
            provider: "openai",
        };
        assert!(st.open_llm_key(&sealed, other).is_err());
    }

    #[tokio::test]
    async fn a_stored_record_carries_ciphertext_not_the_key() {
        let keyring = Arc::new(KeyRing::from_spec(&format!("k1:{KEK}"), "k1").unwrap());
        let st = EdgeState::new(config())
            .with_llm_credentials(Arc::new(InMemoryLlmCredentialStore::default()), keyring);

        let secret = "sk-live-0123456789abcdef";
        let sealed = st.seal_llm_key(secret, binding()).unwrap();
        st.llm_credentials()
            .unwrap()
            .put(
                "acme",
                StoredLlmCredential {
                    provider: "openai".into(),
                    model: "gpt-4o-mini".into(),
                    sealed,
                    fingerprint: crate::port::key_fingerprint(secret),
                    created_at_unix_secs: 0,
                },
            )
            .await
            .unwrap();

        let got = st
            .llm_credentials()
            .unwrap()
            .get("acme")
            .await
            .unwrap()
            .unwrap();
        assert!(
            !got.sealed
                .ciphertext
                .windows(secret.len())
                .any(|w| w == secret.as_bytes()),
            "the stored record must not contain the provider key"
        );
        assert_eq!(got.fingerprint, "…cdef");
        assert_eq!(
            &*st.open_llm_key(&got.sealed, binding()).unwrap(),
            secret.as_bytes()
        );
    }
}
