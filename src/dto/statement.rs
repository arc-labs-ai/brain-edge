//! Statement-graph request/response DTOs — the public JSON contract for the
//! `/v1/statements` verbs (fetch by id, list by filters).
//!
//! Deliberately identical to the Arc cloud gateway's contract. Ids are
//! canonical UUID strings. A statement's object is a tagged union
//! (`kind: entity | value | memory | statement`); a scalar value is itself a
//! tagged union (`type: text | integer | float | bool | unix_nanos | blob`).

use brain_db_sdk::wire::types::{
    StatementGetRequest, StatementKindWire, StatementListRequest, StatementObjectWire,
    StatementValueWire, StatementView,
};
use serde::{Deserialize, Serialize};

use super::{parse_uuid, uuid_string};

/// Default page size for `GET /v1/statements` when the client omits `limit`.
const DEFAULT_LIST_LIMIT: u32 = 100;
/// Hard ceiling on the page size (mirrors Brain's `1..=1000`).
const MAX_LIST_LIMIT: u32 = 1000;

/// Whether a statement read follows supersession by default (the dashboard
/// wants the current head unless told otherwise).
const fn default_follow_supersession() -> bool {
    true
}

/// The snake_case label for a wire statement kind.
fn statement_kind_str(kind: StatementKindWire) -> String {
    match kind {
        StatementKindWire::Fact => "fact".to_owned(),
        StatementKindWire::Preference => "preference".to_owned(),
        StatementKindWire::Event => "event".to_owned(),
        StatementKindWire::Attribute => "attribute".to_owned(),
        StatementKindWire::Relation => "relation".to_owned(),
        StatementKindWire::Directive => "directive".to_owned(),
        StatementKindWire::Custom(_) => "custom".to_owned(),
    }
}

/// The 1-based LIST filter byte for a kind name (`0` = "no filter"). Mirrors the
/// storage byte + 1 (`fact = 1`, `preference = 2`, …).
fn kind_list_filter_byte(raw: &str) -> Result<u8, String> {
    Ok(match raw.trim().to_ascii_lowercase().as_str() {
        "fact" => 1,
        "preference" => 2,
        "event" => 3,
        "attribute" => 4,
        "relation" => 5,
        "directive" => 6,
        other => {
            return Err(format!(
                "kind must be one of: fact, preference, event, attribute, relation, directive (got `{other}`)"
            ))
        }
    })
}

/// A scalar object value in the public JSON contract.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StatementValueDto {
    /// Free text.
    Text { value: String },
    /// Signed integer.
    Integer { value: i64 },
    /// Floating-point number.
    Float { value: f64 },
    /// Boolean.
    Bool { value: bool },
    /// A unix-nanos timestamp value.
    UnixNanos { value: u64 },
    /// Opaque bytes (JSON array of byte values).
    Blob { value: Vec<u8> },
}

impl From<StatementValueWire> for StatementValueDto {
    fn from(v: StatementValueWire) -> Self {
        match v {
            StatementValueWire::Text(s) => Self::Text { value: s },
            StatementValueWire::Integer(i) => Self::Integer { value: i },
            StatementValueWire::Float(f) => Self::Float { value: f },
            StatementValueWire::Bool(b) => Self::Bool { value: b },
            StatementValueWire::UnixNanos(n) => Self::UnixNanos { value: n },
            StatementValueWire::Blob(b) => Self::Blob { value: b },
        }
    }
}

/// A statement object in the public JSON contract. Ids are UUID strings.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StatementObjectDto {
    /// Points at an entity.
    Entity { id: String },
    /// A literal scalar value.
    Value { value: StatementValueDto },
    /// Points at a memory.
    Memory { id: String },
    /// Points at another statement.
    Statement { id: String },
}

impl From<StatementObjectWire> for StatementObjectDto {
    fn from(o: StatementObjectWire) -> Self {
        match o {
            StatementObjectWire::EntityRef(id) => Self::Entity {
                id: uuid_string(&id),
            },
            StatementObjectWire::Value(v) => Self::Value {
                value: StatementValueDto::from(v),
            },
            StatementObjectWire::MemoryRef(id) => Self::Memory {
                id: uuid_string(&id),
            },
            StatementObjectWire::StatementRef(id) => Self::Statement {
                id: uuid_string(&id),
            },
        }
    }
}

