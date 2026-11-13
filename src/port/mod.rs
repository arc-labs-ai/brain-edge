//! The two seams that let one edge serve two deployments.
//!
//! `brain-edge` ships a self-host default: forward the caller's bearer token to
//! Brain (which resolves the identity itself) and meter nothing. The hosted Arc
//! gateway injects its own [`CredentialResolver`] (API key → tenant identity)
//! and [`MeteringSink`] (usage analytics) without forking the translation layer.
//!
//! Both ports are trait objects held in [`crate::EdgeState`], so the router and
//! handlers stay non-generic and the host app plugs in behavior at construction.

pub mod scoped;

pub use scoped::Scoped;

use std::time::Duration;

use axum::http::{HeaderMap, header};
use brain_db_sdk::wire::types::ActAs;
use brain_db_sdk::{ConnectionInfo, derive_space_id};

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
    /// The identity to run the request as, for a deployment whose Brain
    /// connection is not the tenant.
    ///
    /// `None` — the self-host default — means the connection *is* the tenant
    /// and Brain reads the identity off it. The hosted gateway authenticates
    /// one shared service principal for every tenant, so it fills this from
    /// the API key's scope and Brain resolves the effective namespace and
    /// space from it instead. See [`scoped`] for why only the resolver may set
    /// this.
    pub act_as: Option<ActAs>,
}

impl ResolvedCredential {
    /// Stamp this identity onto a request, replacing whatever `act_as` it
    /// carried.
    ///
    /// Unconditional, including when the identity is `None`: a client that put
    /// an `act_as` in its own body must not keep it, or the hosted gateway
    /// would let any tenant address any other tenant's space.
    pub fn scope<R: Scoped>(&self, request: &mut R) {
        request.set_act_as(self.act_as.clone());
    }

    /// The namespace this request runs in.
    ///
    /// A handler that needs the caller's namespace must ask here rather than
    /// read `connection().namespace`: under the hosted gateway the connection
    /// is a shared service principal, so its namespace is the gateway's, not
    /// the tenant's.
    #[must_use]
    pub fn namespace<'a>(&'a self, connection: &'a ConnectionInfo) -> &'a str {
        self.act_as
            .as_ref()
            .map_or(connection.namespace.as_str(), |a| a.namespace.as_str())
    }

    /// The effective space id this request runs in.
    ///
    /// Brain derives the storage id from the `act_as` space *string*, so this
    /// cannot be parsed out of it — [`derive_space_id`] repeats the server's
    /// own `UUIDv5(UUIDv5(ROOT, namespace), space)`. An empty space string
    /// means "the principal's own space", matching Brain's check at ingress.
    #[must_use]
    pub fn space_id(&self, connection: &ConnectionInfo) -> [u8; 16] {
        match &self.act_as {
            Some(a) if !a.space.is_empty() => derive_space_id(&a.namespace, &a.space),
            _ => connection.space_id,
        }
    }
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
        if result.is_ok() { Self::Ok } else { Self::Err }
    }
}

/// Signals a RECALL carried back, for a sink that reports answer quality
/// rather than just call counts.
///
/// Separate from the universal fields because only one verb produces them and
/// a sink that does not care should not pay for an `Option` on every op.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RecallSignals {
    /// Memories returned.
    pub returned_n: u32,
    /// Confidence of the top answer.
    pub confidence: f32,
    /// Whether the answer was grounded in retrieved memories.
    pub grounded: bool,
    /// Candidates the semantic lane contributed before fusion.
    pub semantic_candidates: u32,
    /// Candidates the lexical lane contributed before fusion.
    pub lexical_candidates: u32,
    /// Candidates the graph lane contributed before fusion.
    pub graph_candidates: u32,
    /// Candidates dropped for being stale — a suppressed wrong answer, which
    /// is the one signal here that reports something that did *not* happen.
    pub stale_prevented: u32,
}

