//! BYOK credential handlers: register / inspect / remove a tenant's provider key.
//!
//! # Brain holds the key; this layer only carries it
//!
//! A provider key has exactly one home: Brain's own sealed credential store,
//! which is the only one the extraction, HyPE and consolidation stages read.
//! These handlers translate the HTTP surface onto the `LLM_CREDENTIAL_*` wire
//! verbs and keep nothing. Storing a copy here would make
//! `GET /v1/llm-credential` answer `200` for a key derivation cannot see —
//! every surface reporting success while writes report no credential.
//!
//! # Where the namespace comes from
//!
//! **Never from the request body, and not from `whoami` either.** The identity
//! rides on the request as `act_as`, stamped by the injected credential
//! resolver: unset for self-host, where the connection already *is* the
//! tenant, and the resolved tenant under the hosted gateway, where one service
//! principal multiplexes everyone. Brain resolves the effective namespace from
//! that before its handler runs, so a caller can only ever reach their own key.
//!
//! # The plaintext's lifetime
//!
//! The key exists in the clear for the duration of [`put`]: it arrives in the
//! body and goes out on the wire to Brain, which seals it. Nothing here
//! persists it, and the confidentiality of that hop is the transport's job —
//! Brain refuses a registration that is not TLS, mTLS or loopback.

use std::time::Instant;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Json;
use brain_db_sdk::wire::types::{
    LlmCredentialDeleteRequest, LlmCredentialGetRequest, LlmCredentialPutRequest,
    LlmCredentialVerifyRequest, VerifyOutcome,
};

use crate::app::extract::ApiJson;
use crate::dto::llm_credential::{
    DeleteLlmCredentialDto, LlmCredentialCheckDto, LlmCredentialDto, PutLlmCredentialRequest,
};
use crate::error::ApiError;
use crate::port::Outcome;
use crate::provider::{KeyCheck, Upstream, validate_key};
use crate::state::EdgeState;

/// `PUT /v1/llm-credential` — register (or replace) this tenant's provider key.
///
/// # Errors
/// `400` on a malformed body or a key the provider rejects, and whatever Brain
/// answers for the registration itself.
pub async fn put(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<PutLlmCredentialRequest>,
) -> Result<Json<LlmCredentialDto>, ApiError> {
    // Authenticate BEFORE anything else. Validation would answer 400 to a
    // caller who has not proved who they are, and the probe below is an
    // outbound call to a provider — neither belongs on an unauthenticated
    // path.
    let ident = state.resolve(&headers).await?;
    state.admit("llm_credential_put", &ident).await?;

    let provider = body.validate().map_err(ApiError::bad_request)?;
    // `validate()` already proved this is a provider we serve, so the lookup
    // cannot fail — but expressing it as a fallible step keeps the two in
    // lockstep if the accepted set ever outgrows the routed set.
    let upstream = Upstream::from_name(provider)
        .ok_or_else(|| ApiError::internal("validated provider is not routable"))?;

    // Check the key with the provider before handing it on. A typo'd key
    // registered happily fails on every subsequent write with an opaque
    // upstream 401, which reads like a Brain fault rather than a mistyped
    // secret.
    //
    // Fails CLOSED on rejection, OPEN on an outage: "the provider says this
    // key is bad" is actionable now, while "I could not reach the provider"
    // says nothing about the key and must not block a tenant from onboarding
    // during someone else's incident.
    match validate_key(upstream, &body.api_key).await {
        KeyCheck::Valid => {}
        KeyCheck::Rejected(why) => return Err(ApiError::bad_request(why)),
        KeyCheck::Unreachable(why) => tracing::warn!(
            provider,
            reason = %why,
            "registering provider key without verification",
        ),
    }

    let mut req = LlmCredentialPutRequest {
        provider: provider.to_string(),
        model: body.model.trim().to_string(),
        api_key: body.api_key.clone(),
        request_id: brain_db_sdk::new_id(),
        act_as: None,
    };
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.put_llm_credential(&req).await;
    state.record(
        "llm_credential_put",
        &ident,
        Outcome::of(&out),
        started.elapsed(),
    );
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;

    tracing::info!(
        provider = %resp.provider,
        model = %resp.model,
        fingerprint = %resp.fingerprint,
        replaced = resp.replaced,
        "BYOK provider key registered",
    );
    Ok(Json(LlmCredentialDto::from(resp)))
}

