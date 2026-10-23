//! BYOK credential handlers: register / inspect / remove a tenant's provider key.
//!
//! # Where the namespace comes from
//!
//! **Never from the request body.** The namespace is read from
//! `whoami` — the identity Brain resolves from the caller's own credential —
//! so a tenant can only ever manage their own key. Accepting a namespace
//! parameter would make this endpoint a cross-tenant write primitive, and it
//! would look perfectly reasonable in a diff.
//!
//! # The plaintext's lifetime
//!
//! A provider key exists in the clear for the duration of [`put`] and no
//! longer: it arrives in the body, is sealed against the namespace+provider
//! binding, and only the ciphertext is handed to the store. Nothing that leaves
//! this module can reconstruct it.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Json;
use brain_db_sdk::wire::types::GetCapabilitiesRequest;

use crate::app::handlers::llm_proxy::{KeyCheck, Upstream};
use crate::crypto::SecretBinding;
use crate::dto::llm_credential::{
    DeleteLlmCredentialDto, LlmCredentialCheckDto, LlmCredentialDto, PutLlmCredentialRequest,
};
use crate::error::ApiError;
use crate::port::{LlmCredentialStore, StoredLlmCredential, key_fingerprint};
use crate::state::EdgeState;

/// Current unix seconds. A clock read, not a correctness input: the timestamp
/// is display metadata, so a skewed clock mislabels a row rather than breaking
/// one.
fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Resolve the caller's own namespace via Brain.
///
/// This is the authorization boundary: the returned namespace is the one
/// Brain proved from the caller's credential, not one they asked for.
async fn caller_namespace(state: &EdgeState, headers: &HeaderMap) -> Result<String, ApiError> {
    let ident = state.resolve(headers).await?;
    let client = state.client_for(&ident.credential).await?;
    // `capabilities` round-trips to Brain and returns the connection's resolved
    // identity; `whoami` reads the same `ConnectionInfo` without a round trip.
    let _ = client
        .capabilities(&GetCapabilitiesRequest::default())
        .await;
    Ok(client.connection().namespace.clone())
}

/// The store, or a `501` explaining that this edge has no BYOK configured.
fn store_of(state: &EdgeState) -> Result<&std::sync::Arc<dyn LlmCredentialStore>, ApiError> {
    state.llm_credentials().ok_or_else(|| {
        ApiError::not_implemented(
            "BYOK is not configured on this edge: no key-encryption key is set",
        )
    })
}

/// `PUT /v1/llm-credential` — register (or replace) this tenant's provider key.
///
/// # Errors
/// `400` on a malformed body, `501` when BYOK is unconfigured, `500` if sealing
/// or storage fails.
pub async fn put(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Json(body): Json<PutLlmCredentialRequest>,
) -> Result<Json<LlmCredentialDto>, ApiError> {
    let provider = body.validate().map_err(ApiError::bad_request)?;
    // `validate()` already proved this is a provider we serve, so the lookup
    // cannot fail — but expressing it as a fallible step keeps the two in
    // lockstep if the accepted set ever outgrows the routed set.
    let upstream = Upstream::from_name(provider)
        .ok_or_else(|| ApiError::internal("validated provider is not routable"))?;
    let store = store_of(&state)?;
    let namespace = caller_namespace(&state, &headers).await?;

    // Verify the key with the provider before storing it. A typo'd key stored
    // happily here fails on every subsequent write with an opaque upstream
    // 401, which reads like a Brain bug rather than a mistyped secret.
    //
    // Fails CLOSED on rejection, OPEN on an outage: "the provider says this
    // key is bad" is actionable now, while "I could not reach the provider"
    // says nothing about the key and must not block a tenant from onboarding
    // during someone else's incident.
    match crate::app::handlers::llm_proxy::validate_key(upstream, &body.api_key).await {
        KeyCheck::Valid => {}
        KeyCheck::Rejected(why) => return Err(ApiError::bad_request(why)),
        KeyCheck::Unreachable(why) => tracing::warn!(
            namespace = %namespace,
            provider,
            reason = %why,
            "storing provider key without verification",
        ),
    }

    let binding = SecretBinding {
        namespace: &namespace,
        provider,
    };
    // Seal before anything else touches the key: from here on only ciphertext
    // moves, and `body.api_key` goes out of scope with this function.
    let sealed = state.seal_llm_key(&body.api_key, binding)?;

    let record = StoredLlmCredential {
        provider: provider.to_string(),
        model: body.model.trim().to_string(),
        sealed,
        fingerprint: key_fingerprint(&body.api_key),
        created_at_unix_secs: now_unix_secs(),
    };
    store.put(&namespace, record.clone()).await?;

    tracing::info!(
        namespace = %namespace,
        provider,
        model = %record.model,
        fingerprint = %record.fingerprint,
        "BYOK provider key registered",
    );
    Ok(Json(LlmCredentialDto::from(record)))
}