/// A read-side statement view in the public JSON contract (dashboard subset).
#[derive(Debug, Serialize)]
pub struct StatementDetailDto {
    /// The statement id (UUID).
    pub statement_id: String,
    /// Its kind (`fact`, `preference`, `event`, …).
    pub kind: String,
    /// The subject entity (UUID).
    pub subject: String,
    /// The predicate.
    pub predicate: String,
    /// The object.
    pub object: StatementObjectDto,
    /// Confidence in `[0,1]`.
    pub confidence: f32,
    /// When the content happened (unix nanos; `0` = unset).
    pub event_at_unix_nanos: u64,
    /// Bi-temporal validity start (unix nanos).
    pub valid_from_unix_nanos: u64,
    /// Bi-temporal validity end (`0` = still valid).
    pub valid_to_unix_nanos: u64,
    /// Whether the statement is tombstoned.
    pub tombstoned: bool,
}

impl From<StatementView> for StatementDetailDto {
    fn from(v: StatementView) -> Self {
        Self {
            statement_id: uuid_string(&v.statement_id),
            kind: statement_kind_str(v.kind),
            subject: uuid_string(&v.subject),
            predicate: v.predicate,
            object: StatementObjectDto::from(v.object),
            confidence: v.confidence,
            event_at_unix_nanos: v.event_at_unix_nanos,
            valid_from_unix_nanos: v.valid_from_unix_nanos,
            valid_to_unix_nanos: v.valid_to_unix_nanos,
            tombstoned: v.tombstoned,
        }
    }
}

/// `GET /v1/statements/{id}` query parameters.
#[derive(Debug, Deserialize)]
pub struct GetStatementQuery {
    /// Follow supersession to the current chain head (default `true`).
    #[serde(default = "default_follow_supersession")]
    pub follow_supersession: bool,
}

/// Build a wire `STATEMENT_GET` request from the path id and query.
pub fn get_request_from(id: &str, query: &GetStatementQuery) -> Result<StatementGetRequest, String> {
    Ok(StatementGetRequest {
        statement_id: parse_uuid(id)?,
        follow_supersession: query.follow_supersession,
        act_as: None,
    })
}

/// `GET /v1/statements` query parameters. All optional.
#[derive(Debug, Deserialize)]
pub struct ListStatementsQuery {
    /// Subject entity id (UUID) filter; omitted → any subject.
    #[serde(default)]
    pub subject: Option<String>,
    /// Predicate filter; omitted → any predicate.
    #[serde(default)]
    pub predicate: String,
    /// Kind filter (`fact`, `preference`, …); omitted → any kind.
    #[serde(default)]
    pub kind: Option<String>,
    /// Minimum confidence (`0.0` = no floor).
    #[serde(default)]
    pub min_confidence: f32,
    /// Only current (non-superseded) statements (default `true`).
    #[serde(default = "default_follow_supersession")]
    pub only_current: bool,
    /// Include tombstoned statements (default `false`).
    #[serde(default)]
    pub include_tombstoned: bool,
    /// Page size; omitted → 100, clamped to `1..=1000`.
    pub limit: Option<u32>,
}

impl ListStatementsQuery {
    /// Build the wire request. `None` filters map to the wire's "no filter"
    /// sentinels (`[0;16]` subject, `0` kind byte); v1 has no cursor pagination.
    pub fn to_request(&self) -> Result<StatementListRequest, String> {
        let subject = match &self.subject {
            Some(s) if !s.trim().is_empty() => parse_uuid(s)?,
            _ => [0u8; 16],
        };
        let kind = match &self.kind {
            Some(k) if !k.trim().is_empty() => kind_list_filter_byte(k)?,
            _ => 0,
        };
        let limit = self
            .limit
            .unwrap_or(DEFAULT_LIST_LIMIT)
            .clamp(1, MAX_LIST_LIMIT);
        Ok(StatementListRequest {
            subject,
            predicate: self.predicate.clone(),
            kind,
            min_confidence: self.min_confidence,
            time_range_start_unix_nanos: 0,
            time_range_end_unix_nanos: 0,
            only_current: self.only_current,
            include_tombstoned: self.include_tombstoned,
            limit,
            cursor: Vec::new(),
            act_as: None,
        })
    }
}

