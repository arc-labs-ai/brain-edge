//! Handler happy-path tests driven end to end through the real axum router and
//! a real Brain wire client, against an in-process **mock Brain** TCP server.
//!
//! `tests/ports.rs` proves every route consults the resolver (with a *rejecting*
//! resolver, so no route ever reaches Brain). This file is the complementary
//! half: an **accepting** resolver, a pool pointed at a mock Brain that does the
//! HELLO/WELCOME/AUTH/AUTH_OK handshake and answers each op frame with a canned
//! response, so a request runs all the way through
//! `resolve -> client_for -> <verb> -> DTO` and back out as JSON. Each test
//! asserts both the HTTP status and the response body.
//!
//! The mock is deliberately minimal — a single dispatcher over the ops these
//! tests exercise. Ops it is never sent (e.g. LINK, TRAVERSE) are simply not in
//! the match; adding a route to this file means adding its op to `dispatch`.

use std::sync::Arc;
use std::sync::Mutex;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use tokio::net::{TcpListener, TcpStream};
use tower::ServiceExt;

use brain_db_sdk::transport::{read_frame, write_frame};
use brain_db_sdk::wire::cbor::{from_cbor_bytes, to_cbor_bytes};
use brain_db_sdk::wire::frame::{FLAG_EOS, Frame};
use brain_db_sdk::wire::opcode::Opcode;
use brain_db_sdk::wire::types::{
    AuthOkPayload, AuthPayload, Capabilities, EdgeKindWire, EncodeResponse, EncodeStageArtifact,
    EntityCreateResponse, EntityGetResponse, EntityListItem, EntityListResponseFrame,
    EntityResolveResponse, EntityView, EvidenceRefWire, ForgetResponse, GetCapabilitiesResponse,
    GraphEdge, GraphFetchResponseFrame, GraphNode, HelloPayload, InferenceKind, InferenceStep,
    LinkResponse, MemoryInspectResponse, MemoryKindWire, MemoryListItem, MemoryListResponseFrame,
    MemoryResult, PlanResponseFrame, PlanStatus, PlanStep, ReasonResponseFrame, ReasonStatus,
    RecallResponseFrame, RelationGetResponse, RelationListFromResponseFrame,
    RelationListToResponseFrame, RelationTraverseResponseFrame, RelationView,
    ResolutionOutcomeWire, SchemaGetResponse, SchemaReplaceResponse, SchemaUploadResponse,
    SchemaValidateResponse, ServerFeatures, SpacePermissions, StatementGetResponse,
    StatementKindWire, StatementListResponseFrame, StatementObjectWire, StatementValueWire,
    StatementView, TransitionKind, TraversalPathWire, TraversalStepWire, UnlinkResponse,
    WelcomePayload,
};
use brain_edge::port::{CredentialResolver, MeterEvent, MeteringSink, Outcome, ResolvedCredential};
use brain_edge::{ApiError, EdgeConfig, EdgeState};

// --- test doubles ----------------------------------------------------------

/// A resolver that always accepts, standing in for the gateway's API-key path.
#[derive(Debug)]
struct AcceptingResolver;

#[async_trait::async_trait]
impl CredentialResolver for AcceptingResolver {
    async fn resolve(&self, _headers: &HeaderMap) -> Result<ResolvedCredential, ApiError> {
        Ok(ResolvedCredential {
            credential: "test-key".into(),
            tenant: Some("acme/prod".into()),
        })
    }
}

/// Records `(op, outcome)` so a test can assert the handler metered its verb.
#[derive(Default)]
struct RecordingMeter {
    events: Mutex<Vec<(String, Outcome)>>,
}

impl RecordingMeter {
    fn ops(&self) -> Vec<String> {
        self.events
            .lock()
            .expect("events lock")
            .iter()
            .map(|(op, _)| op.clone())
            .collect()
    }
}

impl MeteringSink for RecordingMeter {
    fn record(&self, event: &MeterEvent<'_>) {
        self.events
            .lock()
            .expect("events lock")
            .push((event.op.to_owned(), event.outcome));
    }
}

// --- the mock Brain server -------------------------------------------------

/// A 16-byte id filled with `seed`, for readable assertions.
fn id16(seed: u8) -> [u8; 16] {
    [seed; 16]
}

fn sample_entity_view() -> EntityView {
    EntityView {
        entity_id: id16(0x07),
        entity_type_id: 1,
        canonical_name: "Alice".into(),
        normalized_name: "alice".into(),
        aliases: vec!["A.".into()],
        attributes_blob: Vec::new(),
        mention_count: 3,
        created_at_unix_nanos: 1,
        updated_at_unix_nanos: 2,
        merged_into: [0; 16],
        embedding_version: 1,
        flags: 0,
    }
}

