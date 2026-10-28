//! Change-feed request/response DTOs — the public JSON contract for
//! `GET /v1/subscribe` (SUBSCRIBE, `0x0030`).
//!
//! The wire op is server-push: one request, then `SUBSCRIBE_EVENT` frames
//! until the stream is torn down. That is exactly Server-Sent Events, so the
//! route is SSE rather than a JSON response — see the handler for why this is
//! not a WebSocket and how teardown works.
//!
//! Every event carries its `lsn`, which is also the SSE `id:` field. A client
//! that reconnects with `Last-Event-ID` resumes from the event after that one,
//! so a dropped connection costs nothing. `?from_lsn=` is the explicit form of
//! the same thing.

// Field names are the API; see `dto/space.rs` for the manifest note.
#![allow(missing_docs)]

use brain_db_sdk::wire::types::{
    EdgeEventPayload, EventType, GraphEventPayload, MemoryKindWire, StageKind, StageOutcome,
    StagePayload, SubscribeRequest, SubscriptionEvent, SubscriptionFilter,
};
use serde::{Deserialize, Serialize};

use super::{parse_memory_id, uuid_string};

/// Default inflight budget when the caller does not say. Bounds how far ahead
/// Brain may run of a slow reader.
const DEFAULT_MAX_INFLIGHT: u32 = 256;
/// Ceiling on that budget.
const MAX_MAX_INFLIGHT: u32 = 4096;

/// `GET /v1/subscribe` query.
///
/// Filters are comma-separated lists rather than repeated params, so a feed
/// subscription is one readable URL that survives being pasted into a shell.
#[derive(Debug, Default, Deserialize)]
pub struct SubscribeQuery {
    /// Only events from these sessions. Comma-separated `u64`s.
    #[serde(default)]
    pub sessions: Option<String>,
    /// Only events for these memory kinds. Comma-separated kind bytes.
    #[serde(default)]
    pub kinds: Option<String>,
    /// Only events for these memory ids. Comma-separated decimal ids.
    #[serde(default)]
    pub memories: Option<String>,
    /// Replay history before following the live feed.
    #[serde(default)]
    pub include_history: bool,
    /// Resume after this LSN. `Last-Event-ID` sets the same thing on an SSE
    /// reconnect, and takes precedence when both are present — the browser
    /// sends it automatically and it is the more specific statement.
    #[serde(default)]
    pub from_lsn: Option<u64>,
    /// How far ahead of a slow reader Brain may run.
    #[serde(default)]
    pub max_inflight: Option<u32>,
}

/// Parse a comma-separated list, skipping empty items.
fn csv<T, F>(raw: Option<&String>, parse: F) -> Result<Option<Vec<T>>, String>
where
    F: Fn(&str) -> Result<T, String>,
{
    let Some(raw) = raw else { return Ok(None) };
    let items = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(parse)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(if items.is_empty() { None } else { Some(items) })
}

