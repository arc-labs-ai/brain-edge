//! Entity-graph request/response DTOs — the public JSON contract for the
//! `/v1/entities*` verbs (create, resolve, get, list, traverse).
//!
//! Deliberately identical to the Arc cloud gateway's contract: snake_case
//! fields, 16-byte entity/relation ids as hyphenated UUID strings.

use brain_db_sdk::wire::types::{
    EntityCreateRequest, EntityCreateResponse, EntityGetRequest, EntityListItem,
    EntityListRequest, EntityResolveRequest, EntityResolveResponse, EntityView,
    RelationTraverseRequest, RelationTraverseResponseFrame, ResolutionOutcomeWire, TraversalPathWire,
};
use serde::{Deserialize, Serialize};

use super::{parse_uuid, uuid_string};

/// Default page size for `GET /v1/entities` when the client omits `limit`.
const DEFAULT_LIST_LIMIT: u32 = 100;
/// Hard ceiling on the page size (mirrors Brain's `1..=1000`).
const MAX_LIST_LIMIT: u32 = 1000;
/// Default traversal depth when the client omits `max_depth`.
const DEFAULT_TRAVERSE_DEPTH: u32 = 3;
/// Hard ceiling on traversal depth (mirrors Brain's `MAX_DEPTH = 5`).
const MAX_TRAVERSE_DEPTH: u32 = 5;
/// Default node budget when the client omits `max_nodes`.
const DEFAULT_TRAVERSE_NODES: u32 = 100;
/// Hard ceiling on the node budget (mirrors Brain's `max_nodes ≤ 1000`).
const MAX_TRAVERSE_NODES: u32 = 1000;

/// Whether an entity is unmerged: the wire carries an all-zero `merged_into`
/// sentinel for "not merged".
fn merged_into_string(id: &[u8; 16]) -> Option<String> {
    (id != &[0u8; 16]).then(|| uuid_string(id))
}

// --- Resolve ---------------------------------------------------------------

/// `POST /v1/entities/resolve` body.
#[derive(Debug, Deserialize)]
pub struct ResolveEntityBody {
    /// The candidate name to resolve (e.g. `"Ada Lovelace"`).
    pub candidate_name: String,
    /// Optional free-text context to disambiguate. Omitted → empty.
    #[serde(default)]
    pub resolution_context: String,
    /// Entity type hint; `0` (the default) resolves across every declared type.
    #[serde(default)]
    pub type_hint: u32,
    /// Whether to mint a new entity on a miss. Defaults to `false` (pure read).
    #[serde(default)]
    pub allow_create: bool,
}

impl ResolveEntityBody {
    /// Build the wire request. Fails on a blank candidate name.
    pub fn to_request(&self) -> Result<EntityResolveRequest, String> {
        let candidate_name = self.candidate_name.trim().to_owned();
        if candidate_name.is_empty() {
            return Err("candidate_name must not be blank".into());
        }
        Ok(EntityResolveRequest {
            candidate_name,
            resolution_context: self.resolution_context.clone(),
            entity_type_hint: self.type_hint,
            allow_create: self.allow_create,
            request_id: brain_db_sdk::new_id(),
            act_as: None,
        })
    }
}

/// `POST /v1/entities/resolve` response.
#[derive(Debug, Serialize)]
pub struct ResolveEntityResponseDto {
    /// The resolution outcome: `resolved`, `created`, `ambiguous`, `not_found`.
    pub outcome: &'static str,
    /// Which resolver tier matched (`1..=5`; `0` if unresolved).
    pub tier: u8,
    /// Match confidence in `[0,1]`.
    pub confidence: f32,
    /// The resolved (or created) entity id as a UUID; `null` when unresolved or
    /// ambiguous.
    pub entity_id: Option<String>,
    /// Ranked candidate entity ids (UUIDs) when `ambiguous`; empty otherwise.
    pub candidate_ids: Vec<String>,
}

