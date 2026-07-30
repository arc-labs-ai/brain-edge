//! Memory-graph edge verbs (link / unlink) + the typed-graph export
//! (GRAPH_FETCH).

// The DTOs below mirror the HTTP contract one-for-one: the JSON field names are
// the API, and `tools/http_manifest.py` emits every one of them — with its type
// and serde attributes — into `contract/http-routes.json`, which the three SDK
// clients are checked against. A doc comment on each of ~312 fields would
// restate the field name; the ones that carry meaning beyond their name have
// one written below.
#![allow(missing_docs)]

use brain_db_sdk::wire::types::{
    EdgeKindWire, GraphEdge, GraphFetchRequest, GraphFetchResponseFrame, GraphNode, LinkResponse,
    UnlinkResponse, WireMemoryId,
};
use serde::{Deserialize, Serialize};

use super::{hex_decode, hex_encode, mem_id_decimal, parse_memory_id, uuid_string};

/// Parse an edge-kind name (snake or lower case) to its wire variant.
fn parse_edge_kind(s: &str) -> Result<EdgeKindWire, String> {
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "caused" => EdgeKindWire::Caused,
        "followed_by" | "followedby" => EdgeKindWire::FollowedBy,
        "derived_from" | "derivedfrom" => EdgeKindWire::DerivedFrom,
        "similar_to" | "similarto" => EdgeKindWire::SimilarTo,
        "contradicts" => EdgeKindWire::Contradicts,
        "supports" => EdgeKindWire::Supports,
        "references" => EdgeKindWire::References,
        "part_of" | "partof" => EdgeKindWire::PartOf,
        other => return Err(format!("unknown edge kind `{other}`")),
    })
}

/// Render an edge kind as its snake-case name.
fn edge_kind_str(k: EdgeKindWire) -> &'static str {
    match k {
        EdgeKindWire::Caused => "caused",
        EdgeKindWire::FollowedBy => "followed_by",
        EdgeKindWire::DerivedFrom => "derived_from",
        EdgeKindWire::SimilarTo => "similar_to",
        EdgeKindWire::Contradicts => "contradicts",
        EdgeKindWire::Supports => "supports",
        EdgeKindWire::References => "references",
        EdgeKindWire::PartOf => "part_of",
    }
}

/// `POST /v1/links` body — create/overwrite a directed edge between two memories.
#[derive(Debug, Deserialize)]
pub struct LinkBody {
    pub source: String,
    pub target: String,
    pub kind: String,
    /// `[0, 1]` for most kinds; `[-1, 1]` for `contradicts`. Defaults to `1.0`.
    #[serde(default = "default_weight")]
    pub weight: f32,
}

fn default_weight() -> f32 {
    1.0
}

impl LinkBody {
    /// Parse into `(source, target, kind, weight)`.
    pub fn parts(&self) -> Result<(WireMemoryId, WireMemoryId, EdgeKindWire, f32), String> {
        Ok((
            parse_memory_id(&self.source)?,
            parse_memory_id(&self.target)?,
            parse_edge_kind(&self.kind)?,
            self.weight,
        ))
    }
}

/// `POST /v1/links` response.
#[derive(Debug, Serialize)]
pub struct LinkResponseDto {
    pub source: String,
    pub target: String,
    pub kind: &'static str,
    pub weight: f32,
    pub created_at_unix_nanos: u64,
    pub already_existed: bool,
}

impl From<LinkResponse> for LinkResponseDto {
    fn from(r: LinkResponse) -> Self {
        Self {
            source: r.source.to_string(),
            target: r.target.to_string(),
            kind: edge_kind_str(r.kind),
            weight: r.weight,
            created_at_unix_nanos: r.created_at_unix_nanos,
            already_existed: r.already_existed,
        }
    }
}

/// `DELETE /v1/links` body — remove a directed edge (idempotent).
#[derive(Debug, Deserialize)]
pub struct UnlinkBody {
    pub source: String,
    pub target: String,
    pub kind: String,
}

impl UnlinkBody {
    /// Parse into `(source, target, kind)`.
    pub fn parts(&self) -> Result<(WireMemoryId, WireMemoryId, EdgeKindWire), String> {
        Ok((
            parse_memory_id(&self.source)?,
            parse_memory_id(&self.target)?,
            parse_edge_kind(&self.kind)?,
        ))
    }
}

