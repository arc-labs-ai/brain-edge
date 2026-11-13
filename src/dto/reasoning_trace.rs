//! PLAN / REASON traces, rendered as JSON with every memory id a string.
//!
//! The engine already builds both traces — `PlanTrace` is the full
//! bidirectional-BFS visited map plus every meeting point it found, and
//! `ReasonTrace` is the base candidate set, every edge the evidence walk
//! considered or pruned with the reason it was pruned, the un-collapsed score
//! components behind each confidence, and whether the topic centroid ran. A
//! request that does not ask for a trace pays nothing for it.
//!
//! WHY THIS IS BUILT BY HAND rather than handed to `serde_json::to_value`:
//! every id-bearing field in both traces is a raw 128-bit `WireMemoryId`, and
//! a JSON number cannot hold one — a browser's `JSON.parse` rounds it and
//! returns a different id, so "inspect this node" would open the wrong memory
//! or none at all. Every id this API returns is a decimal string, and the ids
//! inside a trace are no exception. The field names below are the wire's own,
//! so a reader that already understands one trace understands this one.

use brain_db_sdk::wire::types::{
    PlanTrace, PlanTraceMeetingPoint, PlanTraceNode, ReasonTrace, ReasonTraceBase,
    ReasonTraceCandidate, ReasonTraceCentroid, ReasonTraceEdgeCandidate, ReasonTraceIdWithText,
    ReasonTraceScoreBreakdown, ReasonTraceScoredId, ReasonTraceWalk,
};
use serde_json::{Value, json};

/// A trace as the string the response carries, or `None` when there is none.
///
/// A trace that will not serialize is not worth failing the operation over —
/// the steps are the answer and the trace is diagnostics — so the failure is
/// reported in place of the trace, exactly as `/v1/recall` does.
fn as_string(v: &Value) -> String {
    serde_json::to_string(v)
        .unwrap_or_else(|e| format!("{{\"error\":\"trace could not be serialized: {e}\"}}"))
}

/// One PLAN trace, as the response's `trace` string.
#[must_use]
pub fn plan_trace_string(t: &PlanTrace) -> String {
    as_string(&plan_trace_json(t))
}

/// One REASON trace, as the response's `trace` string.
#[must_use]
pub fn reason_trace_string(t: &ReasonTrace) -> String {
    as_string(&reason_trace_json(t))
}

fn plan_trace_node_json(n: &PlanTraceNode) -> Value {
    json!({
        "memory_id": n.memory_id.to_string(),
        "text": n.text,
        "direction": n.direction as u8,
        "depth": n.depth,
        "parent_edge": n.parent_edge.map(|id| id.to_string()),
        "alignment_score": n.alignment_score,
    })
}

fn plan_trace_meeting_point_json(m: &PlanTraceMeetingPoint) -> Value {
    json!({
        "memory_id": m.memory_id.to_string(),
        "text": m.text,
        "included_in_result": m.included_in_result,
    })
}

fn plan_trace_json(t: &PlanTrace) -> Value {
    json!({
        "explored": t.explored.iter().map(plan_trace_node_json).collect::<Vec<_>>(),
        "meeting_points": t
            .meeting_points
            .iter()
            .map(plan_trace_meeting_point_json)
            .collect::<Vec<_>>(),
    })
}

fn reason_trace_candidate_json(c: &ReasonTraceCandidate) -> Value {
    json!({
        "memory_id": c.memory_id.to_string(),
        "text": c.text,
        "score": c.score,
    })
}

fn reason_trace_base_json(b: &ReasonTraceBase) -> Value {
    json!({
        "candidates": b
            .candidates
            .iter()
            .map(reason_trace_candidate_json)
            .collect::<Vec<_>>(),
    })
}

fn reason_trace_edge_candidate_json(e: &ReasonTraceEdgeCandidate) -> Value {
    json!({
        "memory_id": e.memory_id.to_string(),
        "text": e.text,
        "edge_kind": e.edge_kind as u8,
        "depth": e.depth,
        "from_memory_id": e.from_memory_id.to_string(),
        "raw_score": e.raw_score,
    })
}

fn reason_trace_id_with_text_json(x: &ReasonTraceIdWithText) -> Value {
    json!({
        "memory_id": x.memory_id.to_string(),
        "text": x.text,
    })
}

fn reason_trace_scored_id_json(x: &ReasonTraceScoredId) -> Value {
    json!({
        "memory_id": x.memory_id.to_string(),
        "text": x.text,
        "score": x.score,
    })
}

fn reason_trace_walk_json(w: &ReasonTraceWalk) -> Value {
    json!({
        "considered": w
            .considered
            .iter()
            .map(reason_trace_edge_candidate_json)
            .collect::<Vec<_>>(),
        "dropped_by_edge_kind": w
            .dropped_by_edge_kind
            .iter()
            .map(reason_trace_edge_candidate_json)
            .collect::<Vec<_>>(),
        "dropped_by_tombstone": w
            .dropped_by_tombstone
            .iter()
            .map(reason_trace_id_with_text_json)
            .collect::<Vec<_>>(),
        "dropped_by_visited": w
            .dropped_by_visited
            .iter()
            .map(reason_trace_id_with_text_json)
            .collect::<Vec<_>>(),
        "dropped_by_confidence": w
            .dropped_by_confidence
            .iter()
            .map(reason_trace_scored_id_json)
            .collect::<Vec<_>>(),
        "dropped_by_max_supporting": w
            .dropped_by_max_supporting
            .iter()
            .map(reason_trace_id_with_text_json)
            .collect::<Vec<_>>(),
        "dropped_by_max_contradicting": w
            .dropped_by_max_contradicting
            .iter()
            .map(reason_trace_id_with_text_json)
            .collect::<Vec<_>>(),
    })
}

