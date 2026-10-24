//! Memory verbs: encode / recall / forget.

// The DTOs below mirror the HTTP contract one-for-one: the JSON field names are
// the API, and `tools/http_manifest.py` emits every one of them — with its type
// and serde attributes — into `contract/http-routes.json`, which the three SDK
// clients are checked against. A doc comment on each of ~312 fields would
// restate the field name; the ones that carry meaning beyond their name have
// one written below.
#![allow(missing_docs)]

use brain_db_sdk::RecallAnswer;
use brain_db_sdk::wire::types::{
    AnswerKindWire, EdgeDirectionWire, EncodeGraphEdge, EncodeGraphNode, EncodeResponse,
    EncodeStageArtifact, EncodeStageGraph, EncodeStageKeywordField, EncodeStageRecord,
    EncodeVectorDirectRequest, ForgetResponse, MaterializeProceduralRequest,
    MaterializeProceduralResponse, MemoryEdgeWire, MemoryInspectResponse, MemoryKindWire,
    MemoryListDirWire, MemoryListItem, MemoryListRequest, MemoryListSortWire,
    MemoryListTimeAxisWire, MemoryResult, RecallResponseFrame as RecallResponseFrameWire,
    WireMemoryId,
};
use serde::{Deserialize, Serialize};

use super::{hex_decode, hex_encode, parse_memory_id, uuid_string};

/// `POST /v1/memories` body.
#[derive(Debug, Deserialize)]
pub struct EncodeBody {
    /// The text to remember.
    pub text: String,
    /// Optional session id.
    #[serde(default)]
    pub session: Option<u64>,
    /// Optional event time, unix nanoseconds.
    ///
    /// Spelled with its unit, like every other temporal field on this surface
    /// and like the read side of this same value
    /// (`MemoryListItemDto.occurred_at_unix_nanos`).
    #[serde(default)]
    pub occurred_at_unix_nanos: Option<u64>,
    //
    // NOT IMPLEMENTED ON THIS ROUTE: `wait` / `WaitMode::Derived`.
    //
    // A Brain write has two halves. The synchronous half is durable the moment
    // it acks — that is what `POST /v1/memories` returns. The asynchronous
    // half is where the embedding, the HyPE questions and the knowledge-graph
    // extraction happen, and it runs afterwards, taking seconds because it
    // involves provider round trips.
    //
    // The wire protocol lets a caller wait for the second half
    // (`WaitMode::Derived`), which is what makes read-your-writes possible and
    // what populates `EncodeResponse.trace`. This route does not expose it, so
    // over HTTP every write is ack-only. Two consequences a caller will meet:
    //
    //   * RECALL right after an encode may not find the memory yet. It is
    //     durable, but the semantic index becomes searchable a beat later.
    //     Poll, rather than assuming the write was lost.
    //   * The write trace is never returned here, because it only exists on a
    //     derived write.
    //
    // Left unimplemented deliberately rather than overlooked. The edge pools
    // connections, and a derived write holds one for the whole provider round
    // trip, so exposing it needs a decision about what that does to pool
    // occupancy and to the request timeout — not just a field. The wire client
    // (`brain_db_sdk::BrainClient`) has it today for callers who need it.
}

/// `POST /v1/memories` response.
#[derive(Debug, Serialize)]
pub struct EncodeResponseDto {
    pub memory_id: String,
    pub was_deduplicated: bool,
    pub salience: f32,
    pub kind: u8,
    pub created_at_unix_nanos: u64,
    pub auto_edges_added: u32,
}

impl From<EncodeResponse> for EncodeResponseDto {
    fn from(r: EncodeResponse) -> Self {
        Self {
            memory_id: r.memory_id.to_string(),
            was_deduplicated: r.was_deduplicated,
            salience: r.salience,
            kind: r.kind as u8,
            created_at_unix_nanos: r.created_at_unix_nanos,
            auto_edges_added: r.auto_edges_added,
        }
    }
}

/// `POST /v1/recall` body.
#[derive(Debug, Deserialize)]
pub struct RecallBody {
    /// The cue to recall against.
    pub query: String,
    #[serde(default)]
    pub max_results: Option<u32>,
    #[serde(default)]
    pub subject: Option<String>,
    /// Read scope: `"space"` (default — the caller's own space) or
    /// `"namespace"` (every space in the caller's namespace, fanned out across
    /// shards and merged). Case-insensitive; anything else is rejected.
    #[serde(default)]
    pub scope: Option<String>,
    /// Ask for Brain's read-pipeline trace alongside the hits: which
    /// retrievers ran, what each contributed, how the filter chain narrowed
    /// the candidates, and where the latency went.
    ///
    /// Off by default and free when off — the server only builds the trace
    /// when the request asks for it. All three SDKs have exposed this
    /// parameter since they were written; the edge never read it, so it was
    /// serialized, ignored, and the `trace` field came back null every time.
    ///
    /// It is a large response, and the size does NOT follow `max_results`:
    /// the trace reports every candidate each retriever lane considered, so it
    /// grows with the space being searched. A three-hit recall over ~700
    /// memories measures ~121 KB, nearly all of it per-lane candidate lists.
    /// Fine for debugging one bad answer; not something to leave on.
    #[serde(default)]
    pub include_trace: Option<bool>,
}

/// One recalled memory in the response.
#[derive(Debug, Serialize)]
pub struct MemoryHit {
    pub memory_id: String,
    pub text: String,
    pub similarity_score: f32,
    pub confidence: f32,
    pub salience: f32,
    pub kind: u8,
    pub created_at_unix_nanos: u64,
}