/// `GET /v1/llm-credential` — what is installed for this tenant.
///
/// # Errors
/// `404` when the tenant has registered nothing, `501` when BYOK is
/// unconfigured.
pub async fn get(
    State(state): State<EdgeState>,
    headers: HeaderMap,
) -> Result<Json<LlmCredentialDto>, ApiError> {
    let store = store_of(&state)?;
    let namespace = caller_namespace(&state, &headers).await?;
    let found = store.get(&namespace).await?;
    let record = found.ok_or_else(|| {
        ApiError::not_found("no LLM provider key is registered for this namespace")
    })?;
    Ok(Json(LlmCredentialDto::from(record)))
}

/// `DELETE /v1/llm-credential` — remove this tenant's key.
///
/// Reports whether anything was removed rather than always `204`, so a caller
/// can distinguish "revoked" from "there was nothing there" — which matters
/// when the call is part of an incident response.
///
/// # Errors
/// `501` when BYOK is unconfigured.
pub async fn delete(
    State(state): State<EdgeState>,
    headers: HeaderMap,
) -> Result<Json<DeleteLlmCredentialDto>, ApiError> {
    let store = store_of(&state)?;
    let namespace = caller_namespace(&state, &headers).await?;
    let deleted = store.delete(&namespace).await?;
    if deleted {
        tracing::info!(namespace = %namespace, "BYOK provider key removed");
    }
    Ok(Json(DeleteLlmCredentialDto { deleted }))
}

/// Probe a key against its provider and project the outcome.
///
/// Shared by `test` (a key the caller supplies) and `verify` (the one
/// already stored), so the two cannot drift in how they classify a provider's
/// answer.
async fn probe(provider: &str, key: &str) -> Result<Json<LlmCredentialCheckDto>, ApiError> {
    let upstream = Upstream::from_name(provider).ok_or_else(|| {
        // A provider this build cannot route is version skew, not a bad key —
        // reporting it as `rejected` would send an operator hunting for a
        // typo that is not there.
        ApiError::internal(format!(
            "provider `{provider}` is not routable by this build"
        ))
    })?;
    let check = crate::app::handlers::llm_proxy::validate_key(upstream, key).await;
    Ok(Json(LlmCredentialCheckDto::from_check(check, provider)))
}

/// `POST /v1/llm-credential/test` — check a key WITHOUT storing it.
///
/// For trying a key before committing to it. Nothing is written and nothing
/// existing is touched, so this is safe to call against a namespace that
/// already has a working credential.
///
/// # Errors
/// `400` on a malformed body, `501` when BYOK is unconfigured.
pub async fn test(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Json(body): Json<PutLlmCredentialRequest>,
) -> Result<Json<LlmCredentialCheckDto>, ApiError> {
    let provider = body.validate().map_err(ApiError::bad_request)?;
    // Gated on BYOK being configured even though nothing is stored: an edge
    // that cannot hold a key has no business probing providers on a caller's
    // behalf, and answering here would imply a registration that will 501.
    let _ = store_of(&state)?;
    let _namespace = caller_namespace(&state, &headers).await?;
    probe(provider, &body.api_key).await
}

/// `POST /v1/llm-credential/verify` — re-check the key already stored.
///
/// A key that worked at registration can be revoked or run out of credit
/// later, and without this the first sign of it is an opaque upstream 401 on
/// a write — which reads like a Brain fault rather than an expired secret.
///
/// # Errors
/// `404` when nothing is registered, `501` when BYOK is unconfigured.
pub async fn verify(
    State(state): State<EdgeState>,
    headers: HeaderMap,
) -> Result<Json<LlmCredentialCheckDto>, ApiError> {
    let store = store_of(&state)?;
    let namespace = caller_namespace(&state, &headers).await?;
    let record = store.get(&namespace).await?.ok_or_else(|| {
        ApiError::not_found("no LLM provider key is registered for this namespace")
    })?;

    let opened = state.open_llm_key(
        &record.sealed,
        SecretBinding {
            namespace: &namespace,
            provider: &record.provider,
        },
    )?;
    let key = String::from_utf8(opened.to_vec())
        .map_err(|_| ApiError::internal("stored provider key is not valid UTF-8"))?;
    probe(&record.provider, &key).await
}