impl SubscribeQuery {
    /// Build the wire request. `resume_from` is the `Last-Event-ID` value when
    /// the client sent one.
    ///
    /// # Errors
    /// A filter list contains an item that is not a number of the right kind.
    pub fn to_request(
        &self,
        space_id: [u8; 16],
        resume_from: Option<u64>,
    ) -> Result<SubscribeRequest, String> {
        let sessions = csv(self.sessions.as_ref(), |s| {
            s.parse::<u64>()
                .map_err(|_| format!("`{s}` is not a session id"))
        })?;
        // Accepts the kind byte or its name, since the rest of the contract
        // carries `kind` as a byte while a hand-written URL reads better with
        // the word.
        let kinds = csv(self.kinds.as_ref(), |s| {
            match s.to_ascii_lowercase().as_str() {
                "0" | "episodic" => Ok(MemoryKindWire::Episodic),
                "1" | "semantic" => Ok(MemoryKindWire::Semantic),
                "2" | "consolidated" => Ok(MemoryKindWire::Consolidated),
                _ => Err(format!("`{s}` is not a memory kind")),
            }
        })?;
        let memory_ids = csv(self.memories.as_ref(), parse_memory_id)?;

        Ok(SubscribeRequest {
            filter: SubscriptionFilter {
                session_filter: sessions,
                kinds,
                similar_to: None,
                // The caller's own space, NAMED — not `None`.
                //
                // `None` or an empty list means "every space on the shard" to
                // Brain, which is a cross-tenant feed; Brain refuses it with
                // `filter.spaces must name only the effective space`. Sending
                // `None` and trusting `act_as` to narrow it, as this did, was
                // backwards: `act_as` says who the caller IS, and this filter
                // says what they may watch. The space is taken from the
                // connection Brain resolved, never from the query.
                spaces: Some(vec![space_id]),
                memory_ids,
            },
            include_history: self.include_history,
            // `Last-Event-ID` wins: the browser resends it on an automatic
            // reconnect, where the original `?from_lsn=` is still in the URL
            // and would rewind the feed to where it started.
            from_lsn: resume_from.or(self.from_lsn),
            max_inflight: self
                .max_inflight
                .filter(|n| *n > 0)
                .map_or(DEFAULT_MAX_INFLIGHT, |n| n.min(MAX_MAX_INFLIGHT)),
            act_as: None,
        })
    }
}

/// The snake_case label for a wire event type.
fn event_type_str(t: EventType) -> &'static str {
    match t {
        EventType::Encoded => "encoded",
        EventType::Forgotten => "forgotten",
        EventType::Reclaimed => "reclaimed",
        EventType::KindChanged => "kind_changed",
        EventType::EntityCreated => "entity_created",
        EventType::EntityUpdated => "entity_updated",
        EventType::EntityRenamed => "entity_renamed",
        EventType::EntityMerged => "entity_merged",
        EventType::EntityUnmerged => "entity_unmerged",
        EventType::EntityTombstoned => "entity_tombstoned",
        EventType::StatementCreated => "statement_created",
        EventType::StatementSuperseded => "statement_superseded",
        EventType::StatementTombstoned => "statement_tombstoned",
        EventType::RelationCreated => "relation_created",
        EventType::RelationSuperseded => "relation_superseded",
        EventType::RelationTombstoned => "relation_tombstoned",
        EventType::StageCompleted => "stage_completed",
        EventType::SchemaUpdated => "schema_updated",
        EventType::EdgeAdded => "edge_added",
        EventType::EdgeRemoved => "edge_removed",
        EventType::EdgeSuperseded => "edge_superseded",
    }
}

fn stage_kind_str(k: StageKind) -> &'static str {
    match k {
        StageKind::AutoEdge => "auto_edge",
        StageKind::TemporalEdge => "temporal_edge",
        StageKind::Extractor => "extractor",
        StageKind::Hype => "hype",
    }
}

fn stage_outcome_str(o: StageOutcome) -> &'static str {
    match o {
        StageOutcome::Ok => "ok",
        StageOutcome::Empty => "empty",
        StageOutcome::Failed => "failed",
    }
}