impl From<EntityResolveResponse> for ResolveEntityResponseDto {
    fn from(r: EntityResolveResponse) -> Self {
        // Surface a bound id only when the outcome actually bound one; the wire
        // carries an all-zero sentinel for `Ambiguous`/`NotFound`.
        let (outcome, bound) = match r.outcome {
            ResolutionOutcomeWire::Resolved => ("resolved", true),
            ResolutionOutcomeWire::Created => ("created", true),
            ResolutionOutcomeWire::Ambiguous => ("ambiguous", false),
            ResolutionOutcomeWire::NotFound => ("not_found", false),
        };
        Self {
            outcome,
            tier: r.tier,
            confidence: r.confidence,
            entity_id: bound.then(|| uuid_string(&r.resolved_entity)),
            candidate_ids: r.candidate_ids.iter().map(uuid_string).collect(),
        }
    }
}

// --- Create ----------------------------------------------------------------

/// `POST /v1/entities` body — create a typed entity.
#[derive(Debug, Deserialize)]
pub struct CreateEntityBody {
    /// The registered entity type id (built-in Person=1, …, or a
    /// schema-declared type). Must be non-zero.
    pub entity_type_id: u32,
    /// The canonical display name.
    pub canonical_name: String,
    /// Known aliases.
    #[serde(default)]
    pub aliases: Vec<String>,
}

impl CreateEntityBody {
    /// Build the wire request. Fails on a zero type id or a blank name.
    pub fn to_request(&self) -> Result<EntityCreateRequest, String> {
        if self.entity_type_id == 0 {
            return Err("entity_type_id must be non-zero".into());
        }
        let canonical_name = self.canonical_name.trim().to_owned();
        if canonical_name.is_empty() {
            return Err("canonical_name must not be blank".into());
        }
        Ok(EntityCreateRequest {
            entity_type_id: self.entity_type_id,
            canonical_name,
            aliases: self.aliases.clone(),
            attributes_blob: Vec::new(),
            session_id: 0,
            request_id: brain_db_sdk::new_id(),
            act_as: None,
        })
    }
}

/// `POST /v1/entities` response.
#[derive(Debug, Serialize)]
pub struct CreateEntityResponseDto {
    /// The new entity's id (UUID).
    pub entity_id: String,
}

impl From<EntityCreateResponse> for CreateEntityResponseDto {
    fn from(r: EntityCreateResponse) -> Self {
        Self {
            entity_id: uuid_string(&r.entity_id),
        }
    }
}

// --- Get / List ------------------------------------------------------------

/// A read-side entity view in the public JSON contract. Ids are UUID strings.
#[derive(Debug, Serialize)]
pub struct EntityDetailDto {
    /// The entity id (UUID).
    pub entity_id: String,
    /// Registered type id (`0` = none/unknown).
    pub entity_type_id: u32,
    /// Canonical display name.
    pub canonical_name: String,
    /// Known aliases.
    pub aliases: Vec<String>,
    /// How many memories mention this entity.
    pub mention_count: u32,
    /// Creation time (unix nanos).
    pub created_at_unix_nanos: u64,
    /// Last-update time (unix nanos).
    pub updated_at_unix_nanos: u64,
    /// Survivor id (UUID) if merged away; `null` otherwise.
    pub merged_into: Option<String>,
}

impl From<EntityView> for EntityDetailDto {
    fn from(v: EntityView) -> Self {
        Self {
            entity_id: uuid_string(&v.entity_id),
            entity_type_id: v.entity_type_id,
            canonical_name: v.canonical_name,
            aliases: v.aliases,
            mention_count: v.mention_count,
            created_at_unix_nanos: v.created_at_unix_nanos,
            updated_at_unix_nanos: v.updated_at_unix_nanos,
            merged_into: merged_into_string(&v.merged_into),
        }
    }
}

/// Build a wire `ENTITY_GET` request from a UUID path id.
pub fn get_request_from_id(id: &str) -> Result<EntityGetRequest, String> {
    Ok(EntityGetRequest {
        entity_id: parse_uuid(id)?,
        act_as: None,
    })
}