impl From<MemoryResult> for MemoryHit {
    fn from(m: MemoryResult) -> Self {
        Self {
            memory_id: m.memory_id.to_string(),
            text: m.text,
            similarity_score: m.similarity_score,
            confidence: m.confidence,
            salience: m.salience,
            kind: m.kind as u8,
            created_at_unix_nanos: m.created_at_unix_nanos,
        }
    }
}

/// `POST /v1/recall` response.
#[derive(Debug, Serialize)]
pub struct RecallResponseDto {
    /// `single` / `many` / `none`.
    pub answer_kind: &'static str,
    pub memories: Vec<MemoryHit>,
    /// The read-pipeline trace, as a JSON **string**, when `include_trace`
    /// asked for one. Omitted entirely otherwise, so a caller that did not ask
    /// sees the response it always saw.
    ///
    /// A string rather than a nested object, and that is the contract, not an
    /// implementation detail: the trace carries 128-bit memory ids, and a
    /// JSON number cannot hold one — `JSON.parse` in a browser would round it
    /// and hand back a different id. Every id this API returns is a decimal
    /// string for the same reason; embedding the trace as a string keeps the
    /// ids inside it out of a number type on the way through.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace: Option<String>,
}

impl RecallResponseDto {
    /// Serialize the trace off the frames, without consuming them.
    ///
    /// The server puts the trace on the final frame only. Pulled out so the
    /// blob is produced at most once per recall even when both the caller and
    /// the deployment's metering sink want it.
    #[must_use]
    pub fn serialize_trace(frames: &[RecallResponseFrameWire]) -> Option<String> {
        frames.iter().rev().find_map(|f| f.trace.as_ref()).map(|t| {
            // A trace that will not serialize is not worth failing the recall
            // over — the hits are the answer, the trace is diagnostics. Report
            // it in place of the trace instead.
            serde_json::to_string(t).unwrap_or_else(|e| {
                format!("{{\"error\":\"trace could not be serialized: {e}\"}}")
            })
        })
    }

    /// Build from the streamed frames, with an already-serialized trace or
    /// none.
    ///
    /// Frames rather than the SDK's aggregating `recall()` because the server
    /// puts the trace on the final frame only, and the aggregate keeps the
    /// memories and drops the frames it read them from.
    ///
    /// `None` is what the caller gets when the trace was fetched only because
    /// the metering sink wanted the signals derived from it: those are read
    /// off the frames beforehand, and the blob is diagnostics this client did
    /// not ask for.
    #[must_use]
    pub fn from_frames_with_trace(
        frames: Vec<RecallResponseFrameWire>,
        trace: Option<String>,
    ) -> Self {
        let mut answer_kind = AnswerKindWire::None;
        let mut memories = Vec::new();
        for frame in frames {
            answer_kind = frame.answer_kind;
            memories.extend(frame.memories);
        }
        Self::build(answer_kind, memories, trace)
    }

    fn build(
        answer_kind: AnswerKindWire,
        memories: Vec<MemoryResult>,
        trace: Option<String>,
    ) -> Self {
        Self {
            answer_kind: match answer_kind {
                AnswerKindWire::Single => "single",
                AnswerKindWire::Many => "many",
                AnswerKindWire::None => "none",
            },
            memories: memories.into_iter().map(MemoryHit::from).collect(),
            trace,
        }
    }
}

impl From<RecallAnswer> for RecallResponseDto {
    fn from(a: RecallAnswer) -> Self {
        Self::build(a.answer_kind, a.memories, None)
    }
}

/// `DELETE /v1/memories` body.
#[derive(Debug, Deserialize)]
pub struct ForgetBody {
    /// Decimal string of the 128-bit memory id.
    pub memory_id: String,
    #[serde(default)]
    pub hard: bool,
}

impl ForgetBody {
    /// Parse the decimal id string to a wire id.
    pub fn parse_id(&self) -> Result<WireMemoryId, String> {
        parse_memory_id(&self.memory_id)
    }
}

/// `DELETE /v1/memories` response.
#[derive(Debug, Serialize)]
pub struct ForgetResponseDto {
    pub memory_id: String,
    pub was_already_forgotten: bool,
    pub edges_removed: u32,
}

impl From<ForgetResponse> for ForgetResponseDto {
    fn from(r: ForgetResponse) -> Self {
        Self {
            memory_id: r.memory_id.to_string(),
            was_already_forgotten: r.was_already_forgotten,
            edges_removed: r.edges_removed,
        }
    }
}

// ---- memory list (GET /v1/memories) --------------------------------------

/// Default and max page size for `GET /v1/memories`.
const LIST_DEFAULT_LIMIT: u32 = 50;
const LIST_MAX_LIMIT: u32 = 100;

/// `GET /v1/memories` query parameters — a non-ranked, paginated enumeration
/// of the caller's memories. Only the server-backed `created_at` axis is
/// exposed; ranking/filtering beyond direction + tombstone state is left to
/// RECALL and future filters.
#[derive(Debug, Deserialize)]
pub struct MemoryListQuery {
    /// Page size, clamped to `1..=100` (default 50).
    #[serde(default)]
    pub limit: Option<u32>,
    /// Opaque keyset cursor (hex) from a previous page's `next_cursor`.
    #[serde(default)]
    pub cursor: Option<String>,
    /// `desc` (default, newest first) or `asc`.
    #[serde(default)]
    pub dir: Option<String>,
    /// Include tombstoned memories (default false).
    #[serde(default)]
    pub include_tombstoned: Option<bool>,
}

