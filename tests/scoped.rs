//! The delegated-identity seam: what `act_as` does to a request, and what the
//! edge does with a verb that cannot carry one.

use brain_db_sdk::wire::types::{
    ActAs, EncodeRequest, EntityUpdateRequest, ExtractorListRequest, MemoryInspectRequest,
    RecallRequest, SpacePermissions, StatementHistoryRequest, permissions,
};
use brain_db_sdk::{EncodeBuilder, RecallBuilder};
use brain_edge::port::{
    MeterEvent, MeteringSink, NoopMeter, Outcome, RecallSignals, ResolvedCredential,
};
use std::time::Duration;

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
    assert_eq!(
        act_as.namespace, "acme",
        "the forged namespace must not survive"
    );
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

    let tenant = SpacePermissions::from_bits(permissions::delegated(delegator, permissions::ADMIN));
    assert!(
        !tenant.can_admin,
        "a grant is bounded by the delegator's own bits"
    );
}

// --- the metering port ------------------------------------------------------

/// A sink that keeps whole events, so the fields added for the hosted
/// deployment can be asserted rather than just the tenant label.
/// One recorded event, flattened to what these tests assert on.
type SeenEvent = (
    String,
    Option<ActAs>,
    Option<Duration>,
    Option<RecallSignals>,
);

#[derive(Default)]
struct CapturingMeter {
    seen: std::sync::Mutex<Vec<SeenEvent>>,
    wants_trace: bool,
}

impl MeteringSink for CapturingMeter {
    fn record(&self, event: &MeterEvent<'_>) {
        self.seen.lock().expect("lock").push((
            event.op.to_owned(),
            event.act_as.cloned(),
            event.latency,
            event.recall,
        ));
    }
    fn wants_recall_trace(&self) -> bool {
        self.wants_trace
    }
}

/// The sink has to be able to attribute usage to the tenant that incurred it.
/// Under a shared service principal the connection cannot say who that was, so
/// the delegated identity has to reach the event.
#[test]
fn a_metered_event_carries_the_delegated_identity_and_its_latency() {
    let meter = CapturingMeter::default();
    let ident = hosted();
    meter.record(&MeterEvent {
        tenant: ident.tenant.as_deref(),
        act_as: ident.act_as.as_ref(),
        op: "recall",
        outcome: Outcome::Ok,
        latency: Some(Duration::from_millis(12)),
        recall: None,
        trace: None,
    });

    let seen = meter.seen.lock().expect("lock");
    let (op, act_as, latency, _) = seen.first().expect("one event");
    assert_eq!(op, "recall");
    assert_eq!(
        act_as.as_ref().map(|a| a.namespace.as_str()),
        Some("acme"),
        "usage must be attributable to the tenant, not the service principal"
    );
    assert_eq!(*latency, Some(Duration::from_millis(12)));
}

/// `None` rather than `Duration::ZERO` for an op with nothing to time. A sink
/// averaging latencies must be able to tell "instant" from "not measured", or
/// the wire proxy's unpaired frames drag every average toward zero.
#[test]
fn an_untimed_op_reports_no_latency_rather_than_zero() {
    let meter = CapturingMeter::default();
    meter.record(&MeterEvent {
        tenant: None,
        act_as: None,
        op: "whoami",
        outcome: Outcome::Ok,
        latency: None,
        recall: None,
        trace: None,
    });
    let seen = meter.seen.lock().expect("lock");
    assert_eq!(seen[0].2, None);
}

/// Asking Brain for the read-pipeline trace costs engine compute and wire
/// bytes on every recall, so self-host must not pay for it by default.
#[test]
fn recall_tracing_is_opt_in_per_deployment() {
    assert!(
        !NoopMeter.wants_recall_trace(),
        "the self-host default must not turn on tracing it never reads"
    );
    assert!(
        !CapturingMeter::default().wants_recall_trace(),
        "a sink that does not override must default to off"
    );
    let analytics = CapturingMeter {
        wants_trace: true,
        ..CapturingMeter::default()
    };
    assert!(analytics.wants_recall_trace());
}

/// A sink that refuses one op, to prove `admit` is a real gate rather than a
/// hook nothing consults.
struct RefusingMeter {
    refuse: &'static str,
}

#[async_trait::async_trait]
impl MeteringSink for RefusingMeter {
    fn record(&self, _event: &MeterEvent<'_>) {}
    async fn admit(
        &self,
        op: &str,
        _ident: &ResolvedCredential,
    ) -> Result<(), brain_edge::error::ApiError> {
        if op == self.refuse {
            return Err(brain_edge::error::ApiError::forbidden("over plan"));
        }
        Ok(())
    }
}

#[tokio::test]
async fn admit_refuses_the_named_op_and_admits_the_rest() {
    let meter = RefusingMeter { refuse: "encode" };
    let ident = hosted();
    assert!(meter.admit("encode", &ident).await.is_err());
    assert!(meter.admit("recall", &ident).await.is_ok());
}

/// Self-host has no plan to be over, so the default must not refuse anything.
#[tokio::test]
async fn the_default_sink_admits_every_op() {
    let ident = self_host();
    for op in ["encode", "recall", "query_trace", "whoami"] {
        assert!(
            NoopMeter.admit(op, &ident).await.is_ok(),
            "`{op}` must be admitted by default"
        );
    }
}

// --- the self-host contract -------------------------------------------------

/// What self-host is promised, asserted in one place.
///
/// The open binary serves the whole API and the hosted product differentiates
/// on understanding a tenant's memory, not on withholding capability. Every
/// seam added for the hosted path therefore has to default to "the host is not
/// here", and each of these is a way that could quietly stop being true:
///
/// - `admit` refusing an op would make self-host enforce a plan it does not
///   have.
/// - `wants_recall_trace` returning true would charge every self-hosted recall
///   engine time for a trace nobody reads.
/// - `act_as` being set would make Brain resolve an identity the connection
///   did not authenticate as.
/// - `record` doing anything would be telemetry leaving a deployment that
///   asked for none.
#[tokio::test]
async fn the_self_host_defaults_cost_nothing_and_refuse_nothing() {
    let ident = self_host();

    assert!(
        ident.act_as.is_none(),
        "self-host's connection IS the tenant; a delegated identity would be wrong"
    );

    for op in [
        "encode",
        "recall",
        "query_trace",
        "update_entity",
        "statement_history",
        "subscribe",
        "whoami",
    ] {
        assert!(
            NoopMeter.admit(op, &ident).await.is_ok(),
            "`{op}` must be admitted — self-host has no plan to be over"
        );
    }

    assert!(
        !NoopMeter.wants_recall_trace(),
        "self-host must not pay engine time for a trace no sink reads"
    );

    // Records nothing, and cannot panic doing it.
    NoopMeter.record(&MeterEvent {
        tenant: None,
        act_as: None,
        op: "encode",
        outcome: Outcome::Ok,
        latency: Some(Duration::from_millis(1)),
        recall: None,
        trace: None,
    });
}