/// `DELETE /v1/links` response.
#[derive(Debug, Serialize)]
pub struct UnlinkResponseDto {
    pub source: String,
    pub target: String,
    pub kind: &'static str,
    pub removed: bool,
}

impl From<UnlinkResponse> for UnlinkResponseDto {
    fn from(r: UnlinkResponse) -> Self {
        Self {
            source: r.source.to_string(),
            target: r.target.to_string(),
            kind: edge_kind_str(r.kind),
            removed: r.removed,
        }
    }
}

// ---- typed-graph export (GET /v1/graph) ----------------------------------

/// Default / max statements consumed off the spine per `GET /v1/graph` page.
const GRAPH_DEFAULT_LIMIT: u32 = 200;
const GRAPH_MAX_LIMIT: u32 = 500;

/// Node-kind bytes (mirror the wire `GraphNodeKindWire`).
const NODE_ENTITY: u8 = 0;
const NODE_STATEMENT: u8 = 1;
const NODE_MEMORY: u8 = 2;
/// Edge-kind bytes (mirror the wire `GraphEdgeKindWire`).
const EDGE_MENTIONS: u8 = 3;
/// The eight memory↔memory builtin kinds occupy `4..=11`, one byte per kind,
/// laid out as `EDGE_MEMORY_BASE + EdgeKindWire as u8` — so the byte alone
/// identifies the link and `edge_kind_str` can name it after the shift.
const EDGE_MEMORY_BASE: u8 = 4;
const EDGE_MEMORY_LAST: u8 = EDGE_MEMORY_BASE + 7;

/// Name a graph-export edge-kind byte. `None` for a byte this build doesn't
/// know, which the caller renders as `unknown` rather than guessing.
fn graph_edge_kind_str(kind: u8) -> Option<&'static str> {
    Some(match kind {
        0 => "relation",
        1 => "fact",
        2 => "has_statement",
        EDGE_MENTIONS => "mentions",
        EDGE_MEMORY_BASE..=EDGE_MEMORY_LAST => {
            return memory_edge_kind(kind).map(edge_kind_str);
        }
        _ => return None,
    })
}

/// The `EdgeKindWire` behind a memory↔memory export byte, if it is one.
fn memory_edge_kind(kind: u8) -> Option<EdgeKindWire> {
    if !(EDGE_MEMORY_BASE..=EDGE_MEMORY_LAST).contains(&kind) {
        return None;
    }
    Some(match kind - EDGE_MEMORY_BASE {
        0 => EdgeKindWire::Caused,
        1 => EdgeKindWire::FollowedBy,
        2 => EdgeKindWire::DerivedFrom,
        3 => EdgeKindWire::SimilarTo,
        4 => EdgeKindWire::Contradicts,
        5 => EdgeKindWire::Supports,
        6 => EdgeKindWire::References,
        _ => EdgeKindWire::PartOf,
    })
}

/// Render a 16-byte node id per its kind: memory ids are 128-bit decimals
/// (matching `/v1/memories` + `/v1/recall`); entity/statement ids are UUIDs.
fn node_id_str(id: &[u8; 16], kind: u8) -> String {
    if kind == NODE_MEMORY {
        mem_id_decimal(id)
    } else {
        uuid_string(id)
    }
}

/// `GET /v1/graph` query parameters. Default layer is the concept map
/// (entities + Relation/Fact edges); the toggles add value-object statement
/// nodes and source-memory provenance nodes.
#[derive(Debug, Deserialize)]
pub struct GraphFetchQuery {
    /// Page size (statements off the spine), clamped to `1..=500` (default 200).
    #[serde(default)]
    pub limit: Option<u32>,
    /// Opaque keyset cursor (hex) from a previous page's `next_cursor`.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Emit value-object statement nodes + `has_statement` edges.
    #[serde(default)]
    pub include_statements: Option<bool>,
    /// Emit source memory nodes + `mentions` edges.
    #[serde(default)]
    pub include_memories: Option<bool>,
    /// Emit the stored memory↔memory edges (`similar_to`, `followed_by`, …)
    /// between the page's memory nodes. Requires `include_memories`; asking
    /// for it alone is rejected up front rather than by the server, so the
    /// caller gets a 400 naming the missing layer instead of a wire error.
    #[serde(default)]
    pub include_memory_edges: Option<bool>,
    /// Include tombstoned statements/relations (default false).
    #[serde(default)]
    pub include_tombstoned: Option<bool>,
}

