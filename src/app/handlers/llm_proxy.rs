//! The BYOK broker: Brain's LLM calls, re-signed with the tenant's own key.
//!
//! # Why this exists
//!
//! Brain's extraction and HyPE stages need an LLM, and under BYOK the call must
//! bill to the tenant's provider account. The engine therefore does not hold the
//! key — it points its provider client's `base_url` here and sends a namespace
//! instead. This module opens that namespace's sealed credential and re-signs
//! the request upstream.
//!
//! # Why the namespace is in the path
//!
//! Brain's provider clients build their URL as
//! `format!("{}/v1/chat/completions", base_url)` and authenticate with
//! `.bearer_auth(api_key)`. They cannot set a custom header. So for those
//! clients to work **unchanged**, both routing inputs have to travel in the two
//! channels they do control: the namespace in `base_url`, the broker token as
//! the bearer.
//!
//! Pointing `base_url` at `{edge}/llm/ns/{namespace}/openai` therefore resolves
//! to this module, and the otherwise-redundant-looking path suffix is what
//! makes a zero-change integration possible.
//!
//! A namespace is a tenant identifier, not a secret, and it will appear in
//! access logs — which is acceptable here and is why the *key* never travels
//! this way.
//!
//! # Threat model — stated precisely, because it is easy to overclaim
//!
//! What this buys: **Brain's data at rest contains no provider keys.** Its WAL,
//! snapshots and backups never see one, so a stolen disk or a leaked backup
//! yields nothing. The store behind the edge holds ciphertext only, so a
//! database compromise yields nothing either.
//!
//! What it does **not** buy: a fully compromised *running* Brain can ask the
//! edge for any namespace it can name, and get completions billed to that
//! tenant. The shared service token authenticates "a Brain", not "this
//! namespace's Brain".
//!
//! **That gap is structural, not a missing feature.** It is tempting to reach
//! for per-namespace signed grants, but they do not close it:
//!
//! - If Brain *requests* a grant using the shared token, a compromised Brain
//!   requests any namespace it likes — exactly the situation we started in.
//! - If the grant instead rides on the client's ENCODE request, it has to
//!   survive into derivation, which runs **asynchronously after the write is
//!   acknowledged**. Surviving that means persisting it in Brain, which is
//!   precisely the key-at-rest this design exists to avoid.
//!
//! The root cause is that a single multi-tenant Brain must be able to obtain a
//! key for every namespace it processes writes for. No token scheme removes
//! that. What genuinely limits the blast radius is **detection and bounds**:
//! every resolution is audit-logged below, and per-namespace spend caps belong
//! here too. Separating the credential plane from the data plane
//! (a deployment concern) keeps provider traffic out of the busiest process.
//!
//! Recorded at this length because "we'll add signed grants later" reads like a
//! plan, and acting on it would have produced ceremony with no security gain.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, StatusCode};
use axum::response::Response;

use crate::crypto::SecretBinding;
use crate::error::ApiError;
use crate::state::EdgeState;

/// Header carrying the shared token Brain authenticates to the broker with.
///
/// Accepted as an alternative to `Authorization: Bearer`, which is what Brain's
/// own provider clients send. Both are checked so a hand-rolled caller (curl, a
/// future service) is not forced into the bearer slot.
pub const PROXY_TOKEN_HEADER: &str = "x-brain-edge-llm-token";

/// Env var holding the broker's shared service token. Unset ⇒ the proxy is off.
pub const PROXY_TOKEN_ENV: &str = "BRAIN_EDGE_LLM_PROXY_TOKEN";

/// Upstream bases. Not configurable per request: a caller-supplied upstream
/// would turn this into an open relay that signs arbitrary requests with a
/// tenant's key.
const OPENAI_BASE: &str = "https://api.openai.com";
const ANTHROPIC_BASE: &str = "https://api.anthropic.com";

/// Canonical wire name for OpenAI.
///
/// A named constant rather than a literal at each use: this exact string is
/// the `provider` field of a stored credential, a segment of the broker URL,
/// **and** part of the AEAD binding the credential is sealed under. A typo in
/// any one place is a runtime decrypt failure, not a compile error.
pub const PROVIDER_OPENAI: &str = "openai";
/// Canonical wire name for Anthropic. See [`PROVIDER_OPENAI`].
pub const PROVIDER_ANTHROPIC: &str = "anthropic";