fn sample_relation_view() -> RelationView {
    RelationView {
        relation_id: id16(0x10),
        chain_root: id16(0x10),
        relation_type: "brain:knows".into(),
        from_entity: id16(0x11),
        to_entity: id16(0x12),
        properties_blob: Vec::new(),
        evidence: EvidenceRefWire::Inline(Vec::new()),
        extractor_id: 0,
        extracted_at_unix_nanos: 1,
        confidence: 0.9,
        valid_from_unix_nanos: 0,
        valid_to_unix_nanos: 0,
        version: 1,
        superseded_by: [0; 16],
        supersedes: [0; 16],
        tombstoned: false,
        tombstoned_at_unix_nanos: 0,
        flags: 0,
    }
}

fn sample_memory_result(tag: u8, text: &str) -> MemoryResult {
    MemoryResult {
        memory_id: u128::from(tag),
        text: text.into(),
        similarity_score: 0.9,
        confidence: 0.8,
        salience: 0.5,
        kind: MemoryKindWire::Semantic,
        space_id: [tag; 16],
        session_id: 0,
        created_at_unix_nanos: 1,
        last_accessed_at_unix_nanos: 1,
        edges: None,
        contributing_retrievers: vec![],
        fused_score: 0.7,
        rerank_score: None,
        salience_initial: 0.5,
        access_count: 0,
        lsn: 1,
        flags: 0,
        consolidated_at_unix_nanos: None,
        occurred_at_unix_nanos: None,
        edges_out_count: 0,
        edges_in_count: 0,
        graph: None,
    }
}

fn sample_statement_view() -> StatementView {
    StatementView {
        statement_id: id16(0x20),
        kind: StatementKindWire::Fact,
        subject: id16(0x21),
        subject_pending_audit_id: [0; 16],
        predicate: "brain:works_at".into(),
        object: StatementObjectWire::Value(StatementValueWire::Text("Acme".into())),
        confidence: 0.95,
        evidence: EvidenceRefWire::Inline(Vec::new()),
        extractor_id: 0,
        extracted_at_unix_nanos: 1,
        schema_version: 1,
        valid_from_unix_nanos: 10,
        valid_to_unix_nanos: 0,
        event_at_unix_nanos: 5,
        version: 1,
        superseded_by: [0; 16],
        supersedes: [0; 16],
        chain_root: id16(0x20),
        tombstoned: false,
        tombstoned_at_unix_nanos: 0,
        tombstone_reason: 0,
        flags: 0,
        is_stateful: false,
    }
}

fn sample_traversal_step() -> TraversalStepWire {
    TraversalStepWire {
        relation_id: id16(0x30),
        from: id16(0x31),
        to: id16(0x32),
        relation_type: "brain:knows".into(),
        depth: 1,
    }
}

fn sample_memory_list_item(tag: u8) -> MemoryListItem {
    MemoryListItem {
        memory_id: id16(tag),
        space_id: [0; 16],
        session_id: 0,
        text: "row".into(),
        kind: 1,
        state: 0,
        created_at_unix_nanos: 10,
        occurred_at_unix_nanos: 0,
        last_accessed_at_unix_nanos: 11,
        salience: 0.5,
        access_count: 2,
        source_request_id: [0; 16],
        statement_count: 1,
        entity_count: 2,
        relation_count: 3,
    }
}

fn sample_plan_step(step_index: u32, text: &str, transition_kind: TransitionKind) -> PlanStep {
    PlanStep {
        step_index,
        memory_id: u128::from(step_index) + 1,
        text: text.into(),
        transition_kind,
        confidence: 0.9,
        estimated_distance_to_goal: 1.0,
    }
}

fn sample_inference_step(step_index: u32, claim: &str) -> InferenceStep {
    InferenceStep {
        step_index,
        claim: claim.into(),
        supporting_memories: vec![1, 2],
        contradicting_memories: vec![],
        confidence: 0.8,
        inference_kind: InferenceKind::CausalExplanation,
    }
}

async fn send_frame<T: serde::Serialize + Sync>(
    sock: &mut TcpStream,
    op: Opcode,
    sid: u32,
    payload: &T,
) {
    let frame = Frame::new(op.as_u16(), FLAG_EOS, sid, to_cbor_bytes(payload));
    write_frame(sock, &frame).await.expect("write frame");
}

/// Send one frame of a streamed response, setting the EOS flag only on the
/// final frame. Used by the multi-frame reads to prove the client flattens
/// across frames and stops at EOS.
async fn send_stream_frame<T: serde::Serialize + Sync>(
    sock: &mut TcpStream,
    op: Opcode,
    sid: u32,
    payload: &T,
    is_final: bool,
) {
    let flags = if is_final { FLAG_EOS } else { 0 };
    let frame = Frame::new(op.as_u16(), flags, sid, to_cbor_bytes(payload));
    write_frame(sock, &frame).await.expect("write frame");
}