/// One metered event: who ran which op, how it went, and how long it took.
///
/// Deliberately does **not** carry [`ResolvedCredential::credential`]. That is
/// the Brain wire token; a usage sink has no business seeing it, and passing
/// the whole credential would hand it to every implementation by default.
#[derive(Debug)]
pub struct MeterEvent<'a> {
    /// The resolved tenant label, if the resolver knew it.
    pub tenant: Option<&'a str>,
    /// The identity the op ran as, for a deployment that carries one. Gives a
    /// sink the effective namespace and space; `None` means the op ran as the
    /// connection itself, which the sink already knows.
    pub act_as: Option<&'a ActAs>,
    /// The verb name (`"encode"`, `"recall"`, …).
    pub op: &'a str,
    /// Success or failure.
    pub outcome: Outcome,
    /// Engine round-trip time — the Brain call only, not the HTTP request.
    /// A host that wants end-to-end latency measures that at its own edge.
    ///
    /// `None` where there is nothing to time: an op answered without touching
    /// Brain, or the wire proxy, which splices frames without pairing request
    /// to response. Deliberately not `Duration::ZERO` — a sink averaging these
    /// would silently pull its own numbers down.
    pub latency: Option<Duration>,
    /// Set only for RECALL.
    pub recall: Option<RecallSignals>,
    /// RECALL's read-pipeline trace, serialized, when one was fetched.
    ///
    /// Borrowed: a sink that does not keep traces pays nothing for this. A
    /// host that runs a trace explorer is the reason the blob is here at all —
    /// [`RecallSignals`] is the derived summary, this is the detail behind it.
    pub trace: Option<&'a str>,
}

/// Record usage. Called once per data-plane op; implementations must not block
/// the response path (fire-and-forget).
#[async_trait::async_trait]
pub trait MeteringSink: Send + Sync + 'static {
    /// Record one operation's outcome.
    fn record(&self, event: &MeterEvent<'_>);

    /// Decide whether an op may run, *before* it runs.
    ///
    /// [`Self::record`] reports what happened; this is the other half — the
    /// chance to refuse. A host that bills by usage needs somewhere to say
    /// "this tenant is over its plan", and the only place that knows both the
    /// op and the caller before any work happens is here: the router sees a
    /// path, not a verb, and the engine sees a request with no notion of a
    /// plan.
    ///
    /// Called for every op, so an implementation that bills only some of them
    /// should return early on the rest rather than pay for a lookup. The
    /// default admits everything, which is what self-host wants: there is no
    /// plan to be over.
    ///
    /// Returning `Err` fails the request with that error and nothing runs.
    async fn admit(&self, op: &str, ident: &ResolvedCredential) -> Result<(), ApiError> {
        let _ = (op, ident);
        Ok(())
    }

    /// Whether RECALL should ask Brain for its read-pipeline trace even when
    /// the caller did not.
    ///
    /// The trace is what [`MeterEvent::recall`]'s candidate and
    /// stale-prevented counts are derived from, so a sink that reports answer
    /// quality needs it on every call rather than only when a client opts in.
    /// It is not free — Brain computes it and it rides the wire — so this is
    /// off by default and self-host pays nothing. A hosted deployment that
    /// feeds an analytics pipeline turns it on.
    ///
    /// Turning it on does not put the trace in the HTTP response; that still
    /// follows the request's own `include_trace`.
    fn wants_recall_trace(&self) -> bool {
        false
    }
}

/// The self-host default resolver: forward `Authorization: Bearer <key>` (or
/// `X-API-Key`) to Brain, which resolves the identity itself.
#[derive(Debug)]
pub struct BearerResolver;

#[async_trait::async_trait]
impl CredentialResolver for BearerResolver {
    async fn resolve(&self, headers: &HeaderMap) -> Result<ResolvedCredential, ApiError> {
        Ok(ResolvedCredential {
            credential: bearer_token(headers)?,
            tenant: None,
            // The forwarded token IS the tenant: Brain reads the identity off
            // the connection, so there is nothing to act as.
            act_as: None,
        })
    }
}

/// The self-host default sink: record nothing.
#[derive(Debug)]
pub struct NoopMeter;

impl MeteringSink for NoopMeter {
    fn record(&self, _event: &MeterEvent<'_>) {}
}

/// Read the API key from `Authorization: Bearer …` or `X-API-Key`.
///
/// # Errors
/// Returns a `401` if neither header carries a non-empty token.
pub fn bearer_token(headers: &HeaderMap) -> Result<String, ApiError> {
    if let Some(raw) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
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