/// Path prefix the broker routes live under.
///
/// Kept beside [`Upstream`] because `brain-llm`'s `BrokerResolver` builds
/// `{edge}/llm/ns/{namespace}/{provider}` to match. The two live in separate
/// repos, so neither can reference the other's constant — a test on each side
/// pins the shape instead.
pub const BROKER_PATH_PREFIX: &str = "/llm/ns";

/// Which upstream a route serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upstream {
    /// OpenAI's chat-completions API.
    OpenAi,
    /// Anthropic's messages API.
    Anthropic,
}

impl Upstream {
    /// Every upstream this edge can serve. The single source of truth: the
    /// accepted-provider list and the credential validator both derive from
    /// it, so adding a provider cannot leave one of them behind.
    pub const ALL: [Self; 2] = [Self::OpenAi, Self::Anthropic];

    /// Canonical wire name.
    #[must_use]
    pub const fn provider(self) -> &'static str {
        match self {
            Self::OpenAi => PROVIDER_OPENAI,
            Self::Anthropic => PROVIDER_ANTHROPIC,
        }
    }

    /// Parse a canonical provider name. The inverse of [`Self::provider`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|u| u.provider() == name)
    }

    const fn base(self) -> &'static str {
        match self {
            Self::OpenAi => OPENAI_BASE,
            Self::Anthropic => ANTHROPIC_BASE,
        }
    }
}

/// Compare the presented token against the configured one in constant time.
///
/// A byte-wise `==` short-circuits on the first differing byte, leaking the
/// length of the shared prefix through timing — enough to recover a token given
/// enough attempts.
///
/// `subtle`, not `ring::constant_time::verify_slices_are_equal`: the latter is
/// deprecated and documents "no promises regarding side channels", which is the
/// single promise this function needs.
fn token_matches(presented: &str, configured: &str) -> bool {
    use subtle::ConstantTimeEq;
    // Length is compared non-secretly: a constant-time compare needs equal
    // lengths, and the length of a shared deployment token is not the secret.
    presented.len() == configured.len()
        && bool::from(presented.as_bytes().ct_eq(configured.as_bytes()))
}

/// Authenticate the caller against the configured broker token.
fn authenticate(headers: &HeaderMap) -> Result<(), ApiError> {
    authenticate_with(std::env::var(PROXY_TOKEN_ENV).ok().as_deref(), headers)
}

/// The token the caller presented, from either accepted channel.
fn presented_token(headers: &HeaderMap) -> &str {
    if let Some(v) = headers
        .get(HeaderName::from_static(PROXY_TOKEN_HEADER))
        .and_then(|v| v.to_str().ok())
    {
        return v.trim();
    }
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(str::trim)
        .unwrap_or_default()
}

/// The testable core of [`authenticate`]: the configured token is a parameter
/// rather than an environment read, so the auth rules can be tested without
/// mutating process state (which this crate forbids — `unsafe` is denied, and
/// `set_var` is unsafe in edition 2024).
fn authenticate_with(configured: Option<&str>, headers: &HeaderMap) -> Result<(), ApiError> {
    let Some(configured) = configured.map(str::trim).filter(|s| !s.is_empty()) else {
        return Err(ApiError::not_implemented(
            "the BYOK LLM broker is not enabled on this edge",
        ));
    };
    if !token_matches(presented_token(headers), configured) {
        // Deliberately identical to the missing-token case: distinguishing
        // them would confirm that a token was *shaped* right.
        return Err(ApiError::unauthorized("broker token missing or invalid"));
    }
    Ok(())
}

/// `POST /llm/openai/v1/chat/completions`
///
/// # Errors
/// See [`proxy`].
pub async fn openai_chat_completions(
    State(state): State<EdgeState>,
    axum::extract::Path(namespace): axum::extract::Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    proxy(
        &state,
        &headers,
        &namespace,
        Upstream::OpenAi,
        "/v1/chat/completions",
        body,
    )
    .await
}

/// `POST /llm/anthropic/v1/messages`
///
/// # Errors
/// See [`proxy`].
pub async fn anthropic_messages(
    State(state): State<EdgeState>,
    axum::extract::Path(namespace): axum::extract::Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    proxy(
        &state,
        &headers,
        &namespace,
        Upstream::Anthropic,
        "/v1/messages",
        body,
    )
    .await
}