/// Run the handshake, then dispatch every op frame until the socket closes.
async fn serve(mut sock: TcpStream) {
    let mut buf = Vec::new();

    let hello_frame = read_frame(&mut sock, &mut buf).await.expect("hello");
    let hello: HelloPayload = from_cbor_bytes(&hello_frame.payload).expect("decode hello");
    let welcome = WelcomePayload {
        server_id: "mock-brain".into(),
        chosen_version: 1,
        connection_id: [0xAB; 16],
        capabilities: hello.capabilities,
        server_features: ServerFeatures {
            max_payload_size: 1 << 20,
            max_concurrent_streams: 64,
            idle_timeout_seconds: 300,
            auth_methods: vec![],
        },
    };
    send_frame(&mut sock, Opcode::Welcome, 0, &welcome).await;

    let auth_frame = read_frame(&mut sock, &mut buf).await.expect("auth");
    let _auth: AuthPayload = from_cbor_bytes(&auth_frame.payload).expect("decode auth");
    let auth_ok = AuthOkPayload {
        // whoami surfaces these handshake fields directly (no wire op).
        space_id: {
            let mut s = [0u8; 16];
            s[0] = 0x11;
            s[15] = 0x2a;
            s
        },
        bound_shard_id: 0,
        permissions: SpacePermissions {
            can_act_as: false,
            can_encode: true,
            can_recall: true,
            can_plan: true,
            can_reason: true,
            can_forget: true,
            can_admin: false,
        },
        namespace: "acme".into(),
        server_time_unix_nanos: 1,
    };
    send_frame(&mut sock, Opcode::AuthOk, 0, &auth_ok).await;

    // Dispatch loop: one canned reply per op, keyed by opcode. The client
    // closes with BYE on drop, at which point the read errors and we return.
    while let Ok(f) = read_frame(&mut sock, &mut buf).await {
        dispatch(&mut sock, &f).await;
        if f.opcode == Opcode::Bye.as_u16() {
            break;
        }
    }
}

async fn dispatch(sock: &mut TcpStream, f: &Frame) {
    let sid = f.stream_id;
    let op = f.opcode;

    if op == Opcode::EncodeReq.as_u16() {
        send_frame(
            sock,
            Opcode::EncodeResp,
            sid,
            &EncodeResponse {
                memory_id: 12345,
                was_deduplicated: false,
                salience: 0.42,
                auto_edges_added: 2,
                lsn: 1,
                space_id: [0; 16],
                session_id: 0,
                kind: MemoryKindWire::Semantic,
                created_at_unix_nanos: 1_700_000_000,
                edges_out_count: 2,
                embedding_model_fp: [0; 16],
                pending_stages: vec![],
                has_active_schema: true,
                trace: None,
            },
        )
        .await;
    } else if op == Opcode::RecallReq.as_u16() {
        // Streamed verb: a single EOS frame carrying two hits.
        send_frame(
            sock,
            Opcode::RecallResp,
            sid,
            &RecallResponseFrame {
                answer_kind: brain_db_sdk::wire::types::AnswerKindWire::Many,
                memories: vec![
                    sample_memory_result(0xAA, "first hit"),
                    sample_memory_result(0xBB, "second hit"),
                ],
                is_final: true,
                cumulative_count: 2,
                estimated_remaining: Some(0),
                trace: None,
            },
        )
        .await;
    } else if op == Opcode::GetCapabilitiesReq.as_u16() {
        send_frame(
            sock,
            Opcode::GetCapabilitiesResp,
            sid,
            &GetCapabilitiesResponse {
                capabilities: Capabilities {
                    rerank: true,
                    llm_extractor: false,
                    classifier_extractor: true,
                    pattern_extractor: true,
                    schema_namespaces: vec!["brain".into()],
                    vector_dim: 384,
                },
            },
        )
        .await;
    } else {
        // Typed-graph ops (and BYE / anything unexpected: no reply).
        dispatch_graph(sock, f).await;
    }
}

/// The typed-graph half of the dispatcher, split out only to keep each function
/// under the line ceiling.
async fn dispatch_graph(sock: &mut TcpStream, f: &Frame) {
    let sid = f.stream_id;
    let op = f.opcode;

    if op == Opcode::EntityGetReq.as_u16() {
        send_frame(
            sock,
            Opcode::EntityGetResp,
            sid,
            &EntityGetResponse {
                entity: sample_entity_view(),
                resolved_from: Vec::new(),
            },
        )
        .await;
    } else if op == Opcode::EntityResolveReq.as_u16() {
        send_frame(
            sock,
            Opcode::EntityResolveResp,
            sid,
            &EntityResolveResponse {
                outcome: ResolutionOutcomeWire::Resolved,
                tier: 1,
                confidence: 1.0,
                resolved_entity: id16(0x07),
                candidate_ids: Vec::new(),
                audit_id: [0; 16],
            },
        )
        .await;
    } else if op == Opcode::SchemaGetReq.as_u16() {
        send_frame(
            sock,
            Opcode::SchemaGetResp,
            sid,
            &SchemaGetResponse {
                namespace: "people".into(),
                schema_version: 2,
                schema_document: "entity Person {}".into(),
                source_blob: vec![],
                uploaded_at_unix_nanos: 99,
                validator_version: 1,
            },
        )
        .await;
    } else if op == Opcode::SchemaReplaceReq.as_u16() {
        send_frame(
            sock,
            Opcode::SchemaReplaceResp,
            sid,
            &SchemaReplaceResponse {
                namespace: "people".into(),
                schema_version: 5,
                dropped_count: 12,
                validation_errors: vec![],
            },
        )
        .await;
    } else if op == Opcode::RelationListFromReq.as_u16() {
        send_frame(
            sock,
            Opcode::RelationListFromResp,
            sid,
            &RelationListFromResponseFrame {
                items: vec![sample_relation_view()],
                next_cursor: Vec::new(),
                cumulative_count: 1,
                is_final: true,
            },
        )
        .await;
    } else if op == Opcode::RelationListToReq.as_u16() {
        send_frame(
            sock,
            Opcode::RelationListToResp,
            sid,
            &RelationListToResponseFrame {
                items: vec![sample_relation_view()],
                next_cursor: Vec::new(),
                cumulative_count: 1,
                is_final: true,
            },
        )
        .await;
    } else {
        dispatch_memory_ops(sock, f).await;
    }
}

