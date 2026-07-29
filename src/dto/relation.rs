//! Relation-graph request/response DTOs — the public JSON contract for the
//! `/v1/relations/{id}` verb and `/v1/entities/{id}/relations` (list by
//! direction).
//!
//! Deliberately identical to the Arc cloud gateway's contract. Ids are
//! canonical UUID strings.

use brain_db_sdk::wire::types::{
    RelationGetRequest, RelationListFromRequest, RelationListToRequest, RelationView,
};
use serde::{Deserialize, Serialize};

use super::{parse_uuid, uuid_string};

/// Default page size for the list endpoint when the client omits `limit`.
const DEFAULT_LIST_LIMIT: u32 = 100;
/// Hard ceiling on the page size (mirrors Brain's `1..=1000`).
const MAX_LIST_LIMIT: u32 = 1000;
/// `RelationView.flags` bit 0 = `is_symmetric`.
const RELATION_FLAG_SYMMETRIC: u32 = 1;

/// Whether a relation read follows supersession by default (the dashboard wants
/// the current head unless told otherwise).
const fn default_follow_supersession() -> bool {
    true
}

/// A read-side relation view in the public JSON contract. Ids are UUID strings.
#[derive(Debug, Serialize)]
pub struct RelationDetailDto {
    /// The relation id (UUID).
    pub relation_id: String,
    /// The relation type (`namespace:name`).
    pub relation_type: String,
    /// The `from` endpoint entity (UUID).
    pub from_entity: String,
    /// The `to` endpoint entity (UUID).
    pub to_entity: String,
    /// Confidence in `[0,1]`.
    pub confidence: f32,
    /// Bi-temporal validity start (unix nanos).
    pub valid_from_unix_nanos: u64,
    /// Bi-temporal validity end (`0` = still valid).
    pub valid_to_unix_nanos: u64,
    /// Whether the relation type is symmetric.
    pub is_symmetric: bool,
    /// Whether the relation is tombstoned.
    pub tombstoned: bool,
}

impl From<RelationView> for RelationDetailDto {
    fn from(v: RelationView) -> Self {
        Self {
            relation_id: uuid_string(&v.relation_id),
            relation_type: v.relation_type,
            from_entity: uuid_string(&v.from_entity),
            to_entity: uuid_string(&v.to_entity),
            confidence: v.confidence,
            valid_from_unix_nanos: v.valid_from_unix_nanos,
            valid_to_unix_nanos: v.valid_to_unix_nanos,
            is_symmetric: v.flags & RELATION_FLAG_SYMMETRIC != 0,
            tombstoned: v.tombstoned,
        }
    }
}

/// `GET /v1/relations/{id}` query parameters.
#[derive(Debug, Deserialize)]
pub struct GetRelationQuery {
    /// Follow supersession to the current chain head (default `true`).
    #[serde(default = "default_follow_supersession")]
    pub follow_supersession: bool,
}

/// Build a wire `RELATION_GET` request from the path id and query.
pub fn get_request_from(id: &str, query: &GetRelationQuery) -> Result<RelationGetRequest, String> {
    Ok(RelationGetRequest {
        relation_id: parse_uuid(id)?,
        follow_supersession: query.follow_supersession,
        act_as: None,
    })
}

/// Which side of a relation the anchor entity sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelationSide {
    /// Anchor is the `from` endpoint (outgoing).
    From,
    /// Anchor is the `to` endpoint (incoming).
    To,
}

/// `GET /v1/entities/{id}/relations` query parameters. The anchor entity comes
/// from the path id.
#[derive(Debug, Default, Deserialize)]
pub struct ListRelationsQuery {
    /// `from`/outgoing (default) or `to`/incoming.
    #[serde(default)]
    pub direction: String,
    /// Relation-type filter; omitted → any type.
    #[serde(default, rename = "type")]
    pub relation_type: String,
    /// Include superseded relations (default `false`).
    #[serde(default)]
    pub include_superseded: bool,
    /// Include tombstoned relations (default `false`).
    #[serde(default)]
    pub include_tombstoned: bool,
    /// Page size; omitted → 100, clamped to `1..=1000`.
    pub limit: Option<u32>,
}

/// The resolved side + limit for a relation-list query.
fn parse_side(raw: &str) -> Result<RelationSide, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "from" | "outgoing" | "out" => Ok(RelationSide::From),
        "to" | "incoming" | "in" => Ok(RelationSide::To),
        _ => Err("direction must be one of: from (outgoing), to (incoming)".into()),
    }
}

impl ListRelationsQuery {
    /// Resolve the requested side (outgoing vs incoming).
    pub fn side(&self) -> Result<RelationSide, String> {
        parse_side(&self.direction)
    }

    fn limit(&self) -> u32 {
        self.limit
            .unwrap_or(DEFAULT_LIST_LIMIT)
            .clamp(1, MAX_LIST_LIMIT)
    }