/// `GET /v1/entities` query parameters. All optional; omitted fields fall back
/// to "no filter" (and a default page size).
#[derive(Debug, Default, Deserialize)]
pub struct ListEntitiesQuery {
    /// `0`/omitted = no type filter.
    #[serde(default)]
    pub type_id: u32,
    /// Empty/omitted = no name-prefix filter.
    #[serde(default)]
    pub prefix: String,
    /// Minimum mention count (`0`/omitted = no filter).
    #[serde(default)]
    pub mention_count_min: u32,
    /// Include tombstoned entities (default `false`).
    #[serde(default)]
    pub include_tombstoned: bool,
    /// Include merged-away entities (default `false`).
    #[serde(default)]
    pub include_merged: bool,
    /// Page size; omitted → 100, clamped to `1..=1000`.
    pub limit: Option<u32>,
}

impl ListEntitiesQuery {
    /// Build the wire request. v1 exposes no cursor pagination, so an empty
    /// cursor is always sent.
    pub fn to_request(&self) -> EntityListRequest {
        let limit = self
            .limit
            .unwrap_or(DEFAULT_LIST_LIMIT)
            .clamp(1, MAX_LIST_LIMIT);
        EntityListRequest {
            entity_type_id: self.type_id,
            name_prefix: self.prefix.clone(),
            mention_count_min: self.mention_count_min,
            include_tombstoned: self.include_tombstoned,
            include_merged: self.include_merged,
            limit,
            cursor: Vec::new(),
            act_as: None,
        }
    }
}

/// `GET /v1/entities` response: a page of entities plus its count.
#[derive(Debug, Serialize)]
pub struct ListEntitiesResponseDto {
    /// The entities in this page.
    pub entities: Vec<EntityDetailDto>,
    /// Number of entities returned.
    pub count: usize,
}

impl ListEntitiesResponseDto {
    /// Assemble from the flattened `ENTITY_LIST` items.
    pub fn from_items(items: Vec<EntityListItem>) -> Self {
        let entities: Vec<EntityDetailDto> = items
            .into_iter()
            .map(|item| EntityDetailDto::from(item.entity))
            .collect();
        let count = entities.len();
        Self { entities, count }
    }
}

// --- Traverse --------------------------------------------------------------

/// `POST /v1/entities/{id}/traverse` body. The anchor entity comes from the
/// path id; the body carries the walk parameters. All optional with sane
/// defaults (outgoing, depth 3, 100 nodes).
#[derive(Debug, Default, Deserialize)]
pub struct TraverseBody {
    /// `outgoing` (default), `incoming`, or `both`.
    #[serde(default)]
    pub direction: String,
    /// Relation types to follow (empty = every type).
    #[serde(default)]
    pub relation_types: Vec<String>,
    /// Max hop depth; omitted → 3, clamped to `1..=5`.
    #[serde(default)]
    pub max_depth: Option<u32>,
    /// Max nodes to visit; omitted → 100, clamped to `1..=1000`.
    #[serde(default)]
    pub max_nodes: Option<u32>,
    /// Include superseded relations (default `false`).
    #[serde(default)]
    pub include_superseded: bool,
}

/// The traversal-direction wire byte (`0` = outgoing, `1` = incoming, `2` = both).
fn traverse_direction_wire(raw: &str) -> Result<u8, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "outgoing" | "out" => Ok(0),
        "incoming" | "in" => Ok(1),
        "both" => Ok(2),
        _ => Err("direction must be one of: outgoing, incoming, both".into()),
    }
}

impl TraverseBody {
    /// Build the wire request from the path id and this body. `time_at = 0`
    /// means "as of now".
    pub fn to_request(&self, id: &str) -> Result<RelationTraverseRequest, String> {
        let start_entity = parse_uuid(id)?;
        let direction = traverse_direction_wire(&self.direction)?;
        let max_depth = self
            .max_depth
            .unwrap_or(DEFAULT_TRAVERSE_DEPTH)
            .clamp(1, MAX_TRAVERSE_DEPTH);
        let max_nodes = self
            .max_nodes
            .unwrap_or(DEFAULT_TRAVERSE_NODES)
            .clamp(1, MAX_TRAVERSE_NODES);
        Ok(RelationTraverseRequest {
            start_entity,
            relation_types: self.relation_types.clone(),
            direction,
            max_depth,
            max_nodes,
            time_at_unix_nanos: 0,
            include_superseded: self.include_superseded,
            request_id: brain_db_sdk::new_id(),
            act_as: None,
        })
    }
}