/// `GET /llm/ns/{namespace}/resolve` — what credential serves this namespace.
///
/// Brain's resolver calls this to learn which provider and model to build a
/// client for, and to find out whether a credential exists at all. It returns
/// **no key material** — only what Brain needs to route, which is why Brain can
/// hold the answer in memory without that being a key at rest.
///
/// # Errors
/// `401` on a bad broker token, `409 llm_credential.missing` when the namespace
/// has no key, `501` when BYOK or the broker is unconfigured.
pub async fn resolve(
    State(state): State<EdgeState>,
    axum::extract::Path(namespace): axum::extract::Path<String>,
    headers: HeaderMap,
) -> Result<axum::response::Json<crate::dto::llm_credential::ResolvedLlmDto>, ApiError> {
    authenticate(&headers)?;
    let store = state.llm_credentials().ok_or_else(|| {
        ApiError::not_implemented("BYOK is not configured on this edge: no key-encryption key")
    })?;
    let record = store.get(&namespace).await?.ok_or_else(|| {
        ApiError::llm_credential_missing(format!(
            "no LLM provider key is registered for namespace {namespace}"
        ))
    })?;
    tracing::info!(
        target: "brain_edge::llm_audit",
        namespace = %namespace,
        provider = %record.provider,
        "BYOK: credential resolved for a namespace",
    );
    Ok(axum::response::Json(
        crate::dto::llm_credential::ResolvedLlmDto {
            provider: record.provider,
            model: record.model,
        },
    ))
}

/// Open the namespace's key and re-sign the request upstream.
///
/// # Errors
/// `401` on a bad broker token, `409` when the tenant's registered provider is
/// not the one this route serves, `501` when BYOK or the broker is
/// unconfigured, `502` when the upstream cannot be reached. A missing
/// credential is `409 llm_credential.missing`, matching what the data plane
/// returns for the same condition so one code means one thing everywhere.
#[allow(clippy::too_many_arguments)]
async fn proxy(
    state: &EdgeState,
    headers: &HeaderMap,
    namespace: &str,
    upstream: Upstream,
    path: &str,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    authenticate(headers)?;

    let store = state.llm_credentials().ok_or_else(|| {
        ApiError::not_implemented("BYOK is not configured on this edge: no key-encryption key")
    })?;
    let record = store.get(namespace).await?.ok_or_else(|| {
        ApiError::llm_credential_missing(format!(
            "no LLM provider key is registered for namespace {namespace}"
        ))
    })?;

    // A tenant registered for one provider must not have their key replayed
    // against another: the AAD would reject it anyway, but failing here gives
    // an actionable error instead of an opaque decrypt failure.
    if record.provider != upstream.provider() {
        return Err(ApiError::conflict(format!(
            "namespace {namespace} is registered for `{}`, not `{}`",
            record.provider,
            upstream.provider()
        )));
    }

    let key = state.open_llm_key(
        &record.sealed,
        SecretBinding {
            namespace,
            provider: upstream.provider(),
        },
    )?;
    let key = std::str::from_utf8(&key)
        .map_err(|_| ApiError::internal("stored provider key is not valid UTF-8"))?;

    // Audit trail. Since a compromised Brain can name any namespace (see the
    // module docs), detection is the control that actually applies — an
    // unexpected namespace appearing here is the signal. Logged before the
    // upstream call so an attempt is recorded even if the call then fails.
    tracing::info!(
        target: "brain_edge::llm_audit",
        namespace = %namespace,
        provider = upstream.provider(),
        fingerprint = %record.fingerprint,
        "BYOK: provider key used for an upstream call",
    );

    let url = format!("{}{path}", upstream.base());
    let client = reqwest::Client::new();
    let mut req = client
        .post(&url)
        .header("content-type", "application/json")
        .body(body.to_vec());

    // Each provider authenticates differently. The tenant's key is attached
    // here and nowhere else; the inbound request never carried it.
    req = match upstream {
        Upstream::OpenAi => req.header("authorization", format!("Bearer {key}")),
        Upstream::Anthropic => req
            .header("x-api-key", key)
            .header("anthropic-version", anthropic_version(headers)),
    };

    let resp = req.send().await.map_err(|e| {
        // The error is logged, not returned: a reqwest error can embed the URL
        // and headers, and these headers carry the tenant's key.
        tracing::warn!(
            namespace = %namespace,
            provider = upstream.provider(),
            error = %e,
            "LLM upstream request failed",
        );
        ApiError::bad_gateway("LLM provider request failed")
    })?;

    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();

    // Stream the body through rather than buffering: a streamed completion is
    // the common case and buffering it would both add latency and let one
    // large response pin memory.
    let stream = resp.bytes_stream();
    let mut out = Response::builder()
        .status(status)
        .header("content-type", content_type);
    // Only the status and content type are forwarded. Upstream headers are NOT
    // echoed wholesale: they can carry provider-side rate-limit and
    // organisation identifiers belonging to the tenant's account.
    if status.is_success() {
        out = out.header("x-brain-llm-provider", upstream.provider());
    }
    out.body(Body::from_stream(stream))
        .map_err(|e| ApiError::internal(format!("build proxy response: {e}")))
}