/// `GET /v1/statements` response: a page of statements plus its count.
#[derive(Debug, Serialize)]
pub struct ListStatementsResponseDto {
    /// The statements in this page.
    pub statements: Vec<StatementDetailDto>,
    /// Number of statements returned.
    pub count: usize,
}

impl ListStatementsResponseDto {
    /// Assemble from the flattened `STATEMENT_LIST` views.
    pub fn from_views(views: Vec<StatementView>) -> Self {
        let statements: Vec<StatementDetailDto> =
            views.into_iter().map(StatementDetailDto::from).collect();
        let count = statements.len();
        Self { statements, count }
    }
}

#[cfg(test)]
mod statement_tests {
    use super::*;

    #[test]
    fn list_query_maps_filters_and_sentinels() {
        let none = ListStatementsQuery {
            subject: None,
            predicate: String::new(),
            kind: None,
            min_confidence: 0.0,
            only_current: true,
            include_tombstoned: false,
            limit: None,
        };
        let req = none.to_request().unwrap();
        assert_eq!(req.subject, [0u8; 16]);
        assert_eq!(req.kind, 0);
        assert_eq!(req.limit, DEFAULT_LIST_LIMIT);
        assert!(req.only_current);

        let with = ListStatementsQuery {
            subject: Some("00000000-0000-0000-0000-000000000007".into()),
            predicate: "brain:likes".into(),
            kind: Some("preference".into()),
            min_confidence: 0.5,
            only_current: false,
            include_tombstoned: true,
            limit: Some(9999),
        };
        let req = with.to_request().unwrap();
        assert_eq!(req.subject[15], 7);
        assert_eq!(req.kind, 2); // preference = storage 1 + 1
        assert_eq!(req.limit, MAX_LIST_LIMIT);
        assert!(!req.only_current);
        assert!(req.include_tombstoned);
    }

    #[test]
    fn list_query_rejects_bad_kind_and_subject() {
        assert!(ListStatementsQuery {
            subject: None,
            predicate: String::new(),
            kind: Some("nonsense".into()),
            min_confidence: 0.0,
            only_current: true,
            include_tombstoned: false,
            limit: None,
        }
        .to_request()
        .is_err());
        assert!(ListStatementsQuery {
            subject: Some("nope".into()),
            predicate: String::new(),
            kind: None,
            min_confidence: 0.0,
            only_current: true,
            include_tombstoned: false,
            limit: None,
        }
        .to_request()
        .is_err());
    }

    #[test]
    fn get_request_defaults_follow_supersession() {
        let q = GetStatementQuery {
            follow_supersession: true,
        };
        let req = get_request_from("00000000-0000-0000-0000-000000000001", &q).unwrap();
        assert!(req.follow_supersession);
        assert_eq!(req.statement_id[15], 1);
        assert!(get_request_from("bad", &q).is_err());
    }

    #[test]
    fn object_value_serializes_tagged() {
        let entity = StatementObjectDto::from(StatementObjectWire::EntityRef([0x01u8; 16]));
        let json = serde_json::to_value(&entity).unwrap();
        assert_eq!(json["kind"], "entity");
        assert!(json["id"].as_str().unwrap().contains('-'));

        let value = StatementObjectDto::from(StatementObjectWire::Value(
            StatementValueWire::Integer(42),
        ));
        let json = serde_json::to_value(&value).unwrap();
        assert_eq!(json["kind"], "value");
        assert_eq!(json["value"]["type"], "integer");
        assert_eq!(json["value"]["value"], 42);
    }
}
