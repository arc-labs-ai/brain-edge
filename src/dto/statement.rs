//! Statement-graph request/response DTOs — the public JSON contract for the
//! `/v1/statements` verbs (fetch by id, list by filters).
//!
//! Deliberately identical to the Arc cloud gateway's contract. Ids are
//! canonical UUID strings. A statement's object is a tagged union
//! (`kind: entity | value | memory | statement`); a scalar value is itself a
//! tagged union (`type: text | integer | float | bool | unix_nanos | blob`).

// The DTOs below mirror the HTTP contract one-for-one: the JSON field names are
// the API, and `tools/http_manifest.py` emits every one of them — with its type
// and serde attributes — into `contract/http-routes.json`, which the three SDK
// clients are checked against. A doc comment on each of ~312 fields would
// restate the field name; the ones that carry meaning beyond their name have
// one written below.
#![allow(missing_docs)]

use brain_db_sdk::wire::types::{
    EvidenceRefWire, StatementCreateRequest, StatementCreateResponse, StatementGetRequest, StatementHistoryRequest, StatementHistoryResponseFrame, StatementKindWire, StatementListRequest, StatementObjectWire, StatementRetractRequest, StatementRetractResponse, StatementSupersedeRequest, StatementSupersedeResponse, StatementTombstoneRequest, StatementTombstoneResponse, StatementValueWire, StatementView,
};
use serde::{Deserialize, Serialize};

use super::{hex_decode, hex_encode, mem_id_decimal, parse_memory_id, parse_uuid, uuid_string};

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
            ));
        }
    })
}

/// A scalar object value in the public JSON contract.
///
/// `Deserialize` as well, so a write sends back exactly what a read returned.
#[derive(Debug, Deserialize, Serialize)]
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

/// A statement object in the public JSON contract.
///
/// Entity and statement ids are UUID strings; a MEMORY id is a 128-bit
/// decimal string, the form every other memory id in this contract takes
/// (`dto/mod.rs`: "128-bit ids are decimal strings; 16-byte ids are
/// hyphenated UUID strings").
///
/// `Deserialize` as well as `Serialize` since `POST /v1/statements` takes the
/// same shape it returns — one representation of an object, not two that can
/// drift.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StatementObjectDto {
    /// Points at an entity.
    Entity { id: String },
    /// A literal scalar value.
    Value { value: StatementValueDto },
    /// Points at a memory. A 128-bit decimal id.
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
            // `mem_id_decimal`, NOT `uuid_string`. A memory id is 128-bit and
            // decimal everywhere else in this contract — `parse_memory_id` is
            // what every route that takes one uses — so rendering it here as a
            // UUID produced an id that looked fine and could not be fed back
            // into `GET /v1/memories/{id}/inspect`, `/v1/links`, or
            // `/v1/recall`'s memory refs. It was unusable rather than merely
            // inconsistent.
            StatementObjectWire::MemoryRef(id) => Self::Memory {
                id: mem_id_decimal(&id),
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

    // --- supersession + provenance ---------------------------------------
    //
    // A statement's history was invisible here: the read returned the head of
    // a chain with nothing to say it WAS a head, what it replaced, or what
    // replaced it. That was already thin — `follow_supersession` defaults to
    // true, so a caller asking for one id could silently be handed another —
    // and `POST /v1/statements/{id}/supersede` makes it untenable: a client
    // may now build a chain it cannot then read.
    /// Version within the supersession chain, 1-based.
    pub version: u32,
    /// The statement that replaced this one (UUID), or `null` if it is
    /// current.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    /// The statement this one replaced (UUID), or `null` if it is the first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    /// The first statement in the chain (UUID) — the stable identity of the
    /// claim across every revision of it.
    pub chain_root: String,
    /// Which extractor produced it (`0` = written directly).
    pub extractor_id: u32,
    /// When it was extracted (unix nanos).
    pub extracted_at_unix_nanos: u64,
    /// Schema version it was written against.
    pub schema_version: u32,
    /// When it was tombstoned (unix nanos; `0` = not tombstoned).
    pub tombstoned_at_unix_nanos: u64,
    /// The tombstone reason code (`0` = none).
    pub tombstone_reason: u8,
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
            version: v.version,
            superseded_by: optional_uuid(&v.superseded_by),
            supersedes: optional_uuid(&v.supersedes),
            chain_root: uuid_string(&v.chain_root),
            extractor_id: v.extractor_id,
            extracted_at_unix_nanos: v.extracted_at_unix_nanos,
            schema_version: v.schema_version,
            tombstoned_at_unix_nanos: v.tombstoned_at_unix_nanos,
            tombstone_reason: v.tombstone_reason,
        }
    }
}