/// The typed-graph detail on an event, tagged by `type`.
///
/// Mapped variant by variant rather than passed through as an opaque blob:
/// these carry the ids a subscriber acts on, and a change feed whose graph
/// events are untyped JSON is one a client has to guess its way through.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GraphEventDto {
    EntityCreated {
        entity_id: String,
        entity_type_id: u32,
        canonical_name: String,
    },
    EntityUpdated {
        entity_id: String,
        entity_type_id: u32,
        canonical_name: String,
        embedding_version_changed: bool,
    },
    EntityRenamed {
        entity_id: String,
        old_canonical_name: String,
        new_canonical_name: String,
        old_moved_to_alias: bool,
    },
    EntityMerged {
        survivor: String,
        merged: String,
        audit_id: String,
        confidence: f32,
        statements_rerouted: u32,
        relations_rerouted: u32,
    },
    EntityUnmerged {
        restored_entity_id: String,
        from_survivor: String,
        audit_id: String,
    },
    EntityTombstoned {
        entity_id: String,
        reason: String,
    },
    StatementCreated {
        statement_id: String,
        kind: u8,
        subject: String,
        predicate: String,
        confidence: f32,
    },
    StatementSuperseded {
        old_statement_id: String,
        new_statement_id: String,
        chain_root: String,
    },
    StatementTombstoned {
        statement_id: String,
        reason: String,
    },
    RelationCreated {
        relation_id: String,
        relation_type: String,
        from: String,
        to: String,
    },
    RelationSuperseded {
        old_relation_id: String,
        new_relation_id: String,
    },
    RelationTombstoned {
        relation_id: String,
        reason: String,
    },
    SchemaUpdated {
        namespace: String,
        from_version: u32,
        to_version: u32,
        backward_compatible: bool,
    },
}

impl From<GraphEventPayload> for GraphEventDto {
    fn from(p: GraphEventPayload) -> Self {
        match p {
            GraphEventPayload::EntityCreated(e) => Self::EntityCreated {
                entity_id: uuid_string(&e.entity_id),
                entity_type_id: e.entity_type_id,
                canonical_name: e.canonical_name,
            },
            GraphEventPayload::EntityUpdated(e) => Self::EntityUpdated {
                entity_id: uuid_string(&e.entity_id),
                entity_type_id: e.entity_type_id,
                canonical_name: e.canonical_name,
                embedding_version_changed: e.embedding_version_changed,
            },
            GraphEventPayload::EntityRenamed(e) => Self::EntityRenamed {
                entity_id: uuid_string(&e.entity_id),
                old_canonical_name: e.old_canonical_name,
                new_canonical_name: e.new_canonical_name,
                old_moved_to_alias: e.old_moved_to_alias,
            },
            GraphEventPayload::EntityMerged(e) => Self::EntityMerged {
                survivor: uuid_string(&e.survivor),
                merged: uuid_string(&e.merged),
                audit_id: uuid_string(&e.audit_id),
                confidence: e.confidence,
                statements_rerouted: e.statements_rerouted,
                relations_rerouted: e.relations_rerouted,
            },
            GraphEventPayload::EntityUnmerged(e) => Self::EntityUnmerged {
                restored_entity_id: uuid_string(&e.restored_entity_id),
                from_survivor: uuid_string(&e.from_survivor),
                audit_id: uuid_string(&e.audit_id),
            },
            GraphEventPayload::EntityTombstoned(e) => Self::EntityTombstoned {
                entity_id: uuid_string(&e.entity_id),
                reason: e.reason,
            },
            GraphEventPayload::StatementCreated(e) => Self::StatementCreated {
                statement_id: uuid_string(&e.statement_id),
                kind: e.kind,
                subject: uuid_string(&e.subject),
                predicate: e.predicate,
                confidence: e.confidence,
            },
            GraphEventPayload::StatementSuperseded(e) => Self::StatementSuperseded {
                old_statement_id: uuid_string(&e.old_statement_id),
                new_statement_id: uuid_string(&e.new_statement_id),
                chain_root: uuid_string(&e.chain_root),
            },
            GraphEventPayload::StatementTombstoned(e) => Self::StatementTombstoned {
                statement_id: uuid_string(&e.statement_id),
                reason: e.reason,
            },
            GraphEventPayload::RelationCreated(e) => Self::RelationCreated {
                relation_id: uuid_string(&e.relation_id),
                relation_type: e.relation_type,
                from: uuid_string(&e.from),
                to: uuid_string(&e.to),
            },
            GraphEventPayload::RelationSuperseded(e) => Self::RelationSuperseded {
                old_relation_id: uuid_string(&e.old_relation_id),
                new_relation_id: uuid_string(&e.new_relation_id),
            },
            GraphEventPayload::RelationTombstoned(e) => Self::RelationTombstoned {
                relation_id: uuid_string(&e.relation_id),
                reason: e.reason,
            },
            GraphEventPayload::SchemaUpdated(e) => Self::SchemaUpdated {
                namespace: e.namespace,
                from_version: e.from_version,
                to_version: e.to_version,
                backward_compatible: e.backward_compatible,
            },
        }
    }
}