impl GraphFetchQuery {
    pub fn to_request(&self) -> Result<GraphFetchRequest, String> {
        let limit = self
            .limit
            .unwrap_or(GRAPH_DEFAULT_LIMIT)
            .clamp(1, GRAPH_MAX_LIMIT);
        let cursor = match &self.cursor {
            Some(c) if !c.is_empty() => hex_decode(c)?,
            _ => Vec::new(),
        };
        let include_memories = self.include_memories.unwrap_or(false);
        let include_memory_edges = self.include_memory_edges.unwrap_or(false);
        if include_memory_edges && !include_memories {
            return Err(
                "include_memory_edges requires include_memories: memory↔memory edges hang off \
                 memory nodes, and without that layer their endpoints would not be in the page"
                    .to_string(),
            );
        }
        Ok(GraphFetchRequest {
            limit,
            cursor,
            include_statements: self.include_statements.unwrap_or(false),
            include_memories,
            include_memory_edges,
            include_tombstoned: self.include_tombstoned.unwrap_or(false),
            act_as: None,
        })
    }
}

/// One graph node. `kind`: `entity` | `statement` | `memory`.
#[derive(Debug, Serialize)]
pub struct GraphNodeDto {
    /// Entity/statement UUID or memory decimal id, per `kind`.
    pub id: String,
    pub kind: &'static str,
    pub label: String,
    /// Entity type qname (e.g. `brain:Person`); empty for non-entity nodes.
    pub type_qname: String,
}

impl From<GraphNode> for GraphNodeDto {
    fn from(n: GraphNode) -> Self {
        let kind = match n.kind {
            NODE_ENTITY => "entity",
            NODE_STATEMENT => "statement",
            NODE_MEMORY => "memory",
            _ => "unknown",
        };
        Self {
            id: node_id_str(&n.id, n.kind),
            kind,
            label: n.label,
            type_qname: n.type_qname,
        }
    }
}

/// One graph edge. `kind` is `relation` | `fact` | `has_statement` |
/// `mentions` for the typed-graph projections, or one of the eight
/// memory↔memory link names (`caused` | `followed_by` | `derived_from` |
/// `similar_to` | `contradicts` | `supports` | `references` | `part_of`).
#[derive(Debug, Serialize)]
pub struct GraphEdgeDto {
    pub from_id: String,
    pub to_id: String,
    pub kind: &'static str,
    /// Predicate / relation-type label; empty for `mentions`. For a
    /// memory↔memory edge it repeats the kind name, so a renderer can label
    /// every edge uniformly off `label`.
    pub label: String,
}

impl From<GraphEdge> for GraphEdgeDto {
    fn from(e: GraphEdge) -> Self {
        let kind = graph_edge_kind_str(e.kind).unwrap_or("unknown");
        // `mentions` runs memory→entity; the eight builtin kinds run
        // memory→memory; everything else is entity/statement→entity/statement.
        // The id-space matters: memory ids render as 128-bit decimals, the
        // rest as UUIDs, so a mis-inferred end would print an unmatchable id.
        let (from_kind, to_kind) = if e.kind == EDGE_MENTIONS {
            (NODE_MEMORY, NODE_ENTITY)
        } else if memory_edge_kind(e.kind).is_some() {
            (NODE_MEMORY, NODE_MEMORY)
        } else {
            (NODE_ENTITY, NODE_ENTITY)
        };
        Self {
            from_id: node_id_str(&e.from_id, from_kind),
            to_id: node_id_str(&e.to_id, to_kind),
            kind,
            label: e.label,
        }
    }
}

/// `GET /v1/graph` response: a page of nodes + edges plus the resume cursor.
/// Nodes/edges may repeat across pages (completeness, not disjointness) —
/// dedup by id. `next_cursor` is omitted when the export is exhausted.
#[derive(Debug, Serialize)]
pub struct GraphPageDto {
    pub nodes: Vec<GraphNodeDto>,
    pub edges: Vec<GraphEdgeDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

impl GraphPageDto {
    pub fn from_frames(frames: Vec<GraphFetchResponseFrame>) -> Self {
        let next = frames
            .last()
            .map(|f| f.next_cursor.clone())
            .filter(|c| !c.is_empty())
            .map(|c| hex_encode(&c));
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        for f in frames {
            nodes.extend(f.nodes.into_iter().map(GraphNodeDto::from));
            edges.extend(f.edges.into_iter().map(GraphEdgeDto::from));
        }
        Self {
            nodes,
            edges,
            next_cursor: next,
        }
    }
}

#[cfg(test)]
mod graph_tests {
    use super::*;