impl MemoryListQuery {
    /// Build the wire request. Fails on a malformed cursor or `dir`.
    pub fn to_request(&self) -> Result<MemoryListRequest, String> {
        let limit = self
            .limit
            .unwrap_or(LIST_DEFAULT_LIMIT)
            .clamp(1, LIST_MAX_LIMIT);
        let dir = match self.dir.as_deref().map(str::trim) {
            None | Some("" | "desc") => MemoryListDirWire::Desc,
            Some("asc") => MemoryListDirWire::Asc,
            Some(other) => return Err(format!("dir must be asc|desc, got `{other}`")),
        };
        let cursor = match &self.cursor {
            Some(c) if !c.is_empty() => hex_decode(c)?,
            _ => Vec::new(),
        };
        Ok(MemoryListRequest {
            sort: MemoryListSortWire::Created,
            dir,
            limit,
            cursor,
            kinds: Vec::new(),
            include_tombstoned: self.include_tombstoned.unwrap_or(false),
            time_axis: MemoryListTimeAxisWire::Created,
            from_unix_nanos: 0,
            to_unix_nanos: 0,
            salience_min: 0.0,
            salience_max: 1.0,
            text_contains: String::new(),
            act_as: None,
        })
    }
}

/// One memory in a `GET /v1/memories` page. Carries the enumeration fields
/// plus typed-graph handle counts so a UI row can show link counts without a
/// second call.
#[derive(Debug, Serialize)]
pub struct MemoryListItemDto {
    /// 128-bit memory id, decimal string.
    pub memory_id: String,
    pub text: String,
    /// 0 = Episodic, 1 = Semantic, 2 = Consolidated.
    pub kind: u8,
    /// 0 = active, 1 = tombstoned.
    pub state: u8,
    pub created_at_unix_nanos: u64,
    pub occurred_at_unix_nanos: u64,
    pub last_accessed_at_unix_nanos: u64,
    pub salience: f32,
    pub access_count: u32,
    pub statement_count: u32,
    pub entity_count: u32,
    pub relation_count: u32,
}

impl From<MemoryListItem> for MemoryListItemDto {
    fn from(m: MemoryListItem) -> Self {
        Self {
            memory_id: m.memory_id.to_string(),
            text: m.text,
            kind: m.kind,
            state: m.state,
            created_at_unix_nanos: m.created_at_unix_nanos,
            occurred_at_unix_nanos: m.occurred_at_unix_nanos,
            last_accessed_at_unix_nanos: m.last_accessed_at_unix_nanos,
            salience: m.salience,
            access_count: m.access_count,
            statement_count: m.statement_count,
            entity_count: m.entity_count,
            relation_count: m.relation_count,
        }
    }
}