/// A memory-graph edge event.
#[derive(Debug, Serialize)]
pub struct EdgeEventDto {
    pub from_kind: u8,
    pub from_id: String,
    pub to_kind: u8,
    pub to_id: String,
    pub edge_kind_tag: u8,
    pub edge_kind_byte: u8,
    pub relation_type_id: Option<u32>,
    pub weight: f32,
    pub relation_id: Option<String>,
    pub superseded_relation_id: Option<String>,
    pub origin: u8,
}

impl From<EdgeEventPayload> for EdgeEventDto {
    fn from(e: EdgeEventPayload) -> Self {
        Self {
            from_kind: e.from_kind,
            from_id: uuid_string(&e.from_id),
            to_kind: e.to_kind,
            to_id: uuid_string(&e.to_id),
            edge_kind_tag: e.edge_kind_tag,
            edge_kind_byte: e.edge_kind_byte,
            relation_type_id: e.relation_type_id,
            weight: e.weight,
            relation_id: e.relation_id.as_ref().map(uuid_string),
            superseded_relation_id: e.superseded_relation_id.as_ref().map(uuid_string),
            origin: e.origin,
        }
    }
}

/// What a write-pipeline stage produced, tagged by `type`.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StageEventDto {
    AutoEdge {
        edges_written: u32,
    },
    TemporalEdge {
        edges_written: u32,
    },
    Extractor {
        entity_count: u32,
        statement_count: u32,
        relation_count: u32,
        error_message: String,
    },
    Hype {
        questions_written: u32,
        cost_micro_usd: u64,
        /// Why the stage failed; empty when it did not. Relayed for the
        /// same reason the extractor's is: without it a failed stage
        /// reaches a subscriber as two zeroed counters and no cause.
        error_message: String,
    },
}

impl From<StagePayload> for StageEventDto {
    fn from(p: StagePayload) -> Self {
        match p {
            StagePayload::AutoEdge(s) => Self::AutoEdge {
                edges_written: s.edges_written,
            },
            StagePayload::TemporalEdge(s) => Self::TemporalEdge {
                edges_written: s.edges_written,
            },
            StagePayload::Extractor(s) => Self::Extractor {
                entity_count: s.entity_count,
                statement_count: s.statement_count,
                relation_count: s.relation_count,
                error_message: s.error_message,
            },
            StagePayload::Hype(s) => Self::Hype {
                questions_written: s.questions_written,
                cost_micro_usd: s.cost_micro_usd,
                error_message: s.error_message,
            },
        }
    }
}

/// One change-feed event — the body of an SSE `data:` frame.
#[derive(Debug, Serialize)]
pub struct SubscriptionEventDto {
    pub event_type: &'static str,
    /// Decimal string: a memory id exceeds the JS safe-integer range.
    pub memory_id: String,
    pub session_id: u64,
    pub text: String,
    pub kind: u8,
    pub salience: f32,
    pub timestamp_unix_nanos: u64,
    /// The feed position. Also the SSE `id:`, so `Last-Event-ID` resumes here.
    pub lsn: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph: Option<GraphEventDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge: Option<EdgeEventDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage_kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage_outcome: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<StageEventDto>,
}