/// Anthropic requires an API version header. Pass the caller's through when
/// present so Brain controls it, with a pinned default otherwise.
fn anthropic_version(headers: &HeaderMap) -> String {
    headers
        .get("anthropic-version")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("2023-06-01")
        .to_string()
}

/// Outcome of probing a provider key before storing it.
///
/// The three-way split mirrors the credential-resolution one, for the same
/// reason: "the provider says this key is bad" and "I could not reach the
/// provider" demand opposite responses, and collapsing them would either
/// accept dead keys during an outage or refuse good ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyCheck {
    /// The provider accepted the key.
    Valid,
    /// The provider rejected it — a typo or a revoked key.
    Rejected(String),
    /// The provider could not be reached, or answered something unexpected.
    Unreachable(String),
}

/// Classify a probe response status.
///
/// Only `401`/`403` are treated as "this key is bad". A `429` or `5xx` says
/// nothing about the key — rejecting on those would make key registration fail
/// whenever the provider is rate-limiting or having an incident.
fn classify_probe(status: u16) -> KeyCheck {
    match status {
        200..=299 => KeyCheck::Valid,
        401 | 403 => KeyCheck::Rejected(format!("the provider rejected this key ({status})")),
        other => KeyCheck::Unreachable(format!("provider returned {other} while verifying")),
    }
}

