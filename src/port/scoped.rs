//! Carrying a tenant identity that is not the connection's own.
//!
//! Two deployments authenticate to Brain in two different ways, and this is
//! the seam between them.
//!
//! **Self-host** forwards the caller's own bearer token, so the Brain
//! connection *is* the tenant. Brain reads the identity off the connection and
//! `act_as` stays unset — the historical behaviour, and still the default.
//!
//! **The hosted gateway** authenticates one shared service principal and
//! multiplexes every tenant over it, so the connection says nothing about who
//! is calling. There the identity rides on the request as `act_as`, and Brain
//! resolves the effective namespace and space from that before its handler
//! runs.
//!
//! Which of the two applies is decided by the injected
//! [`CredentialResolver`](super::CredentialResolver) — never by the request.
//! That is the security property worth stating plainly: a client cannot ask to
//! act as someone else, because handlers overwrite `act_as` with what the
//! resolver returned, discarding whatever the body carried. The self-host
//! default resolves to `None`, so a deployment that plugs in nothing is
//! unreachable by this path at all.

use brain_db_sdk::wire::types::{
    ActAs, EncodeRequest, EncodeVectorDirectRequest, EntityCreateRequest, EntityGetRequest,
    EntityListRequest, EntityMergeRequest, EntityRenameRequest, EntityResolveRequest,
    EntityTombstoneRequest, EntityUnmergeRequest, EntityUpdateRequest, ExtractorListRequest,
    ForgetRequest, GetCapabilitiesRequest, GraphFetchRequest, LinkRequest,
    MaterializeProceduralRequest, MemoryInspectRequest, MemoryListRequest, PlanRequest,
    QueryExplainRequest, QueryTraceRequest, ReasonRequest, RecallRequest, RelationCreateRequest,
    RelationGetRequest, RelationListFromRequest, RelationListToRequest, RelationSupersedeRequest,
    RelationTombstoneRequest, RelationTraverseRequest, SchemaDropRequest, SchemaGetRequest,
    SchemaListRequest, SchemaReplaceRequest, SchemaUploadRequest, SchemaValidateRequest,
    SessionCreateRequest, SessionDeleteRequest, SessionListRequest, SpaceCreateRequest,
    SpaceDeleteRequest, SpaceListRequest, StatementCreateRequest, StatementGetRequest,
    StatementHistoryRequest, StatementListRequest, StatementRetractRequest,
    StatementSupersedeRequest, StatementTombstoneRequest, SubscribeRequest, TxnBeginRequest,
    UnlinkRequest,
};

/// A wire request that can be run on behalf of a tenant other than the one the
/// connection authenticated as.
///
/// Implemented for every request type in the protocol that carries an `act_as`
/// field. The blanket [`ResolvedCredential::scope`] is the only intended
/// caller, so a handler cannot set the identity to anything but what the
/// resolver returned.
///
/// [`ResolvedCredential::scope`]: super::ResolvedCredential::scope
pub trait Scoped {
    /// Overwrite the request's `act_as` with the resolved identity.
    fn set_act_as(&mut self, act_as: Option<ActAs>);
}

macro_rules! impl_scoped {
    ($($t:ty),+ $(,)?) => {
        $(
            impl Scoped for $t {
                fn set_act_as(&mut self, act_as: Option<ActAs>) {
                    self.act_as = act_as;
                }
            }
        )+
    };
}

// Every `*Request` in the wire protocol that has an `act_as` field — all 53
// of them. The protocol used to leave sixteen of the typed-graph and
// introspection verbs without one, so this list was once a subset and those
// verbs had to be refused outright for a hosted tenant.
//
// The two directions of drift are not equally protected, so it is worth being
// precise about which is which. Naming a type here that has no `act_as` field
// fails to compile, so that direction is safe. The reverse — a new request
// type that grows an `act_as` and is not added here — compiles fine and makes
// the verb silently run as the connection identity, which under the hosted
// gateway means running as the service principal instead of the tenant.
// Nothing catches that automatically today; adding a request type to the
// protocol means revisiting this list.
impl_scoped!(
    EncodeRequest,
    EntityCreateRequest,
    EntityGetRequest,
    EntityListRequest,
    EntityResolveRequest,
    ForgetRequest,
    GetCapabilitiesRequest,
    GraphFetchRequest,
    LinkRequest,
    MemoryInspectRequest,
    MemoryListRequest,
    PlanRequest,
    ReasonRequest,
    RecallRequest,
    RelationCreateRequest,
    RelationGetRequest,
    RelationListFromRequest,
    RelationListToRequest,
    RelationTraverseRequest,
    SchemaDropRequest,
    SchemaGetRequest,
    SchemaListRequest,
    SchemaReplaceRequest,
    SchemaUploadRequest,
    SchemaValidateRequest,
    SessionCreateRequest,
    SessionDeleteRequest,
    SessionListRequest,
    SpaceCreateRequest,
    SpaceDeleteRequest,
    SpaceListRequest,
    StatementCreateRequest,
    StatementGetRequest,
    StatementListRequest,
    SubscribeRequest,
    TxnBeginRequest,
    UnlinkRequest,
    // The sixteen that gained a selector when the protocol grew one for them.
    EntityUpdateRequest,
    EntityRenameRequest,
    EntityMergeRequest,
    EntityUnmergeRequest,
    EntityTombstoneRequest,
    StatementSupersedeRequest,
    StatementTombstoneRequest,
    StatementRetractRequest,
    StatementHistoryRequest,
    RelationSupersedeRequest,
    RelationTombstoneRequest,
    ExtractorListRequest,
    QueryExplainRequest,
    QueryTraceRequest,
    MaterializeProceduralRequest,
    EncodeVectorDirectRequest,
);