impl From<SubscriptionEvent> for SubscriptionEventDto {
    fn from(e: SubscriptionEvent) -> Self {
        Self {
            event_type: event_type_str(e.event_type),
            memory_id: e.memory_id.to_string(),
            session_id: e.session_id,
            text: e.text,
            kind: e.kind as u8,
            salience: e.salience,
            timestamp_unix_nanos: e.timestamp_unix_nanos,
            lsn: e.lsn,
            graph: e.graph_payload.map(GraphEventDto::from),
            edge: e.edge_payload.map(EdgeEventDto::from),
            stage_kind: e.stage_kind.map(stage_kind_str),
            stage_outcome: e.stage_outcome.map(stage_outcome_str),
            stage: e.stage_payload.map(StageEventDto::from),
        }
    }
}

/// The terminal event, sent once when the feed closes.
///
/// A plain SSE close tells a client the connection ended but not where the
/// feed got to; `final_lsn` is what `UNSUBSCRIBE` reports, and handing it over
/// is what lets the next connection resume exactly rather than guess.
#[derive(Debug, Serialize)]
pub struct SubscriptionClosedDto {
    pub final_lsn: u64,
    /// Why the feed ended: `unsubscribed`, `ended` (Brain closed it) or
    /// `error`.
    pub reason: &'static str,
}

#[cfg(test)]
mod subscribe_tests {
    use super::*;
    use brain_db_sdk::wire::types::{EntityCreatedEvent, StageHypePayload};

    fn query() -> SubscribeQuery {
        SubscribeQuery::default()
    }

    #[test]
    fn filters_parse_as_comma_separated_lists() {
        let q = SubscribeQuery {
            sessions: Some("1, 2 ,3".into()),
            kinds: Some("semantic,2".into()),
            memories: Some("7".into()),
            ..query()
        };
        let req = q.to_request([7u8; 16], None).unwrap();
        assert_eq!(req.filter.session_filter.unwrap(), vec![1, 2, 3]);
        assert_eq!(
            req.filter.kinds.unwrap(),
            vec![MemoryKindWire::Semantic, MemoryKindWire::Consolidated],
            "a kind is accepted by name or by byte"
        );
        assert_eq!(req.filter.memory_ids.unwrap().len(), 1);
    }

    #[test]
    fn an_empty_filter_list_is_no_filter_rather_than_an_empty_one() {
        // `?sessions=` must not mean "no sessions match", which would produce
        // a feed that is silently always empty.
        let req = SubscribeQuery {
            sessions: Some(",, ".into()),
            ..query()
        }
        .to_request([7u8; 16], None)
        .unwrap();
        assert!(req.filter.session_filter.is_none());
    }

    #[test]
    fn the_feed_names_the_caller_s_own_space_and_only_that() {
        let req = query().to_request([7u8; 16], None).unwrap();
        // `None` means "every space on the shard" to Brain — a cross-tenant
        // feed, which it refuses with `filter.spaces must name only the
        // effective space`. Caught live; this pins the fix.
        assert_eq!(req.filter.spaces.as_deref(), Some(&[[7u8; 16]][..]));
    }

    #[test]
    fn last_event_id_wins_over_the_from_lsn_still_in_the_url() {
        let q = SubscribeQuery {
            from_lsn: Some(10),
            ..query()
        };
        // A browser resends `Last-Event-ID` on an automatic reconnect while
        // the original `?from_lsn=` is still in the URL; honouring the URL
        // would rewind the feed to where it started.
        assert_eq!(
            q.to_request([7u8; 16], Some(99)).unwrap().from_lsn,
            Some(99)
        );

        let q = SubscribeQuery {
            from_lsn: Some(10),
            ..query()
        };
        assert_eq!(q.to_request([7u8; 16], None).unwrap().from_lsn, Some(10));
    }

    #[test]
    fn inflight_defaults_and_clamps() {
        assert_eq!(
            query().to_request([7u8; 16], None).unwrap().max_inflight,
            DEFAULT_MAX_INFLIGHT
        );
        assert_eq!(
            SubscribeQuery {
                max_inflight: Some(1_000_000),
                ..query()
            }
            .to_request([7u8; 16], None)
            .unwrap()
            .max_inflight,
            MAX_MAX_INFLIGHT
        );
    }