    /// Build the outgoing (`RELATION_LIST_FROM`) wire request from the path id.
    pub fn to_from_request(&self, id: &str) -> Result<RelationListFromRequest, String> {
        Ok(RelationListFromRequest {
            from_entity: parse_uuid(id)?,
            relation_type_filter: self.relation_type.clone(),
            time_range_start_unix_nanos: 0,
            time_range_end_unix_nanos: 0,
            include_superseded: self.include_superseded,
            include_tombstoned: self.include_tombstoned,
            limit: self.limit(),
            cursor: Vec::new(),
            act_as: None,
        })
    }

    /// Build the incoming (`RELATION_LIST_TO`) wire request from the path id.
    pub fn to_to_request(&self, id: &str) -> Result<RelationListToRequest, String> {
        Ok(RelationListToRequest {
            to_entity: parse_uuid(id)?,
            relation_type_filter: self.relation_type.clone(),
            time_range_start_unix_nanos: 0,
            time_range_end_unix_nanos: 0,
            include_superseded: self.include_superseded,
            include_tombstoned: self.include_tombstoned,
            limit: self.limit(),
            cursor: Vec::new(),
            act_as: None,
        })
    }
}

/// `GET /v1/entities/{id}/relations` response: a page of relations plus its
/// count.
#[derive(Debug, Serialize)]
pub struct ListRelationsResponseDto {
    /// The relations in this page.
    pub relations: Vec<RelationDetailDto>,
    /// Number of relations returned.
    pub count: usize,
}

impl ListRelationsResponseDto {
    /// Assemble from the flattened relation-list views.
    pub fn from_views(views: Vec<RelationView>) -> Self {
        let relations: Vec<RelationDetailDto> =
            views.into_iter().map(RelationDetailDto::from).collect();
        let count = relations.len();
        Self { relations, count }
    }
}

#[cfg(test)]
mod relation_tests {
    use super::*;
    use brain_db_sdk::wire::types::EvidenceRefWire;

    fn sample_view(flags: u32) -> RelationView {
        RelationView {
            relation_id: [0x01u8; 16],
            chain_root: [0u8; 16],
            relation_type: "brain:works_at".into(),
            from_entity: [0x02u8; 16],
            to_entity: [0x03u8; 16],
            properties_blob: Vec::new(),
            evidence: EvidenceRefWire::Inline(Vec::new()),
            extractor_id: 0,
            extracted_at_unix_nanos: 0,
            confidence: 0.9,
            valid_from_unix_nanos: 10,
            valid_to_unix_nanos: 0,
            version: 1,
            superseded_by: [0u8; 16],
            supersedes: [0u8; 16],
            tombstoned: false,
            tombstoned_at_unix_nanos: 0,
            flags,
        }
    }

    #[test]
    fn detail_reads_symmetric_from_flags_and_uuidifies() {
        let dto = RelationDetailDto::from(sample_view(RELATION_FLAG_SYMMETRIC));
        assert!(dto.is_symmetric);
        assert!(dto.relation_id.contains('-'));
        assert!(dto.from_entity.contains('-') && dto.to_entity.contains('-'));

        let dto = RelationDetailDto::from(sample_view(0));
        assert!(!dto.is_symmetric);
    }

    #[test]
    fn direction_parses_both_sides_and_rejects_junk() {
        assert_eq!(
            ListRelationsQuery::default().side().unwrap(),
            RelationSide::From
        );
        assert_eq!(
            ListRelationsQuery {
                direction: "to".into(),
                ..Default::default()
            }
            .side()
            .unwrap(),
            RelationSide::To
        );
        assert_eq!(
            ListRelationsQuery {
                direction: "incoming".into(),
                ..Default::default()
            }
            .side()
            .unwrap(),
            RelationSide::To
        );
        assert!(
            ListRelationsQuery {
                direction: "sideways".into(),
                ..Default::default()
            }
            .side()
            .is_err()
        );
    }

    #[test]
    fn from_and_to_requests_target_correct_endpoint() {
        let id = "00000000-0000-0000-0000-000000000005";
        let q = ListRelationsQuery {
            direction: "from".into(),
            relation_type: "brain:knows".into(),
            include_superseded: true,
            include_tombstoned: false,
            limit: Some(9999),
        };
        let from = q.to_from_request(id).unwrap();
        assert_eq!(from.from_entity[15], 5);
        assert_eq!(from.relation_type_filter, "brain:knows");
        assert_eq!(from.limit, MAX_LIST_LIMIT);
        assert!(from.include_superseded);

        let to = q.to_to_request(id).unwrap();
        assert_eq!(to.to_entity[15], 5);
        assert!(q.to_from_request("bad").is_err());
    }

    #[test]
    fn get_request_defaults_follow_supersession() {
        let q = GetRelationQuery {
            follow_supersession: true,
        };
        let req = get_request_from("00000000-0000-0000-0000-000000000001", &q).unwrap();
        assert!(req.follow_supersession);
        assert_eq!(req.relation_id[15], 1);
        assert!(get_request_from("bad", &q).is_err());
    }
}