/// Memory-verb ops beyond encode/recall: list (streamed), forget, inspect.
async fn dispatch_memory_ops(sock: &mut TcpStream, f: &Frame) {
    let sid = f.stream_id;
    let op = f.opcode;

    if op == Opcode::MemoryListReq.as_u16() {
        // Two frames: the first non-final, the tail EOS-final with a cursor.
        send_stream_frame(
            sock,
            Opcode::MemoryListResp,
            sid,
            &MemoryListResponseFrame {
                items: vec![sample_memory_list_item(0xC1)],
                next_cursor: Vec::new(),
                cumulative_count: 1,
                is_final: false,
            },
            false,
        )
        .await;
        send_stream_frame(
            sock,
            Opcode::MemoryListResp,
            sid,
            &MemoryListResponseFrame {
                items: vec![sample_memory_list_item(0xC2)],
                next_cursor: vec![0x2a, 0xff],
                cumulative_count: 2,
                is_final: true,
            },
            true,
        )
        .await;
    } else if op == Opcode::ForgetReq.as_u16() {
        send_frame(
            sock,
            Opcode::ForgetResp,
            sid,
            &ForgetResponse {
                memory_id: 12345,
                was_already_forgotten: false,
                edges_removed: 3,
            },
        )
        .await;
    } else if op == Opcode::MemoryInspectReq.as_u16() {
        send_frame(
            sock,
            Opcode::MemoryInspectResp,
            sid,
            &MemoryInspectResponse {
                found: true,
                memory_id: id16(0x2a),
                text: "remembered text".into(),
                artifact: EncodeStageArtifact {
                    vector: vec![0.1, 0.2, 0.3],
                    record: None,
                    hype_questions: vec!["who works where?".into()],
                    keyword_fields: Vec::new(),
                    graph: None,
                },
            },
        )
        .await;
    } else {
        dispatch_reasoning_ops(sock, f).await;
    }
}

/// Reasoning + memory-graph edge ops: plan, reason (both streamed), link, unlink.
async fn dispatch_reasoning_ops(sock: &mut TcpStream, f: &Frame) {
    let sid = f.stream_id;
    let op = f.opcode;

    if op == Opcode::PlanReq.as_u16() {
        // Two frames of plan steps; the client flattens both into one path.
        send_stream_frame(
            sock,
            Opcode::PlanResp,
            sid,
            &PlanResponseFrame {
                steps: vec![sample_plan_step(0, "start", TransitionKind::Initial)],
                is_final: false,
                plan_status: None,
                trace: None,
            },
            false,
        )
        .await;
        send_stream_frame(
            sock,
            Opcode::PlanResp,
            sid,
            &PlanResponseFrame {
                steps: vec![sample_plan_step(1, "goal", TransitionKind::Causal)],
                is_final: true,
                plan_status: Some(PlanStatus::GoalReached),
                trace: None,
            },
            true,
        )
        .await;
    } else if op == Opcode::ReasonReq.as_u16() {
        send_stream_frame(
            sock,
            Opcode::ReasonResp,
            sid,
            &ReasonResponseFrame {
                inferences: vec![sample_inference_step(0, "it rained")],
                is_final: false,
                reason_status: None,
                trace: None,
            },
            false,
        )
        .await;
        send_stream_frame(
            sock,
            Opcode::ReasonResp,
            sid,
            &ReasonResponseFrame {
                inferences: vec![sample_inference_step(1, "the ground is wet")],
                is_final: true,
                reason_status: Some(ReasonStatus::Complete),
                trace: None,
            },
            true,
        )
        .await;
    } else if op == Opcode::LinkReq.as_u16() {
        send_frame(
            sock,
            Opcode::LinkResp,
            sid,
            &LinkResponse {
                source: 100,
                target: 200,
                kind: EdgeKindWire::Caused,
                weight: 1.0,
                created_at_unix_nanos: 1_700_000_000,
                already_existed: false,
            },
        )
        .await;
    } else if op == Opcode::UnlinkReq.as_u16() {
        send_frame(
            sock,
            Opcode::UnlinkResp,
            sid,
            &UnlinkResponse {
                source: 100,
                target: 200,
                kind: EdgeKindWire::Caused,
                removed: true,
            },
        )
        .await;
    } else {
        dispatch_typed_ops(sock, f).await;
    }
}