fn reason_trace_score_breakdown_json(s: &ReasonTraceScoreBreakdown) -> Value {
    json!({
        "memory_id": s.memory_id.to_string(),
        "text": s.text,
        "base_similarity": s.base_similarity,
        "decay": s.decay,
        "weight_product": s.weight_product,
        "alignment": s.alignment,
        "analogical_fit": s.analogical_fit,
        "final_score": s.final_score,
    })
}

fn reason_trace_centroid_json(c: &ReasonTraceCentroid) -> Value {
    json!({
        "computed": c.computed,
        "skipped_reason": c.skipped_reason,
    })
}

fn reason_trace_json(t: &ReasonTrace) -> Value {
    json!({
        "base": reason_trace_base_json(&t.base),
        "walk": reason_trace_walk_json(&t.walk),
        "scoring": t
            .scoring
            .iter()
            .map(reason_trace_score_breakdown_json)
            .collect::<Vec<_>>(),
        "centroid": reason_trace_centroid_json(&t.centroid),
    })
}

#[cfg(test)]
mod reasoning_trace_tests {
    use super::*;
    use brain_db_sdk::wire::types::{EdgeKindWire, PlanTraceDirection};

    /// An id too large for a JSON number must leave as a string, exactly as
    /// it arrived. This is the whole reason the builders are written by hand.
    #[test]
    fn plan_trace_ids_survive_as_strings() {
        let id: u128 = 664_082_786_657_838_825_472;
        let t = PlanTrace {
            explored: vec![PlanTraceNode {
                memory_id: id,
                text: "start".into(),
                direction: PlanTraceDirection::Backward,
                depth: 2,
                parent_edge: Some(id),
                alignment_score: Some(0.5),
            }],
            meeting_points: vec![PlanTraceMeetingPoint {
                memory_id: id,
                text: "met".into(),
                included_in_result: true,
            }],
        };
        let s = plan_trace_string(&t);
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["explored"][0]["memory_id"], json!(id.to_string()));
        assert_eq!(v["explored"][0]["parent_edge"], json!(id.to_string()));
        assert_eq!(v["explored"][0]["direction"], json!(1));
        assert_eq!(v["meeting_points"][0]["memory_id"], json!(id.to_string()));
        assert_eq!(v["meeting_points"][0]["included_in_result"], json!(true));
    }

    /// Every prune bucket is present even when empty, so a reader can tell
    /// "nothing was pruned here" from "this trace does not report it".
    #[test]
    fn reason_trace_reports_every_prune_bucket() {
        let id: u128 = 645_636_042_584_129_273_856;
        let t = ReasonTrace {
            base: ReasonTraceBase {
                candidates: vec![ReasonTraceCandidate {
                    memory_id: id,
                    text: "base".into(),
                    score: 0.8,
                }],
            },
            walk: ReasonTraceWalk {
                considered: vec![ReasonTraceEdgeCandidate {
                    memory_id: id,
                    text: "edge".into(),
                    edge_kind: EdgeKindWire::Supports,
                    depth: 1,
                    from_memory_id: id,
                    raw_score: 0.4,
                }],
                dropped_by_edge_kind: vec![],
                dropped_by_tombstone: vec![],
                dropped_by_visited: vec![ReasonTraceIdWithText {
                    memory_id: id,
                    text: "seen".into(),
                }],
                dropped_by_confidence: vec![ReasonTraceScoredId {
                    memory_id: id,
                    text: "weak".into(),
                    score: 0.1,
                }],
                dropped_by_max_supporting: vec![],
                dropped_by_max_contradicting: vec![],
            },
            scoring: vec![],
            centroid: ReasonTraceCentroid {
                computed: false,
                skipped_reason: Some("no topic".into()),
            },
        };
        let v: Value = serde_json::from_str(&reason_trace_string(&t)).unwrap();
        assert_eq!(
            v["base"]["candidates"][0]["memory_id"],
            json!(id.to_string())
        );
        assert_eq!(
            v["walk"]["considered"][0]["from_memory_id"],
            json!(id.to_string())
        );
        for bucket in [
            "dropped_by_edge_kind",
            "dropped_by_tombstone",
            "dropped_by_visited",
            "dropped_by_confidence",
            "dropped_by_max_supporting",
            "dropped_by_max_contradicting",
        ] {
            assert!(v["walk"][bucket].is_array(), "{bucket} must be reported");
        }
        assert_eq!(v["centroid"]["computed"], json!(false));
        assert_eq!(v["centroid"]["skipped_reason"], json!("no topic"));
    }
}