/// `GET /v1/llm-credential` — what is installed for this tenant.
///
/// # Errors
/// `404` when the tenant has registered nothing.
pub async fn get(
    State(state): State<EdgeState>,
    headers: HeaderMap,
) -> Result<Json<LlmCredentialDto>, ApiError> {
    let mut req = LlmCredentialGetRequest { act_as: None };
    let ident = state.resolve(&headers).await?;
    state.admit("llm_credential_get", &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.get_llm_credential(&req).await;
    state.record(
        "llm_credential_get",
        &ident,
        Outcome::of(&out),
        started.elapsed(),
    );
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;

    let view = resp.credential.ok_or_else(|| {
        ApiError::not_found("no LLM provider key is registered for this namespace")
    })?;
    Ok(Json(LlmCredentialDto::from(view)))
}

/// `DELETE /v1/llm-credential` — remove this tenant's key.
///
/// Reports whether anything was removed rather than always `204`, so a caller
/// can distinguish "revoked" from "there was nothing there" — which matters
/// when the call is part of an incident response.
///
/// # Errors
/// Whatever Brain answers for the removal.
pub async fn delete(
    State(state): State<EdgeState>,
    headers: HeaderMap,
) -> Result<Json<DeleteLlmCredentialDto>, ApiError> {
    let mut req = LlmCredentialDeleteRequest {
        request_id: brain_db_sdk::new_id(),
        act_as: None,
    };
    let ident = state.resolve(&headers).await?;
    state.admit("llm_credential_delete", &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.delete_llm_credential(&req).await;
    state.record(
        "llm_credential_delete",
        &ident,
        Outcome::of(&out),
        started.elapsed(),
    );
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;

    if resp.existed {
        tracing::info!("BYOK provider key removed");
    }
    Ok(Json(DeleteLlmCredentialDto {
        deleted: resp.existed,
    }))
}

/// `POST /v1/llm-credential/test` — check a key WITHOUT registering it.
///
/// For trying a key before committing to it. Nothing is written and nothing
/// existing is touched, so this is safe to call against a namespace that
/// already has a working credential. It is the one credential route that does
/// not reach Brain: the key under test is the caller's, supplied in the body.
///
/// # Errors
/// `400` on a malformed body.
pub async fn test(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<PutLlmCredentialRequest>,
) -> Result<Json<LlmCredentialCheckDto>, ApiError> {
    // Authenticated even though nothing is stored: this makes an outbound
    // call to a provider, and an unauthenticated caller must not be able to
    // spend this edge's network on one.
    let ident = state.resolve(&headers).await?;
    state.admit("llm_credential_test", &ident).await?;

    let provider = body.validate().map_err(ApiError::bad_request)?;
    let upstream = Upstream::from_name(provider).ok_or_else(|| {
        // A provider this build cannot route is version skew, not a bad key —
        // reporting it as `rejected` would send an operator hunting for a
        // typo that is not there.
        ApiError::internal(format!(
            "provider `{provider}` is not routable by this build"
        ))
    })?;
    let check = validate_key(upstream, &body.api_key).await;
    Ok(Json(LlmCredentialCheckDto::from_check(check, provider)))
}

/// `POST /v1/llm-credential/verify` — re-check the key already registered.
///
/// A key that worked at registration can be revoked or run out of credit
/// later, and without this the first sign of it is an opaque upstream 401 on
/// a write — which reads like a Brain fault rather than an expired secret.
///
/// Brain answers this, not a probe from here, and that is the stronger check:
/// it resolves the credential through the same path derivation uses, so a pass
/// means the stages will actually work. An independent probe can pass while
/// derivation still fails.
///
/// # Errors
/// `404` when nothing is registered.
pub async fn verify(
    State(state): State<EdgeState>,
    headers: HeaderMap,
) -> Result<Json<LlmCredentialCheckDto>, ApiError> {
    let mut req = LlmCredentialVerifyRequest {
        request_id: brain_db_sdk::new_id(),
        act_as: None,
    };
    let ident = state.resolve(&headers).await?;
    state.admit("llm_credential_verify", &ident).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.verify_llm_credential(&req).await;
    state.record(
        "llm_credential_verify",
        &ident,
        Outcome::of(&out),
        started.elapsed(),
    );
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;

    if matches!(resp.outcome, VerifyOutcome::NoCredential) {
        return Err(ApiError::not_found(
            "no LLM provider key is registered for this namespace",
        ));
    }
    Ok(Json(LlmCredentialCheckDto::from_verify(
        resp.outcome,
        resp.provider,
        resp.detail,
    )))
}
