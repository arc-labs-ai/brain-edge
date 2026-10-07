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
    AnswerKindWire, EncodeGraphEdge, EncodeGraphNode, EncodeResponse, EncodeStageArtifact,
    EncodeStageGraph, EncodeStageKeywordField, EncodeStageRecord, ForgetResponse,
    MemoryInspectResponse, MemoryListDirWire, MemoryListItem, MemoryListRequest,
    MemoryListSortWire, MemoryListTimeAxisWire, MemoryResult, WireMemoryId,
};
use serde::{Deserialize, Serialize};

use super::{hex_decode, hex_encode, mem_id_decimal, parse_memory_id};

/// `POST /v1/memories` body.
#[derive(Debug, Deserialize)]
pub struct EncodeBody {
    /// The text to remember.
    pub text: String,
    /// Optional session id.
    #[serde(default)]
    pub session: Option<u64>,
    /// Optional event time (unix nanos).
    #[serde(default)]
    pub occurred_at: Option<u64>,
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

/// `POST /v1/recall` body. Accepts `query` or its alias `cue`.
#[derive(Debug, Deserialize)]
pub struct RecallBody {
    #[serde(alias = "cue")]
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
}

impl From<RecallAnswer> for RecallResponseDto {
    fn from(a: RecallAnswer) -> Self {
        let answer_kind = match a.answer_kind {
            AnswerKindWire::Single => "single",
            AnswerKindWire::Many => "many",
            AnswerKindWire::None => "none",
        };
        Self {
            answer_kind,
            memories: a.memories.into_iter().map(MemoryHit::from).collect(),
        }
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
            memory_id: mem_id_decimal(&m.memory_id),
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
/// `found = false` (with an empty `artifact`) when the id doesn't exist under
/// the caller's scope.
#[derive(Debug, Serialize)]
pub struct MemoryInspectDto {
    pub found: bool,
    /// Decimal string of the 128-bit memory id (matches `/v1/memories`).
    pub memory_id: String,
    pub text: String,
    pub artifact: StageArtifactDto,
}

impl From<MemoryInspectResponse> for MemoryInspectDto {
    fn from(r: MemoryInspectResponse) -> Self {
        Self {
            found: r.found,
            memory_id: mem_id_decimal(&r.memory_id),
            text: r.text,
            artifact: StageArtifactDto::from(r.artifact),
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
            memory_id: mem_id_decimal(&r.memory_id),
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
        EncodeGraphEdge, EncodeGraphNode, EncodeResponse, EncodeStageArtifact, EncodeStageGraph,
        EncodeStageKeywordField, EncodeStageRecord, ForgetResponse, MemoryInspectResponse,
        MemoryKindWire, MemoryListItem, MemoryListResponseFrame, MemoryResult,
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
            memory_id: [tag; 16],
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
            found: true,
            memory_id: [0; 16],
            text: "hello".into(),
            artifact: EncodeStageArtifact {
                vector: vec![0.1, 0.2],
                record: Some(EncodeStageRecord {
                    memory_id: [0; 16],
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
        };
        let dto = MemoryInspectDto::from(resp);
        assert!(dto.found);
        assert_eq!(dto.text, "hello");
        assert_eq!(dto.artifact.vector, vec![0.1, 0.2]);
        assert_eq!(dto.artifact.record.as_ref().unwrap().vector_dim, 384);
        assert_eq!(dto.artifact.hype_questions, vec!["who?".to_string()]);
        let graph = dto.artifact.graph.unwrap();
        assert_eq!(graph.nodes[0].id, "11".repeat(16)); // 16-byte id, lowercase hex
        assert_eq!(graph.edges[0].source, "11".repeat(16));
        assert_eq!(graph.edges[0].target, "22".repeat(16));
        assert_eq!(graph.edges[0].event_at_unix_nanos, Some(123));
    }
}