/// An all-zero 16-byte id means "unset" on the wire, not "the nil UUID".
/// Rendering it as `00000000-...` would present absence as a real id.
fn optional_uuid(b: &[u8; 16]) -> Option<String> {
    (b != &[0u8; 16]).then(|| uuid_string(b))
}

/// `GET /v1/statements/{id}` query parameters.
#[derive(Debug, Deserialize)]
pub struct GetStatementQuery {
    /// Follow supersession to the current chain head (default `true`).
    #[serde(default = "default_follow_supersession")]
    pub follow_supersession: bool,
}

/// Build a wire `STATEMENT_GET` request from the path id and query.
pub fn get_request_from(
    id: &str,
    query: &GetStatementQuery,
) -> Result<StatementGetRequest, String> {
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
        assert!(
            ListStatementsQuery {
                subject: None,
                predicate: String::new(),
                kind: Some("nonsense".into()),
                min_confidence: 0.0,
                only_current: true,
                include_tombstoned: false,
                limit: None,
            }
            .to_request()
            .is_err()
        );
        assert!(
            ListStatementsQuery {
                subject: Some("nope".into()),
                predicate: String::new(),
                kind: None,
                min_confidence: 0.0,
                only_current: true,
                include_tombstoned: false,
                limit: None,
            }
            .to_request()
            .is_err()
        );
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

        let value =
            StatementObjectDto::from(StatementObjectWire::Value(StatementValueWire::Integer(42)));
        let json = serde_json::to_value(&value).unwrap();
        assert_eq!(json["kind"], "value");
        assert_eq!(json["value"]["type"], "integer");
        assert_eq!(json["value"]["value"], 42);
    }
}

impl From<StatementValueDto> for StatementValueWire {
    fn from(v: StatementValueDto) -> Self {
        match v {
            StatementValueDto::Text { value } => Self::Text(value),
            StatementValueDto::Integer { value } => Self::Integer(value),
            StatementValueDto::Float { value } => Self::Float(value),
            StatementValueDto::Bool { value } => Self::Bool(value),
            StatementValueDto::UnixNanos { value } => Self::UnixNanos(value),
            StatementValueDto::Blob { value } => Self::Blob(value),
        }
    }
}

impl StatementObjectDto {
    /// The wire form of this object.
    ///
    /// # Errors
    /// An id that is not a UUID (entity, statement) or not a 128-bit decimal
    /// (memory).
    pub fn to_wire(self) -> Result<StatementObjectWire, String> {
        Ok(match self {
            Self::Entity { id } => StatementObjectWire::EntityRef(parse_uuid(&id)?),
            Self::Value { value } => StatementObjectWire::Value(value.into()),
            // Decimal in, matching what the read hands out.
            Self::Memory { id } => {
                StatementObjectWire::MemoryRef(parse_memory_id(&id)?.to_be_bytes())
            }
            Self::Statement { id } => StatementObjectWire::StatementRef(parse_uuid(&id)?),
        })
    }
}

// --- Authoring --------------------------------------------------------------
//
// Writing a claim directly, revising it, and retiring it. Reads were already
// here; without these a caller could only ever see what the extractor decided,
// never correct it.

