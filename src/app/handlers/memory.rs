//! Memory verb handlers: encode / recall / forget.

use std::time::Instant;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Json;
use brain_db_sdk::wire::types::{
    AnswerKindWire, MemoryInspectRequest, RecallResponseFrame as RecallResponseFrameWire,
    RecallScopeWire, RetrieverNameWire,
};
use brain_db_sdk::{EncodeBuilder, ForgetBuilder, RecallBuilder};

use crate::dto::memory::{
    EncodeBody, EncodeResponseDto, EncodeVectorBody, ForgetBody, ForgetResponseDto,
    MaterializeProceduralBody, MaterializeProceduralResponseDto, MemoryInspectDto,
    MemoryListPageDto, MemoryListQuery, RecallBody, RecallResponseDto,
};
use crate::dto::parse_memory_id;
use crate::error::ApiError;
use crate::port::{Outcome, RecallSignals};
use crate::state::EdgeState;

/// `POST /v1/memories` — store a memory.
pub async fn encode(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Json(body): Json<EncodeBody>,
) -> Result<Json<EncodeResponseDto>, ApiError> {
    if body.text.trim().is_empty() {
        return Err(ApiError::bad_request("text must not be blank"));
    }
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    // Ack-only: no `.wait(WaitMode::Derived)`. The HTTP surface has no `wait`
    // field, so the async half of the write (embedding, HyPE, extraction) runs
    // after this returns. See the note on `EncodeBody` for why, and for what a
    // caller has to do about read-your-writes.
    let mut builder = EncodeBuilder::new(body.text);
    if let Some(sess) = body.session {
        builder = builder.session(sess);
    }
    if let Some(at) = body.occurred_at_unix_nanos {
        builder = builder.occurred_at(at);
    }
    let mut req = builder.build();
    ident.scope(&mut req);
    let started = Instant::now();
    let out = client.encode(&req).await;
    state.record("encode", &ident, Outcome::of(&out), started.elapsed());
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(EncodeResponseDto::from(resp)))
}

/// `POST /v1/recall` — recall memories for a cue.
pub async fn recall(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Json(body): Json<RecallBody>,
) -> Result<Json<RecallResponseDto>, ApiError> {
    if body.query.trim().is_empty() {
        return Err(ApiError::bad_request("query must not be blank"));
    }
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let mut builder = RecallBuilder::new(body.query)
        .include_edges(false)
        .include_graph(false)
        .include_text(true);
    if let Some(max) = body.max_results {
        builder = builder.max_results(max);
    }
    if let Some(subject) = body.subject {
        builder = builder.subject(subject);
    }
    if let Some(scope) = body.scope.as_deref() {
        let scope = match scope.to_ascii_lowercase().as_str() {
            "space" => RecallScopeWire::Space,
            "namespace" => RecallScopeWire::Namespace,
            other => {
                return Err(ApiError::bad_request(format!(
                    "scope must be \"space\" or \"namespace\", got {other:?}"
                )));
            }
        };
        builder = builder.scope(scope);
    }
    // `recall_frames`, not `recall`, when a trace was asked for: the server
    // puts the trace on the FINAL frame, and the aggregating `recall()` keeps
    // the memories and drops the frames it read them from. Without the trace
    // the aggregate is the cheaper call and stays the default path.
    //
    // Two independent reasons to ask for one, and they are not the same
    // question. The caller wants it in the response; the deployment's metering
    // sink wants the counts derived from it on every recall. Only the first
    // puts the blob in the body.
    let in_response = body.include_trace.unwrap_or(false);
    let traced = in_response || state.wants_recall_trace();
    if traced {
        builder = builder.trace(true);
    }
    let mut req = builder.build();
    ident.scope(&mut req);
    if traced {
        let started = Instant::now();
        let out = client.recall_frames(&req).await;
        let latency = started.elapsed();
        let signals = out.as_ref().ok().map(|f| recall_signals(f));
        state.record_recall("recall", &ident, Outcome::of(&out), Some(latency), signals);
        let frames = out.map_err(|e| ApiError::from_brain(&e))?;
        return Ok(Json(if in_response {
            RecallResponseDto::from_frames(frames)
        } else {
            RecallResponseDto::from_frames_untraced(frames)
        }));
    }
    let started = Instant::now();
    let out = client.recall(&req).await;
    state.record("recall", &ident, Outcome::of(&out), started.elapsed());
    let answer = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(RecallResponseDto::from(answer)))
}

/// `GET /v1/memories` — paginated enumeration of the caller's memories.
/// Not RECALL: no cue, no ranking. Walks the tenant timeline newest-first and
/// returns a page plus an opaque keyset cursor.
pub async fn list(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Query(query): Query<MemoryListQuery>,
) -> Result<Json<MemoryListPageDto>, ApiError> {
    let mut req = query.to_request().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.memory_list_frames(&req).await;
    state.record("memory_list", &ident, Outcome::of(&out), started.elapsed());
    let frames = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(MemoryListPageDto::from_frames(frames)))
}

