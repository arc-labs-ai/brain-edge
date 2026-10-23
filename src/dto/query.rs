//! Query-plan introspection — the public JSON contract for
//! `POST /v1/query/explain` (`QUERY_EXPLAIN`, `0x0161`) and
//! `POST /v1/query/trace` (`QUERY_TRACE`, `0x0162`).
//!
//! Both take the same query shape and differ in what they do with it: explain
//! returns the plan Brain WOULD run and its estimated cost without running it;
//! trace runs it and returns what actually happened, with the real latency.
//! So explain is free and trace is not, which is the whole reason they are two
//! verbs rather than one with a flag.
//!
//! There is no `POST /v1/query` that executes this shape and returns rows —
//! Brain has no such opcode, only these two. Retrieval is `/v1/recall`, whose
//! request is a different and much smaller thing. These exist to answer "why
//! was that slow" and "which retrievers would this hit", not to fetch
//! memories.

// Field names are the API; see `dto/space.rs` for the manifest note.
#![allow(missing_docs)]

use brain_db_sdk::wire::types::{
    FusionConfigWire, QueryExplainRequest, QueryExplainResponse, QueryRequest, QueryTraceRequest,
    QueryTraceResponse, RetrieverSelectionWire, RetrieverWire, TimeRangeWire,
};
use serde::{Deserialize, Serialize};

use super::parse_uuid;

/// Default row budget when the caller does not say.
const DEFAULT_LIMIT: u32 = 50;
/// Ceiling on that budget.
const MAX_LIMIT: u32 = 1000;

/// Per-query fusion weights.
#[derive(Debug, Deserialize, Serialize)]
pub struct FusionConfigDto {
    /// Reciprocal-rank-fusion constant.
    pub k: u32,
    pub semantic_weight: f32,
    pub lexical_weight: f32,
    pub graph_weight: f32,
}

impl From<FusionConfigDto> for FusionConfigWire {
    fn from(f: FusionConfigDto) -> Self {
        Self {
            k: f.k,
            semantic_weight: f.semantic_weight,
            lexical_weight: f.lexical_weight,
            graph_weight: f.graph_weight,
        }
    }
}

/// A bi-temporal window. Milliseconds, matching the wire — the one place in
/// this contract that is not nanos, and changing it here would hide that.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct TimeRangeDto {
    #[serde(default)]
    pub from_unix_ms: Option<u64>,
    #[serde(default)]
    pub to_unix_ms: Option<u64>,
}

/// `POST /v1/query/{explain,trace}` body.
#[derive(Debug, Default, Deserialize)]
pub struct QueryBody {
    /// The query text.
    pub text: String,
    /// Anchor the query on one entity (UUID).
    #[serde(default)]
    pub entity_anchor: Option<String>,
    /// Only these statement kinds (`fact`, `preference`, …).
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Only these predicates.
    #[serde(default)]
    pub predicates: Vec<String>,
    /// Only these sessions.
    #[serde(default)]
    pub sessions: Vec<u64>,
    /// Restrict to a time window.
    #[serde(default)]
    pub time_range: Option<TimeRangeDto>,
    /// Read the graph as it was known at this record time (unix nanos) —
    /// the "what did we believe then" axis, distinct from `time_range`,
    /// which is about when the content was true.
    #[serde(default)]
    pub as_of_record_time_unix_nanos: Option<u64>,
    /// Drop anything below this confidence.
    #[serde(default)]
    pub confidence_min: Option<f32>,
    /// Include tombstoned rows.
    #[serde(default)]
    pub include_tombstoned: bool,
    /// Include superseded versions rather than only chain heads.
    #[serde(default)]
    pub include_superseded: bool,
    /// Row budget.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Which retrievers to use: omitted or `["auto"]` lets Brain route, and a
    /// list of `semantic` / `lexical` / `graph` pins it. Pinning is what makes
    /// an explain answer "what would THIS plan cost" rather than "what would
    /// the router pick today".
    #[serde(default)]
    pub retrievers: Vec<String>,
    /// Per-query fusion override.
    #[serde(default)]
    pub fusion: Option<FusionConfigDto>,
}

/// The 1-based storage byte for a statement kind name.
fn kind_byte(raw: &str) -> Result<u8, String> {
    Ok(match raw.trim().to_ascii_lowercase().as_str() {
        "fact" => 1,
        "preference" => 2,
        "event" => 3,
        "attribute" => 4,
        "relation" => 5,
        "directive" => 6,
        other => return Err(format!("`{other}` is not a statement kind")),
    })
}