/// `POST /v1/statements` body — assert a claim.
///
/// The temporal fields are the ones worth reading twice. `event_at` is when
/// the thing HAPPENED; `valid_from`/`valid_to` are when the claim is held to
/// be true. A meeting that happened on Monday and is believed from Tuesday
/// has different values for the two, and collapsing them is how bi-temporal
/// data stops being bi-temporal.
#[derive(Debug, Deserialize)]
pub struct CreateStatementBody {
    /// `fact` | `preference` | `event` | `attribute` | `relation` | `directive`.
    pub kind: String,
    /// The subject entity (UUID).
    pub subject: String,
    /// The predicate.
    pub predicate: String,
    /// The object — the same tagged shape a read returns.
    pub object: StatementObjectDto,
    /// Confidence in `[0,1]`. Defaults to certain.
    #[serde(default = "default_confidence")]
    pub confidence: f32,
    /// When the content happened (unix nanos). `0` = unset.
    #[serde(default)]
    pub event_at_unix_nanos: u64,
    /// Bi-temporal validity start (unix nanos). `0` = now, server-side.
    #[serde(default)]
    pub valid_from_unix_nanos: u64,
    /// Bi-temporal validity end (unix nanos). `0` = still valid.
    #[serde(default)]
    pub valid_to_unix_nanos: u64,
    /// Which extractor to attribute it to. `0` = written directly, which is
    /// what a statement asserted over HTTP is.
    #[serde(default)]
    pub extractor_id: u32,
    /// Schema version to write against. `0` = current.
    #[serde(default)]
    pub schema_version: u32,
    /// Session to attribute it to. `0` = the default session.
    #[serde(default)]
    pub session_id: u64,
    /// Memory ids this claim rests on (128-bit decimal strings).
    #[serde(default)]
    pub evidence: Vec<String>,
}

const fn default_confidence() -> f32 {
    1.0
}

/// The 1-based storage byte for a statement kind.
fn kind_byte(raw: &str) -> Result<StatementKindWire, String> {
    Ok(match raw.trim().to_ascii_lowercase().as_str() {
        "fact" => StatementKindWire::Fact,
        "preference" => StatementKindWire::Preference,
        "event" => StatementKindWire::Event,
        "attribute" => StatementKindWire::Attribute,
        "relation" => StatementKindWire::Relation,
        "directive" => StatementKindWire::Directive,
        other => return Err(format!("`{other}` is not a statement kind")),
    })
}

impl CreateStatementBody {
    /// Build the wire request.
    ///
    /// # Errors
    /// An unknown kind, an id that will not parse, a confidence outside
    /// `[0,1]`, a blank predicate, or a validity window that ends before it
    /// starts.
    pub fn to_request(self) -> Result<StatementCreateRequest, String> {
        let predicate = self.predicate.trim().to_owned();
        if predicate.is_empty() {
            return Err("predicate must not be blank".into());
        }
        if !(0.0..=1.0).contains(&self.confidence) {
            return Err("confidence must be between 0.0 and 1.0".into());
        }
        // `0` means "still valid", so only a real end before a real start is
        // backwards.
        if self.valid_to_unix_nanos != 0 && self.valid_to_unix_nanos < self.valid_from_unix_nanos {
            return Err("valid_to_unix_nanos must not precede valid_from_unix_nanos".into());
        }
        let evidence = self
            .evidence
            .iter()
            .map(|id| parse_memory_id(id).map(u128::to_be_bytes))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(StatementCreateRequest {
            kind: kind_byte(&self.kind)?,
            subject: parse_uuid(&self.subject)?,
            predicate,
            object: self.object.to_wire()?,
            confidence: self.confidence,
            evidence: EvidenceRefWire::Inline(evidence),
            extractor_id: self.extractor_id,
            valid_from_unix_nanos: self.valid_from_unix_nanos,
            valid_to_unix_nanos: self.valid_to_unix_nanos,
            event_at_unix_nanos: self.event_at_unix_nanos,
            schema_version: self.schema_version,
            session_id: self.session_id,
            request_id: brain_db_sdk::new_id(),
            act_as: None,
        })
    }
}