/// `GET /v1/memories` response: a page of items plus the resume cursor.
/// `next_cursor` is omitted when the enumeration is exhausted.
#[derive(Debug, Serialize)]
pub struct MemoryListPageDto {
    pub items: Vec<MemoryListItemDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

impl MemoryListPageDto {
    /// Assemble from the streamed frames: flatten items, carry the tail
    /// frame's cursor (hex, or omitted when empty).
    pub fn from_frames(frames: Vec<brain_db_sdk::wire::types::MemoryListResponseFrame>) -> Self {
        let next = frames
            .last()
            .map(|f| f.next_cursor.clone())
            .filter(|c| !c.is_empty())
            .map(|c| hex_encode(&c));
        let items = frames
            .into_iter()
            .flat_map(|f| f.items)
            .map(MemoryListItemDto::from)
            .collect();
        Self {
            items,
            next_cursor: next,
        }
    }
}

// ---- memory inspect (GET /v1/memories/{id}/inspect) ----------------------

/// `GET /v1/memories/{id}/inspect` response — one memory's durable
/// write-artifact bundle in a friendly, per-stage shape: what each write
/// stage produced (embedding vector, stored record, analyzed keyword terms,
/// write-time HyPE questions, extracted knowledge graph) plus the memory text.
///
/// An absent memory is a `404`, the same as every other single-resource read
/// on this edge (`/v1/entities/{id}`, `/v1/statements/{id}`,
/// `/v1/relations/{id}`). No special handling here produces that: Brain
/// answers `MemoryNotFound` in the `NotFound` category and
/// [`ApiError::from_brain`] maps the category, exactly as it does for the
/// siblings.
///
/// An existing memory whose artifact bundle has not been written yet is a
/// different case and still a `200`: the memory is there, with an empty
/// `artifact`.
#[derive(Debug, Serialize)]
pub struct MemoryInspectDto {
    /// Decimal string of the 128-bit memory id (matches `/v1/memories`).
    pub memory_id: String,
    pub text: String,
    pub artifact: StageArtifactDto,
    /// Memory↔memory edges anchored at this memory, both directions.
    ///
    /// This is the read side of `POST /v1/links`. `artifact.graph` is a
    /// different thing — the extractor's write-time knowledge graph — and an
    /// edge asserted with `/v1/links` never appears there, which left linking
    /// write-only over HTTP.
    pub edges: Vec<MemoryEdgeDto>,
}

/// One memory↔memory edge.
///
/// `peer` is a decimal string like `memory_id`, so it can be fed straight
/// back into `/v1/memories/{id}` or `/v1/links` without conversion.
#[derive(Debug, Serialize)]
pub struct MemoryEdgeDto {
    /// Snake-case kind name — the same vocabulary `POST /v1/links` accepts.
    pub kind: &'static str,
    /// `outgoing` = this memory is the edge's source.
    pub direction: &'static str,
    /// Decimal string of the memory at the other end.
    pub peer: String,
    pub weight: f32,
    pub created_at_unix_nanos: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotation: Option<String>,
}

impl From<MemoryEdgeWire> for MemoryEdgeDto {
    fn from(e: MemoryEdgeWire) -> Self {
        Self {
            kind: super::graph::edge_kind_str(e.kind),
            direction: match e.direction {
                EdgeDirectionWire::Outgoing => "outgoing",
                EdgeDirectionWire::Incoming => "incoming",
            },
            peer: e.peer.to_string(),
            weight: e.weight,
            created_at_unix_nanos: e.created_at_unix_nanos,
            annotation: e.annotation,
        }
    }
}

impl From<MemoryInspectResponse> for MemoryInspectDto {
    fn from(r: MemoryInspectResponse) -> Self {
        Self {
            memory_id: r.memory_id.to_string(),
            text: r.text,
            artifact: StageArtifactDto::from(r.artifact),
            edges: r.edges.into_iter().map(MemoryEdgeDto::from).collect(),
        }
    }
}

/// The per-stage output bag. Every field is optional; a stage that produced
/// nothing leaves its field empty/null.
#[derive(Debug, Serialize)]
pub struct StageArtifactDto {
    /// The embedding vector the `embed` stage produced.
    pub vector: Vec<f32>,
    /// The metadata row the `persist` stage committed.
    pub record: Option<StageRecordDto>,
    /// Hypothetical questions the write-time HyPE step generated.
    pub hype_questions: Vec<String>,
    /// Analyzed keyword terms per text-index field.
    pub keyword_fields: Vec<StageKeywordFieldDto>,
    /// The knowledge graph the extractor produced.
    pub graph: Option<StageGraphDto>,
}

impl From<EncodeStageArtifact> for StageArtifactDto {
    fn from(a: EncodeStageArtifact) -> Self {
        Self {
            vector: a.vector,
            record: a.record.map(StageRecordDto::from),
            hype_questions: a.hype_questions,
            keyword_fields: a
                .keyword_fields
                .into_iter()
                .map(StageKeywordFieldDto::from)
                .collect(),
            graph: a.graph.map(StageGraphDto::from),
        }
    }
}

/// The stored metadata row a `persist` stage wrote.
#[derive(Debug, Serialize)]
pub struct StageRecordDto {
    pub memory_id: String,
    pub kind: u8,
    pub salience: f32,
    pub created_at_unix_nanos: u64,
    pub occurred_at_unix_nanos: u64,
    pub vector_dim: u32,
    pub text_len: u32,
    pub lsn: u64,
}

impl From<EncodeStageRecord> for StageRecordDto {
    fn from(r: EncodeStageRecord) -> Self {
        Self {
            memory_id: r.memory_id.to_string(),
            kind: r.kind,
            salience: r.salience,
            created_at_unix_nanos: r.created_at_unix_nanos,
            occurred_at_unix_nanos: r.occurred_at_unix_nanos,
            vector_dim: r.vector_dim,
            text_len: r.text_len,
            lsn: r.lsn,
        }
    }
}

/// One text-index field and the analyzed terms it will match on.
#[derive(Debug, Serialize)]
pub struct StageKeywordFieldDto {
    pub field: String,
    pub terms: Vec<String>,
}

impl From<EncodeStageKeywordField> for StageKeywordFieldDto {
    fn from(k: EncodeStageKeywordField) -> Self {
        Self {
            field: k.field,
            terms: k.terms,
        }
    }
}

/// The knowledge graph an ENCODE produced — nodes + directed edges.
#[derive(Debug, Serialize)]
pub struct StageGraphDto {
    pub nodes: Vec<StageGraphNodeDto>,
    pub edges: Vec<StageGraphEdgeDto>,
}

impl From<EncodeStageGraph> for StageGraphDto {
    fn from(g: EncodeStageGraph) -> Self {
        Self {
            nodes: g.nodes.into_iter().map(StageGraphNodeDto::from).collect(),
            edges: g.edges.into_iter().map(StageGraphEdgeDto::from).collect(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct StageGraphNodeDto {
    /// Hex of the 16-byte node id.
    pub id: String,
    pub name: String,
    pub kind: String,
    pub type_qname: String,
}

impl From<EncodeGraphNode> for StageGraphNodeDto {
    fn from(n: EncodeGraphNode) -> Self {
        Self {
            id: hex_encode(&n.id),
            name: n.name,
            kind: n.kind,
            type_qname: n.type_qname,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct StageGraphEdgeDto {
    /// Hex of the 16-byte source node id.
    pub source: String,
    /// Hex of the 16-byte target node id.
    pub target: String,
    pub predicate: String,
    pub kind: String,
    pub confidence: f32,
    /// When the EVENT this edge records happened, in unix nanos —
    /// denormalised from the backing statement. Omitted for an undated
    /// statement and for every non-statement edge kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_at_unix_nanos: Option<u64>,
}

impl From<EncodeGraphEdge> for StageGraphEdgeDto {
    fn from(e: EncodeGraphEdge) -> Self {
        Self {
            source: hex_encode(&e.source),
            target: hex_encode(&e.target),
            predicate: e.predicate,
            kind: e.kind,
            confidence: e.confidence,
            event_at_unix_nanos: e.event_at_unix_nanos,
        }
    }
}

#[cfg(test)]
mod memory_tests {
    use super::*;
    use brain_db_sdk::RecallAnswer;
    use brain_db_sdk::wire::types::{
        EdgeKindWire, EncodeGraphEdge, EncodeGraphNode, EncodeResponse, EncodeStageArtifact,
        EncodeStageGraph, EncodeStageKeywordField, EncodeStageRecord, ForgetResponse,
        MemoryInspectResponse, MemoryKindWire, MemoryListItem, MemoryListResponseFrame,
        MemoryResult,
    };

    fn sample_result(tag: u8, text: &str) -> MemoryResult {
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

    #[test]
    fn list_query_clamps_limit_defaults_dir_and_rejects_bad_dir() {
        // Over-max clamps; asc/desc parse; malformed dir is an error.
        let over = MemoryListQuery {
            limit: Some(9999),
            cursor: None,
            dir: None,
            include_tombstoned: None,
        };
        let req = over.to_request().unwrap();
        assert_eq!(req.limit, LIST_MAX_LIMIT);
        assert!(matches!(req.dir, MemoryListDirWire::Desc)); // default
        assert_eq!(req.cursor, [] as [u8; 0]);
        assert!(!req.include_tombstoned);

        let asc = MemoryListQuery {
            limit: None,
            cursor: Some("2a00ff".into()),
            dir: Some(" asc ".into()),
            include_tombstoned: Some(true),
        }
        .to_request()
        .unwrap();
        assert_eq!(asc.limit, LIST_DEFAULT_LIMIT);
        assert!(matches!(asc.dir, MemoryListDirWire::Asc));
        assert_eq!(asc.cursor, vec![0x2a, 0x00, 0xff]); // hex-decoded
        assert!(asc.include_tombstoned);

        assert!(
            MemoryListQuery {
                limit: None,
                cursor: None,
                dir: Some("sideways".into()),
                include_tombstoned: None,
            }
            .to_request()
            .is_err()
        );
        // A malformed cursor is rejected too.
        assert!(
            MemoryListQuery {
                limit: None,
                cursor: Some("zz".into()),
                dir: None,
                include_tombstoned: None,
            }
            .to_request()
            .is_err()
        );
    }

    #[test]
    fn forget_body_parses_decimal_id_and_rejects_junk() {
        let ok = ForgetBody {
            memory_id: "42".into(),
            hard: true,
        };
        assert_eq!(ok.parse_id().unwrap(), 42u128);
        assert!(
            ForgetBody {
                memory_id: "not-a-number".into(),
                hard: false,
            }
            .parse_id()
            .is_err()
        );
    }

    #[test]
    fn encode_response_maps_every_field_and_kind_byte() {
        let resp = EncodeResponse {
            memory_id: 7,
            was_deduplicated: true,
            salience: 0.42,
            auto_edges_added: 3,
            lsn: 99,
            space_id: [0xAB; 16],
            session_id: 5,
            kind: MemoryKindWire::Consolidated,
            created_at_unix_nanos: 1_700_000_000,
            edges_out_count: 0,
            embedding_model_fp: [0; 16],
            pending_stages: vec![],
            has_active_schema: true,
            trace: None,
        };
        let dto = EncodeResponseDto::from(resp);
        assert_eq!(dto.memory_id, "7"); // 128-bit id -> decimal string
        assert!(dto.was_deduplicated);
        assert_eq!(dto.salience.to_bits(), 0.42f32.to_bits());
        assert_eq!(dto.kind, 2); // Consolidated
        assert_eq!(dto.created_at_unix_nanos, 1_700_000_000);
        assert_eq!(dto.auto_edges_added, 3);
    }

    #[test]
    fn recall_answer_kind_maps_single_many_none() {
        let cases = [
            (AnswerKindWire::Single, "single"),
            (AnswerKindWire::Many, "many"),
            (AnswerKindWire::None, "none"),
        ];
        for (kind, expected) in cases {
            let answer = RecallAnswer {
                answer_kind: kind,
                memories: vec![sample_result(0xAA, "hit")],
            };
            let dto = RecallResponseDto::from(answer);
            assert_eq!(dto.answer_kind, expected);
            assert_eq!(dto.memories.len(), 1);
            assert_eq!(dto.memories[0].memory_id, "170"); // 0xAA decimal
            assert_eq!(dto.memories[0].text, "hit");
            assert_eq!(dto.memories[0].kind, 1); // Semantic
        }
    }

    #[test]
    fn forget_response_maps_fields() {
        let dto = ForgetResponseDto::from(ForgetResponse {
            memory_id: 42,
            was_already_forgotten: true,
            edges_removed: 4,
        });
        assert_eq!(dto.memory_id, "42");
        assert!(dto.was_already_forgotten);
        assert_eq!(dto.edges_removed, 4);
    }

    fn sample_list_item(tag: u8) -> MemoryListItem {
        MemoryListItem {
            memory_id: u128::from_be_bytes([tag; 16]),
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

    #[test]
    fn list_page_flattens_frames_and_hex_encodes_tail_cursor() {
        // Two frames; the tail carries a resume cursor -> surfaced as hex.
        let frames = vec![
            MemoryListResponseFrame {
                items: vec![sample_list_item(1)],
                next_cursor: Vec::new(),
                cumulative_count: 1,
                is_final: false,
            },
            MemoryListResponseFrame {
                items: vec![sample_list_item(2)],
                next_cursor: vec![0x2a, 0xff],
                cumulative_count: 2,
                is_final: true,
            },
        ];
        let dto = MemoryListPageDto::from_frames(frames);
        assert_eq!(dto.items.len(), 2);
        assert_eq!(dto.items[0].statement_count, 1);
        assert_eq!(dto.items[1].relation_count, 3);
        assert_eq!(dto.next_cursor.as_deref(), Some("2aff"));

        // An empty tail cursor means "exhausted" -> omitted.
        let done = MemoryListPageDto::from_frames(vec![MemoryListResponseFrame {
            items: vec![sample_list_item(3)],
            next_cursor: Vec::new(),
            cumulative_count: 1,
            is_final: true,
        }]);
        assert!(done.next_cursor.is_none());
    }

    #[test]
    fn inspect_maps_stages_and_hex_encodes_graph_ids() {
        let resp = MemoryInspectResponse {
            memory_id: 0,
            text: "hello".into(),
            artifact: EncodeStageArtifact {
                vector: vec![0.1, 0.2],
                record: Some(EncodeStageRecord {
                    memory_id: 0,
                    kind: 1,
                    salience: 0.5,
                    created_at_unix_nanos: 1,
                    occurred_at_unix_nanos: 0,
                    vector_dim: 384,
                    text_len: 5,
                    lsn: 7,
                }),
                hype_questions: vec!["who?".into()],
                keyword_fields: vec![EncodeStageKeywordField {
                    field: "text".into(),
                    terms: vec!["hello".into()],
                }],
                graph: Some(EncodeStageGraph {
                    nodes: vec![EncodeGraphNode {
                        id: [0x11; 16],
                        name: "Ada".into(),
                        kind: "entity".into(),
                        type_qname: "brain:Person".into(),
                    }],
                    edges: vec![EncodeGraphEdge {
                        source: [0x11; 16],
                        target: [0x22; 16],
                        predicate: "born_in".into(),
                        kind: "statement".into(),
                        confidence: 0.9,
                        event_at_unix_nanos: Some(123),
                    }],
                }),
            },
            edges: vec![
                MemoryEdgeWire {
                    kind: EdgeKindWire::Caused,
                    direction: EdgeDirectionWire::Outgoing,
                    peer: 42,
                    weight: 0.75,
                    created_at_unix_nanos: 900,
                    annotation: Some("manually asserted".into()),
                },
                MemoryEdgeWire {
                    kind: EdgeKindWire::Supports,
                    direction: EdgeDirectionWire::Incoming,
                    peer: 7,
                    weight: 0.25,
                    created_at_unix_nanos: 901,
                    annotation: None,
                },
            ],
        };
        let dto = MemoryInspectDto::from(resp);
        assert_eq!(dto.text, "hello");
        assert_eq!(dto.artifact.vector, vec![0.1, 0.2]);
        assert_eq!(dto.artifact.record.as_ref().unwrap().vector_dim, 384);
        assert_eq!(dto.artifact.hype_questions, vec!["who?".to_string()]);
        let graph = dto.artifact.graph.unwrap();
        assert_eq!(graph.nodes[0].id, "11".repeat(16)); // 16-byte id, lowercase hex
        assert_eq!(graph.edges[0].source, "11".repeat(16));
        assert_eq!(graph.edges[0].target, "22".repeat(16));
        assert_eq!(graph.edges[0].event_at_unix_nanos, Some(123));

        // The memory-edge read side: both directions survive, the kind keeps
        // the wire vocabulary `POST /v1/links` accepts, and `peer` is the
        // decimal form that can be fed straight back into a path parameter.
        // `artifact.graph` above is a different thing entirely — these two must
        // not be conflated, which is why both are asserted in one test.
        assert_eq!(dto.edges.len(), 2);
        assert_eq!(dto.edges[0].kind, "caused");
        assert_eq!(dto.edges[0].direction, "outgoing");
        assert_eq!(dto.edges[0].peer, "42");
        assert_eq!(dto.edges[0].weight, 0.75);
        assert_eq!(dto.edges[0].created_at_unix_nanos, 900);
        assert_eq!(
            dto.edges[0].annotation.as_deref(),
            Some("manually asserted")
        );
        assert_eq!(dto.edges[1].kind, "supports");
        assert_eq!(dto.edges[1].direction, "incoming");
        assert_eq!(dto.edges[1].peer, "7");
        assert!(dto.edges[1].annotation.is_none());
    }
}

// --- Direct vector encode ---------------------------------------------------

/// `POST /v1/memories/vector` body — store a memory with a caller-supplied
/// embedding (`ENCODE_VECTOR_DIRECT`).
///
/// For callers that already embed with their own model and want Brain to skip
/// its embedder. `model_fingerprint` identifies that model: vectors from
/// different models are not comparable, so a recall that mixed them would
/// return nonsense ranked confidently. It is required for that reason.
#[derive(Debug, Deserialize)]
pub struct EncodeVectorBody {
    /// The text the vector was computed from. Stored as the memory's content.
    pub text: String,
    /// The embedding.
    pub vector: Vec<f32>,
    /// A 16-byte identifier for the model that produced it, lowercase hex
    /// (32 characters).
    pub model_fingerprint: String,
    /// Optional session id.
    #[serde(default)]
    pub session: Option<u64>,
    /// Memory kind byte (`0` = episodic, `1` = semantic, `2` = consolidated).
    #[serde(default)]
    pub kind: Option<u8>,
    /// A salience hint in `[0,1]`; Brain decides the final value.
    #[serde(default)]
    pub salience_hint: Option<f32>,
    /// Skip the write when an identical memory already exists.
    #[serde(default)]
    pub deduplicate: bool,
}

impl EncodeVectorBody {
    /// Build the wire request.
    ///
    /// # Errors
    /// Blank text, an empty vector, a fingerprint that is not 32 hex
    /// characters, an unknown kind, or a salience hint outside `[0,1]`.
    pub fn to_request(self) -> Result<EncodeVectorDirectRequest, String> {
        let text = self.text.trim().to_owned();
        if text.is_empty() {
            return Err("text must not be blank".into());
        }
        if self.vector.is_empty() {
            return Err("vector must not be empty".into());
        }
        let fp = hex_decode(&self.model_fingerprint)?;
        let model_fingerprint: [u8; 16] = fp
            .try_into()
            .map_err(|_| "model_fingerprint must be 16 bytes (32 hex characters)".to_string())?;
        if self
            .salience_hint
            .is_some_and(|s| !(0.0..=1.0).contains(&s))
        {
            return Err("salience_hint must be between 0.0 and 1.0".into());
        }
        let kind = match self.kind.unwrap_or(0) {
            0 => MemoryKindWire::Episodic,
            1 => MemoryKindWire::Semantic,
            2 => MemoryKindWire::Consolidated,
            other => return Err(format!("`{other}` is not a memory kind")),
        };
        Ok(EncodeVectorDirectRequest {
            text,
            vector: self.vector,
            model_fingerprint,
            session_id: self.session.unwrap_or(0),
            kind,
            salience_hint: self.salience_hint.unwrap_or(0.0),
            edges: Vec::new(),
            request_id: brain_db_sdk::new_id(),
            txn_id: None,
            deduplicate: self.deduplicate,
            act_as: None,
        })
    }
}

// --- Procedural materialisation ---------------------------------------------

/// `POST /v1/procedural/materialize` body — render the caller's directives
/// into a system prompt block (`MATERIALIZE_PROCEDURAL`).
///
/// This is the op that turns remembered preferences and directives into text
/// an agent can put in front of a model, which is otherwise something every
/// caller reimplements over `/v1/recall`.
#[derive(Debug, Default, Deserialize)]
pub struct MaterializeProceduralBody {
    /// Only directives from these sessions.
    #[serde(default)]
    pub sessions: Vec<u64>,
    /// How many to consider. `0` = the server default.
    #[serde(default)]
    pub top_k: u32,
    /// Drop anything below this confidence.
    #[serde(default)]
    pub min_confidence: f32,
    /// Only these directive categories.
    #[serde(default)]
    pub categories: Vec<String>,
}

impl MaterializeProceduralBody {
    /// Build the wire request. The space is the caller's own, so `space_id`
    /// is left zero for Brain to fill from `act_as`.
    ///
    /// # Errors
    /// A confidence floor outside `[0,1]`.
    pub fn to_request(self) -> Result<MaterializeProceduralRequest, String> {
        if !(0.0..=1.0).contains(&self.min_confidence) {
            return Err("min_confidence must be between 0.0 and 1.0".into());
        }
        Ok(MaterializeProceduralRequest {
            space_id: [0u8; 16],
            session_filter: (!self.sessions.is_empty()).then_some(self.sessions),
            top_k: self.top_k,
            min_confidence: self.min_confidence,
            categories: self.categories,
            request_id: brain_db_sdk::new_id(),
            act_as: None,
        })
    }
}

/// `POST /v1/procedural/materialize` response.
#[derive(Debug, Serialize)]
pub struct MaterializeProceduralResponseDto {
    /// The rendered block, ready to put in a system prompt.
    pub system_block: String,
    /// The statements it was rendered from (UUIDs) — so a caller can show its
    /// work, or follow one back to the memory it came from.
    pub statement_ids: Vec<String>,
    /// How many candidates were considered before trimming.
    pub total_candidates: u32,
    /// Whether the block was cut short by the token budget, meaning some
    /// directives did not make it in.
    pub trimmed_by_budget: bool,
}

impl From<MaterializeProceduralResponse> for MaterializeProceduralResponseDto {
    fn from(r: MaterializeProceduralResponse) -> Self {
        Self {
            system_block: r.system_block,
            statement_ids: r.statement_ids.iter().map(uuid_string).collect(),
            total_candidates: r.total_candidates,
            trimmed_by_budget: r.trimmed_by_budget,
        }
    }
}

#[cfg(test)]
mod memory_extras_tests {
    use super::*;

    fn vector_body() -> EncodeVectorBody {
        EncodeVectorBody {
            text: "hello".into(),
            vector: vec![0.1, 0.2],
            model_fingerprint: "000102030405060708090a0b0c0d0e0f".into(),
            session: None,
            kind: None,
            salience_hint: None,
            deduplicate: false,
        }
    }

    #[test]
    fn occurred_at_has_one_spelling_and_it_carries_its_unit() {
        // An unrecognised key lands in serde's unknown-field bucket, which this
        // body ignores, so the time is silently not set. Asserted rather than
        // assumed: silence is exactly what makes a misspelled field hard to
        // notice from the caller's side.
        let named: EncodeBody =
            serde_json::from_str(r#"{"text":"x","occurred_at_unix_nanos":7}"#).unwrap();
        assert_eq!(named.occurred_at_unix_nanos, Some(7));

        let short: EncodeBody = serde_json::from_str(r#"{"text":"x","occurred_at":7}"#).unwrap();
        assert_eq!(short.occurred_at_unix_nanos, None);
    }

    #[test]
    fn a_vector_encode_requires_the_model_that_produced_it() {
        let req = vector_body().to_request().unwrap();
        assert_eq!(req.model_fingerprint[15], 0x0f);
        assert_eq!(req.vector, vec![0.1, 0.2]);
        assert_eq!(req.kind, MemoryKindWire::Episodic, "0 is the default kind");

        // Vectors from different models are not comparable, so a recall that
        // mixed them would rank nonsense confidently. A wrong-length
        // fingerprint must not silently become a zero one.
        assert!(
            EncodeVectorBody {
                model_fingerprint: "00ff".into(),
                ..vector_body()
            }
            .to_request()
            .is_err()
        );
        assert!(
            EncodeVectorBody {
                model_fingerprint: "nothex".repeat(6),
                ..vector_body()
            }
            .to_request()
            .is_err()
        );
    }

    #[test]
    fn a_vector_encode_rejects_empty_text_vector_and_bad_hint() {
        assert!(
            EncodeVectorBody {
                text: "  ".into(),
                ..vector_body()
            }
            .to_request()
            .is_err()
        );
        assert!(
            EncodeVectorBody {
                vector: vec![],
                ..vector_body()
            }
            .to_request()
            .is_err(),
            "an empty vector is not an embedding"
        );
        assert!(
            EncodeVectorBody {
                salience_hint: Some(2.0),
                ..vector_body()
            }
            .to_request()
            .is_err()
        );
        assert!(
            EncodeVectorBody {
                kind: Some(9),
                ..vector_body()
            }
            .to_request()
            .is_err(),
            "an unknown kind byte"
        );
    }

    #[test]
    fn materialize_defaults_to_no_filters() {
        let req = MaterializeProceduralBody::default().to_request().unwrap();
        assert!(
            req.session_filter.is_none(),
            "an empty list is no filter, not a filter matching nothing"
        );
        assert_eq!(req.categories, Vec::<String>::new());
        assert_eq!(req.space_id, [0u8; 16], "the space comes from act_as");
    }

    #[test]
    fn materialize_carries_filters_and_guards_confidence() {
        let req = MaterializeProceduralBody {
            sessions: vec![1, 2],
            top_k: 5,
            min_confidence: 0.4,
            categories: vec!["tone".into()],
        }
        .to_request()
        .unwrap();
        assert_eq!(req.session_filter.unwrap(), vec![1, 2]);
        assert_eq!(req.top_k, 5);
        assert_eq!(req.categories, vec!["tone".to_string()]);

        assert!(
            MaterializeProceduralBody {
                min_confidence: 1.5,
                ..MaterializeProceduralBody::default()
            }
            .to_request()
            .is_err()
        );
    }

    #[test]
    fn a_trimmed_block_says_so() {
        let dto = MaterializeProceduralResponseDto::from(MaterializeProceduralResponse {
            system_block: "Be terse.".into(),
            statement_ids: vec![[1u8; 16]],
            total_candidates: 9,
            trimmed_by_budget: true,
        });
        // Without this a caller assumes the block is complete and silently
        // drops directives it never learned about.
        assert!(dto.trimmed_by_budget);
        assert_eq!(dto.total_candidates, 9);
        assert_eq!(dto.statement_ids.len(), 1);
        assert!(dto.statement_ids[0].contains('-'));
    }
}

#[cfg(test)]
mod recall_trace_tests {
    use super::RecallResponseDto;
    use brain_db_sdk::wire::types::{
        AnswerKindWire, RecallResponseFrame, RecallTrace, RecallTraceFilterChain,
    };

    fn frame_with_trace() -> RecallResponseFrame {
        RecallResponseFrame {
            answer_kind: AnswerKindWire::None,
            memories: Vec::new(),
            is_final: true,
            cumulative_count: 0,
            estimated_remaining: Some(0),
            trace: Some(RecallTrace {
                retrievers: Vec::new(),
                filter_chain: RecallTraceFilterChain {
                    before: 9,
                    after_type: 9,
                    after_temporal: 9,
                    after_confidence: 9,
                    after_tombstone: 9,
                    after_supersession: 4,
                    after_as_of: 4,
                    after_limit: 4,
                    dropped_by_type: Vec::new(),
                    dropped_by_temporal: Vec::new(),
                    dropped_by_confidence: Vec::new(),
                    dropped_by_tombstone: Vec::new(),
                    dropped_by_supersession: Vec::new(),
                    dropped_by_as_of: Vec::new(),
                    dropped_by_limit: Vec::new(),
                },
                rerank: None,
                total_latency_ms: 1.5,
                fusion: None,
            }),
        }
    }

    /// The deployment's metering sink and the caller ask independently. When
    /// only the sink wanted the trace, the blob must not leak into the body of
    /// a response that did not request it.
    #[test]
    fn a_trace_fetched_for_the_sink_stays_out_of_the_response() {
        let frames = vec![frame_with_trace()];
        let serialized = RecallResponseDto::serialize_trace(&frames);
        assert!(
            serialized.is_some(),
            "the sink's copy is produced from the frames"
        );

        let dto = RecallResponseDto::from_frames_with_trace(frames, None);
        assert!(
            dto.trace.is_none(),
            "a caller that did not ask for the trace must not receive it"
        );
    }

    /// And when the caller did ask, it is the same serialization, not a second
    /// one — `serialize_trace` runs once per recall and both consumers share it.
    #[test]
    fn the_caller_gets_the_same_serialization_the_sink_saw() {
        let frames = vec![frame_with_trace()];
        let serialized = RecallResponseDto::serialize_trace(&frames);
        let dto = RecallResponseDto::from_frames_with_trace(frames, serialized.clone());
        assert_eq!(dto.trace, serialized);
    }

    #[test]
    fn no_trace_on_the_frames_means_nothing_to_serialize() {
        let mut frame = frame_with_trace();
        frame.trace = None;
        assert!(RecallResponseDto::serialize_trace(&[frame]).is_none());
    }
}