/// Typed-graph reads/writes: graph fetch (streamed), entity create, entity list
/// (streamed), relation traverse (streamed).
async fn dispatch_typed_ops(sock: &mut TcpStream, f: &Frame) {
    let sid = f.stream_id;
    let op = f.opcode;

    if op == Opcode::GraphFetchReq.as_u16() {
        // Two frames: node in the first, edge in the tail (EOS + cursor).
        send_stream_frame(
            sock,
            Opcode::GraphFetchResp,
            sid,
            &GraphFetchResponseFrame {
                nodes: vec![GraphNode {
                    id: id16(0x07),
                    kind: 0, // entity
                    label: "Alice".into(),
                    type_qname: "brain:Person".into(),
                }],
                edges: Vec::new(),
                next_cursor: Vec::new(),
                is_final: false,
            },
            false,
        )
        .await;
        send_stream_frame(
            sock,
            Opcode::GraphFetchResp,
            sid,
            &GraphFetchResponseFrame {
                nodes: Vec::new(),
                edges: vec![GraphEdge {
                    from_id: id16(0x07),
                    to_id: id16(0x08),
                    kind: 0, // relation
                    label: "brain:knows".into(),
                }],
                next_cursor: vec![0x2a, 0xff],
                is_final: true,
            },
            true,
        )
        .await;
    } else if op == Opcode::EntityCreateReq.as_u16() {
        send_frame(
            sock,
            Opcode::EntityCreateResp,
            sid,
            &EntityCreateResponse {
                entity_id: id16(0x07),
            },
        )
        .await;
    } else if op == Opcode::EntityListReq.as_u16() {
        send_frame(
            sock,
            Opcode::EntityListResp,
            sid,
            &EntityListResponseFrame {
                items: vec![EntityListItem {
                    entity: sample_entity_view(),
                }],
                next_cursor: Vec::new(),
                cumulative_count: 1,
                is_final: true,
            },
        )
        .await;
    } else if op == Opcode::RelationTraverseReq.as_u16() {
        send_frame(
            sock,
            Opcode::RelationTraverseResp,
            sid,
            &RelationTraverseResponseFrame {
                paths: vec![TraversalPathWire {
                    steps: vec![sample_traversal_step()],
                }],
                total_paths: 1,
                truncated: false,
                is_final: true,
            },
        )
        .await;
    } else {
        dispatch_statement_schema_ops(sock, f).await;
    }
}

/// Statement + relation reads and schema writes: statement list (streamed),
/// statement get, relation get, schema upload, schema validate.
async fn dispatch_statement_schema_ops(sock: &mut TcpStream, f: &Frame) {
    let sid = f.stream_id;
    let op = f.opcode;

    if op == Opcode::StatementListReq.as_u16() {
        send_frame(
            sock,
            Opcode::StatementListResp,
            sid,
            &StatementListResponseFrame {
                items: vec![sample_statement_view()],
                next_cursor: Vec::new(),
                cumulative_count: 1,
                is_final: true,
            },
        )
        .await;
    } else if op == Opcode::StatementGetReq.as_u16() {
        send_frame(
            sock,
            Opcode::StatementGetResp,
            sid,
            &StatementGetResponse {
                statement: sample_statement_view(),
                returned_via_supersession: false,
            },
        )
        .await;
    } else if op == Opcode::RelationGetReq.as_u16() {
        send_frame(
            sock,
            Opcode::RelationGetResp,
            sid,
            &RelationGetResponse {
                relation: sample_relation_view(),
                returned_via_supersession: false,
            },
        )
        .await;
    } else if op == Opcode::SchemaUploadReq.as_u16() {
        send_frame(
            sock,
            Opcode::SchemaUploadResp,
            sid,
            &SchemaUploadResponse {
                namespace: "people".into(),
                schema_version: 3,
                validation_errors: Vec::new(),
                backward_compatible: true,
                migration_summary_blob: Vec::new(),
            },
        )
        .await;
    } else if op == Opcode::SchemaValidateReq.as_u16() {
        send_frame(
            sock,
            Opcode::SchemaValidateResp,
            sid,
            &SchemaValidateResponse {
                namespace: "people".into(),
                would_be_version: 4,
                validation_errors: Vec::new(),
            },
        )
        .await;
    }
    // Bye and anything unexpected: nothing to reply.
}