/// `POST /v1/statements/{id}/supersede` body — replace a claim with a revised
/// one, keeping both and the link between them.
///
/// The replacement is a whole statement rather than a patch, because a
/// revision that changed only the confidence and one that changed the object
/// are the same operation to the chain, and expressing one as a diff would
/// invite the other to be too.
#[derive(Debug, Deserialize)]
pub struct SupersedeStatementBody {
    /// The replacement claim.
    #[serde(flatten)]
    pub statement: CreateStatementBody,
}

impl SupersedeStatementBody {
    /// Build the wire request replacing `old_id`.
    ///
    /// # Errors
    /// The old id is not a UUID, or the replacement claim is invalid.
    pub fn to_request(self, old_id: &str) -> Result<StatementSupersedeRequest, String> {
        Ok(StatementSupersedeRequest {
            old_statement_id: parse_uuid(old_id)?,
            new_statement: self.statement.to_request()?,
            request_id: brain_db_sdk::new_id(),
        })
    }
}

/// `DELETE /v1/statements/{id}` and `POST /v1/statements/{id}/retract` query.
#[derive(Debug, Default, Deserialize)]
pub struct StatementRetireQuery {
    /// A reason code kept on the record (`0` = unspecified).
    #[serde(default)]
    pub reason: u8,
    /// A human note kept alongside it.
    #[serde(default)]
    pub message: Option<String>,
}

impl StatementRetireQuery {
    /// Build the wire tombstone request for `id`.
    ///
    /// # Errors
    /// The id is not a UUID.
    pub fn to_tombstone(&self, id: &str) -> Result<StatementTombstoneRequest, String> {
        Ok(StatementTombstoneRequest {
            statement_id: parse_uuid(id)?,
            reason: self.reason,
            reason_message: self.message.clone().unwrap_or_default(),
            request_id: brain_db_sdk::new_id(),
        })
    }

    /// Build the wire retract request for `id`.
    ///
    /// # Errors
    /// The id is not a UUID.
    pub fn to_retract(&self, id: &str) -> Result<StatementRetractRequest, String> {
        Ok(StatementRetractRequest {
            statement_id: parse_uuid(id)?,
            reason: self.reason,
            reason_message: self.message.clone().unwrap_or_default(),
            request_id: brain_db_sdk::new_id(),
        })
    }
}

/// `POST /v1/statements` response.
#[derive(Debug, Serialize)]
pub struct CreateStatementResponseDto {
    /// The new statement's id (UUID).
    pub statement_id: String,
    /// A statement this one automatically superseded (UUID), when Brain
    /// recognised it as a revision of an existing claim — or `null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_superseded: Option<String>,
    /// The chain this claim belongs to (UUID) — its stable identity across
    /// revisions.
    pub chain_root: String,
}

impl From<StatementCreateResponse> for CreateStatementResponseDto {
    fn from(r: StatementCreateResponse) -> Self {
        Self {
            statement_id: uuid_string(&r.statement_id),
            auto_superseded: optional_uuid(&r.auto_superseded),
            chain_root: uuid_string(&r.chain_root),
        }
    }
}

/// `POST /v1/statements/{id}/supersede` response.
#[derive(Debug, Serialize)]
pub struct SupersedeStatementResponseDto {
    /// The replacement's id (UUID).
    pub new_statement_id: String,
    /// The chain both versions belong to (UUID).
    pub chain_root: String,
    /// The replacement's version within that chain.
    pub version: u32,
}

impl From<StatementSupersedeResponse> for SupersedeStatementResponseDto {
    fn from(r: StatementSupersedeResponse) -> Self {
        Self {
            new_statement_id: uuid_string(&r.new_statement_id),
            chain_root: uuid_string(&r.chain_root),
            version: r.version,
        }
    }
}