/// One hop in a traversal path (public JSON). Ids are UUID strings.
#[derive(Debug, Serialize)]
pub struct TraversalStepDto {
    /// The relation traversed (UUID).
    pub relation_id: String,
    /// The `from` endpoint (UUID).
    pub from: String,
    /// The `to` endpoint (UUID).
    pub to: String,
    /// The relation type (`namespace:name`).
    pub relation_type: String,
    /// Hop depth (`1`-based).
    pub depth: u32,
}

/// One full path (public JSON).
#[derive(Debug, Serialize)]
pub struct TraversalPathDto {
    /// The ordered hops of this path.
    pub steps: Vec<TraversalStepDto>,
}

impl From<TraversalPathWire> for TraversalPathDto {
    fn from(p: TraversalPathWire) -> Self {
        Self {
            steps: p
                .steps
                .into_iter()
                .map(|s| TraversalStepDto {
                    relation_id: uuid_string(&s.relation_id),
                    from: uuid_string(&s.from),
                    to: uuid_string(&s.to),
                    relation_type: s.relation_type,
                    depth: s.depth,
                })
                .collect(),
        }
    }
}

/// `POST /v1/entities/{id}/traverse` response.
#[derive(Debug, Serialize)]
pub struct TraverseResponseDto {
    /// The paths found.
    pub paths: Vec<TraversalPathDto>,
    /// Total paths (may exceed `paths.len()` when truncated).
    pub total_paths: u32,
    /// Whether a bound was hit before the graph was exhausted.
    pub truncated: bool,
}

impl TraverseResponseDto {
    /// Fold the streamed `RELATION_TRAVERSE` frames into one response. v1 emits a
    /// single final frame; this aggregates defensively — paths flatten across
    /// frames, `truncated` is true if any frame hit a bound, `total_paths` comes
    /// from the final frame's running total.
    pub fn from_frames(frames: Vec<RelationTraverseResponseFrame>) -> Self {
        let truncated = frames.iter().any(|f| f.truncated);
        let total_paths = frames.last().map_or(0, |f| f.total_paths);
        let paths = frames
            .into_iter()
            .flat_map(|f| f.paths.into_iter().map(TraversalPathDto::from))
            .collect();
        Self {
            paths,
            total_paths,
            truncated,
        }
    }
}

#[cfg(test)]
mod entity_tests {
    use super::*;
    use brain_db_sdk::wire::types::{TraversalStepWire, TraversalPathWire};

    #[test]
    fn create_body_rejects_zero_type_and_blank_name() {
        let bad_type = CreateEntityBody {
            entity_type_id: 0,
            canonical_name: "Ada".into(),
            aliases: vec![],
        };
        assert!(bad_type.to_request().is_err());
        let blank = CreateEntityBody {
            entity_type_id: 1,
            canonical_name: "   ".into(),
            aliases: vec![],
        };
        assert!(blank.to_request().is_err());
        let ok = CreateEntityBody {
            entity_type_id: 1,
            canonical_name: "  Ada Lovelace  ".into(),
            aliases: vec!["Ada".into()],
        };
        let req = ok.to_request().unwrap();
        assert_eq!(req.canonical_name, "Ada Lovelace"); // trimmed
        assert_eq!(req.entity_type_id, 1);
        assert!(req.attributes_blob.is_empty());
    }

    #[test]
    fn resolve_body_maps_type_hint_and_rejects_blank() {
        assert!(ResolveEntityBody {
            candidate_name: " ".into(),
            resolution_context: String::new(),
            type_hint: 0,
            allow_create: false,
        }
        .to_request()
        .is_err());
        let req = ResolveEntityBody {
            candidate_name: "Ada".into(),
            resolution_context: "the mathematician".into(),
            type_hint: 5,
            allow_create: true,
        }
        .to_request()
        .unwrap();
        assert_eq!(req.entity_type_hint, 5);
        assert!(req.allow_create);
        assert_eq!(req.resolution_context, "the mathematician");
    }