fn retriever(raw: &str) -> Result<RetrieverWire, String> {
    Ok(match raw.trim().to_ascii_lowercase().as_str() {
        "semantic" => RetrieverWire::Semantic,
        "lexical" => RetrieverWire::Lexical,
        "graph" => RetrieverWire::Graph,
        other => return Err(format!("`{other}` is not a retriever")),
    })
}

impl QueryBody {
    /// Build the wire query.
    ///
    /// # Errors
    /// Blank text, an anchor that is not a UUID, an unknown kind or
    /// retriever, or a confidence floor outside `[0,1]`.
    pub fn to_request(self) -> Result<QueryRequest, String> {
        let text = self.text.trim().to_owned();
        if text.is_empty() {
            return Err("text must not be blank".into());
        }
        if self
            .confidence_min
            .is_some_and(|c| !(0.0..=1.0).contains(&c))
        {
            return Err("confidence_min must be between 0.0 and 1.0".into());
        }
        // Empty or an explicit "auto" both mean let Brain route; anything else
        // pins the list.
        let retrievers = if self.retrievers.is_empty()
            || self
                .retrievers
                .iter()
                .any(|r| r.trim().eq_ignore_ascii_case("auto"))
        {
            RetrieverSelectionWire::Auto
        } else {
            RetrieverSelectionWire::Explicit(
                self.retrievers
                    .iter()
                    .map(|r| retriever(r))
                    .collect::<Result<Vec<_>, _>>()?,
            )
        };

        Ok(QueryRequest {
            text,
            entity_anchor: self.entity_anchor.as_deref().map(parse_uuid).transpose()?,
            kind_filter: self
                .kinds
                .iter()
                .map(|k| kind_byte(k))
                .collect::<Result<Vec<_>, _>>()?,
            predicate_filter: self.predicates,
            session_filter: (!self.sessions.is_empty()).then_some(self.sessions),
            time_filter: self.time_range.map(|t| TimeRangeWire {
                from_unix_ms: t.from_unix_ms,
                to_unix_ms: t.to_unix_ms,
            }),
            as_of_record_time_unix_nanos: self.as_of_record_time_unix_nanos,
            confidence_min: self.confidence_min,
            include_tombstoned: self.include_tombstoned,
            include_superseded: self.include_superseded,
            limit: self
                .limit
                .filter(|n| *n > 0)
                .map_or(DEFAULT_LIMIT, |n| n.min(MAX_LIMIT)),
            retrievers,
            fusion_config: self.fusion.map(Into::into),
            request_id: brain_db_sdk::new_id(),
        })
    }

    /// Wrap the query as an EXPLAIN.
    ///
    /// # Errors
    /// As [`Self::to_request`].
    pub fn to_explain(self) -> Result<QueryExplainRequest, String> {
        Ok(QueryExplainRequest {
            query: self.to_request()?,
        })
    }

    /// Wrap the query as a TRACE.
    ///
    /// # Errors
    /// As [`Self::to_request`].
    pub fn to_trace(self) -> Result<QueryTraceRequest, String> {
        Ok(QueryTraceRequest {
            query: self.to_request()?,
        })
    }
}

/// `POST /v1/query/explain` response.
#[derive(Debug, Serialize)]
pub struct QueryExplainResponseDto {
    /// The plan, as Brain renders it.
    pub plan_text: String,
    /// What Brain expects it to cost. An estimate, not a measurement — for
    /// the real figure, trace it.
    pub estimated_cost_ms: f32,
}

impl From<QueryExplainResponse> for QueryExplainResponseDto {
    fn from(r: QueryExplainResponse) -> Self {
        Self {
            plan_text: r.plan_text,
            estimated_cost_ms: r.estimated_cost_ms,
        }
    }
}

/// `POST /v1/query/trace` response.
#[derive(Debug, Serialize)]
pub struct QueryTraceResponseDto {
    /// What actually happened, stage by stage.
    pub trace_text: String,
    /// The measured latency. The query RAN to produce this.
    pub total_latency_ms: f64,
}

impl From<QueryTraceResponse> for QueryTraceResponseDto {
    fn from(r: QueryTraceResponse) -> Self {
        Self {
            trace_text: r.trace_text,
            total_latency_ms: r.total_latency_ms,
        }
    }
}