/// Spawn a mock Brain server on the test's own runtime and return its address.
/// The listener is bound before returning, so a pool that connects immediately
/// afterwards always finds it. Each accepted connection is served
/// independently, so a pool of any width is handled.
async fn spawn_mock() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        loop {
            let Ok((sock, _peer)) = listener.accept().await else {
                break;
            };
            tokio::spawn(serve(sock));
        }
    });
    addr
}

// --- harness ---------------------------------------------------------------

fn config(brain_addr: std::net::SocketAddr) -> EdgeConfig {
    EdgeConfig {
        listen_addr: "127.0.0.1:0".parse().expect("listen addr"),
        brain_addr,
        // One connection per credential keeps the mock's bookkeeping trivial.
        pool_size: 1,
        max_credentials: 16,
        idle_ttl_secs: 900,
        request_timeout_secs: 30,
        max_body_bytes: 1 << 20,
        wire_listen_addr: None,
        wire_rate_capacity: 0,
        wire_rate_refill_per_sec: 0,
    }
}

/// Spawn a mock Brain and build accepting state pointed at it, plus the meter.
async fn state_and_meter() -> (EdgeState, Arc<RecordingMeter>) {
    let brain_addr = spawn_mock().await;
    let meter = Arc::new(RecordingMeter::default());
    let state = EdgeState::with_ports(
        config(brain_addr),
        Arc::new(AcceptingResolver),
        meter.clone(),
    );
    (state, meter)
}

async fn send(
    state: EdgeState,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().method(method).uri(path);
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    let req = req
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_owned())))
        .expect("build request");
    let resp = brain_edge::app::router(state)
        .oneshot(req)
        .await
        .expect("router response");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("read body");
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("body is json")
    };
    (status, json)
}

// --- the tests -------------------------------------------------------------

#[tokio::test]
async fn whoami_reflects_handshake_metadata() {
    // No wire op: the handler reads the connection's AUTH_OK fields.
    let (state, meter) = state_and_meter().await;
    let (status, body) = send(state, "GET", "/v1/whoami", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["namespace"], "acme");
    assert_eq!(body["space_id"], "11000000-0000-0000-0000-00000000002a");
    assert_eq!(body["permissions"]["can_encode"], true);
    assert_eq!(body["permissions"]["can_admin"], false);
    assert_eq!(meter.ops(), vec!["whoami"]);
}

#[tokio::test]
async fn capabilities_maps_shard_flags() {
    let (state, _meter) = state_and_meter().await;
    let (status, body) = send(state, "GET", "/v1/capabilities", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["rerank"], true);
    assert_eq!(body["llm_extractor"], false);
    assert_eq!(body["vector_dim"], 384);
}

