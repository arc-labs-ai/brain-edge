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
    AuthOkPayload, AuthPayload, Capabilities, EncodeResponse, EntityGetResponse,
    EntityResolveResponse, EntityView, EvidenceRefWire, GetCapabilitiesResponse, HelloPayload,
    MemoryKindWire, MemoryResult, RecallResponseFrame, RelationListFromResponseFrame,
    RelationListToResponseFrame, RelationView, ResolutionOutcomeWire, SchemaGetResponse,
    SchemaReplaceResponse, ServerFeatures, SpacePermissions, WelcomePayload,
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

async fn send_frame<T: serde::Serialize + Sync>(
    sock: &mut TcpStream,
    op: Opcode,
    sid: u32,
    payload: &T,
) {
    let frame = Frame::new(op.as_u16(), FLAG_EOS, sid, to_cbor_bytes(payload));
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