/// Probe a provider key with a cheap read-only call before storing it.
///
/// Without this a typo'd key is accepted happily and then fails on every write
/// with an opaque upstream `401` that looks like a Brain bug. Catching it at
/// registration puts the error where the mistake was made.
///
/// `GET /v1/models` on both providers: read-only, no tokens billed, and it
/// exercises exactly the credential path a real call would.
pub async fn validate_key(upstream: Upstream, key: &str) -> KeyCheck {
    let url = format!("{}/v1/models", upstream.base());
    let client = reqwest::Client::new();
    let req = match upstream {
        Upstream::OpenAi => client.get(&url).bearer_auth(key),
        Upstream::Anthropic => client
            .get(&url)
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01"),
    };

    match req.send().await {
        Ok(resp) => classify_probe(resp.status().as_u16()),
        Err(e) => {
            // Logged, not returned: a reqwest error can embed the request
            // headers, which carry the key being verified.
            tracing::warn!(
                target: "brain_edge::llm_proxy",
                provider = upstream.provider(),
                error = %e,
                "could not reach provider to verify key",
            );
            KeyCheck::Unreachable("could not reach the provider to verify this key".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_comparison_rejects_wrong_and_accepts_right() {
        assert!(token_matches("s3cret", "s3cret"));
        assert!(!token_matches("s3cret", "s3creT"));
        assert!(!token_matches("", "s3cret"));
        assert!(!token_matches("s3cret", ""));
        assert!(!token_matches("s3cret-longer", "s3cret"));
    }

    #[test]
    fn upstream_bases_are_fixed() {
        // A caller-supplied upstream would make this an open relay signing
        // arbitrary requests with a tenant's key.
        assert_eq!(Upstream::OpenAi.base(), "https://api.openai.com");
        assert_eq!(Upstream::Anthropic.base(), "https://api.anthropic.com");
    }

    #[test]
    fn provider_names_round_trip() {
        // The name is the stored `provider` field AND part of the sealing
        // AAD, so the two directions disagreeing means credentials stop
        // opening.
        for u in Upstream::ALL {
            assert_eq!(Upstream::from_name(u.provider()), Some(u));
        }
        assert_eq!(Upstream::from_name("cohere"), None);
    }

    #[test]
    fn the_broker_path_matches_what_brain_builds() {
        // `brain-llm`'s BrokerResolver constructs
        // `{edge}/llm/ns/{namespace}/{provider}` and appends the provider's
        // own suffix. Separate repos, so this pins our half of the contract.
        assert_eq!(BROKER_PATH_PREFIX, "/llm/ns");
        assert_eq!(
            format!(
                "{BROKER_PATH_PREFIX}/acme/{}/v1/chat/completions",
                Upstream::OpenAi.provider()
            ),
            "/llm/ns/acme/openai/v1/chat/completions"
        );
        assert_eq!(
            format!(
                "{BROKER_PATH_PREFIX}/acme/{}/v1/messages",
                Upstream::Anthropic.provider()
            ),
            "/llm/ns/acme/anthropic/v1/messages"
        );
    }

    #[test]
    fn anthropic_version_passes_through_or_defaults() {
        let mut h = HeaderMap::new();
        assert_eq!(anthropic_version(&h), "2023-06-01");
        h.insert("anthropic-version", "2024-10-22".parse().unwrap());
        assert_eq!(anthropic_version(&h), "2024-10-22");
        h.insert("anthropic-version", "  ".parse().unwrap());
        assert_eq!(anthropic_version(&h), "2023-06-01", "blank falls back");
    }

    fn headers_with(token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        if !token.is_empty() {
            h.insert(PROXY_TOKEN_HEADER, token.parse().unwrap());
        }
        h
    }

    fn bearer_headers(token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        h
    }

    #[test]
    fn an_unconfigured_broker_is_501_not_401() {
        // The deployment has not turned the broker on, which is a different
        // problem from a bad credential — and a 401 would send an operator
        // hunting for a wrong token that does not exist.
        for configured in [None, Some(""), Some("   ")] {
            let err = authenticate_with(configured, &headers_with("t")).unwrap_err();
            assert_eq!(err.status(), StatusCode::NOT_IMPLEMENTED);
        }
    }

    #[test]
    fn a_wrong_or_missing_token_is_rejected_identically() {
        // Same status and message either way: distinguishing them confirms a
        // token was shaped right, which is a probing aid.
        let missing = authenticate_with(Some("real"), &headers_with("")).unwrap_err();
        let wrong = authenticate_with(Some("real"), &headers_with("nope")).unwrap_err();
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(missing.message(), wrong.message());
    }

    #[test]
    fn a_valid_token_authenticates_via_either_channel() {
        // Brain's provider clients can only send a bearer; a hand-rolled caller
        // may prefer the explicit header. Both must work.
        assert!(authenticate_with(Some("real"), &headers_with("real")).is_ok());
        assert!(authenticate_with(Some("real"), &bearer_headers("real")).is_ok());
    }

    #[test]
    fn the_explicit_header_wins_over_a_bearer() {
        // Deterministic precedence: a caller sending both should not get a
        // result that depends on header iteration order.
        let mut h = bearer_headers("real");
        h.insert(PROXY_TOKEN_HEADER, "wrong".parse().unwrap());
        assert!(authenticate_with(Some("real"), &h).is_err());
    }

    #[test]
    fn a_bearer_without_the_prefix_is_not_accepted() {
        let mut h = HeaderMap::new();
        h.insert(axum::http::header::AUTHORIZATION, "real".parse().unwrap());
        assert!(authenticate_with(Some("real"), &h).is_err());
    }
}

#[cfg(test)]
mod key_check_tests {
    use super::*;

    #[test]
    fn only_auth_statuses_mean_the_key_is_bad() {
        assert_eq!(classify_probe(200), KeyCheck::Valid);
        assert_eq!(classify_probe(204), KeyCheck::Valid);
        assert!(matches!(classify_probe(401), KeyCheck::Rejected(_)));
        assert!(matches!(classify_probe(403), KeyCheck::Rejected(_)));
    }

    #[test]
    fn transient_statuses_do_not_condemn_the_key() {
        // Rejecting on these would make key registration fail whenever the
        // provider is rate-limiting or having an incident — the user would
        // conclude their perfectly good key is invalid.
        for status in [429, 500, 502, 503, 504] {
            assert!(
                matches!(classify_probe(status), KeyCheck::Unreachable(_)),
                "{status} says nothing about the key"
            );
        }
    }

    #[test]
    fn an_unexpected_status_is_unreachable_not_rejected() {
        // Fail open on the unknown: a 404 means the probe endpoint moved, not
        // that the operator's key is wrong.
        assert!(matches!(classify_probe(404), KeyCheck::Unreachable(_)));
        assert!(matches!(classify_probe(418), KeyCheck::Unreachable(_)));
    }

    #[test]
    fn no_check_message_can_contain_a_key() {
        // These strings are returned to the caller and logged.
        for c in [
            classify_probe(401),
            classify_probe(500),
            classify_probe(200),
        ] {
            let text = format!("{c:?}");
            assert!(!text.contains("sk-"), "{text}");
        }
    }
}