/// `GET /v1/memories/{id}/inspect` — one memory's durable write-artifact
/// bundle (embedding vector, stored record, analyzed keyword terms, write-time
/// HyPE questions, extracted graph) + its text. An id that does not exist
/// under the caller's scope is a `404`, mapped from Brain's `MemoryNotFound`
/// like every other single-resource read here.
pub async fn inspect(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<MemoryInspectDto>, ApiError> {
    let wire_id = parse_memory_id(&id).map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let req = MemoryInspectRequest {
        memory_id: wire_id,
        act_as: ident.act_as.clone(),
    };
    let started = Instant::now();
    let out = client.memory_inspect(&req).await;
    state.record("memory_inspect", &ident, Outcome::of(&out), started.elapsed());
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(MemoryInspectDto::from(resp)))
}

/// `DELETE /v1/memories` — forget a memory.
pub async fn forget(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Json(body): Json<ForgetBody>,
) -> Result<Json<ForgetResponseDto>, ApiError> {
    let id = body.parse_id().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;

    let mut builder = ForgetBuilder::new(id);
    if body.hard {
        builder = builder.hard();
    }
    let mut req = builder.build();
    ident.scope(&mut req);
    let started = Instant::now();
    let out = client.forget(&req).await;
    state.record("forget", &ident, Outcome::of(&out), started.elapsed());
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(ForgetResponseDto::from(resp)))
}

/// `POST /v1/memories/vector` — store a memory with a caller-supplied
/// embedding (`ENCODE_VECTOR_DIRECT`).
///
/// Returns the same shape as `POST /v1/memories`: the difference is where the
/// vector came from, not what was stored.
pub async fn encode_vector(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    Json(body): Json<EncodeVectorBody>,
) -> Result<Json<EncodeResponseDto>, ApiError> {
    let mut req = body.to_request().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.encode_vector_direct(&req).await;
    state.record("encode_vector_direct", &ident, Outcome::of(&out), started.elapsed());
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(EncodeResponseDto::from(resp)))
}

/// `POST /v1/procedural/materialize` — render the caller's directives into a
/// system-prompt block (`MATERIALIZE_PROCEDURAL`).
pub async fn materialize_procedural(
    State(state): State<EdgeState>,
    headers: HeaderMap,
    body: Option<Json<MaterializeProceduralBody>>,
) -> Result<Json<MaterializeProceduralResponseDto>, ApiError> {
    // Optional body: every field is a filter, and asking for `{}` to get the
    // default rendering is friction for no gain.
    let body = body.map_or_else(MaterializeProceduralBody::default, |Json(b)| b);
    let mut req = body.to_request().map_err(ApiError::bad_request)?;
    let ident = state.resolve(&headers).await?;
    ident.scope(&mut req);
    let client = state.client_for(&ident.credential).await?;

    let started = Instant::now();
    let out = client.materialize_procedural(&req).await;
    state.record("materialize_procedural", &ident, Outcome::of(&out), started.elapsed());
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(MaterializeProceduralResponseDto::from(resp)))
}

/// Derive the metering sink's answer-quality signals from a recall's frames.
///
/// Reads the typed trace rather than the serialized blob: the sink only ever
/// receives these derived counts, never the trace itself. `stale_prevented` is
/// the one number here that reports something that did *not* happen — the
/// candidates the supersession filter removed, i.e. wrong answers the read
/// pipeline suppressed.
fn recall_signals(frames: &[RecallResponseFrameWire]) -> RecallSignals {
    let returned_n = frames.iter().map(|f| f.memories.len()).sum::<usize>();
    let confidence = frames
        .iter()
        .flat_map(|f| f.memories.iter())
        .map(|m| m.confidence)
        .fold(0.0_f32, f32::max);
    let answer_kind = frames.last().map_or(AnswerKindWire::None, |f| f.answer_kind);
    let Some(trace) = frames.iter().rev().find_map(|f| f.trace.as_ref()) else {
        return RecallSignals {
            returned_n: u32::try_from(returned_n).unwrap_or(u32::MAX),
            confidence,
            grounded: answer_kind != AnswerKindWire::None && returned_n > 0,
            ..RecallSignals::default()
        };
    };
    let lane = |name: RetrieverNameWire| {
        trace
            .retrievers
            .iter()
            .find(|r| r.name == name)
            .map_or(0, |r| r.candidate_count)
    };
    RecallSignals {
        returned_n: u32::try_from(returned_n).unwrap_or(u32::MAX),
        confidence,
        grounded: answer_kind != AnswerKindWire::None && returned_n > 0,
        semantic_candidates: lane(RetrieverNameWire::Semantic),
        lexical_candidates: lane(RetrieverNameWire::Lexical),
        graph_candidates: lane(RetrieverNameWire::Graph),
        stale_prevented: trace
            .filter_chain
            .before
            .saturating_sub(trace.filter_chain.after_supersession),
    }
}