#[tokio::test]
async fn encode_returns_memory_id_and_meters() {
    let (state, meter) = state_and_meter().await;
    let (status, body) = send(
        state,
        "POST",
        "/v1/memories",
        Some(r#"{"text":"remember this"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["memory_id"], "12345");
    assert_eq!(body["was_deduplicated"], false);
    assert_eq!(body["auto_edges_added"], 2);
    assert_eq!(meter.ops(), vec!["encode"]);
}

#[tokio::test]
async fn recall_flattens_streamed_hits() {
    let (state, meter) = state_and_meter().await;
    let (status, body) = send(
        state,
        "POST",
        "/v1/recall",
        Some(r#"{"query":"dark mode"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["answer_kind"], "many");
    let mems = body["memories"].as_array().expect("memories array");
    assert_eq!(mems.len(), 2);
    assert_eq!(mems[0]["text"], "first hit");
    assert_eq!(mems[1]["text"], "second hit");
    assert_eq!(meter.ops(), vec!["recall"]);
}

#[tokio::test]
async fn entity_get_returns_detail() {
    let (state, _meter) = state_and_meter().await;
    let (status, body) = send(
        state,
        "GET",
        "/v1/entities/07070707-0707-0707-0707-070707070707",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["canonical_name"], "Alice");
    assert_eq!(body["entity_type_id"], 1);
    assert_eq!(body["entity_id"], "07070707-0707-0707-0707-070707070707");
}

#[tokio::test]
async fn entity_resolve_binds_id() {
    let (state, _meter) = state_and_meter().await;
    let (status, body) = send(
        state,
        "POST",
        "/v1/entities/resolve",
        Some(r#"{"candidate_name":"Alice"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["outcome"], "resolved");
    assert_eq!(body["entity_id"], "07070707-0707-0707-0707-070707070707");
}

#[tokio::test]
async fn schema_get_returns_active_version() {
    let (state, _meter) = state_and_meter().await;
    let (status, body) = send(state, "GET", "/v1/schema", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["namespace"], "people");
    assert_eq!(body["schema_version"], 2);
    assert_eq!(body["schema_document"], "entity Person {}");
}

#[tokio::test]
async fn schema_replace_surfaces_dropped_count() {
    let (state, meter) = state_and_meter().await;
    let (status, body) = send(
        state,
        "PUT",
        "/v1/schema",
        Some(r#"{"schema_document":"entity P {}","force_drop_existing":true}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["schema_version"], 5);
    assert_eq!(body["dropped_count"], 12);
    assert_eq!(meter.ops(), vec!["replace_schema"]);
}

#[tokio::test]
async fn relation_list_from_side() {
    let (state, _meter) = state_and_meter().await;
    let (status, body) = send(
        state,
        "GET",
        "/v1/entities/11111111-1111-1111-1111-111111111111/relations?direction=from",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["count"], 1);
    let rels = body["relations"].as_array().expect("relations array");
    assert_eq!(rels[0]["relation_type"], "brain:knows");
    assert_eq!(
        rels[0]["from_entity"],
        "11111111-1111-1111-1111-111111111111"
    );
}

#[tokio::test]
async fn relation_list_to_side() {
    // The `to`/incoming branch (RelationListToReq) — the one ports.rs never
    // reaches. Same shape as `from`, dispatched through the other wire op.
    let (state, _meter) = state_and_meter().await;
    let (status, body) = send(
        state,
        "GET",
        "/v1/entities/12121212-1212-1212-1212-121212121212/relations?direction=to",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["count"], 1);
    let rels = body["relations"].as_array().expect("relations array");
    assert_eq!(rels[0]["to_entity"], "12121212-1212-1212-1212-121212121212");
}

#[tokio::test]
async fn memory_list_flattens_frames_and_surfaces_cursor() {
    // Two streamed frames; the client flattens both items and the tail cursor
    // is surfaced as hex.
    let (state, meter) = state_and_meter().await;
    let (status, body) = send(state, "GET", "/v1/memories?limit=2", None).await;
    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["relation_count"], 3);
    assert_eq!(body["next_cursor"], "2aff");
    assert_eq!(meter.ops(), vec!["memory_list"]);
}

#[tokio::test]
async fn forget_reports_edges_removed() {
    let (state, meter) = state_and_meter().await;
    let (status, body) = send(
        state,
        "DELETE",
        "/v1/memories",
        Some(r#"{"memory_id":"12345","hard":true}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["memory_id"], "12345");
    assert_eq!(body["was_already_forgotten"], false);
    assert_eq!(body["edges_removed"], 3);
    assert_eq!(meter.ops(), vec!["forget"]);
}

#[tokio::test]
async fn memory_inspect_returns_artifact_bundle() {
    let (state, meter) = state_and_meter().await;
    let (status, body) = send(state, "GET", "/v1/memories/42/inspect", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["found"], true);
    assert_eq!(body["text"], "remembered text");
    let vector = body["artifact"]["vector"].as_array().expect("vector array");
    assert_eq!(vector.len(), 3);
    let hype = body["artifact"]["hype_questions"]
        .as_array()
        .expect("hype array");
    assert_eq!(hype[0], "who works where?");
    assert_eq!(meter.ops(), vec!["memory_inspect"]);
}

#[tokio::test]
async fn plan_flattens_streamed_steps() {
    let (state, meter) = state_and_meter().await;
    let (status, body) = send(
        state,
        "POST",
        "/v1/plan",
        Some(r#"{"start":{"text":"at home"},"goal":{"text":"at work"}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let steps = body["steps"].as_array().expect("steps array");
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0]["transition_kind"], "initial");
    assert_eq!(steps[1]["transition_kind"], "causal");
    assert_eq!(meter.ops(), vec!["plan"]);
}

#[tokio::test]
async fn reason_flattens_streamed_inferences() {
    let (state, meter) = state_and_meter().await;
    let (status, body) = send(
        state,
        "POST",
        "/v1/reason",
        Some(r#"{"observation":{"text":"the ground is wet"}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let inferences = body["inferences"].as_array().expect("inferences array");
    assert_eq!(inferences.len(), 2);
    assert_eq!(inferences[0]["claim"], "it rained");
    assert_eq!(inferences[0]["inference_kind"], "causal_explanation");
    assert_eq!(meter.ops(), vec!["reason"]);
}

#[tokio::test]
async fn link_creates_directed_edge() {
    let (state, meter) = state_and_meter().await;
    let (status, body) = send(
        state,
        "POST",
        "/v1/links",
        Some(r#"{"source":"100","target":"200","kind":"caused"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["source"], "100");
    assert_eq!(body["target"], "200");
    assert_eq!(body["kind"], "caused");
    assert_eq!(body["already_existed"], false);
    assert_eq!(meter.ops(), vec!["link"]);
}

#[tokio::test]
async fn unlink_removes_directed_edge() {
    let (state, meter) = state_and_meter().await;
    let (status, body) = send(
        state,
        "DELETE",
        "/v1/links",
        Some(r#"{"source":"100","target":"200","kind":"caused"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["kind"], "caused");
    assert_eq!(body["removed"], true);
    assert_eq!(meter.ops(), vec!["unlink"]);
}

#[tokio::test]
async fn graph_fetch_flattens_nodes_and_edges() {
    // Two frames: a node in the first, an edge in the tail (EOS + cursor).
    let (state, meter) = state_and_meter().await;
    let (status, body) = send(state, "GET", "/v1/graph?limit=10", None).await;
    assert_eq!(status, StatusCode::OK);
    let nodes = body["nodes"].as_array().expect("nodes array");
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["kind"], "entity");
    assert_eq!(nodes[0]["id"], "07070707-0707-0707-0707-070707070707");
    let edges = body["edges"].as_array().expect("edges array");
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0]["kind"], "relation");
    assert_eq!(edges[0]["label"], "brain:knows");
    assert_eq!(body["next_cursor"], "2aff");
    assert_eq!(meter.ops(), vec!["graph_fetch"]);
}

#[tokio::test]
async fn entity_create_returns_new_id() {
    let (state, meter) = state_and_meter().await;
    let (status, body) = send(
        state,
        "POST",
        "/v1/entities",
        Some(r#"{"entity_type_id":1,"canonical_name":"Alice"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["entity_id"], "07070707-0707-0707-0707-070707070707");
    assert_eq!(meter.ops(), vec!["create_entity"]);
}

#[tokio::test]
async fn entity_list_returns_page_with_count() {
    let (state, meter) = state_and_meter().await;
    let (status, body) = send(state, "GET", "/v1/entities?limit=10", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["count"], 1);
    let entities = body["entities"].as_array().expect("entities array");
    assert_eq!(entities[0]["canonical_name"], "Alice");
    assert_eq!(
        entities[0]["entity_id"],
        "07070707-0707-0707-0707-070707070707"
    );
    assert_eq!(meter.ops(), vec!["list_entities"]);
}

#[tokio::test]
async fn entity_traverse_flattens_paths() {
    let (state, meter) = state_and_meter().await;
    let (status, body) = send(
        state,
        "POST",
        "/v1/entities/07070707-0707-0707-0707-070707070707/traverse",
        Some(r#"{"direction":"both","max_depth":2}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total_paths"], 1);
    assert_eq!(body["truncated"], false);
    let paths = body["paths"].as_array().expect("paths array");
    assert_eq!(paths.len(), 1);
    let steps = paths[0]["steps"].as_array().expect("steps array");
    assert_eq!(steps[0]["relation_type"], "brain:knows");
    assert_eq!(meter.ops(), vec!["traverse"]);
}

#[tokio::test]
async fn statement_list_returns_page_with_count() {
    let (state, meter) = state_and_meter().await;
    let (status, body) = send(state, "GET", "/v1/statements?limit=10", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["count"], 1);
    let stmts = body["statements"].as_array().expect("statements array");
    assert_eq!(stmts[0]["predicate"], "brain:works_at");
    assert_eq!(stmts[0]["object"]["kind"], "value");
    assert_eq!(stmts[0]["object"]["value"]["value"], "Acme");
    assert_eq!(meter.ops(), vec!["list_statements"]);
}

#[tokio::test]
async fn statement_get_returns_detail() {
    let (state, meter) = state_and_meter().await;
    let (status, body) = send(
        state,
        "GET",
        "/v1/statements/20202020-2020-2020-2020-202020202020",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["statement_id"], "20202020-2020-2020-2020-202020202020");
    assert_eq!(body["kind"], "fact");
    assert_eq!(body["predicate"], "brain:works_at");
    assert_eq!(meter.ops(), vec!["get_statement"]);
}

#[tokio::test]
async fn relation_get_returns_detail() {
    let (state, meter) = state_and_meter().await;
    let (status, body) = send(
        state,
        "GET",
        "/v1/relations/10101010-1010-1010-1010-101010101010",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["relation_type"], "brain:knows");
    assert_eq!(body["from_entity"], "11111111-1111-1111-1111-111111111111");
    assert_eq!(body["to_entity"], "12121212-1212-1212-1212-121212121212");
    assert_eq!(meter.ops(), vec!["get_relation"]);
}

#[tokio::test]
async fn schema_upload_merges_document() {
    let (state, meter) = state_and_meter().await;
    let (status, body) = send(
        state,
        "POST",
        "/v1/schema",
        Some(r#"{"schema_document":"entity P {}"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["namespace"], "people");
    assert_eq!(body["schema_version"], 3);
    assert_eq!(body["backward_compatible"], true);
    assert_eq!(meter.ops(), vec!["upload_schema"]);
}

#[tokio::test]
async fn schema_validate_reports_would_be_version() {
    let (state, meter) = state_and_meter().await;
    let (status, body) = send(
        state,
        "POST",
        "/v1/schema/validate",
        Some(r#"{"schema_document":"entity P {}"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["namespace"], "people");
    assert_eq!(body["would_be_version"], 4);
    let errs = body["validation_errors"].as_array().expect("errors array");
    assert!(errs.is_empty());
    assert_eq!(meter.ops(), vec!["validate_schema"]);
}