    #[test]
    fn query_clamps_limit_and_defaults_layers() {
        let q = GraphFetchQuery {
            limit: Some(9999),
            cursor: None,
            include_statements: None,
            include_memories: Some(true),
            include_memory_edges: None,
            include_tombstoned: None,
        };
        let r = q.to_request().unwrap();
        assert_eq!(r.limit, GRAPH_MAX_LIMIT);
        assert!(!r.include_statements);
        assert!(r.include_memories);
        assert!(!r.include_memory_edges);
        assert!(r.cursor.is_empty());
    }

    #[test]
    fn memory_edges_require_the_memory_layer() {
        let alone = GraphFetchQuery {
            limit: None,
            cursor: None,
            include_statements: None,
            include_memories: None,
            include_memory_edges: Some(true),
            include_tombstoned: None,
        };
        let err = alone.to_request().unwrap_err();
        assert!(err.contains("include_memories"), "{err}");

        let together = GraphFetchQuery {
            limit: None,
            cursor: None,
            include_statements: None,
            include_memories: Some(true),
            include_memory_edges: Some(true),
            include_tombstoned: None,
        };
        let r = together.to_request().unwrap();
        assert!(r.include_memories && r.include_memory_edges);
    }

    #[test]
    fn memory_edge_bytes_name_each_kind_and_read_both_ends_as_memories() {
        let mut a = [0u8; 16];
        a[15] = 11;
        let mut b = [0u8; 16];
        b[15] = 12;
        let expected = [
            (4u8, "caused"),
            (5, "followed_by"),
            (6, "derived_from"),
            (7, "similar_to"),
            (8, "contradicts"),
            (9, "supports"),
            (10, "references"),
            (11, "part_of"),
        ];
        for (byte, name) in expected {
            let e = GraphEdgeDto::from(GraphEdge {
                from_id: a,
                to_id: b,
                kind: byte,
                label: name.to_string(),
            });
            assert_eq!(e.kind, name, "byte {byte}");
            // Both ends are memories → 128-bit decimals, never UUIDs.
            assert_eq!(e.from_id, "11");
            assert_eq!(e.to_id, "12");
        }
    }

    #[test]
    fn unknown_edge_byte_is_not_guessed() {
        let e = GraphEdgeDto::from(GraphEdge {
            from_id: [0u8; 16],
            to_id: [0u8; 16],
            kind: 99,
            label: String::new(),
        });
        assert_eq!(e.kind, "unknown");
    }

    #[test]
    fn entity_node_is_uuid_memory_node_is_decimal() {
        let mut id = [0u8; 16];
        id[15] = 5;
        let ent = GraphNodeDto::from(GraphNode {
            id,
            kind: NODE_ENTITY,
            label: "Sarah Chen".into(),
            type_qname: "brain:Person".into(),
        });
        assert_eq!(ent.kind, "entity");
        assert_eq!(ent.id, "00000000-0000-0000-0000-000000000005");

        let mem = GraphNodeDto::from(GraphNode {
            id,
            kind: NODE_MEMORY,
            label: "a memory".into(),
            type_qname: String::new(),
        });
        assert_eq!(mem.kind, "memory");
        assert_eq!(mem.id, "5"); // 128-bit decimal, not UUID
    }

    #[test]
    fn mentions_edge_source_is_decimal_memory() {
        let mut mem = [0u8; 16];
        mem[15] = 7;
        let ent = [0x11u8; 16];
        let e = GraphEdgeDto::from(GraphEdge {
            from_id: mem,
            to_id: ent,
            kind: EDGE_MENTIONS,
            label: String::new(),
        });
        assert_eq!(e.kind, "mentions");
        assert_eq!(e.from_id, "7"); // memory → decimal
        assert!(e.to_id.contains('-')); // entity → uuid
    }

    #[test]
    fn relation_edge_both_ends_uuid() {
        let a = [0x01u8; 16];
        let b = [0x02u8; 16];
        let e = GraphEdgeDto::from(GraphEdge {
            from_id: a,
            to_id: b,
            kind: 0,
            label: "brain:works_at".into(),
        });
        assert_eq!(e.kind, "relation");
        assert!(e.from_id.contains('-') && e.to_id.contains('-'));
    }
}