/// `DELETE /v1/statements/{id}` response.
#[derive(Debug, Serialize)]
pub struct TombstoneStatementResponseDto {
    /// When the tombstone took effect (unix nanos).
    pub tombstoned_at_unix_nanos: u64,
}

impl From<StatementTombstoneResponse> for TombstoneStatementResponseDto {
    fn from(r: StatementTombstoneResponse) -> Self {
        Self {
            tombstoned_at_unix_nanos: r.tombstoned_at_unix_nanos,
        }
    }
}

/// `POST /v1/statements/{id}/retract` response.
///
/// A retraction is stronger than a tombstone: the claim is not merely retired,
/// it is withdrawn, and `will_zero_at_unix_nanos` is when the content itself
/// is scheduled to be zeroed.
#[derive(Debug, Serialize)]
pub struct RetractStatementResponseDto {
    /// When the retraction took effect (unix nanos).
    pub retracted_at_unix_nanos: u64,
    /// When the content is scheduled to be zeroed (unix nanos).
    pub will_zero_at_unix_nanos: u64,
}

impl From<StatementRetractResponse> for RetractStatementResponseDto {
    fn from(r: StatementRetractResponse) -> Self {
        Self {
            retracted_at_unix_nanos: r.retracted_at_unix_nanos,
            will_zero_at_unix_nanos: r.will_zero_at_unix_nanos,
        }
    }
}

/// `GET /v1/statements/{id}/history` query.
#[derive(Debug, Default, Deserialize)]
pub struct StatementHistoryQuery {
    /// Include tombstoned versions. Off by default: the usual question is
    /// "how did this claim change", not "what was retired".
    #[serde(default)]
    pub include_tombstoned: bool,
    /// Page size.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Opaque keyset cursor from the previous page, lowercase hex.
    #[serde(default)]
    pub cursor: Option<String>,
}

impl StatementHistoryQuery {
    /// Build the wire request for the chain anchored at `id`.
    ///
    /// # Errors
    /// The id is not a UUID, or the cursor is not hex.
    pub fn to_request(&self, id: &str) -> Result<StatementHistoryRequest, String> {
        Ok(StatementHistoryRequest {
            anchor_id: parse_uuid(id)?,
            include_tombstoned: self.include_tombstoned,
            limit: self
                .limit
                .filter(|n| *n > 0)
                .map_or(DEFAULT_LIST_LIMIT, |n| n.min(MAX_LIST_LIMIT)),
            cursor: match self.cursor.as_deref() {
                Some(c) => hex_decode(c)?,
                None => Vec::new(),
            },
        })
    }
}

/// `GET /v1/statements/{id}/history` response — every version of one claim.
#[derive(Debug, Serialize)]
pub struct StatementHistoryResponseDto {
    /// The versions, oldest first.
    pub items: Vec<StatementDetailDto>,
    /// The chain's stable identity (UUID).
    pub chain_root: String,
    /// How many versions exist in total, across every page.
    pub total_versions: u32,
    /// Cursor for the next page, or `null` on the last one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

impl StatementHistoryResponseDto {
    /// Fold the streamed frames into one page.
    #[must_use]
    pub fn from_frames(frames: Vec<StatementHistoryResponseFrame>) -> Self {
        let chain_root = frames
            .first()
            .map(|f| uuid_string(&f.chain_root))
            .unwrap_or_default();
        let total_versions = frames.first().map_or(0, |f| f.total_versions);
        // The cursor that matters is the LAST frame's: earlier frames carry
        // intermediate positions, and resuming from one of those would replay
        // rows this page already returned.
        let next_cursor = frames
            .last()
            .filter(|f| !f.next_cursor.is_empty())
            .map(|f| hex_encode(&f.next_cursor));
        Self {
            items: frames
                .into_iter()
                .flat_map(|f| f.items)
                .map(StatementDetailDto::from)
                .collect(),
            chain_root,
            total_versions,
            next_cursor,
        }
    }
}
