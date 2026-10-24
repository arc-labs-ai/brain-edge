//! The delegated-identity seam: what `act_as` does to a request, and what the
//! edge does with a verb that cannot carry one.

use brain_db_sdk::wire::types::{
    ActAs, EncodeRequest, EntityUpdateRequest, ExtractorListRequest, MemoryInspectRequest,
    RecallRequest, SpacePermissions, StatementHistoryRequest, permissions,
};
use brain_db_sdk::{EncodeBuilder, RecallBuilder};
use brain_edge::port::ResolvedCredential;

fn encode_request() -> EncodeRequest {
    EncodeBuilder::new("a memory").build()
}

fn recall_request() -> RecallRequest {
    RecallBuilder::new("a cue").build()
}

fn hosted() -> ResolvedCredential {
    ResolvedCredential {
        credential: "service-principal".into(),
        tenant: Some("acme/prod".into()),
        act_as: Some(ActAs {
            namespace: "acme".into(),
            space: "support-bot:user123".into(),
            grant: 0,
        }),
    }
}

fn self_host() -> ResolvedCredential {
    ResolvedCredential {
        credential: "brain_tenant_key".into(),
        tenant: None,
        act_as: None,
    }
}

#[test]
fn scoping_stamps_the_resolved_identity_onto_a_request() {
    let mut req = encode_request();
    hosted().scope(&mut req);
    let act_as = req.act_as.expect("hosted identity must reach the wire");
    assert_eq!(act_as.namespace, "acme");
    assert_eq!(act_as.space, "support-bot:user123");
}

#[test]
fn the_self_host_default_leaves_the_connection_as_the_identity() {
    let mut req = recall_request();
    self_host().scope(&mut req);
    assert!(
        req.act_as.is_none(),
        "self-host must not claim to act as anyone; Brain reads the identity \
         off the connection"
    );
}

/// The security property the whole seam rests on: a client cannot choose who
/// it acts as. A body that arrives carrying an `act_as` has it replaced by
/// what the resolver returned — including replaced with `None`.
#[test]
fn a_client_supplied_act_as_is_discarded() {
    let mut req = MemoryInspectRequest {
        memory_id: 7,
        act_as: Some(ActAs {
            namespace: "victim".into(),
            space: "victim-space".into(),
            grant: u32::MAX,
        }),
    };

    hosted().scope(&mut req);
    let act_as = req.act_as.clone().expect("resolver identity");
    assert_eq!(act_as.namespace, "acme", "the forged namespace must not survive");
    assert_eq!(act_as.grant, 0, "the forged grant must not survive");

    // And the same request against a self-host deployment loses it entirely,
    // rather than being forwarded to Brain as a delegation attempt.
    self_host().scope(&mut req);
    assert!(req.act_as.is_none(), "the forged identity must not survive");
}

#[test]
fn every_scoped_request_type_round_trips() {
    // One representative per request shape the handlers build differently:
    // builder-produced, DTO-produced, and struct-literal.
    let ident = hosted();
    let mut encode = encode_request();
    let mut recall = recall_request();
    let mut inspect = MemoryInspectRequest {
        memory_id: 1,
        act_as: None,
    };
    ident.scope(&mut encode);
    ident.scope(&mut recall);
    ident.scope(&mut inspect);
    assert!(encode.act_as.is_some() && recall.act_as.is_some() && inspect.act_as.is_some());
}

/// The verbs that once had nowhere to put an identity. Each is a typed-graph
/// mutation, a query-introspection call or a single-space op that a hosted
/// tenant could not be offered at all until the protocol grew `act_as` for
/// them — with no field to carry the tenant, they would have run as the
/// gateway's own principal in the gateway's own space.
#[test]
fn the_formerly_non_delegable_verbs_now_carry_an_identity() {
    let ident = hosted();

    let mut update = EntityUpdateRequest {
        entity_id: [0u8; 16],
        canonical_name: "n".into(),
        aliases: Vec::new(),
        attributes_blob: Vec::new(),
        request_id: [1u8; 16],
        act_as: None,
    };
    let mut history = StatementHistoryRequest {
        anchor_id: [0u8; 16],
        include_tombstoned: true,
        limit: 10,
        cursor: Vec::new(),
        act_as: None,
    };
    let mut extractors = ExtractorListRequest::default();

    ident.scope(&mut update);
    ident.scope(&mut history);
    ident.scope(&mut extractors);

    for (name, act_as) in [
        ("entity_update", update.act_as),
        ("statement_history", history.act_as),
        ("extractor_list", extractors.act_as),
    ] {
        let a = act_as.unwrap_or_else(|| panic!("{name} lost its identity"));
        assert_eq!(a.namespace, "acme", "{name}");
        assert_eq!(a.space, "support-bot:user123", "{name}");
    }
}

/// What Brain will actually enforce for a delegated identity, mirrored so
/// `/v1/whoami` can report it rather than the service principal's own powers.
#[test]
fn delegated_permissions_are_the_standard_mask_not_the_delegators() {
    // A gateway principal that can do everything, including act-as.
    let delegator = SpacePermissions {
        can_encode: true,
        can_recall: true,
        can_plan: true,
        can_reason: true,
        can_forget: true,
        can_admin: true,
        can_act_as: true,
    }
    .to_bits();

    // grant = 0 is what the gateway sends: the tenant gets the standard mask.
    let tenant = SpacePermissions::from_bits(permissions::delegated(delegator, 0));
    assert!(tenant.can_encode && tenant.can_recall && tenant.can_forget);
    assert!(
        !tenant.can_admin,
        "a tenant must never inherit the gateway's admin rights"
    );
    assert!(
        !tenant.can_act_as,
        "a delegated op can never itself delegate"
    );
}

#[test]
fn a_grant_cannot_exceed_what_the_delegator_holds() {
    // A delegator WITHOUT admin asking to grant admin.
    let delegator = SpacePermissions {
        can_encode: true,
        can_recall: true,
        can_plan: true,
        can_reason: true,
        can_forget: true,
        can_admin: false,
        can_act_as: true,
    }
    .to_bits();

    let tenant =
        SpacePermissions::from_bits(permissions::delegated(delegator, permissions::ADMIN));
    assert!(
        !tenant.can_admin,
        "a grant is bounded by the delegator's own bits"
    );
}
