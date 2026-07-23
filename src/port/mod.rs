//! The two seams that let one edge serve two deployments.
//!
//! `brain-edge` ships a self-host default: forward the caller's bearer token to
//! Brain (which resolves the identity itself) and meter nothing. The hosted Arc
//! gateway injects its own [`CredentialResolver`] (API key → tenant identity)
//! and [`MeteringSink`] (usage analytics) without forking the translation layer.
//!
//! Both ports are trait objects held in [`crate::EdgeState`], so the router and
//! handlers stay non-generic and the host app plugs in behavior at construction.

use axum::http::{header, HeaderMap};

use crate::error::ApiError;

/// The Brain wire credential the edge authenticates with, plus optional resolved
/// metadata for metering and logging.
#[derive(Clone, Debug)]
pub struct ResolvedCredential {
    /// Opaque token: the connection-pool cache key and — in the self-host
    /// default — the Brain wire credential forwarded downstream.
    pub credential: String,
    /// A resolved tenant label, when the resolver knows it. The gateway fills
    /// this from the API key (e.g. `namespace/agent`); the bearer default leaves
    /// it `None` because Brain, not the edge, resolves the identity there.
    pub tenant: Option<String>,
}

/// Resolve an incoming request to a Brain credential.
///
/// The default is [`BearerResolver`]. The hosted gateway supplies an
/// implementation that looks an API key up in its key store and returns the
/// tenant's Brain credential (and, later, a per-request identity).
#[async_trait::async_trait]
pub trait CredentialResolver: Send + Sync + 'static {
    /// Map request headers to a [`ResolvedCredential`], or a `401`/`403` error.
    async fn resolve(&self, headers: &HeaderMap) -> Result<ResolvedCredential, ApiError>;
}

/// The outcome of one data-plane operation, for metering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The operation completed successfully.
    Ok,
    /// The operation failed (transport, engine, or validation).
    Err,
}

impl Outcome {
    /// Classify a verb `Result` without inspecting its payload.
    #[must_use]
    pub fn of<T, E>(result: &Result<T, E>) -> Self {
        if result.is_ok() {
            Self::Ok
        } else {
            Self::Err
        }
    }
}

/// One metered event: which tenant ran which op, and whether it succeeded.
#[derive(Debug)]
pub struct MeterEvent<'a> {
    /// The resolved tenant label, if the resolver knew it.
    pub tenant: Option<&'a str>,
    /// The verb name (`"encode"`, `"recall"`, …).
    pub op: &'a str,
    /// Success or failure.
    pub outcome: Outcome,
}

/// Record usage. Called once per data-plane op; implementations must not block
/// the response path (fire-and-forget).
pub trait MeteringSink: Send + Sync + 'static {
    /// Record one operation's outcome.
    fn record(&self, event: &MeterEvent<'_>);
}

/// The self-host default resolver: forward `Authorization: Bearer <key>` (or
/// `X-API-Key`) to Brain, which resolves the identity itself.
pub struct BearerResolver;

#[async_trait::async_trait]
impl CredentialResolver for BearerResolver {
    async fn resolve(&self, headers: &HeaderMap) -> Result<ResolvedCredential, ApiError> {
        Ok(ResolvedCredential {
            credential: bearer_token(headers)?,
            tenant: None,
        })
    }
}

/// The self-host default sink: record nothing.
pub struct NoopMeter;

impl MeteringSink for NoopMeter {
    fn record(&self, _event: &MeterEvent<'_>) {}
}

/// Read the API key from `Authorization: Bearer …` or `X-API-Key`.
///
/// # Errors
/// Returns a `401` if neither header carries a non-empty token.
pub fn bearer_token(headers: &HeaderMap) -> Result<String, ApiError> {
    if let Some(raw) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        let stripped = raw
            .strip_prefix("Bearer ")
            .or_else(|| raw.strip_prefix("bearer "));
        if let Some(tok) = stripped {
            let tok = tok.trim();
            if !tok.is_empty() {
                return Ok(tok.to_string());
            }
        }
    }
    if let Some(raw) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
        let tok = raw.trim();
        if !tok.is_empty() {
            return Ok(tok.to_string());
        }
    }
    Err(ApiError::unauthorized(
        "missing API key — send `Authorization: Bearer <key>` or `X-API-Key`",
    ))
}