    #[test]
    fn resolve_response_surfaces_id_only_when_bound() {
        let mut id = [0u8; 16];
        id[15] = 9;
        let resolved = EntityResolveResponse {
            outcome: ResolutionOutcomeWire::Resolved,
            tier: 1,
            confidence: 1.0,
            resolved_entity: id,
            candidate_ids: vec![],
            audit_id: [0u8; 16],
        };
        let dto = ResolveEntityResponseDto::from(resolved);
        assert_eq!(dto.outcome, "resolved");
        assert_eq!(dto.entity_id.as_deref(), Some("00000000-0000-0000-0000-000000000009"));

        let ambiguous = EntityResolveResponse {
            outcome: ResolutionOutcomeWire::Ambiguous,
            tier: 0,
            confidence: 0.0,
            resolved_entity: id, // sentinel present on the wire, must be dropped
            candidate_ids: vec![[0x11u8; 16], [0x22u8; 16]],
            audit_id: [0u8; 16],
        };
        let dto = ResolveEntityResponseDto::from(ambiguous);
        assert_eq!(dto.outcome, "ambiguous");
        assert!(dto.entity_id.is_none());
        assert_eq!(dto.candidate_ids.len(), 2);
    }

    #[test]
    fn list_query_clamps_limit_and_defaults() {
        let q = ListEntitiesQuery {
            limit: Some(9999),
            ..Default::default()
        };
        let req = q.to_request();
        assert_eq!(req.limit, MAX_LIST_LIMIT);
        assert!(req.cursor.is_empty());
        let none = ListEntitiesQuery::default().to_request();
        assert_eq!(none.limit, DEFAULT_LIST_LIMIT);
    }

    #[test]
    fn traverse_body_parses_direction_and_clamps() {
        let id = "00000000-0000-0000-0000-000000000001";
        let req = TraverseBody {
            direction: "both".into(),
            relation_types: vec!["brain:works_at".into()],
            max_depth: Some(99),
            max_nodes: Some(0),
            include_superseded: true,
        }
        .to_request(id)
        .unwrap();
        assert_eq!(req.direction, 2);
        assert_eq!(req.max_depth, MAX_TRAVERSE_DEPTH);
        assert_eq!(req.max_nodes, 1); // clamped up from 0
        assert!(req.include_superseded);

        // default direction is outgoing (0)
        let req = TraverseBody::default().to_request(id).unwrap();
        assert_eq!(req.direction, 0);
        assert_eq!(req.max_depth, DEFAULT_TRAVERSE_DEPTH);

        assert!(TraverseBody {
            direction: "sideways".into(),
            ..Default::default()
        }
        .to_request(id)
        .is_err());
        assert!(TraverseBody::default().to_request("not-a-uuid").is_err());
    }

    #[test]
    fn traverse_response_folds_frames_and_uuidifies() {
        let step = TraversalStepWire {
            relation_id: [0x01u8; 16],
            from: [0x02u8; 16],
            to: [0x03u8; 16],
            relation_type: "brain:works_at".into(),
            depth: 1,
        };
        let frames = vec![
            RelationTraverseResponseFrame {
                paths: vec![TraversalPathWire { steps: vec![step.clone()] }],
                total_paths: 2,
                truncated: false,
                is_final: false,
            },
            RelationTraverseResponseFrame {
                paths: vec![TraversalPathWire { steps: vec![step] }],
                total_paths: 2,
                truncated: true,
                is_final: true,
            },
        ];
        let dto = TraverseResponseDto::from_frames(frames);
        assert_eq!(dto.paths.len(), 2);
        assert_eq!(dto.total_paths, 2);
        assert!(dto.truncated);
        assert!(dto.paths[0].steps[0].from.contains('-'));
    }
}