    #[test]
    fn an_unknown_kind_is_rejected_rather_than_ignored() {
        assert!(
            SubscribeQuery {
                kinds: Some("telepathic".into()),
                ..query()
            }
            .to_request([7u8; 16], None)
            .is_err(),
            "silently dropping it would give a feed that quietly filters \
             nothing the caller asked for"
        );
    }

    fn event(t: EventType) -> SubscriptionEvent {
        SubscriptionEvent {
            event_type: t,
            memory_id: 5,
            session_id: 1,
            text: "hello".into(),
            kind: MemoryKindWire::Semantic,
            salience: 0.5,
            timestamp_unix_nanos: 7,
            lsn: 42,
            graph_payload: None,
            edge_payload: None,
            stage_kind: None,
            stage_outcome: None,
            stage_payload: None,
        }
    }

    #[test]
    fn a_memory_id_on_the_feed_is_decimal_like_everywhere_else() {
        let dto = SubscriptionEventDto::from(event(EventType::Encoded));
        assert_eq!(dto.memory_id, "5");
        assert_eq!(dto.event_type, "encoded");
        assert_eq!(dto.lsn, 42);
    }

    #[test]
    fn graph_events_are_typed_not_opaque() {
        let mut e = event(EventType::EntityCreated);
        e.graph_payload = Some(GraphEventPayload::EntityCreated(EntityCreatedEvent {
            entity_id: [3u8; 16],
            entity_type_id: 1,
            canonical_name: "Ada".into(),
        }));
        let json = serde_json::to_value(SubscriptionEventDto::from(e)).unwrap();
        // Tagged by `type`, with the variant's own fields beside it — a
        // subscriber acts on these ids, so they are mapped rather than
        // handed over as an untyped blob.
        assert_eq!(json["graph"]["type"], "entity_created");
        assert_eq!(json["graph"]["canonical_name"], "Ada");
        assert!(json["graph"]["entity_id"].as_str().unwrap().contains('-'));
    }

    #[test]
    fn stage_events_carry_their_outcome_and_payload() {
        let mut e = event(EventType::StageCompleted);
        e.stage_kind = Some(StageKind::Hype);
        e.stage_outcome = Some(StageOutcome::Failed);
        e.stage_payload = Some(StagePayload::Hype(StageHypePayload {
            questions_written: 0,
            cost_micro_usd: 12,
            error_message: "no LLM provider key registered for this namespace".to_string(),
        }));
        let json = serde_json::to_value(SubscriptionEventDto::from(e)).unwrap();
        assert_eq!(json["stage_kind"], "hype");
        assert_eq!(json["stage_outcome"], "failed");
        assert_eq!(json["stage"]["type"], "hype");
        assert_eq!(json["stage"]["cost_micro_usd"], 12);
        // The event this test builds is a FAILED stage. Relaying the outcome
        // without the reason is what made a degraded write unreadable over
        // HTTP: two zeroed counters, and the cause left behind in Brain.
        assert_eq!(
            json["stage"]["error_message"],
            "no LLM provider key registered for this namespace",
        );
    }

    #[test]
    fn absent_payloads_are_omitted_not_nulled() {
        let json =
            serde_json::to_value(SubscriptionEventDto::from(event(EventType::Encoded))).unwrap();
        for key in ["graph", "edge", "stage", "stage_kind", "stage_outcome"] {
            assert!(
                json.get(key).is_none(),
                "`{key}` should be absent on an event that has none"
            );
        }
    }

    #[test]
    fn the_terminal_event_hands_back_where_the_feed_stopped() {
        let json = serde_json::to_value(SubscriptionClosedDto {
            final_lsn: 99,
            reason: "unsubscribed",
        })
        .unwrap();
        // This is what makes the next connection resume exactly rather than
        // guess, since unsubscribe is a client disconnect.
        assert_eq!(json["final_lsn"], 99);
        assert_eq!(json["reason"], "unsubscribed");
    }
}
